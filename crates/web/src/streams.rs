// SPDX-License-Identifier: GPL-3.0-or-later
//! Playback: provider stream → this server → **ffmpeg's stdin** → HLS files → browser.
//!
//! The server fetches the provider's stream itself through the SSRF-safe client (for an
//! HLS source it walks the playlists with `opensmarters_core::hls` and fetches each
//! segment), and writes the bytes into `ffmpeg -i pipe:0`. ffmpeg never opens a URL:
//! its input protocol whitelist is `pipe`, its demuxers are limited to the containers
//! IPTV uses, and it only writes fragmented-MP4 HLS (`index.m3u8`, `init.mp4`,
//! `seg-NNNNN.m4s`) into a per-stream folder under `<data>/streams` that this server
//! owns. Video is copied (H.264; HEVC when the browser can play it) or converted to
//! H.264 when the instance allows it; audio is copied when it is AAC, else converted to
//! AAC (cheap). A short `ffprobe` of the first bytes (also stdin only) decides.
//!
//! Lifetimes and limits:
//!
//! - a stream nobody requested for `stream_idle_timeout` (30 s) is stopped: ffmpeg is
//!   killed, the provider connection closed (stream connections are never pooled), the
//!   folder deleted; `DELETE /api/streams/<sid>` does the same at once;
//! - at most `streams_per_user` streams per person (a new stream from the same browser
//!   session replaces the previous one *before* the new provider connection opens),
//!   `max_streams` for the instance, and the provider's own `max_connections`;
//! - films and catch-up are read only about two minutes ahead of what the browser
//!   fetched (flow control on the provider connection); a dropped connection is resumed
//!   with a `Range` request;
//! - a wall-clock watchdog: 12 h at most per stream, and a stream that stops producing
//!   segments for 90 s while it isn't waiting on the viewer is stopped.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use bytes::Bytes;
use opensmarters_core::hls::{self, HlsError, HlsLimits, Playlist as HlsPlaylist};
use opensmarters_core::url::Url;
use serde::Deserialize;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::watch;

use crate::config::{Config, TranscodeVideo};
use crate::error::{ApiError, ApiResult};
use crate::net::{Net, NetError, Purpose, Response};

/// HLS segment length (seconds).
const SEGMENT_SECS: u32 = 4;
/// Films are read at most this many segments ahead of the browser.
const MAX_AHEAD_SEGMENTS: i64 = 30;
/// Bytes collected before probing.
const PROBE_BYTES: usize = 2 * 1024 * 1024;
/// Longest wait for the probe bytes.
const PROBE_WAIT: Duration = Duration::from_secs(8);
/// Longest wait for the first playlist.
const START_WAIT: Duration = Duration::from_secs(30);
/// Wall-clock cap per stream.
const MAX_LIFETIME: Duration = Duration::from_secs(12 * 3600);
/// No new segment for this long (while not throttled) stops the stream.
const STALL_LIMIT: Duration = Duration::from_secs(90);
/// Demuxers ffmpeg may use.
const FORMATS: &str = "mpegts,matroska,mov,avi,flv,mpeg,aac,mp3";

/// What to play.
#[derive(Debug)]
pub struct StartSpec {
    /// Owner.
    pub user_id: String,
    /// The owner's browser session (zapping replaces streams of the same session).
    pub session: [u8; 32],
    /// Provider account.
    pub account_id: String,
    /// Identity of the provider subscription (for `max_connections`).
    pub provider_key: String,
    /// The provider's connection limit.
    pub provider_limit: Option<u32>,
    /// Stream address. **Secret** (credentials): never logged unredacted.
    pub url: Url,
    /// `User-Agent` for the stream request (M3U `#EXTVLCOPT`), validated.
    pub user_agent: Option<String>,
    /// Live (sliding window) or not (event playlist, flow control).
    pub live: bool,
    /// Duration known from the provider's metadata.
    pub duration_hint: Option<f64>,
    /// Start position (seconds) for films / episodes / catch-up.
    pub start: f64,
    /// The browser can play HEVC.
    pub hevc: bool,
}

/// A started stream.
#[derive(Debug, Clone)]
pub struct Started {
    /// Stream id.
    pub id: String,
    /// Live.
    pub live: bool,
    /// Duration (seconds), when known.
    pub duration: Option<f64>,
    /// Where the stream's time 0 is in the programme.
    pub start: f64,
}

/// One running stream.
#[derive(Debug)]
pub struct Stream {
    /// Id (random, 128-bit).
    pub id: String,
    /// Owner.
    pub user_id: String,
    session: [u8; 32],
    /// Provider account.
    pub account_id: String,
    provider_key: String,
    /// Output folder.
    pub dir: PathBuf,
    /// Live.
    pub live: bool,
    last_access: Mutex<Instant>,
    requested: AtomicI64,
    newest: AtomicI64,
    progress_at: Mutex<Instant>,
    throttled: AtomicBool,
    ended: AtomicBool,
    supervised: AtomicBool,
    ready: AtomicBool,
    stop_tx: watch::Sender<bool>,
    done_tx: watch::Sender<bool>,
    pid: Mutex<Option<u32>>,
    created: Instant,
}

impl Stream {
    fn touch(&self) {
        *self
            .last_access
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_access
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .elapsed()
    }

    fn stopping(&self) -> bool {
        *self.stop_tx.borrow()
    }

    /// The ffmpeg process id while it runs.
    pub fn ffmpeg_pid(&self) -> Option<u32> {
        *self.pid.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a request for a file of this stream (idle timer, flow control).
    pub fn note_request(&self, file: &str) {
        self.touch();
        if let Some(n) = segment_index(file) {
            self.requested.fetch_max(n, Ordering::Relaxed);
        }
    }

    fn scan_newest(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let newest = entries
            .filter_map(Result::ok)
            .filter_map(|e| segment_index(&e.file_name().to_string_lossy()))
            .max()
            .unwrap_or(-1);
        if newest > self.newest.swap(newest, Ordering::Relaxed) {
            *self
                .progress_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Instant::now();
        }
    }
}

/// `seg-00012.m4s` → 12.
pub fn segment_index(file: &str) -> Option<i64> {
    let n = file.strip_prefix("seg-")?.strip_suffix(".m4s")?;
    (n.len() >= 5 && n.len() <= 8 && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

/// Whether `file` is a name this server's streams produce.
pub fn valid_file(file: &str) -> bool {
    file == "index.m3u8" || file == "init.mp4" || segment_index(file).is_some()
}

/// All running streams and the limits.
#[derive(Debug)]
pub struct StreamManager {
    root: PathBuf,
    max_streams: usize,
    per_user: usize,
    idle: Duration,
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    transcode: TranscodeVideo,
    streams: Mutex<HashMap<String, Arc<Stream>>>,
}

fn limit_reached(message: String) -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "limit_reached", message).retry_after(30)
}

fn provider_error(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", message)
}

fn open_error(e: &NetError) -> ApiError {
    match e {
        NetError::Blocked(r) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "blocked_address", r.message())
        }
        NetError::Http(401 | 403) => provider_error(
            "Your provider refused the stream. Too many connections may be open; wait a minute and try again.",
        ),
        NetError::Http(404) => provider_error("Your provider doesn't have this stream any more."),
        NetError::Http(_) => {
            provider_error("Your provider couldn't start this stream. Try again in a moment.")
        }
        NetError::InsecureRedirect => provider_error(
            "Your provider tried to switch this stream from a secure to an unencrypted connection, which isn't allowed.",
        ),
        NetError::TooLarge(_) => {
            provider_error("Your provider sent a playlist larger than this server accepts.")
        }
        NetError::Timeout | NetError::Network(_) | NetError::InvalidUrl => {
            provider_error("Your provider can't be reached right now. Try again in a moment.")
        }
    }
}

fn unsupported(message: &str) -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_format",
        message,
    )
}

fn not_video() -> ApiError {
    provider_error("The provider sent something that isn't a video.")
}

/// Removes the stream from the map on drop unless disarmed (a start that failed or was
/// superseded).
struct Reservation<'a> {
    manager: &'a StreamManager,
    stream: Arc<Stream>,
    armed: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.manager
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.stream.id);
        let _ = self.stream.stop_tx.send(true);
        if !self.stream.supervised.load(Ordering::Relaxed) {
            // No ffmpeg was started: nothing else will clean up.
            let _ = std::fs::remove_dir_all(&self.stream.dir);
            let _ = self.stream.done_tx.send(true);
        }
    }
}

impl StreamManager {
    /// A manager writing under `<data>/streams` (emptied now).
    pub fn new(config: &Config) -> Self {
        // Absolute (ffmpeg runs inside the stream folder) and with native separators
        // (ffmpeg derives the init segment's folder from the playlist path).
        let root = config.data_dir.join("streams");
        let root = std::path::absolute(&root).unwrap_or(root);
        let root: PathBuf = root.components().collect();
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        StreamManager {
            root,
            max_streams: config.max_streams,
            per_user: config.streams_per_user,
            idle: config.stream_idle_timeout,
            ffmpeg: config.ffmpeg.clone(),
            ffprobe: config.ffprobe.clone(),
            transcode: config.transcode_video,
            streams: Mutex::new(HashMap::new()),
        }
    }

    /// A stream by id.
    pub fn get(&self, id: &str) -> Option<Arc<Stream>> {
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Number of streams.
    pub fn count(&self) -> usize {
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Every stream (tests, diagnostics).
    pub fn list(&self) -> Vec<Arc<Stream>> {
        self.streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    /// Stops a stream and waits (up to 10 s) until ffmpeg is gone, the provider
    /// connection closed and the folder deleted.
    pub async fn stop(&self, id: &str) {
        let removed = self
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(s) = removed {
            stop_and_wait(&s).await;
        }
    }

    /// Stops every stream of an account (account deleted or edited).
    pub async fn stop_account(&self, account_id: &str) {
        let ids: Vec<String> = self
            .list()
            .into_iter()
            .filter(|s| s.account_id == account_id)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.stop(&id).await;
        }
    }

    /// Stops every stream of a user (disabled or deleted).
    pub async fn stop_user(&self, user_id: &str) {
        let ids: Vec<String> = self
            .list()
            .into_iter()
            .filter(|s| s.user_id == user_id)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.stop(&id).await;
        }
    }

    /// Stops every stream of one browser session (sign-out).
    pub async fn stop_session(&self, session: &[u8; 32]) {
        let ids: Vec<String> = self
            .list()
            .into_iter()
            .filter(|s| &s.session == session)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.stop(&id).await;
        }
    }

    /// Stops idle and stalled streams; call every few seconds.
    pub async fn reap(&self) {
        let now_due: Vec<String> = self
            .list()
            .into_iter()
            .filter(|s| {
                // A stream that is still starting has its own time limits.
                if !s.ready.load(Ordering::Relaxed) {
                    return s.created.elapsed() > START_WAIT + PROBE_WAIT + Duration::from_secs(60);
                }
                let idle = s.idle_for() > self.idle;
                let stalled = !s.ended.load(Ordering::Relaxed)
                    && !s.throttled.load(Ordering::Relaxed)
                    && s.created.elapsed() > STALL_LIMIT
                    && s.progress_at
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .elapsed()
                        > STALL_LIMIT;
                let too_old = s.created.elapsed() > MAX_LIFETIME;
                if stalled {
                    tracing::warn!(stream = %s.id, "stream stalled; stopping it");
                }
                idle || stalled || too_old
            })
            .map(|s| s.id.clone())
            .collect();
        for id in now_due {
            tracing::info!(stream = %id, "stopping idle stream");
            self.stop(&id).await;
        }
    }

    /// Runs [`StreamManager::reap`] forever (spawned at start-up).
    pub async fn run_reaper(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            self.reap().await;
        }
    }

    fn reserve(&self, spec: &StartSpec) -> ApiResult<(Arc<Stream>, Vec<Arc<Stream>>)> {
        let mut map = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        let replaced: Vec<Arc<Stream>> = map
            .values()
            .filter(|s| s.user_id == spec.user_id && s.session == spec.session)
            .cloned()
            .collect();
        let is_replaced = |s: &Stream| replaced.iter().any(|r| r.id == s.id);
        let elsewhere = map
            .values()
            .filter(|s| s.user_id == spec.user_id && !is_replaced(s))
            .count();
        if elsewhere >= self.per_user {
            return Err(limit_reached(
                "You're already watching on another device. Stop playback there first, or wait a minute.".into(),
            ));
        }
        if map.len() - replaced.len() >= self.max_streams {
            return Err(limit_reached(
                "This server is already playing as many streams as it allows. Try again later."
                    .into(),
            ));
        }
        if let Some(limit) = spec.provider_limit.filter(|l| *l > 0) {
            let used = map
                .values()
                .filter(|s| s.provider_key == spec.provider_key && !is_replaced(s))
                .count();
            if used >= limit as usize {
                let streams = if limit == 1 {
                    "1 stream".to_owned()
                } else {
                    format!("{limit} streams")
                };
                return Err(limit_reached(format!(
                    "The provider allows {streams} at a time, and they're in use. Stop another stream, or wait a minute."
                )));
            }
        }
        for r in &replaced {
            map.remove(&r.id);
        }
        let id = crate::crypto::random_id().map_err(|_| ApiError::internal("random"))?;
        let (stop_tx, _) = watch::channel(false);
        let (done_tx, _) = watch::channel(false);
        let stream = Arc::new(Stream {
            dir: self.root.join(&id),
            id: id.clone(),
            user_id: spec.user_id.clone(),
            session: spec.session,
            account_id: spec.account_id.clone(),
            provider_key: spec.provider_key.clone(),
            live: spec.live,
            last_access: Mutex::new(Instant::now()),
            requested: AtomicI64::new(-1),
            newest: AtomicI64::new(-1),
            progress_at: Mutex::new(Instant::now()),
            throttled: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            supervised: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stop_tx,
            done_tx,
            pid: Mutex::new(None),
            created: Instant::now(),
        });
        map.insert(id, Arc::clone(&stream));
        Ok((stream, replaced))
    }

    /// Starts a stream: limits, provider connection, probe, ffmpeg, first playlist.
    ///
    /// # Errors
    ///
    /// `limit_reached` (503), `provider_error` / `blocked_address` (502),
    /// `unsupported_format` (415), `ffmpeg_missing` (503), `stream_replaced` (409).
    pub async fn start(&self, net: &Net, spec: StartSpec) -> ApiResult<Started> {
        let (stream, replaced) = self.reserve(&spec)?;
        let mut reservation = Reservation {
            manager: self,
            stream: Arc::clone(&stream),
            armed: true,
        };
        // The previous stream of this browser goes first: its provider connection must
        // be closed before a new one is opened.
        for r in &replaced {
            stop_and_wait(r).await;
        }
        let mut stop_rx = stream.stop_tx.subscribe();
        let result = tokio::select! {
            r = self.setup(net, &spec, &stream) => r,
            () = wait_stop(&mut stop_rx) => Err(ApiError::new(
                StatusCode::CONFLICT,
                "stream_replaced",
                "Another stream was started in the meantime.",
            )),
        };
        if result.is_ok() {
            reservation.armed = false;
        }
        result
    }

    async fn setup(&self, net: &Net, spec: &StartSpec, stream: &Arc<Stream>) -> ApiResult<Started> {
        tokio::fs::create_dir_all(&stream.dir)
            .await
            .map_err(|_| ApiError::internal("stream folder"))?;
        let mut source = Source::open(net, spec).await?;
        let skipped = source.skipped_secs();
        let mut head = Vec::new();
        let began = Instant::now();
        let probe_deadline = began + PROBE_WAIT;
        // Enough to probe: a full buffer, or what a real-time stream sent in a moment
        // (zapping must stay quick).
        let enough = |len: usize| {
            let t = began.elapsed();
            len >= PROBE_BYTES
                || (t >= Duration::from_millis(1500) && len >= 64 * 1024)
                || (t >= Duration::from_secs(4) && len >= 8 * 1024)
        };
        while !enough(head.len()) && Instant::now() < probe_deadline {
            let left = probe_deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(500));
            match tokio::time::timeout(left, source.next_chunk(net)).await {
                Ok(Ok(Some(chunk))) => head.extend_from_slice(&chunk),
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    if head.is_empty() {
                        return Err(open_error(&e));
                    }
                    break;
                }
                Err(_) => break,
            }
        }
        if head.is_empty() {
            return Err(provider_error(
                "Your provider sent no data for this stream.",
            ));
        }
        let probe = probe(&self.ffprobe, &head).await?;
        let plan = plan(&probe, spec.hevc, self.transcode)?;
        let ss = (spec.start - skipped).max(0.0);
        let args = ffmpeg_args(&stream.dir, &plan, ss, spec.live);
        let mut child = Command::new(&self.ffmpeg)
            .args(&args)
            .current_dir(&stream.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ffmpeg_missing())?;
        *stream.pid.lock().unwrap_or_else(PoisonError::into_inner) = child.id();
        tracing::info!(
            stream = %stream.id,
            live = spec.live,
            video = probe.video.as_deref().unwrap_or("none"),
            audio = probe.audio.as_deref().unwrap_or("none"),
            plan = ?plan,
            "stream started"
        );
        let stdin = child.stdin.take();
        if let Some(stderr) = child.stderr.take() {
            let id = stream.id.clone();
            let redactor = net.redactor().clone();
            tokio::spawn(drain_stderr(id, stderr, redactor));
        }
        // From here on the supervisor owns ffmpeg and the folder.
        stream.supervised.store(true, Ordering::Relaxed);
        let pump = tokio::spawn(pump(Arc::clone(stream), source, head, stdin, net.clone()));
        tokio::spawn(supervise(Arc::clone(stream), child, pump));

        // Wait for the first playlist (or a failure).
        let playlist = stream.dir.join("index.m3u8");
        let deadline = Instant::now() + START_WAIT;
        loop {
            if tokio::fs::metadata(&playlist).await.is_ok() {
                break;
            }
            if stream.ended.load(Ordering::Relaxed) || *stream.done_tx.borrow() {
                return Err(provider_error(
                    "This stream couldn't be prepared for the browser. Try again, or try another channel.",
                ));
            }
            if Instant::now() > deadline {
                return Err(provider_error(
                    "Your provider is too slow to start this stream. Try again in a moment.",
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        stream.touch();
        stream.ready.store(true, Ordering::Relaxed);
        let duration = if spec.live {
            None
        } else {
            spec.duration_hint
                .or(probe.duration)
                .filter(|d| d.is_finite() && *d > 0.0)
        };
        Ok(Started {
            id: stream.id.clone(),
            live: spec.live,
            duration,
            start: if spec.live { 0.0 } else { spec.start.max(0.0) },
        })
    }
}

fn ffmpeg_missing() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "ffmpeg_missing",
        "Playback isn't available: ffmpeg isn't installed on this server.",
    )
}

async fn stop_and_wait(s: &Stream) {
    let _ = s.stop_tx.send(true);
    let mut done = s.done_tx.subscribe();
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while !*done.borrow_and_update() {
            if done.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
}

async fn drain_stderr(
    id: String,
    mut stderr: tokio::process::ChildStderr,
    redactor: opensmarters_core::Redactor,
) {
    let mut buf = vec![0u8; 8192];
    let mut pending = String::new();
    let mut lines = 0usize;
    while let Ok(n) = stderr.read(&mut buf).await {
        if n == 0 {
            break;
        }
        pending.push_str(&String::from_utf8_lossy(&buf[..n]));
        while let Some(nl) = pending.find('\n') {
            let line: String = pending.drain(..=nl).collect();
            lines += 1;
            if lines <= 200 {
                tracing::debug!(stream = %id, "ffmpeg: {}", redactor.redact(line.trim_end()));
            }
        }
        if pending.len() > 16 * 1024 {
            pending.clear();
        }
    }
}

async fn wait_stop(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow_and_update() {
        if rx.changed().await.is_err() {
            // The sender lives in the stream; this only happens when it is dropped.
            std::future::pending::<()>().await;
        }
    }
}

async fn supervise(stream: Arc<Stream>, mut child: Child, pump: tokio::task::JoinHandle<()>) {
    let mut stop_rx = stream.stop_tx.subscribe();
    let natural = tokio::select! {
        () = wait_stop(&mut stop_rx) => false,
        status = child.wait() => {
            tracing::debug!(stream = %stream.id, ?status, "ffmpeg exited");
            true
        }
        () = tokio::time::sleep(MAX_LIFETIME) => false,
    };
    // Dropping the pump drops the provider response: the connection closes.
    pump.abort();
    let _ = pump.await;
    if natural {
        stream.ended.store(true, Ordering::Relaxed);
        *stream.pid.lock().unwrap_or_else(PoisonError::into_inner) = None;
        // Keep the files for the browser until the stream is stopped.
        wait_stop(&mut stop_rx).await;
    } else {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        *stream.pid.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    for _ in 0..20 {
        match tokio::fs::remove_dir_all(&stream.dir).await {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    tracing::info!(stream = %stream.id, "stream stopped");
    let _ = stream.done_tx.send(true);
}

async fn pump(
    stream: Arc<Stream>,
    mut source: Source,
    head: Vec<u8>,
    stdin: Option<ChildStdin>,
    net: Net,
) {
    let Some(mut stdin) = stdin else { return };
    if stdin.write_all(&head).await.is_err() {
        return;
    }
    drop(head);
    let mut last_scan = Instant::now();
    loop {
        if stream.stopping() {
            return;
        }
        if last_scan.elapsed() >= Duration::from_secs(1) {
            stream.scan_newest();
            last_scan = Instant::now();
        }
        if !stream.live {
            // Flow control: don't read far ahead of the browser.
            loop {
                let ahead = stream.newest.load(Ordering::Relaxed)
                    - stream.requested.load(Ordering::Relaxed);
                if ahead < MAX_AHEAD_SEGMENTS || stream.stopping() {
                    break;
                }
                stream.throttled.store(true, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(500)).await;
                stream.scan_newest();
            }
            stream.throttled.store(false, Ordering::Relaxed);
        }
        match source.next_chunk(&net).await {
            Ok(Some(chunk)) => {
                if stdin.write_all(&chunk).await.is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::info!(stream = %stream.id, error = %e, "provider stream ended with an error");
                break;
            }
        }
    }
    // End of input: ffmpeg finishes the playlist and exits.
    let _ = stdin.shutdown().await;
}

// ------------------------------------------------------------------------------------
// Sources
// ------------------------------------------------------------------------------------

enum Source {
    Http(Box<HttpSource>),
    Hls(Box<HlsSource>),
}

struct HttpSource {
    response: Response,
    url: Url,
    user_agent: Option<String>,
    read: u64,
    resumable: bool,
    retries: u32,
    pending: Option<Bytes>,
}

struct HlsSource {
    media: Url,
    live: bool,
    queue: VecDeque<hls::Segment>,
    next_seq: u64,
    current: Option<Response>,
    last_init: Option<Url>,
    reload_after: Duration,
    last_reload: Instant,
    last_new: Instant,
    ended: bool,
    errors: u32,
    skipped_secs: f64,
    user_agent: Option<String>,
}

fn is_hls_type(ct: Option<&str>) -> bool {
    ct.is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("mpegurl") || c.contains("x-mpegurl")
    })
}

fn hls_error(e: &HlsError) -> ApiError {
    match e {
        HlsError::Encrypted { .. } => {
            unsupported("This stream is encrypted, which the web edition can't play.")
        }
        HlsError::Unsupported { .. } | HlsError::NoVariants => {
            unsupported("This stream uses an HLS feature the web edition can't play.")
        }
        _ => not_video(),
    }
}

async fn fetch_media_playlist(net: &Net, url: &Url) -> ApiResult<(Url, hls::Media)> {
    let (body, _) = net
        .get(url, Purpose::Hls)
        .await
        .map_err(|e| open_error(&e))?;
    match hls::parse(&body, url, &HlsLimits::default()).map_err(|e| hls_error(&e))? {
        HlsPlaylist::Media(m) => Ok((url.clone(), m)),
        HlsPlaylist::Master(master) => {
            let variant = master
                .best()
                .ok_or_else(|| hls_error(&HlsError::NoVariants))?;
            let vurl = variant.uri.clone();
            let (body, _) = net
                .get(&vurl, Purpose::Hls)
                .await
                .map_err(|e| open_error(&e))?;
            match hls::parse(&body, &vurl, &HlsLimits::default()).map_err(|e| hls_error(&e))? {
                HlsPlaylist::Media(m) => Ok((vurl, m)),
                HlsPlaylist::Master(_) => Err(not_video()),
            }
        }
    }
}

impl Source {
    async fn open(net: &Net, spec: &StartSpec) -> ApiResult<Source> {
        let mut response = net
            .open(&spec.url, None, spec.user_agent.as_deref())
            .await
            .map_err(|e| open_error(&e))?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let final_url = response.url().clone();
        let mut first = Vec::new();
        while first.len() < 16 {
            match tokio::time::timeout(Duration::from_secs(20), net.chunk(&mut response)).await {
                Ok(Ok(Some(c))) => first.extend_from_slice(&c),
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(open_error(&e)),
                Err(_) => return Err(open_error(&NetError::Timeout)),
            }
        }
        if is_hls_type(content_type.as_deref()) || hls::looks_like_playlist(&first) {
            // Read the rest of the playlist (capped), then follow it ourselves.
            let mut body = first;
            while let Some(c) = net.chunk(&mut response).await.map_err(|e| open_error(&e))? {
                if body.len() + c.len() > HlsLimits::default().max_bytes {
                    return Err(open_error(&NetError::TooLarge(
                        HlsLimits::default().max_bytes as u64,
                    )));
                }
                body.extend_from_slice(&c);
            }
            drop(response);
            let (media_url, media) = match hls::parse(&body, &final_url, &HlsLimits::default())
                .map_err(|e| hls_error(&e))?
            {
                HlsPlaylist::Media(m) => (final_url, m),
                HlsPlaylist::Master(master) => {
                    let variant = master
                        .best()
                        .ok_or_else(|| hls_error(&HlsError::NoVariants))?;
                    fetch_media_playlist(net, &variant.uri.clone()).await?
                }
            };
            return Ok(Source::Hls(Box::new(HlsSource::new(
                media_url, media, spec,
            ))));
        }
        let resumable = !spec.live
            && (response
                .headers()
                .get(reqwest::header::ACCEPT_RANGES)
                .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"bytes"))
                || response.content_length().is_some());
        Ok(Source::Http(Box::new(HttpSource {
            response,
            url: final_url,
            user_agent: spec.user_agent.clone(),
            read: first.len() as u64,
            resumable,
            retries: 0,
            pending: Some(Bytes::from(first)),
        })))
    }

    fn skipped_secs(&self) -> f64 {
        match self {
            Source::Http(_) => 0.0,
            Source::Hls(h) => h.skipped_secs,
        }
    }

    async fn next_chunk(&mut self, net: &Net) -> Result<Option<Bytes>, NetError> {
        match self {
            Source::Http(h) => h.next_chunk(net).await,
            Source::Hls(h) => h.next_chunk(net).await,
        }
    }
}

impl HttpSource {
    async fn next_chunk(&mut self, net: &Net) -> Result<Option<Bytes>, NetError> {
        if let Some(p) = self.pending.take() {
            return Ok(Some(p));
        }
        loop {
            match net.chunk(&mut self.response).await {
                Ok(Some(c)) => {
                    self.read += c.len() as u64;
                    return Ok(Some(c));
                }
                Ok(None) => return Ok(None),
                Err(e) if self.resumable && self.retries < 3 => {
                    self.retries += 1;
                    tracing::info!(error = %e, "film connection dropped; resuming with a range request");
                    let response = net
                        .open(&self.url, Some(self.read), self.user_agent.as_deref())
                        .await?;
                    let range_ok = response.status() == reqwest::StatusCode::PARTIAL_CONTENT
                        && response
                            .headers()
                            .get(reqwest::header::CONTENT_RANGE)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.strip_prefix("bytes "))
                            .and_then(|v| v.split('-').next())
                            .and_then(|v| v.trim().parse::<u64>().ok())
                            == Some(self.read);
                    if !range_ok {
                        return Err(e);
                    }
                    self.response = response;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl HlsSource {
    fn new(media_url: Url, media: hls::Media, spec: &StartSpec) -> Self {
        let live = !media.ended;
        let mut segments: VecDeque<hls::Segment> = media.segments.into_iter().collect();
        let mut skipped_secs = 0.0;
        if live {
            // Start near the live edge.
            while segments.len() > 3 {
                segments.pop_front();
            }
        } else if spec.start > 0.0 {
            while let Some(s) = segments.front() {
                if skipped_secs + s.duration > spec.start || segments.len() == 1 {
                    break;
                }
                skipped_secs += s.duration;
                segments.pop_front();
            }
        }
        let next_seq = segments
            .back()
            .map_or(media.media_sequence, |s| s.sequence + 1);
        let reload_after = hls::Media {
            target_duration: media.target_duration,
            ..hls::Media::default()
        }
        .reload_after();
        HlsSource {
            media: media_url,
            live,
            queue: segments,
            next_seq,
            current: None,
            last_init: None,
            reload_after,
            last_reload: Instant::now(),
            last_new: Instant::now(),
            ended: !live,
            errors: 0,
            skipped_secs,
            user_agent: spec.user_agent.clone(),
        }
    }

    async fn reload(&mut self, net: &Net) -> Result<(), NetError> {
        let wait = self.reload_after.saturating_sub(self.last_reload.elapsed());
        tokio::time::sleep(wait).await;
        self.last_reload = Instant::now();
        let (body, _) = net.get(&self.media, Purpose::Hls).await?;
        let Ok(HlsPlaylist::Media(m)) = hls::parse(&body, &self.media, &HlsLimits::default())
        else {
            return Err(NetError::Network(
                "the live playlist became unreadable".into(),
            ));
        };
        let mut added = false;
        for s in m.segments {
            if s.sequence >= self.next_seq {
                self.next_seq = s.sequence + 1;
                self.queue.push_back(s);
                added = true;
            }
        }
        if added {
            self.last_new = Instant::now();
        }
        if m.ended {
            self.ended = true;
        }
        Ok(())
    }

    async fn next_chunk(&mut self, net: &Net) -> Result<Option<Bytes>, NetError> {
        loop {
            if let Some(resp) = self.current.as_mut() {
                match net.chunk(resp).await {
                    Ok(Some(c)) => return Ok(Some(c)),
                    Ok(None) => {
                        self.current = None;
                        self.errors = 0;
                        continue;
                    }
                    Err(e) => {
                        self.current = None;
                        self.errors += 1;
                        if self.errors > 5 {
                            return Err(e);
                        }
                        continue;
                    }
                }
            }
            // The segment leaves the queue only once it is open, so a cancelled call
            // (the probe's time limit) loses nothing.
            if let Some(seg) = self.queue.front().cloned() {
                if let Some(init) = seg.init.clone()
                    && self.last_init.as_ref() != Some(&init)
                {
                    let (bytes, _) = net.get(&init, Purpose::Hls).await?;
                    self.last_init = Some(init);
                    return Ok(Some(Bytes::from(bytes)));
                }
                match net.open(&seg.uri, None, self.user_agent.as_deref()).await {
                    Ok(r) => {
                        self.queue.pop_front();
                        self.current = Some(r);
                    }
                    Err(e @ NetError::Blocked(_)) => return Err(e),
                    Err(e) => {
                        self.queue.pop_front();
                        self.errors += 1;
                        if self.errors > 5 {
                            return Err(e);
                        }
                    }
                }
                continue;
            }
            if self.ended {
                return Ok(None);
            }
            if self.live && self.last_new.elapsed() > Duration::from_secs(60) {
                return Ok(None);
            }
            self.reload(net).await?;
        }
    }
}

// ------------------------------------------------------------------------------------
// Probe and plan
// ------------------------------------------------------------------------------------

/// What `ffprobe` found.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// First video codec.
    pub video: Option<String>,
    /// First audio codec.
    pub audio: Option<String>,
    /// Container duration.
    pub duration: Option<f64>,
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    codec_name: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    #[serde(default)]
    duration: Option<String>,
}

async fn probe(ffprobe: &Path, head: &[u8]) -> ApiResult<Probe> {
    let mut child = Command::new(ffprobe)
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-protocol_whitelist",
            "pipe",
            "-format_whitelist",
            FORMATS,
            "-probesize",
            "2000000",
            "-analyzeduration",
            "6000000",
            "-show_entries",
            "stream=codec_type,codec_name:format=duration",
            "-of",
            "json",
            "-i",
            "pipe:0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ffmpeg_missing())?;
    let mut stdin = child.stdin.take();
    let data = head.to_vec();
    let writer = tokio::spawn(async move {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(&data).await;
            let _ = s.shutdown().await;
        }
    });
    let mut out = Vec::new();
    let read = async {
        if let Some(mut stdout) = child.stdout.take() {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 || out.len() > 256 * 1024 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
        }
        child.wait().await
    };
    let status = tokio::time::timeout(Duration::from_secs(20), read).await;
    writer.abort();
    if status.is_err() {
        let _ = child.start_kill();
        return Err(not_video());
    }
    let parsed: ProbeJson = serde_json::from_slice(&out).map_err(|_| not_video())?;
    let first = |kind: &str| {
        parsed
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some(kind))
            .and_then(|s| s.codec_name.clone())
            .map(|c| {
                c.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(32)
                    .collect::<String>()
            })
    };
    let probe = Probe {
        video: first("video"),
        audio: first("audio"),
        duration: parsed
            .format
            .and_then(|f| f.duration)
            .and_then(|d| d.trim().parse::<f64>().ok())
            .filter(|d| d.is_finite() && *d > 0.0),
    };
    if probe.video.is_none() && probe.audio.is_none() {
        return Err(not_video());
    }
    Ok(probe)
}

/// How the video track is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPlan {
    /// Stream copy (`hevc`: tag as `hvc1`).
    Copy {
        /// HEVC.
        hevc: bool,
    },
    /// Convert to H.264.
    Transcode,
}

/// How the audio track is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlan {
    /// Stream copy (AAC).
    Copy,
    /// Convert to AAC stereo.
    Aac,
}

/// The ffmpeg plan for one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Video handling (`None`: no video, e.g. radio).
    pub video: Option<VideoPlan>,
    /// Audio handling (`None`: no audio).
    pub audio: Option<AudioPlan>,
}

/// Decides copy / convert from the probe, the browser's HEVC support and the
/// instance policy.
///
/// # Errors
///
/// 415 `unsupported_format` when the browser can't play the video and the policy
/// doesn't allow converting it.
pub fn plan(probe: &Probe, browser_hevc: bool, policy: TranscodeVideo) -> ApiResult<Plan> {
    let video = match probe.video.as_deref() {
        None => None,
        Some("h264") => Some(VideoPlan::Copy { hevc: false }),
        Some("hevc") if browser_hevc => Some(VideoPlan::Copy { hevc: true }),
        Some("hevc") if policy != TranscodeVideo::Never => Some(VideoPlan::Transcode),
        Some("hevc") => {
            return Err(unsupported(
                "This video is in HEVC (H.265), which your browser can't play, and this server doesn't convert video.",
            ));
        }
        Some(_) if policy == TranscodeVideo::Always => Some(VideoPlan::Transcode),
        Some(_) => {
            return Err(unsupported(
                "This video uses a format browsers can't play, and this server doesn't convert video.",
            ));
        }
    };
    let audio = match probe.audio.as_deref() {
        None => None,
        Some("aac") => Some(AudioPlan::Copy),
        Some(_) => Some(AudioPlan::Aac),
    };
    Ok(Plan { video, audio })
}

/// The ffmpeg command line (everything after the program name).
pub fn ffmpeg_args(dir: &Path, plan: &Plan, ss: f64, live: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |s: &str| a.push(OsString::from(s));
    for s in [
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "warning",
        "-protocol_whitelist",
        "pipe",
        "-format_whitelist",
        FORMATS,
        "-probesize",
        "2000000",
        "-analyzeduration",
        "3000000",
        "-fflags",
        "+genpts+discardcorrupt",
    ] {
        push(s);
    }
    if ss > 0.05 {
        push("-ss");
        push(&format!("{ss:.3}"));
    }
    for s in ["-i", "pipe:0"] {
        push(s);
    }
    if plan.video.is_some() {
        push("-map");
        push("0:v:0");
    }
    if plan.audio.is_some() {
        push("-map");
        push("0:a:0");
    }
    match plan.video {
        Some(VideoPlan::Copy { hevc }) => {
            push("-c:v");
            push("copy");
            if hevc {
                push("-tag:v");
                push("hvc1");
            }
        }
        Some(VideoPlan::Transcode) => {
            for s in [
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
                "-vf",
                "scale=w='min(1920,iw)':h=-2",
                "-sc_threshold",
                "0",
            ] {
                push(s);
            }
            push("-force_key_frames");
            push(&format!("expr:gte(t,n_forced*{SEGMENT_SECS})"));
        }
        None => {}
    }
    match plan.audio {
        Some(AudioPlan::Copy) => {
            // ADTS (MPEG-TS) AAC must become raw AAC in MP4; a no-op for other sources.
            for s in ["-c:a", "copy", "-bsf:a", "aac_adtstoasc"] {
                push(s);
            }
        }
        Some(AudioPlan::Aac) => {
            for s in ["-c:a", "aac", "-b:a", "160k", "-ac", "2", "-ar", "48000"] {
                push(s);
            }
        }
        None => {}
    }
    for s in [
        "-sn",
        "-dn",
        "-max_muxing_queue_size",
        "4096",
        "-f",
        "hls",
        "-hls_time",
    ] {
        push(s);
    }
    push(&SEGMENT_SECS.to_string());
    for s in [
        "-hls_segment_type",
        "fmp4",
        "-hls_fmp4_init_filename",
        "init.mp4",
        "-hls_segment_filename",
    ] {
        push(s);
    }
    a.push(dir.join("seg-%05d.m4s").into_os_string());
    if live {
        for s in [
            "-hls_flags",
            "delete_segments+independent_segments+temp_file",
            // Short first segments: a channel starts faster.
            "-hls_init_time",
            "2",
            "-hls_list_size",
            "8",
            "-hls_delete_threshold",
            "4",
        ] {
            a.push(OsString::from(s));
        }
    } else {
        for s in [
            "-hls_flags",
            "independent_segments+temp_file",
            "-hls_playlist_type",
            "event",
            "-hls_list_size",
            "0",
        ] {
            a.push(OsString::from(s));
        }
    }
    a.push(OsString::from("-y"));
    a.push(dir.join("index.m3u8").into_os_string());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert!(valid_file("index.m3u8"));
        assert!(valid_file("init.mp4"));
        assert!(valid_file("seg-00001.m4s"));
        assert_eq!(segment_index("seg-00012.m4s"), Some(12));
        assert!(!valid_file("../index.m3u8"));
        assert!(!valid_file("seg-1.m4s"));
        assert!(!valid_file("seg-00001.m4s.tmp"));
        assert!(!valid_file("index.m3u8.tmp"));
    }

    #[test]
    fn plans() {
        let p = |v: Option<&str>, a: Option<&str>| Probe {
            video: v.map(str::to_owned),
            audio: a.map(str::to_owned),
            duration: None,
        };
        let never = TranscodeVideo::Never;
        assert_eq!(
            plan(&p(Some("h264"), Some("ac3")), false, never).ok(),
            Some(Plan {
                video: Some(VideoPlan::Copy { hevc: false }),
                audio: Some(AudioPlan::Aac)
            })
        );
        assert_eq!(
            plan(&p(Some("hevc"), Some("aac")), true, never)
                .ok()
                .and_then(|p| p.video),
            Some(VideoPlan::Copy { hevc: true })
        );
        assert!(plan(&p(Some("hevc"), None), false, never).is_err());
        assert_eq!(
            plan(&p(Some("hevc"), None), false, TranscodeVideo::Hevc)
                .ok()
                .and_then(|p| p.video),
            Some(VideoPlan::Transcode)
        );
        assert!(plan(&p(Some("mpeg2video"), None), false, TranscodeVideo::Hevc).is_err());
        assert_eq!(
            plan(&p(Some("mpeg2video"), None), false, TranscodeVideo::Always)
                .ok()
                .and_then(|p| p.video),
            Some(VideoPlan::Transcode)
        );
        assert_eq!(
            plan(&p(None, Some("mp3")), false, never)
                .ok()
                .and_then(|p| p.audio),
            Some(AudioPlan::Aac)
        );
    }

    #[test]
    fn ffmpeg_reads_only_stdin() {
        let plan = Plan {
            video: Some(VideoPlan::Copy { hevc: false }),
            audio: Some(AudioPlan::Copy),
        };
        let args: Vec<String> = ffmpeg_args(Path::new("out"), &plan, 0.0, true)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let i = args.iter().position(|a| a == "-i").unwrap_or(0);
        assert_eq!(args[i + 1], "pipe:0");
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 1);
        let w = args
            .iter()
            .position(|a| a == "-protocol_whitelist")
            .unwrap_or(0);
        assert_eq!(args[w + 1], "pipe");
        assert!(w < i);
        assert!(!args.iter().any(|a| a.contains("://")));
    }
}

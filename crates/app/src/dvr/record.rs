// SPDX-License-Identifier: GPL-3.0-or-later
//! A live recording job: records a channel into `<name>.part` until the end of its
//! window (programme + padding, or "until I stop" with the 8-hour cap), then renames it.
//!
//! - **Source:** rebuilt from the profile ([`super::source::live_source`]): Xtream `.ts`
//!   when allowed, otherwise what the player would open.
//! - **Direct streams** (TS…): the body is appended as it arrives; the first bytes must
//!   be video. When the stream ends or drops, the job reconnects after 2, 5, 10, 30 s
//!   (then every 30 s) until the end, and keeps appending to the same file.
//! - **HLS** (detected by the content, `#EXTM3U`): the master playlist's best variant
//!   with its audio inside ([`opensmarters_core::hls`]), then the media playlist is
//!   reloaded every half target duration and new segments are fetched in order and
//!   appended (TS, or fragmented MP4 with its init section first). A live playlist starts
//!   at its last three segments. Encrypted streams fail with a clear message.
//! - Never preempted by viewing (its lease is a `Recording`); stops on "Stop recording"
//!   (keeps the file), cancel (deletes it), app exit (continues next time), or when free
//!   space runs low (keeps what was recorded).

use std::sync::Arc;
use std::time::{Duration, Instant};

use opensmarters_core::chrono::{DateTime, Utc};
use opensmarters_core::download::DownloadRequest;
use opensmarters_core::hls::{self, HlsError, HlsLimits, Media, Playlist};
use opensmarters_core::url::Url;
use opensmarters_player::{Container, SNIFF_BYTES, sniff_container};
use opensmarters_store::{Recording, RecordingId, RecordingState};
use tokio::sync::watch;

use super::engine::{Control, INTERRUPTED, Shared, live_window};
use super::jobs::{
    Meter, NOT_VIDEO, PartWriter, backoff, disk_ok, failure_message, low_disk_message, next_control,
};
use super::{files, source};
use crate::services::connections::Lease;

/// Largest playlist fetched.
const MAX_PLAYLIST: u64 = 4 * 1024 * 1024;
/// Largest fMP4 init section fetched.
const MAX_INIT: u64 = 16 * 1024 * 1024;
/// Segments a live playlist starts with.
const LIVE_EDGE: usize = 3;

/// How a run ended.
#[derive(Debug)]
enum Outcome {
    /// Keep the file (an optional note for the row).
    Done(Option<String>),
    Failed(String),
    Cancelled,
    Interrupted,
}

/// Why one connection ended.
#[derive(Debug)]
enum Stop {
    /// The window is over.
    Deadline,
    /// A control message.
    Control(Control),
    /// Not recordable at all (stop retrying).
    Fatal(String),
    /// The stream ended or dropped (reconnect).
    Lost(String),
    /// The disk is nearly full.
    Disk,
}

/// Runs the recording of row `id` while holding `lease`.
pub(crate) async fn run(
    sh: Arc<Shared>,
    id: RecordingId,
    lease: Lease,
    mut control: watch::Receiver<Control>,
) {
    let Some(Some(rec)) = sh
        .store("load a recording", move |s| s.recordings().get(id))
        .await
    else {
        return;
    };
    if !matches!(rec.state, RecordingState::Queued | RecordingState::Active) {
        return;
    }
    sh.set_state(id, RecordingState::Active, None).await;
    let outcome = record(&sh, &rec, &mut control).await;
    drop(lease);
    tracing::info!(%id, ?outcome, "recording ended");
    match outcome {
        Outcome::Done(note) => sh.set_state(id, RecordingState::Done, note).await,
        Outcome::Failed(msg) => sh.set_state(id, RecordingState::Failed, Some(msg)).await,
        Outcome::Interrupted => {
            sh.set_state(id, RecordingState::Paused, Some(INTERRUPTED.to_owned()))
                .await;
        }
        Outcome::Cancelled => super::download::remove(&sh, id).await,
    }
}

/// Where one recording writes, and what it has seen so far.
struct Rec<'a> {
    sh: &'a Arc<Shared>,
    id: RecordingId,
    writer: PartWriter,
    container: Option<Container>,
    deadline: tokio::time::Instant,
    dir: std::path::PathBuf,
    keep_free: u64,
    meter: Meter,
    last_store: Instant,
    next_disk_check: u64,
    user_agent: Option<String>,
    /// HLS: the newest segment written, and the init section in effect.
    last_seq: Option<u64>,
    init: Option<Url>,
    bad_segments: u32,
}

impl Rec<'_> {
    /// Writes `data` (after the first-bytes check has passed) and publishes progress.
    async fn write(&mut self, data: &[u8]) -> Result<(), Stop> {
        if let Err(e) = self.writer.write(data).await {
            tracing::warn!(error = %e, "recording write failed");
            return Err(Stop::Disk);
        }
        let bytes = self.writer.len();
        let rate = self.meter.update(bytes);
        self.sh.set_progress(self.id, |p| {
            p.bytes = bytes;
            p.rate = rate;
            p.note = None;
        });
        if self.last_store.elapsed() >= super::engine::PROGRESS_EVERY {
            self.last_store = Instant::now();
            let id = self.id;
            self.sh
                .store("save recording progress", move |s| {
                    s.recordings().set_progress(id, bytes, None)
                })
                .await;
        }
        if bytes >= self.next_disk_check {
            self.next_disk_check = bytes + super::engine::DISK_CHECK_BYTES;
            if !disk_ok(&self.dir, self.keep_free).await {
                return Err(Stop::Disk);
            }
        }
        Ok(())
    }

    /// Checks the first bytes of a body (or segment) and writes them. `None` = not video.
    async fn accept_head(&mut self, head: &[u8]) -> Result<bool, Stop> {
        let ok = match self.container {
            None => match sniff_container(head) {
                Some(c) => {
                    self.container = Some(c);
                    true
                }
                None => false,
            },
            Some(c) => files::segment_looks_like(c, head),
        };
        if ok {
            self.write(head).await?;
        }
        Ok(ok)
    }
}

#[allow(clippy::too_many_lines)]
async fn record(
    sh: &Arc<Shared>,
    rec: &Recording,
    control: &mut watch::Receiver<Control>,
) -> Outcome {
    let settings = sh.settings();
    let Some((_, end)) = live_window(rec, &settings) else {
        return Outcome::Failed("This recording has no time window.".to_owned());
    };
    let now = Utc::now();
    let cap = now + to_chrono(super::MAX_RECORDING);
    let end = end.min(cap);
    if end <= now {
        return Outcome::Failed(super::engine::MISSED.to_owned());
    }
    let dir = {
        let sh2 = Arc::clone(sh);
        tokio::task::spawn_blocking(move || sh2.dir())
            .await
            .ok()
            .flatten()
    };
    let Some(dir) = dir else {
        return Outcome::Failed(
            "The recordings folder can't be used. Choose another one in Settings › \
             Recordings & downloads."
                .to_owned(),
        );
    };
    let (_channel, source) = match source::live_source(&sh.session, &rec.item_id).await {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e.user_message()),
    };
    let Ok(url) = source.http_url() else {
        return Outcome::Failed("This channel's address can't be recorded.".to_owned());
    };

    let name = match &rec.file_name {
        Some(n) => n.clone(),
        None => {
            let taken = sh.taken_names().await;
            let when = rec
                .start_at
                .and_then(|t| DateTime::from_timestamp_millis(t.as_unix_millis()))
                .unwrap_or(now);
            let base = super::file_name(&rec.title, when, "ts");
            let d = dir.clone();
            let Some(name) =
                tokio::task::spawn_blocking(move || files::unique_name(&d, &taken, &base))
                    .await
                    .ok()
                    .flatten()
            else {
                return Outcome::Failed(
                    "No free file name was found in the recordings folder.".to_owned(),
                );
            };
            let (id, n) = (rec.id, name.clone());
            sh.store("save a file name", move |s| {
                s.recordings().set_file_name(id, &n)
            })
            .await;
            name
        }
    };
    let Some(part) = files::part_path(&dir, &name) else {
        return Outcome::Failed("This file name isn't valid.".to_owned());
    };
    let exists = {
        let p = part.clone();
        tokio::task::spawn_blocking(move || p.exists())
            .await
            .unwrap_or(false)
    };
    let writer = match PartWriter::open(&part, !exists).await {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "couldn't open a recording file");
            return Outcome::Failed(
                "Couldn't write to the recordings folder. Check that it still exists and \
                 that you can write to it."
                    .to_owned(),
            );
        }
    };
    let container = if !writer.is_empty() {
        let p = part.clone();
        tokio::task::spawn_blocking(move || files::sniff_file(&p))
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let left = (end - now).to_std().unwrap_or(Duration::ZERO);
    let mut r = Rec {
        sh,
        id: rec.id,
        meter: Meter::new(writer.len()),
        next_disk_check: writer.len(),
        writer,
        container,
        deadline: tokio::time::Instant::now() + left,
        dir: dir.clone(),
        keep_free: settings.keep_free_bytes(),
        last_store: Instant::now(),
        user_agent: source
            .headers
            .user_agent
            .as_ref()
            .map(|u| u.as_str().to_owned()),
        last_seq: None,
        init: None,
        bad_segments: 0,
    };
    let referrer = source
        .headers
        .referrer
        .as_ref()
        .map(|x| x.as_str().to_owned());

    let mut attempt: u32 = 0;
    let stop = loop {
        if !disk_ok(&dir, r.keep_free).await {
            break Stop::Disk;
        }
        let before = r.writer.len();
        let ended = connect_once(&mut r, &url, referrer.as_deref(), control).await;
        match ended {
            Stop::Lost(why) => {
                if r.writer.len() > before {
                    attempt = 0;
                }
                let delay = backoff(attempt);
                attempt = attempt.saturating_add(1);
                tracing::info!(attempt, reason = %why, "recording reconnecting");
                let n = attempt;
                sh.set_progress(r.id, |p| {
                    p.note = Some(format!("Reconnecting… ({n})"));
                });
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = tokio::time::sleep_until(r.deadline) => break Stop::Deadline,
                    c = next_control(control) => break Stop::Control(c),
                }
            }
            other => break other,
        }
    };

    let id = r.id;
    let bytes = r.writer.close().await;
    match stop {
        Stop::Control(Control::Cancel) => return Outcome::Cancelled,
        Stop::Control(Control::Interrupt) => return Outcome::Interrupted,
        Stop::Fatal(msg) if bytes == 0 => {
            let (d, n) = (dir.clone(), name.clone());
            let _ = tokio::task::spawn_blocking(move || files::delete(&d, &n)).await;
            return Outcome::Failed(msg);
        }
        _ => {}
    }
    if bytes == 0 {
        let (d, n) = (dir.clone(), name.clone());
        let _ = tokio::task::spawn_blocking(move || files::delete(&d, &n)).await;
        return Outcome::Failed(
            "Nothing was recorded: the channel didn't send any video.".to_owned(),
        );
    }
    let note = match &stop {
        Stop::Disk => Some(low_disk_message(r.keep_free, false)),
        Stop::Fatal(msg) => Some(format!("Stopped early: {msg}")),
        _ => None,
    };
    // Rename the part to the final name (another one if that was taken meanwhile).
    let taken = sh.taken_names().await;
    let (d, n) = (dir.clone(), name.clone());
    let renamed = tokio::task::spawn_blocking(move || {
        let part = files::part_path(&d, &n)?;
        let mut final_name = n.clone();
        if files::path_in(&d, &n)?.exists() {
            let mut others = taken;
            others.remove(&n);
            final_name = files::unique_name(&d, &others, &n)?;
        }
        std::fs::rename(part, files::path_in(&d, &final_name)?).ok()?;
        Some(final_name)
    })
    .await
    .ok()
    .flatten();
    let Some(final_name) = renamed else {
        return Outcome::Failed("The recording couldn't be saved under its final name.".to_owned());
    };
    sh.store("finish a recording", move |s| {
        if final_name != name {
            s.recordings().set_file_name(id, &final_name)?;
        }
        s.recordings().set_progress(id, bytes, Some(bytes))
    })
    .await;
    Outcome::Done(note)
}

fn to_chrono(d: Duration) -> opensmarters_core::chrono::Duration {
    opensmarters_core::chrono::Duration::from_std(d)
        .unwrap_or_else(|_| opensmarters_core::chrono::Duration::hours(8))
}

/// Opens the channel once and records until it ends, drops, or the job must stop.
async fn connect_once(
    r: &mut Rec<'_>,
    url: &Url,
    referrer: Option<&str>,
    control: &mut watch::Receiver<Control>,
) -> Stop {
    let req = DownloadRequest {
        offset: 0,
        user_agent: r.user_agent.as_deref(),
        referrer,
    };
    let opened = tokio::select! {
        o = r.sh.downloader.open(url, &req) => o,
        () = tokio::time::sleep_until(r.deadline) => return Stop::Deadline,
        c = next_control(control) => return Stop::Control(c),
    };
    let mut dl = match opened {
        Ok(d) => d,
        Err(e) => return Stop::Lost(failure_message(&e)),
    };
    // The first bytes decide: a playlist (HLS), video, or neither.
    let mut head: Vec<u8> = Vec::new();
    let mut ended = false;
    while head.len() < SNIFF_BYTES {
        let chunk = tokio::select! {
            c = dl.chunk() => c,
            () = tokio::time::sleep_until(r.deadline) => return Stop::Deadline,
            c = next_control(control) => return Stop::Control(c),
        };
        match chunk {
            Ok(Some(b)) => head.extend_from_slice(&b),
            Ok(None) => {
                ended = true;
                break;
            }
            Err(e) => return Stop::Lost(failure_message(&e)),
        }
    }
    if hls::looks_like_playlist(&head) {
        let base = dl.url().clone();
        let mut body = head;
        if !ended {
            match dl
                .read_to_end(MAX_PLAYLIST.saturating_sub(body.len() as u64))
                .await
            {
                Ok(rest) => body.extend_from_slice(&rest),
                Err(e) => return Stop::Lost(failure_message(&e)),
            }
        }
        return record_hls(r, &body, base, control).await;
    }
    match r.accept_head(&head).await {
        Ok(true) => {}
        Ok(false) => {
            return if r.writer.is_empty() {
                Stop::Fatal(NOT_VIDEO.to_owned())
            } else {
                Stop::Lost("the channel sent something that isn't video".to_owned())
            };
        }
        Err(stop) => return stop,
    }
    if ended {
        return Stop::Lost("the stream ended".to_owned());
    }
    loop {
        let chunk = tokio::select! {
            c = dl.chunk() => c,
            () = tokio::time::sleep_until(r.deadline) => return Stop::Deadline,
            c = next_control(control) => return Stop::Control(c),
        };
        match chunk {
            Ok(Some(b)) => {
                if let Err(stop) = r.write(&b).await {
                    return stop;
                }
            }
            Ok(None) => return Stop::Lost("the stream ended".to_owned()),
            Err(e) => return Stop::Lost(failure_message(&e)),
        }
    }
}

/// The segments of `media` to fetch after `last` (see the [module docs](self)).
fn new_segments(media: &Media, last: Option<u64>) -> Vec<hls::Segment> {
    let from_edge = |m: &Media| {
        let n = if m.ended {
            m.segments.len()
        } else {
            LIVE_EDGE.min(m.segments.len())
        };
        m.segments[m.segments.len() - n..].to_vec()
    };
    match last {
        None => from_edge(media),
        Some(last) => {
            let newer: Vec<hls::Segment> = media
                .segments
                .iter()
                .filter(|s| s.sequence > last)
                .cloned()
                .collect();
            let newest = media.segments.last().map(|s| s.sequence);
            if newer.is_empty() && newest.is_some_and(|n| n + 64 < last) && !media.ended {
                // The stream restarted its numbering: start again at the live edge.
                from_edge(media)
            } else {
                newer
            }
        }
    }
}

fn hls_fatal(e: &HlsError) -> String {
    match e {
        HlsError::Encrypted { .. } => {
            "This channel's stream is encrypted, which recording doesn't support.".to_owned()
        }
        HlsError::Unsupported { what } => {
            format!("This channel's stream uses {what}, which recording doesn't support.")
        }
        HlsError::NoVariants | HlsError::NotAPlaylist | HlsError::TooLarge { .. } => {
            NOT_VIDEO.to_owned()
        }
        _ => NOT_VIDEO.to_owned(),
    }
}

/// Fetches a small body (playlist, init section) with the recording's User-Agent.
async fn fetch_small(r: &Rec<'_>, url: &Url, limit: u64) -> Result<(Url, Vec<u8>), String> {
    let req = DownloadRequest {
        offset: 0,
        user_agent: r.user_agent.as_deref(),
        referrer: None,
    };
    let dl =
        r.sh.downloader
            .open(url, &req)
            .await
            .map_err(|e| failure_message(&e))?;
    let base = dl.url().clone();
    let body = dl
        .read_to_end(limit)
        .await
        .map_err(|e| failure_message(&e))?;
    Ok((base, body))
}

async fn load_media(r: &Rec<'_>, url: &Url) -> Result<Media, Stop> {
    let (base, body) = fetch_small(r, url, MAX_PLAYLIST)
        .await
        .map_err(Stop::Lost)?;
    match hls::parse(&body, &base, &HlsLimits::default()) {
        Ok(Playlist::Media(m)) => Ok(m),
        Ok(Playlist::Master(_)) => Err(Stop::Fatal(NOT_VIDEO.to_owned())),
        Err(e @ (HlsError::Encrypted { .. } | HlsError::Unsupported { .. })) => {
            Err(Stop::Fatal(hls_fatal(&e)))
        }
        Err(e) => Err(Stop::Lost(e.to_string())),
    }
}

#[allow(clippy::too_many_lines)]
async fn record_hls(
    r: &mut Rec<'_>,
    body: &[u8],
    base: Url,
    control: &mut watch::Receiver<Control>,
) -> Stop {
    let (media_url, mut media) = match hls::parse(body, &base, &HlsLimits::default()) {
        Ok(Playlist::Media(m)) => (base, m),
        Ok(Playlist::Master(m)) => {
            let Some(v) = m.best() else {
                return Stop::Fatal(
                    "This channel's stream keeps its sound separately, which recording \
                     doesn't support yet."
                        .to_owned(),
                );
            };
            let u = v.uri.clone();
            match load_media(r, &u).await {
                Ok(m) => (u, m),
                Err(stop) => return stop,
            }
        }
        Err(e) => return Stop::Fatal(hls_fatal(&e)),
    };
    let mut errors = 0u32;
    let mut last_new = Instant::now();
    loop {
        if media.segments.is_empty() && media.skipped > 0 && r.writer.is_empty() {
            return Stop::Fatal(NOT_VIDEO.to_owned());
        }
        let todo = new_segments(&media, r.last_seq);
        if !todo.is_empty() {
            last_new = Instant::now();
        }
        for seg in todo {
            if tokio::time::Instant::now() >= r.deadline {
                return Stop::Deadline;
            }
            // A new init section (fragmented MP4) goes in before its segments.
            if seg.init.is_some() && seg.init != r.init {
                if let Some(init) = &seg.init {
                    match fetch_small(r, init, MAX_INIT).await {
                        Ok((_, bytes)) if sniff_container(&bytes) == Some(Container::Mp4) => {
                            if r.container.is_none() {
                                r.container = Some(Container::Mp4);
                            }
                            if let Err(stop) = r.write(&bytes).await {
                                return stop;
                            }
                        }
                        Ok(_) => return Stop::Fatal(NOT_VIDEO.to_owned()),
                        Err(why) => return Stop::Lost(why),
                    }
                }
                r.init = seg.init.clone();
            }
            match fetch_segment(r, &seg.uri, control).await {
                Ok(true) => {}
                Ok(false) => {
                    r.bad_segments += 1;
                    if r.writer.is_empty() && r.bad_segments >= 3 {
                        return Stop::Fatal(NOT_VIDEO.to_owned());
                    }
                }
                Err(stop) => return stop,
            }
            r.last_seq = Some(seg.sequence);
        }
        // Wait, then reload the playlist.
        let wait = if media.ended {
            Duration::from_secs(10)
        } else {
            media.reload_after()
        };
        let stalled_after = media.target_duration.map_or(Duration::from_secs(30), |t| {
            Duration::from_secs_f64((t * 3.0).max(30.0))
        });
        if !media.ended && last_new.elapsed() > stalled_after {
            return Stop::Lost("the live playlist stopped moving".to_owned());
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = tokio::time::sleep_until(r.deadline) => return Stop::Deadline,
            c = next_control(control) => return Stop::Control(c),
        }
        match load_media(r, &media_url).await {
            Ok(m) => {
                errors = 0;
                media = m;
            }
            Err(Stop::Lost(why)) => {
                errors += 1;
                if errors >= 3 {
                    return Stop::Lost(why);
                }
            }
            Err(stop) => return stop,
        }
    }
}

/// Fetches one segment and appends it. `Ok(false)` when it wasn't video (skipped).
async fn fetch_segment(
    r: &mut Rec<'_>,
    url: &Url,
    control: &mut watch::Receiver<Control>,
) -> Result<bool, Stop> {
    let req = DownloadRequest {
        offset: 0,
        user_agent: r.user_agent.as_deref(),
        referrer: None,
    };
    let opened = tokio::select! {
        o = r.sh.downloader.open(url, &req) => o,
        () = tokio::time::sleep_until(r.deadline) => return Err(Stop::Deadline),
        c = next_control(control) => return Err(Stop::Control(c)),
    };
    let mut dl = opened.map_err(|e| Stop::Lost(failure_message(&e)))?;
    let mut head: Vec<u8> = Vec::new();
    let mut checked = false;
    loop {
        let chunk = tokio::select! {
            c = dl.chunk() => c,
            () = tokio::time::sleep_until(r.deadline) => return Err(Stop::Deadline),
            c = next_control(control) => return Err(Stop::Control(c)),
        };
        match chunk {
            Ok(Some(b)) => {
                if checked {
                    r.write(&b).await?;
                } else {
                    head.extend_from_slice(&b);
                    if head.len() >= SNIFF_BYTES {
                        if !r.accept_head(&head).await? {
                            return Ok(false);
                        }
                        checked = true;
                        head.clear();
                    }
                }
            }
            Ok(None) => {
                if !checked {
                    return r.accept_head(&head).await;
                }
                return Ok(true);
            }
            Err(e) => return Err(Stop::Lost(failure_message(&e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(seqs: std::ops::Range<u64>, ended: bool) -> Media {
        let segments = seqs
            .map(|n| hls::Segment {
                uri: Url::parse(&format!("http://cdn.example/s{n}.ts")).unwrap_or_else(|_| {
                    panic!("url");
                }),
                sequence: n,
                duration: 4.0,
                discontinuity: false,
                init: None,
            })
            .collect();
        Media {
            target_duration: Some(4.0),
            media_sequence: 0,
            segments,
            ended,
            skipped: 0,
        }
    }

    fn seqs(v: &[hls::Segment]) -> Vec<u64> {
        v.iter().map(|s| s.sequence).collect()
    }

    #[test]
    fn segment_selection() {
        // A live playlist starts at the edge; a finished one from the start.
        assert_eq!(
            seqs(&new_segments(&media(10..20, false), None)),
            [17, 18, 19]
        );
        assert_eq!(seqs(&new_segments(&media(0..4, true), None)), [0, 1, 2, 3]);
        // Then only newer segments.
        assert_eq!(
            seqs(&new_segments(&media(12..22, false), Some(19))),
            [20, 21]
        );
        assert!(new_segments(&media(12..20, false), Some(19)).is_empty());
        // Numbering restarted far below: back to the edge.
        assert_eq!(
            seqs(&new_segments(&media(0..5, false), Some(5000))),
            [2, 3, 4]
        );
        // A finished playlist doesn't repeat itself.
        assert!(new_segments(&media(0..4, true), Some(3)).is_empty());
    }
}

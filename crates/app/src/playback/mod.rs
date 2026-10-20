// SPDX-License-Identifier: GPL-3.0-or-later
//! Playback: the single mpv [`Player`] + [`VideoView`] shared by the Live TV preview and
//! the fullscreen player (`cx.playback`), and the logic around it:
//!
//! - **One stream at a time.** Providers count every open stream against
//!   `max_connections` (and keep counting abandoned ones for a while, then answer 403 to
//!   everything). The previous stream is always stopped before the next one is loaded,
//!   and zapping is debounced ([`ZAP_DEBOUNCE`]): pressing channel-up five times opens one
//!   stream, not five.
//! - **Live reconnect** with backoff ([`RETRY_DELAYS`]: 2, 5, 10, 30, 30 s, then give up)
//!   on `EndFile(Error)`, on `EndFile(Eof)` (the server closed the stream) and on stalls
//!   (no progress for [`STALL_AFTER`]). The status shows "Reconnecting… (n)".
//! - **Connection limit:** when a stream fails on an Xtream account, the account info is
//!   fetched (an API call, not a stream); if `active_cons >= max_connections` the UI shows
//!   [`PlaybackController::notice`] ("Connection limit reached (3/3) …") instead of a
//!   generic error.
//! - **Subtitles are off by default** (mpv would auto-select embedded closed captions on
//!   live channels). For VOD, the preferred subtitle language (Settings → Playback) or a
//!   language the user picked in the track sheet is used; the preferred **audio** language
//!   is selected when a stream has it (see [`tracks`]).
//! - **Hardware decoding and the network buffer** come from Settings → Playback
//!   ([`PlayerTuning`]) and apply when the player starts.
//! - **VOD end of file = finished**, not an error: the resume point is marked finished.
//!   A finished episode offers the next one ([`next`]: `up_next`, `play_up_next`).
//! - **Progress** of movies/episodes/catch-up is saved every [`SAVE_EVERY`] and when
//!   playback stops or switches.
//! - **Recent channels** are recorded when a live channel starts.
//! - **Connection budget:** opening a provider stream takes a lease from
//!   `session.connections()` ([`crate::services::connections`]) with this controller's
//!   [`Purpose`] (`Watch`, or `Tile(n)` for multiscreen); it may take a download's slot.
//!   The lease is kept while zapping and reconnecting and released when playback stops,
//!   a VOD finishes or the stream gives up. Local files (recordings) need none. When
//!   recordings hold every connection, the status says so instead of opening a stream
//!   the provider would refuse.
//! - **mpv log lines are redacted** with the app-wide redactor before they are logged.
//! - **Aspect/zoom** (`VideoFit`) persists across streams and restarts.
//!
//! Shutdown: [`PlaybackController::destroy`] from `eframe::App::on_exit` (frees the render
//! context while GL is current), then the player is dropped.

pub mod multi;
pub mod next;
pub mod request;
pub mod sleep;
pub mod tracks;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_core::{DEFAULT_USER_AGENT, Redactor};
use opensmarters_player::{
    EndReason, Hwdec, LoadOptions, LogLevel, Player, PlayerConfig, PlayerEvent, PlayerState,
    TrackKind, TrackSelection, UserAgent, VideoFit, VideoView,
};
use opensmarters_store::{ItemSnapshot, PlaybackSnapshot};

pub use request::{
    MediaKind, PlaybackItem, PlaybackRequest, StreamHeaders, ZapContext, stream_headers,
};
pub use tracks::{BufferPreset, PlayerTuning};

use crate::services::Session;
use crate::services::connections::{Lease, Purpose};

/// Reconnect delays (seconds) for successive attempts; after the last one we give up.
pub const RETRY_DELAYS: [u64; 5] = [2, 5, 10, 30, 30];
/// Maximum reconnect attempts before giving up.
pub const MAX_RETRIES: u32 = RETRY_DELAYS.len() as u32;
/// Zapping waits this long for further channel changes before opening a stream.
pub const ZAP_DEBOUNCE: Duration = Duration::from_millis(450);
/// Minimum time between two account checks after stream failures.
pub const LIMIT_CHECK_EVERY: Duration = Duration::from_secs(10);
/// Progress save interval.
pub const SAVE_EVERY: Duration = Duration::from_secs(10);
/// A live stream with no progress for this long is reloaded.
pub const STALL_AFTER: Duration = Duration::from_secs(20);

/// What the player is doing (for overlays and status text).
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackStatus {
    /// Nothing loaded.
    Idle,
    /// Opening the stream.
    Opening,
    /// Playing.
    Playing,
    /// Paused by the user.
    Paused,
    /// Waiting for the network.
    Buffering,
    /// Live stream dropped; retrying.
    Reconnecting {
        /// Attempt number (1-based).
        attempt: u32,
    },
    /// VOD reached the end.
    Finished,
    /// Gave up (plain-language message).
    Failed(String),
    /// libmpv is not available (message names `docs/LIBMPV.md`).
    Unavailable(String),
}

struct Engine {
    player: Player,
    video: VideoView,
}

struct Active {
    req: PlaybackRequest,
    session: Arc<Session>,
    retries: u32,
    retry_at: Option<Instant>,
    last_save: Instant,
    last_pos: Option<f64>,
    last_advance: Instant,
    finished: bool,
    failed: Option<String>,
    loaded_once: bool,
    /// Deferred start (debounced zapping).
    load_at: Option<Instant>,
    subs_applied: bool,
    /// The provider connection this stream uses (`None` for local files, or before the
    /// first load).
    lease: Option<Lease>,
}

/// The user's subtitle preference for VOD in this run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum SubPref {
    /// Off (default).
    #[default]
    Off,
    /// The track language the user picked last.
    Lang(String),
}

/// The shared player. See the [module docs](self).
pub struct PlaybackController {
    engine: Option<Engine>,
    init_error: Option<String>,
    active: Option<Active>,
    redactor: Redactor,
    ctx: egui::Context,
    fit_index: usize,
    saves: Arc<std::sync::atomic::AtomicU64>,
    notice: Arc<std::sync::Mutex<Option<String>>>,
    last_limit_check: Option<Instant>,
    sub_pref: SubPref,
    /// The playing episode's series, for "Next episode" (see [`next`]).
    queue: Arc<std::sync::Mutex<Option<next::EpisodeQueue>>>,
    audio_pref: Option<String>,
    tuning: PlayerTuning,
    purpose: Purpose,
}

impl std::fmt::Debug for PlaybackController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackController")
            .field("available", &self.engine.is_some())
            .field("playing", &self.active.as_ref().map(|a| a.req.kind))
            .finish_non_exhaustive()
    }
}

impl PlaybackController {
    /// Creates the player. Without libmpv (or the glow renderer) the controller still
    /// works but reports [`PlaybackStatus::Unavailable`]. `tuning` (hardware decoding,
    /// buffer) is fixed for the player's lifetime.
    pub fn new(
        get_proc_address: Option<opensmarters_player::GetProcAddress>,
        ctx: &egui::Context,
        redactor: Redactor,
        fit_index: u8,
        volume: u8,
        mpv_log: LogLevel,
        tuning: PlayerTuning,
    ) -> Self {
        let mut init_error = None;
        let engine = match get_proc_address {
            None => {
                init_error = Some("Video needs the OpenGL renderer.".to_owned());
                None
            }
            Some(gpa) => {
                let config = PlayerConfig::default()
                    .log_level(mpv_log)
                    .initial_volume(volume.min(100))
                    .hwdec(if tuning.hw_decoding {
                        Hwdec::AutoSafe
                    } else {
                        Hwdec::Disabled
                    })
                    .cache_mib(tuning.buffer.forward_mib, tuning.buffer.back_mib)
                    .network_timeout(Duration::from_secs(tuning.buffer.timeout_secs));
                let config = match UserAgent::new(DEFAULT_USER_AGENT) {
                    Ok(ua) => config.user_agent(ua),
                    Err(_) => config,
                };
                match Player::new(config) {
                    Ok(player) => {
                        let c = ctx.clone();
                        player.set_event_notify(move || c.request_repaint());
                        tracing::info!(mpv = player.mpv_version(), "player ready");
                        let fit_index = usize::from(fit_index).min(VideoFit::PRESETS.len() - 1);
                        let _ = player.set_video_fit(VideoFit::PRESETS[fit_index].1);
                        Some(Engine {
                            player,
                            video: VideoView::new(gpa),
                        })
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "libmpv unavailable");
                        init_error = Some(e.to_string());
                        None
                    }
                }
            }
        };
        Self {
            engine,
            init_error,
            active: None,
            redactor,
            ctx: ctx.clone(),
            fit_index: usize::from(fit_index),
            saves: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            notice: Arc::new(std::sync::Mutex::new(None)),
            last_limit_check: None,
            sub_pref: SubPref::Off,
            queue: Arc::default(),
            audio_pref: None,
            tuning,
            purpose: Purpose::Watch,
        }
    }

    /// Which connection-budget purpose this controller's streams use (default
    /// [`Purpose::Watch`]; multiscreen tiles use [`Purpose::Tile`]).
    pub fn set_connection_purpose(&mut self, purpose: Purpose) {
        self.purpose = purpose;
    }

    /// The hardware decoding / buffer options the player started with.
    pub fn tuning(&self) -> PlayerTuning {
        self.tuning
    }

    /// Sets the preferred audio and subtitle languages (Settings → Playback; codes as
    /// stored in the preferences, `None` = stream default / off). Applies to the next
    /// stream; a track-sheet choice still overrides the subtitle language for the run.
    pub fn set_track_prefs(&mut self, audio: Option<String>, subtitles: Option<String>) {
        self.audio_pref = audio;
        self.sub_pref = subtitles.map_or(SubPref::Off, SubPref::Lang);
    }

    /// True if libmpv loaded.
    pub fn is_available(&self) -> bool {
        self.engine.is_some()
    }

    /// The mpv version string, if available.
    pub fn mpv_version(&self) -> Option<String> {
        self.engine
            .as_ref()
            .map(|e| e.player.mpv_version().to_owned())
    }

    /// The ffmpeg version string, if available.
    pub fn ffmpeg_version(&self) -> Option<String> {
        self.engine
            .as_ref()
            .map(|e| e.player.ffmpeg_version().to_owned())
    }

    /// The underlying player (advanced use).
    pub fn player(&self) -> Option<&Player> {
        self.engine.as_ref().map(|e| &e.player)
    }

    /// What is loaded.
    pub fn request(&self) -> Option<&PlaybackRequest> {
        self.active.as_ref().map(|a| &a.req)
    }

    /// The session the current item belongs to.
    pub fn session(&self) -> Option<&Arc<Session>> {
        self.active.as_ref().map(|a| &a.session)
    }

    /// Key of the live channel playing, if any.
    pub fn live_key(&self) -> Option<&str> {
        match self.request().map(|r| &r.item) {
            Some(PlaybackItem::Live { key, .. }) => Some(key),
            _ => None,
        }
    }

    /// A snapshot of mpv's state (default when unavailable).
    pub fn state(&self) -> PlayerState {
        self.engine
            .as_ref()
            .map(|e| e.player.state())
            .unwrap_or_default()
    }

    /// The current status.
    pub fn status(&self) -> PlaybackStatus {
        let Some(engine) = &self.engine else {
            return PlaybackStatus::Unavailable(
                self.init_error
                    .clone()
                    .unwrap_or_else(|| "The video player is not available.".into()),
            );
        };
        let Some(a) = &self.active else {
            return PlaybackStatus::Idle;
        };
        if let Some(msg) = &a.failed {
            return PlaybackStatus::Failed(self.notice().unwrap_or_else(|| msg.clone()));
        }
        if a.finished {
            return PlaybackStatus::Finished;
        }
        if a.retry_at.is_some() {
            return PlaybackStatus::Reconnecting { attempt: a.retries };
        }
        if a.load_at.is_some() {
            return PlaybackStatus::Opening;
        }
        let st = engine.player.state();
        if st.loading || (!st.file_loaded && !a.loaded_once) || engine.player.has_pending_load() {
            PlaybackStatus::Opening
        } else if st.paused_for_cache
            || (st.file_loaded && !st.has_video() && st.time_pos.is_none())
        {
            PlaybackStatus::Buffering
        } else if st.paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        }
    }

    /// A message to show next to the status, e.g. "Connection limit reached (3/3) — …".
    pub fn notice(&self) -> Option<String> {
        self.notice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Plays `req` now (saving the progress of what was playing and stopping its stream
    /// first).
    pub fn play(&mut self, session: Arc<Session>, req: PlaybackRequest) {
        self.start(session, req, None);
    }

    /// Like [`play`](Self::play) but opens the stream only after `delay` (debounced
    /// zapping); the previous stream is stopped at once. A later call replaces it.
    pub fn play_after(&mut self, session: Arc<Session>, req: PlaybackRequest, delay: Duration) {
        self.start(session, req, Some(Instant::now() + delay));
    }

    fn start(&mut self, session: Arc<Session>, req: PlaybackRequest, load_at: Option<Instant>) {
        self.save_progress(true);
        // Free the provider connection before opening another one.
        if self.active.is_some()
            && let Some(e) = &self.engine
        {
            let _ = e.player.stop();
        }
        *self
            .notice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        tracing::debug!(kind = ?req.kind, "play");
        // Zapping keeps the connection lease of the same session (the old stream was just
        // stopped); a local file needs none.
        let lease = self
            .active
            .take()
            .filter(|old| Arc::ptr_eq(&old.session, &session) && !req.url.is_local_file())
            .and_then(|old| old.lease);
        self.active = Some(Active {
            req,
            session,
            retries: 0,
            retry_at: None,
            last_save: Instant::now(),
            last_pos: None,
            last_advance: Instant::now(),
            finished: false,
            failed: None,
            loaded_once: false,
            load_at,
            subs_applied: false,
            lease,
        });
        self.sync_episode_queue();
        if let Some(at) = load_at {
            self.ctx
                .request_repaint_after(at.saturating_duration_since(Instant::now()));
        } else {
            self.load_current(true);
        }
    }

    fn load_current(&mut self, from_resume: bool) {
        let (Some(engine), Some(a)) = (&self.engine, &mut self.active) else {
            return;
        };
        let start = if from_resume {
            a.req.resume_from
        } else {
            a.last_pos.filter(|_| !a.req.kind.is_live())
        };
        // Record a live channel as recently watched when its stream really opens (not for
        // channels skipped while zapping, nor on reconnects).
        if from_resume && let PlaybackItem::Live { key, .. } = &a.req.item {
            let profile = a.session.profile_id();
            let key = key.clone();
            let snapshot = ItemSnapshot {
                name: a.req.title.clone(),
                icon_url: a.req.artwork.clone(),
                category_id: None,
            };
            a.session
                .services()
                .blocking("record recent channel", move |s| {
                    s.recent_live().touch(profile, &key, &snapshot)
                });
        }
        if !a.req.url.is_local_file() && a.lease.is_none() {
            match a.session.connections().acquire_for_viewing(self.purpose) {
                Ok(lease) => a.lease = Some(lease),
                Err(busy) => {
                    tracing::info!(holders = ?busy.holders, "no free provider connection");
                    a.failed = Some(format!(
                        "{} Stop a recording, or wait until one finishes.",
                        busy.message()
                    ));
                    return;
                }
            }
        }
        let opts = LoadOptions {
            user_agent: a.req.headers.user_agent.clone(),
            referrer: a.req.headers.referrer.clone(),
            start,
        };
        tracing::info!(kind = ?a.req.kind, resume = ?start, "opening stream");
        if let Err(e) = engine.player.load(&a.req.url, &opts) {
            tracing::warn!(error = %e, "load refused");
            a.failed = Some(crate::error::AppError::from(e).user_message());
        }
        let _ = engine.player.play();
        a.last_advance = Instant::now();
    }

    /// Stops playback (saving progress).
    pub fn stop(&mut self) {
        self.save_progress(true);
        if let Some(e) = &self.engine {
            let _ = e.player.stop();
        }
        self.active = None;
    }

    /// Restarts the current item from the beginning (Replay).
    pub fn replay(&mut self) {
        if let Some(a) = &mut self.active {
            a.finished = false;
            a.failed = None;
            a.retries = 0;
            a.retry_at = None;
            a.last_pos = Some(0.0);
            a.req.resume_from = None;
        }
        self.load_current(false);
    }

    /// Selects a subtitle track chosen by the user and remembers its language for later
    /// VOD items (`None` = off).
    pub fn choose_subtitle(&mut self, sel: TrackSelection) {
        let lang = match sel {
            TrackSelection::Id(id) => self
                .state()
                .subtitle_tracks()
                .find(|t| t.id == id)
                .and_then(|t| t.lang.clone()),
            _ => None,
        };
        self.sub_pref = lang.map_or(SubPref::Off, SubPref::Lang);
        self.select_subtitle(sel);
    }

    fn apply_subtitle_policy(&mut self) {
        let (Some(engine), Some(a)) = (&self.engine, &mut self.active) else {
            return;
        };
        if a.subs_applied {
            return;
        }
        let st = engine.player.state();
        if st.tracks.is_empty() {
            return;
        }
        a.subs_applied = true;
        let pref = match &self.sub_pref {
            SubPref::Lang(lang) => Some(lang.as_str()),
            SubPref::Off => None,
        };
        let subs = tracks::pick_subtitle(&st.tracks, pref, a.req.kind.is_live());
        let _ = engine.player.select_subtitle(subs);
        if let Some(id) = tracks::pick_audio(&st.tracks, self.audio_pref.as_deref()) {
            tracing::debug!(id, "selecting the preferred audio language");
            let _ = engine.player.select_audio(TrackSelection::Id(id));
        }
    }

    /// Asks the provider whether the account's connection limit is exhausted (after a
    /// stream failure) and sets [`notice`](Self::notice).
    fn check_connection_limit(&mut self) {
        if self
            .last_limit_check
            .is_some_and(|t| t.elapsed() < LIMIT_CHECK_EVERY)
        {
            return;
        }
        let Some(a) = &self.active else { return };
        if !a.session.is_xtream() {
            return;
        }
        self.last_limit_check = Some(Instant::now());
        let session = Arc::clone(&a.session);
        let notice = Arc::clone(&self.notice);
        let ctx = self.ctx.clone();
        session.services().spawn({
            let session = Arc::clone(&session);
            async move {
                if let Ok((Some(active), Some(max))) = session.connection_usage().await
                    && max > 0
                    && active >= max
                {
                    *notice
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!(
                        "Connection limit reached ({active}/{max}). Close the stream on another \
                         device, or wait a minute and try again."
                    ));
                    ctx.request_repaint();
                }
            }
        });
    }

    /// Retries after a failure.
    pub fn retry(&mut self) {
        if let Some(a) = &mut self.active {
            a.failed = None;
            a.retries = 0;
            a.retry_at = None;
        }
        self.load_current(false);
    }

    /// Per-frame housekeeping: player events, reconnects, stalls, progress saves.
    pub fn update(&mut self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let mut ended: Option<EndReason> = None;
        let mut restarted = false;
        for ev in engine.player.poll_events() {
            match ev {
                PlayerEvent::Log(line) => {
                    let text = self.redactor.redact(&line.text);
                    let prefix = line.prefix;
                    match line.level {
                        LogLevel::Fatal | LogLevel::Error => {
                            tracing::warn!(target: "mpv", "{prefix}: {text}");
                        }
                        LogLevel::Warn => tracing::info!(target: "mpv", "{prefix}: {text}"),
                        _ => tracing::debug!(target: "mpv", "{prefix}: {text}"),
                    }
                }
                PlayerEvent::EndFile(reason) => ended = Some(reason),
                PlayerEvent::PlaybackRestart | PlayerEvent::FileLoaded => restarted = true,
                PlayerEvent::CommandFailed { op, error } => {
                    tracing::debug!(?op, %error, "mpv command failed");
                }
                _ => {}
            }
        }
        let st = engine.player.state();
        let now = Instant::now();
        // Debounced start.
        if let Some(at) = self.active.as_ref().and_then(|a| a.load_at) {
            if now >= at {
                if let Some(a) = &mut self.active {
                    a.load_at = None;
                }
                self.load_current(true);
            } else {
                self.ctx.request_repaint_after(at - now);
                return;
            }
        }
        if restarted {
            self.apply_subtitle_policy();
        }
        let Some(a) = &mut self.active else {
            return;
        };
        if restarted {
            a.loaded_once = true;
            if st.time_pos.is_some_and(|p| p > 0.5) || a.req.kind.is_live() {
                *self
                    .notice
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
        }
        // Progress tracking.
        if let Some(pos) = st.time_pos {
            if a.last_pos.is_none_or(|p| (p - pos).abs() > 0.2) {
                a.last_advance = now;
                if a.retry_at.is_none() && pos > 1.0 {
                    a.retries = 0;
                }
            }
            a.last_pos = Some(pos);
        } else if st.paused || st.idle {
            a.last_advance = now;
        }
        if st.paused {
            a.last_advance = now;
        }

        let mut finished_now = false;
        let failed_now = matches!(ended, Some(EndReason::Error(_)));
        match ended {
            Some(EndReason::Eof) if !a.req.kind.is_live() => {
                a.finished = true;
                finished_now = true;
            }
            Some(EndReason::Eof | EndReason::Error(_)) if a.req.kind.is_live() => {
                Self::schedule_retry(a, now);
            }
            Some(EndReason::Error(e)) => {
                if a.loaded_once && a.retries < 2 {
                    Self::schedule_retry(a, now);
                } else {
                    tracing::info!(error = %e, "playback failed");
                    a.failed = Some(if a.req.kind == MediaKind::Catchup {
                        "Catch-up isn't available for this programme right now. Your provider \
                         may not keep an archive for it, or it may be busy."
                            .to_owned()
                    } else {
                        "This video couldn't be played. The provider may be busy or the file \
                         may be unavailable."
                            .to_owned()
                    });
                }
            }
            _ => {}
        }

        if finished_now {
            self.mark_finished();
        }
        if failed_now {
            self.check_connection_limit();
        }
        let Some(a) = &mut self.active else {
            return;
        };
        // The stream is closed for good: free its connection (replay/retry take one again).
        if a.finished || a.failed.is_some() {
            a.lease = None;
        }
        // Live stall detection.
        if a.req.kind.is_live()
            && a.retry_at.is_none()
            && a.failed.is_none()
            && a.loaded_once
            && now.duration_since(a.last_advance) > STALL_AFTER
        {
            tracing::info!("live stream stalled; reconnecting");
            Self::schedule_retry(a, now);
        }

        if let Some(at) = a.retry_at {
            if now >= at {
                a.retry_at = None;
                self.load_current(false);
                if let Some(a) = &mut self.active {
                    a.retry_at = None;
                }
            } else {
                self.ctx.request_repaint_after(at - now);
            }
        }

        let due = self
            .active
            .as_ref()
            .is_some_and(|a| a.last_save.elapsed() >= SAVE_EVERY);
        if due {
            self.save_progress(false);
        }
        if self.active.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn schedule_retry(a: &mut Active, now: Instant) {
        if a.retries >= MAX_RETRIES {
            a.failed = Some(if a.req.kind.is_live() {
                "The channel stopped and didn't come back. Try again, or pick another channel."
                    .to_owned()
            } else {
                "The connection dropped and didn't come back.".to_owned()
            });
            a.retry_at = None;
            return;
        }
        let delay =
            Duration::from_secs(RETRY_DELAYS.get(a.retries as usize).copied().unwrap_or(30));
        a.retries += 1;
        a.retry_at = Some(now + delay);
        a.last_advance = now + delay;
        tracing::info!(attempt = a.retries, ?delay, "reconnecting");
    }

    fn snapshot(a: &Active) -> PlaybackSnapshot {
        let (series_id, season, episode, ext) = match &a.req.item {
            PlaybackItem::Episode {
                series_id,
                season,
                episode,
                container_extension,
                ..
            } => (
                Some(series_id.to_string()),
                *season,
                *episode,
                container_extension.clone(),
            ),
            PlaybackItem::Movie {
                container_extension,
                ..
            } => (None, None, None, container_extension.clone()),
            _ => (None, None, None, None),
        };
        PlaybackSnapshot {
            title: a.req.title.clone(),
            poster_url: a.req.artwork.clone(),
            series_id,
            season,
            episode,
            container_extension: ext,
        }
    }

    /// Saves the resume point now (VOD/episodes/catch-up). `final_save` is used on stop.
    fn save_progress(&mut self, final_save: bool) {
        let Some(a) = &mut self.active else {
            return;
        };
        a.last_save = Instant::now();
        if a.finished {
            return;
        }
        let (Some(kind), Some(id), Some(pos)) = (
            a.req.kind.playback_kind(),
            a.req.item.progress_id(),
            a.last_pos,
        ) else {
            return;
        };
        if pos < 1.0 {
            return;
        }
        let duration = self
            .engine
            .as_ref()
            .and_then(|e| e.player.state().duration)
            .or(a.req.duration_hint);
        let snapshot = Self::snapshot(a);
        let profile = a.session.profile_id();
        if final_save {
            tracing::debug!(pos, "saving progress");
        }
        let saves = Arc::clone(&self.saves);
        a.session.services().blocking("save progress", move |s| {
            let r = s
                .playback()
                .record_progress(profile, kind, &id, pos, duration, &snapshot)
                .map(|_| ());
            saves.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            r
        });
    }

    fn mark_finished(&mut self) {
        let Some(a) = &self.active else {
            return;
        };
        let (Some(kind), Some(id)) = (a.req.kind.playback_kind(), a.req.item.progress_id()) else {
            return;
        };
        let duration = self
            .engine
            .as_ref()
            .and_then(|e| e.player.state().duration)
            .or(a.req.duration_hint);
        let pos = duration.or(a.last_pos).unwrap_or(0.0);
        let snapshot = Self::snapshot(a);
        let profile = a.session.profile_id();
        let saves = Arc::clone(&self.saves);
        a.session.services().blocking("mark finished", move |s| {
            let r = s
                .playback()
                .record_progress(profile, kind, &id, pos, duration, &snapshot)
                .and_then(|_| s.playback().mark_finished(profile, kind, &id).map(|_| ()));
            saves.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            r
        });
    }

    /// Counts completed progress writes. Screens showing watch progress (Home's Continue
    /// watching, detail pages) reload when it changes.
    pub fn progress_epoch(&self) -> u64 {
        self.saves.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Paints the video into `rect` (black when nothing plays).
    pub fn paint_video(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        match &mut self.engine {
            Some(e) => e.video.paint_at(ui, rect, &e.player),
            None => {
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            }
        }
    }

    /// Video rendering error, if any.
    pub fn video_error(&self) -> Option<String> {
        self.engine.as_ref().and_then(|e| e.video.last_error())
    }

    /// Frames rendered so far (diagnostics; the harness checks video works).
    pub fn frames_rendered(&self) -> u64 {
        self.engine
            .as_ref()
            .map_or(0, |e| e.video.stats().frames_rendered)
    }

    // ---------------------------------------------------------------------------------
    // Controls
    // ---------------------------------------------------------------------------------

    /// Play/pause.
    pub fn toggle_pause(&self) {
        if let Some(e) = &self.engine {
            let _ = e.player.toggle_pause();
        }
    }

    /// Relative seek (VOD).
    pub fn seek_relative(&self, secs: f64) {
        if let Some(e) = &self.engine {
            let _ = e.player.seek_relative(secs);
        }
    }

    /// Absolute seek (VOD).
    pub fn seek_absolute(&self, secs: f64) {
        if let Some(e) = &self.engine {
            let _ = e.player.seek_absolute(secs.max(0.0));
        }
    }

    /// Volume 0..=100.
    pub fn set_volume(&self, volume: f64) {
        if let Some(e) = &self.engine {
            let _ = e.player.set_volume(volume.clamp(0.0, 100.0));
        }
    }

    /// Mute toggle.
    pub fn toggle_mute(&self) {
        if let Some(e) = &self.engine {
            let _ = e.player.toggle_mute();
        }
    }

    /// Selects an audio track.
    pub fn select_audio(&self, sel: TrackSelection) {
        if let Some(e) = &self.engine {
            let _ = e.player.select_audio(sel);
        }
    }

    /// Selects a subtitle track.
    pub fn select_subtitle(&self, sel: TrackSelection) {
        if let Some(e) = &self.engine {
            let _ = e.player.select_subtitle(sel);
        }
    }

    /// Cycles to the next audio track (A key).
    pub fn cycle_audio(&self) {
        let st = self.state();
        let tracks: Vec<i64> = st.audio_tracks().map(|t| t.id).collect();
        if tracks.is_empty() {
            return;
        }
        let cur = st.selected(TrackKind::Audio).map(|t| t.id);
        let i = cur
            .and_then(|c| tracks.iter().position(|t| *t == c))
            .map_or(0, |i| (i + 1) % tracks.len());
        self.select_audio(TrackSelection::Id(tracks[i]));
    }

    /// Cycles subtitles: off → each track → off (S key). Remembered like a track-sheet
    /// choice.
    pub fn cycle_subtitles(&mut self) {
        let st = self.state();
        let tracks: Vec<i64> = st.subtitle_tracks().map(|t| t.id).collect();
        let cur = st.selected(TrackKind::Subtitle).map(|t| t.id);
        let next = match cur.and_then(|c| tracks.iter().position(|t| *t == c)) {
            None => tracks.first().copied(),
            Some(i) => tracks.get(i + 1).copied(),
        };
        self.choose_subtitle(next.map_or(TrackSelection::Off, TrackSelection::Id));
    }

    /// The aspect preset index (into `VideoFit::PRESETS`).
    pub fn fit_index(&self) -> usize {
        self.fit_index
    }

    /// Sets an aspect preset; returns its name. Persist with `Prefs::video_fit`.
    pub fn set_fit_index(&mut self, i: usize) -> &'static str {
        let i = i % VideoFit::PRESETS.len();
        self.fit_index = i;
        if let Some(e) = &self.engine {
            let _ = e.player.set_video_fit(VideoFit::PRESETS[i].1);
        }
        VideoFit::PRESETS[i].0
    }

    /// Next aspect preset; returns its name.
    pub fn cycle_fit(&mut self) -> &'static str {
        self.set_fit_index(self.fit_index + 1)
    }

    /// Zaps `delta` channels within the live list. Returns the new channel's name.
    pub fn zap(&mut self, delta: isize) -> Option<String> {
        let a = self.active.as_ref()?;
        let zap = a.req.zap.clone()?;
        let (index, channel) = zap.step(delta)?;
        self.zap_to(zap.clone(), index, &channel.clone())
    }

    /// Zaps to the channel with `number` in the live list.
    pub fn zap_to_number(&mut self, number: u64) -> Option<String> {
        let a = self.active.as_ref()?;
        let zap = a.req.zap.clone()?;
        let (index, channel) = zap.find_number(number)?;
        self.zap_to(zap.clone(), index, &channel.clone())
    }

    fn zap_to(
        &mut self,
        zap: ZapContext,
        index: usize,
        channel: &crate::services::catalog::LiveChannel,
    ) -> Option<String> {
        let session = Arc::clone(&self.active.as_ref()?.session);
        let ctx = ZapContext {
            channels: zap.channels,
            index,
        };
        match session.live_request(channel, Some(ctx)) {
            Ok(req) => {
                let name = req.title.clone();
                self.play_after(session, req, ZAP_DEBOUNCE);
                Some(name)
            }
            Err(e) => {
                tracing::info!(error = %e, "zap target not playable");
                None
            }
        }
    }

    /// Frees GL resources (call from `on_exit` with the GL context).
    pub fn destroy(&mut self, gl: &eframe::glow::Context) {
        self.save_progress(true);
        if let Some(e) = &mut self.engine {
            e.video.destroy(gl);
        }
    }
}

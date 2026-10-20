// SPDX-License-Identifier: GPL-3.0-or-later
//! Playback: the single mpv [`Player`] + [`VideoView`] shared by the Live TV preview and
//! the fullscreen player (`cx.playback`), and the logic around it:
//!
//! - **One stream at a time.** Providers count every open stream against
//!   `max_connections` (and keep counting abandoned ones for a while, then answer 403 to
//!   everything). The previous stream is always stopped before the next one is loaded,
//!   and zapping is debounced ([`ZAP_DEBOUNCE`]): pressing channel-up five times opens one
//!   stream, not five.
//! - **Two choke points:** `open_stream` is the only place a provider stream is opened
//!   (first open, reconnects, retry, replay) and `end_stream` the only place an item's
//!   stream ends for good (finished, failed / gave up, stopped, replaced); a reconnect
//!   stops and reopens without ending the item. Connection accounting belongs there.
//! - **Stream health** ([`tracker`], pure and unit-tested): a live stream whose position
//!   stops advancing for [`tracker::STALL_AFTER`] (while not paused) is stopped and
//!   reconnected ("Stream stalled — reconnecting…"); `EndFile(Error)` and a live
//!   `EndFile(Eof)` (the server closed the stream) reconnect too. One reconnect is pending
//!   at a time, with backoff ([`tracker::RETRY_DELAYS`]: 2, 5, 10, 30, 30 s, then give up);
//!   the counter resets after 30 s of continuous progress, and a live stream that ends
//!   cleanly after real playback reconnects at the first delay. The status shows
//!   "Reconnecting… (n)".
//! - **Load generations:** mpv events are tied to the load that caused them
//!   ([`tracker::LoadGuard`]); late events of the previous stream are ignored.
//! - **Connection limit:** when a stream fails on an Xtream account, the account info is
//!   fetched (an API call, not a stream); if `active_cons >= max_connections` the UI shows
//!   [`PlaybackController::notice`] ("Connection limit reached (3/3) …") instead of a
//!   generic error.
//! - **Tracks:** every load starts from mpv's automatic audio choice and subtitles off
//!   (a choice never leaks from one stream into the next). Then the language the user
//!   picked in this run (track sheet, A/S keys), else Settings → Playback, is selected
//!   when the stream has it; live subtitles stay off (see [`tracks`]).
//! - **Hardware decoding and the network buffer** come from Settings → Playback
//!   ([`PlayerTuning`]) and apply when the player starts.
//! - **VOD end of file** is *finished* only near the end ([`tracker::vod_end`]); an
//!   earlier end is a dropped connection and resumes from the last position (bounded
//!   backoff), so the resume point survives. A finished episode offers the next one
//!   ([`next`]: `up_next`, `play_up_next`).
//! - **Progress** of movies/episodes/catch-up is saved every [`SAVE_EVERY`], when playback
//!   stops or switches, and synchronously on exit ([`PlaybackController::destroy`]).
//! - **Zap banner:** every live channel change (and a refused zap at the ends of the list)
//!   raises [`PlaybackController::banner`] for [`BANNER_FOR`]; screens draw it.
//! - **The last frame stays up** (under the screens' scrim and spinner) while the next
//!   live channel or a reconnect opens, instead of going black.
//! - **Recent channels** are recorded when a live channel starts.
//! - **mpv log lines are redacted** with the app-wide redactor before they are logged.
//! - **Aspect/zoom** (`VideoFit`) persists across streams and restarts.
//!
//! Shutdown: [`PlaybackController::destroy`] from `eframe::App::on_exit` (saves progress,
//! frees the render context while GL is current), then the player is dropped.

pub mod keys;
pub mod next;
pub mod request;
pub mod tracker;
pub mod tracks;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_core::{DEFAULT_USER_AGENT, Redactor};
use opensmarters_player::{
    EndReason, Hwdec, LoadOptions, LogLevel, Op, Player, PlayerConfig, PlayerEvent, PlayerState,
    TrackKind, TrackSelection, UserAgent, VideoFit, VideoView,
};
use opensmarters_store::{ItemSnapshot, PlaybackSnapshot};

pub use request::{
    MediaKind, PlaybackItem, PlaybackRequest, StreamHeaders, ZapContext, ZapEdge, stream_headers,
};
pub use tracker::RETRY_DELAYS;
pub use tracks::{BufferPreset, PlayerTuning};

use crate::services::Session;
use crate::services::catalog::LiveChannel;
use tracker::{Cause, LoadGuard, ProgressTracker, Reconnector, Schedule, VodEnd};

/// Maximum reconnect attempts for a live channel before giving up.
pub const MAX_RETRIES: u32 = tracker::LIVE_MAX_RETRIES;
/// Zapping waits this long for further channel changes before opening a stream.
pub const ZAP_DEBOUNCE: Duration = Duration::from_millis(450);
/// Minimum time between two account checks after stream failures.
pub const LIMIT_CHECK_EVERY: Duration = Duration::from_secs(10);
/// Progress save interval.
pub const SAVE_EVERY: Duration = Duration::from_secs(10);
/// A live stream with no progress for this long is reconnected.
pub const STALL_AFTER: Duration = tracker::STALL_AFTER;
/// How long the zap banner stays up (plus its fade-out).
pub const BANNER_FOR: Duration = Duration::from_secs(3);
/// Fade in/out of the zap banner.
pub const BANNER_FADE: Duration = Duration::from_millis(200);

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

/// The zap banner's state ([`PlaybackController::banner`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Banner {
    /// When it was raised.
    pub since: Instant,
    /// Set when a zap was refused at an end of the list.
    pub edge: Option<ZapEdge>,
}

impl Banner {
    /// Opacity 0..=1 at `now` (200 ms fade in and out around [`BANNER_FOR`]).
    pub fn alpha(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.since).as_secs_f32();
        let fade = BANNER_FADE.as_secs_f32();
        let total = BANNER_FOR.as_secs_f32() + fade;
        ((age / fade).min(1.0) * ((total - age) / fade).min(1.0)).clamp(0.0, 1.0)
    }
}

/// What a zap did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZapOutcome {
    /// Switching to this channel (name), after the debounce.
    Zapped(String),
    /// Already at this end of the list; nothing changed.
    Edge(ZapEdge),
    /// No zap list, or the channel can't be played.
    Unavailable,
}

struct Engine {
    player: Player,
    video: VideoView,
}

/// How an item's stream ended for good ([`PlaybackController::end_stream`]).
#[derive(Debug, Clone, PartialEq)]
enum StreamEnd {
    /// Watched to the end (VOD).
    Finished,
    /// Failed, or gave up after the reconnects (message for the user).
    Failed(String),
    /// Stopped by the user, or replaced by another item.
    Stopped,
}

struct Active {
    req: PlaybackRequest,
    session: Arc<Session>,
    reconnect: Reconnector,
    tracker: ProgressTracker,
    last_save: Instant,
    last_pos: Option<f64>,
    duration: Option<f64>,
    finished: bool,
    failed: Option<String>,
    /// Some load of this item reached `FileLoaded`.
    loaded_once: bool,
    /// The current load reached `FileLoaded`.
    file_loaded: bool,
    /// The current load reached `PlaybackRestart` (frames are coming).
    started: bool,
    /// Deferred start (debounced zapping).
    load_at: Option<Instant>,
    subs_applied: bool,
    /// Between [`PlaybackController::open_stream`] and
    /// [`PlaybackController::end_stream`] (reconnects in between keep it).
    stream_open: bool,
}

impl Active {
    fn new(req: PlaybackRequest, session: Arc<Session>, load_at: Option<Instant>) -> Self {
        let now = Instant::now();
        let max = if req.kind.is_live() {
            tracker::LIVE_MAX_RETRIES
        } else {
            tracker::VOD_MAX_RETRIES
        };
        Self {
            req,
            session,
            reconnect: Reconnector::new(max),
            tracker: ProgressTracker::new(now),
            last_save: now,
            last_pos: None,
            duration: None,
            finished: false,
            failed: None,
            loaded_once: false,
            file_loaded: false,
            started: false,
            load_at,
            subs_applied: false,
            stream_open: false,
        }
    }
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
    /// Settings → Playback audio language.
    audio_pref: Option<String>,
    /// The audio language picked in this run (track sheet, A key); wins over the setting.
    audio_choice: Option<String>,
    /// The settings last applied by [`set_track_prefs`](Self::set_track_prefs).
    track_settings: Option<(Option<String>, Option<String>)>,
    tuning: PlayerTuning,
    guard: LoadGuard,
    banner: Option<Banner>,
    /// Counts finished items (keys the next-episode countdown).
    finishes: u64,
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
                        let video = VideoView::new(gpa);
                        // Keep the last picture up while the next channel opens.
                        video.set_hold_last_frame(true);
                        Some(Engine { player, video })
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
            audio_choice: None,
            track_settings: None,
            tuning,
            guard: LoadGuard::default(),
            banner: None,
            finishes: 0,
        }
    }

    /// The hardware decoding / buffer options the player started with.
    pub fn tuning(&self) -> PlayerTuning {
        self.tuning
    }

    /// Sets the preferred audio and subtitle languages (Settings → Playback; codes as
    /// stored in the preferences, `None` = stream default / off). Applies to the next
    /// stream. Called whenever preferences are saved: only a *changed* setting replaces
    /// the languages picked in the track sheet during this run.
    pub fn set_track_prefs(&mut self, audio: Option<String>, subtitles: Option<String>) {
        let new = (audio, subtitles);
        let old = self.track_settings.replace(new.clone());
        let (audio, subtitles) = new;
        if old.as_ref().is_none_or(|(a, _)| *a != audio) {
            self.audio_pref = audio;
            self.audio_choice = None;
        }
        if old.as_ref().is_none_or(|(_, s)| *s != subtitles) {
            self.sub_pref = subtitles.map_or(SubPref::Off, SubPref::Lang);
        }
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

    /// The playing live channel, from the zap list (`None` without one).
    pub fn live_channel(&self) -> Option<&LiveChannel> {
        let key = self.live_key()?;
        self.request()?
            .zap
            .as_ref()?
            .current()
            .filter(|c| c.key == key)
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
        if a.reconnect.pending().is_some() {
            return PlaybackStatus::Reconnecting {
                attempt: a.reconnect.attempts(),
            };
        }
        if a.load_at.is_some() || engine.player.has_pending_load() {
            return PlaybackStatus::Opening;
        }
        let st = engine.player.state();
        if !a.started {
            if a.file_loaded && st.paused_for_cache {
                PlaybackStatus::Buffering
            } else {
                PlaybackStatus::Opening
            }
        } else if st.paused_for_cache {
            PlaybackStatus::Buffering
        } else if st.paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        }
    }

    /// The text for a pending reconnect: "Stream stalled — reconnecting…" after a stall,
    /// else "Reconnecting… (n)".
    pub fn reconnect_message(&self) -> Option<String> {
        let a = self.active.as_ref()?;
        a.reconnect.pending()?;
        Some(match a.reconnect.cause() {
            Some(Cause::Stalled) => "Stream stalled \u{2014} reconnecting\u{2026}".to_owned(),
            _ => format!("Reconnecting\u{2026} ({})", a.reconnect.attempts()),
        })
    }

    /// True while the screen should show "opening" over the held last frame: a zap or
    /// reconnect is in progress.
    pub fn is_switching(&self) -> bool {
        matches!(
            self.status(),
            PlaybackStatus::Opening
                | PlaybackStatus::Buffering
                | PlaybackStatus::Reconnecting { .. }
        )
    }

    /// A message to show next to the status, e.g. "Connection limit reached (3/3) — …".
    pub fn notice(&self) -> Option<String> {
        self.notice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The zap banner, while it is up (any live channel change, or a refused zap at an
    /// end of the list). Draw it with `widgets::zap_banner`.
    pub fn banner(&self) -> Option<Banner> {
        self.banner
            .filter(|b| b.since.elapsed() < BANNER_FOR + BANNER_FADE)
            .filter(|_| self.live_key().is_some())
    }

    /// Shows the zap banner for the playing channel again (Info key).
    pub fn show_banner(&mut self) {
        if self.live_key().is_some() {
            self.banner = Some(Banner {
                since: Instant::now(),
                edge: None,
            });
        }
    }

    /// Hides the zap banner (the full OSD took over).
    pub fn hide_banner(&mut self) {
        self.banner = None;
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

    /// Stops mpv's current stream on purpose (its late events are ignored). Low level:
    /// a reconnect uses it without ending the item; [`end_stream`](Self::end_stream) for
    /// good.
    fn stop_stream(&mut self) {
        if let Some(e) = &self.engine {
            if e.player.has_pending_load() {
                self.guard.withdrawn();
            }
            let _ = e.player.stop();
        }
        self.guard.abandon();
    }

    /// **The one place where the current item's provider stream ends for good**:
    /// finished, failed / gave up after the reconnects, stopped by the user, or replaced
    /// by another item. Runs its "stream closed" part at most once per
    /// [`open_stream`](Self::open_stream) (a reconnect in between doesn't end the item).
    /// A connection budget would release its lease here.
    fn end_stream(&mut self, how: StreamEnd) {
        let Some(a) = &mut self.active else {
            return;
        };
        let was_open = std::mem::take(&mut a.stream_open);
        a.reconnect.reset();
        a.load_at = None;
        match &how {
            StreamEnd::Finished => a.finished = true,
            StreamEnd::Failed(msg) => a.failed = Some(msg.clone()),
            StreamEnd::Stopped => {}
        }
        if was_open {
            tracing::debug!(?how, "stream ended for good");
        }
        match how {
            // mpv is already at the end of the file.
            StreamEnd::Finished => self.mark_finished(),
            // Free the provider connection (and ignore the stream's late events).
            StreamEnd::Failed(_) | StreamEnd::Stopped => self.stop_stream(),
        }
    }

    fn start(&mut self, session: Arc<Session>, req: PlaybackRequest, load_at: Option<Instant>) {
        self.save_progress(true);
        let prev = self
            .active
            .as_ref()
            .map(|a| (a.req.kind, a.req.item.clone()));
        // Free the provider connection before opening another one.
        if prev.is_some() {
            self.end_stream(StreamEnd::Stopped);
        }
        // Live → live keeps the old picture (under a scrim) until the new one arrives;
        // anything else starts from black.
        let zapping = req.kind.is_live() && prev.as_ref().is_some_and(|(k, _)| k.is_live());
        if !zapping && let Some(e) = &self.engine {
            e.video.forget_frame();
        }
        let same = prev
            .as_ref()
            .is_some_and(|(_, item)| item.same_item(&req.item));
        if req.kind.is_live() && !same {
            self.banner = Some(Banner {
                since: Instant::now(),
                edge: None,
            });
        } else if !req.kind.is_live() {
            self.banner = None;
        }
        *self
            .notice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        tracing::debug!(kind = ?req.kind, "play");
        self.active = Some(Active::new(req, session, load_at));
        self.sync_episode_queue();
        if let Some(at) = load_at {
            self.ctx
                .request_repaint_after(at.saturating_duration_since(Instant::now()));
        } else {
            self.open_stream(true);
        }
    }

    /// **The one place where a provider stream is opened** (`Player::load`): the first
    /// open of an item (`from_resume`: from its resume point) and every reconnect, retry
    /// and replay (from the last position). A connection budget would acquire its lease
    /// here (`Active::stream_open` tells whether the item already holds one).
    fn open_stream(&mut self, from_resume: bool) {
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
        // Start every stream from mpv's own audio choice and no subtitles: a track id
        // picked for the previous stream means nothing in this one. The language policy
        // runs once the new tracks are known.
        let _ = engine.player.select_audio(TrackSelection::Auto);
        let _ = engine.player.select_subtitle(TrackSelection::Off);
        let opts = LoadOptions {
            user_agent: a.req.headers.user_agent.clone(),
            referrer: a.req.headers.referrer.clone(),
            start,
        };
        tracing::info!(kind = ?a.req.kind, resume = ?start, "opening stream");
        let was_held = engine.player.has_pending_load();
        let refused = match engine.player.load(&a.req.url, &opts) {
            Ok(()) => {
                a.stream_open = true;
                // A held load (no video view yet) replacing another held one adds no
                // StartFile.
                if !(was_held && engine.player.has_pending_load()) {
                    self.guard.issued();
                }
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "load refused");
                Some(crate::error::AppError::from(e).user_message())
            }
        };
        let _ = engine.player.play();
        let now = Instant::now();
        a.tracker.reset(now);
        a.file_loaded = false;
        a.started = false;
        a.subs_applied = false;
        if let Some(msg) = refused {
            self.end_stream(StreamEnd::Failed(msg));
        }
    }

    /// Stops playback (saving progress).
    pub fn stop(&mut self) {
        self.save_progress(true);
        self.end_stream(StreamEnd::Stopped);
        if let Some(e) = &self.engine {
            e.video.forget_frame();
        }
        self.active = None;
        self.banner = None;
    }

    /// Restarts the current item from the beginning (Replay).
    pub fn replay(&mut self) {
        if let Some(a) = &mut self.active {
            a.finished = false;
            a.failed = None;
            a.reconnect.reset();
            a.last_pos = Some(0.0);
            a.req.resume_from = None;
        }
        self.open_stream(false);
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

    /// Selects an audio track chosen by the user and remembers its language for the next
    /// streams of this run (the track id itself is never carried over).
    pub fn choose_audio(&mut self, sel: TrackSelection) {
        if let TrackSelection::Id(id) = sel
            && let Some(lang) = self
                .state()
                .audio_tracks()
                .find(|t| t.id == id)
                .and_then(|t| t.lang.clone())
        {
            self.audio_choice = Some(lang);
        }
        self.select_audio(sel);
    }

    fn apply_track_policy(&mut self) {
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
        let audio = self.audio_choice.as_deref().or(self.audio_pref.as_deref());
        if let Some(id) = tracks::pick_audio(&st.tracks, audio) {
            tracing::debug!(id, "selecting the preferred audio language");
            let _ = engine.player.select_audio(TrackSelection::Id(id));
        }
        // A radio channel has no picture: don't keep the previous channel's on screen.
        if !st.tracks.iter().any(|t| t.kind == TrackKind::Video) {
            engine.video.forget_frame();
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
            a.reconnect.reset();
        }
        self.open_stream(false);
    }

    /// Per-frame housekeeping: player events, reconnects, stalls, progress saves.
    pub fn update(&mut self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let mut ended: Option<EndReason> = None;
        let mut loaded = false;
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
                PlayerEvent::StartFile => self.guard.started(),
                PlayerEvent::QueueOverflow => {
                    tracing::debug!("mpv event queue overflowed");
                    self.guard.resync();
                }
                PlayerEvent::CommandFailed {
                    op: Op::Load,
                    error,
                } => {
                    tracing::info!(%error, "mpv refused the stream");
                    self.guard.withdrawn();
                    if self.guard.is_current() {
                        ended = Some(EndReason::Error(error));
                    }
                }
                PlayerEvent::CommandFailed { op, error } => {
                    tracing::debug!(?op, %error, "mpv command failed");
                }
                PlayerEvent::EndFile(_)
                | PlayerEvent::FileLoaded
                | PlayerEvent::PlaybackRestart
                    if !self.guard.is_current() =>
                {
                    tracing::debug!(?ev, "ignoring an event of the previous stream");
                }
                PlayerEvent::EndFile(reason) => ended = Some(reason),
                PlayerEvent::FileLoaded => loaded = true,
                PlayerEvent::PlaybackRestart => restarted = true,
                _ => {}
            }
        }
        let st = engine.player.state();
        let held_load = engine.player.has_pending_load();
        let now = Instant::now();
        // Debounced start.
        if let Some(at) = self.active.as_ref().and_then(|a| a.load_at) {
            if now >= at {
                if let Some(a) = &mut self.active {
                    a.load_at = None;
                }
                self.open_stream(true);
                return;
            }
            self.ctx.request_repaint_after(at - now);
            return;
        }
        let Some(a) = &mut self.active else {
            return;
        };
        if loaded || restarted {
            a.file_loaded = true;
            a.loaded_once = true;
        }
        if restarted {
            a.started = true;
            if st.time_pos.is_some_and(|p| p > 0.5) || a.req.kind.is_live() {
                *self
                    .notice
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
        }
        if loaded || restarted {
            self.apply_track_policy();
        }
        let Some(a) = &mut self.active else {
            return;
        };

        // Progress tracking (only the current load's position counts).
        let healthy_now = a.reconnect.pending().is_none() && a.failed.is_none() && !a.finished;
        if healthy_now {
            if a.file_loaded {
                a.tracker.observe(st.time_pos, st.paused || held_load, now);
                if let Some(pos) = st.time_pos {
                    a.last_pos = Some(pos);
                }
                if let Some(d) = st.duration.filter(|d| *d > 0.0) {
                    a.duration = Some(d);
                }
                a.reconnect.note_progress(a.tracker.progressing_for(now));
            } else if held_load || st.paused {
                a.tracker.observe(None, true, now);
            }
        }
        let played = a.tracker.progressing_for(now);

        // What the end of the file means; every "for good" goes through `end_stream`.
        let mut end: Option<StreamEnd> = None;
        let mut stream_error = false;
        let live = a.req.kind.is_live();
        match ended {
            None | Some(EndReason::Stop | EndReason::Quit | EndReason::Redirect) => {}
            Some(EndReason::Eof) if live => {
                tracing::info!(played_secs = played.as_secs(), "the live stream ended");
                end = Self::schedule(a, Cause::Ended, played, now);
            }
            Some(EndReason::Eof) => {
                match tracker::vod_end(a.last_pos, a.duration.or(a.req.duration_hint)) {
                    VodEnd::Finished => end = Some(StreamEnd::Finished),
                    VodEnd::Dropped { at } => {
                        tracing::info!(at, "the stream ended before the end; resuming");
                        end = Self::schedule(a, Cause::Ended, played, now);
                    }
                    VodEnd::Failed => {
                        stream_error = true;
                        end = Some(StreamEnd::Failed(Self::vod_failure(a.req.kind)));
                    }
                }
            }
            Some(EndReason::Error(e)) => {
                stream_error = true;
                if live || a.loaded_once {
                    tracing::info!(error = %e, "stream error");
                    end = Self::schedule(a, Cause::Failed, played, now);
                } else {
                    tracing::info!(error = %e, "playback failed");
                    end = Some(StreamEnd::Failed(Self::vod_failure(a.req.kind)));
                }
            }
            Some(EndReason::Unknown(code)) => {
                tracing::debug!(code, "stream ended for an unknown reason");
            }
        }

        // Live stall detection.
        let mut stop_stalled = false;
        if live
            && end.is_none()
            && a.reconnect.pending().is_none()
            && a.failed.is_none()
            && !held_load
            && a.tracker.is_stalled(now)
        {
            tracing::info!(
                idle_secs = a.tracker.idle_for(now).as_secs(),
                "live stream stalled; reconnecting"
            );
            end = Self::schedule(a, Cause::Stalled, Duration::ZERO, now);
            stop_stalled = end.is_none();
        }
        let due = a.reconnect.take_due(now);
        let retry_at = a.reconnect.pending();

        if stream_error {
            self.check_connection_limit();
        }
        if let Some(how) = end {
            self.end_stream(how);
        } else if stop_stalled {
            // Not the end of the item: free the provider connection until the reconnect;
            // the frame stays up under the overlay.
            self.stop_stream();
        }
        if due {
            self.open_stream(false);
        } else if let Some(at) = retry_at {
            self.ctx
                .request_repaint_after(at.saturating_duration_since(now));
        }

        let save_due = self
            .active
            .as_ref()
            .is_some_and(|a| a.last_save.elapsed() >= SAVE_EVERY);
        if save_due {
            self.save_progress(false);
        }
        if self.active.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn vod_failure(kind: MediaKind) -> String {
        if kind == MediaKind::Catchup {
            "Catch-up isn't available for this programme right now. Your provider may not \
             keep an archive for it, or it may be busy."
                .to_owned()
        } else {
            "This video couldn't be played. The provider may be busy or the file may be \
             unavailable."
                .to_owned()
        }
    }

    /// Schedules a reconnect; returns the end for good when the policy gives up.
    fn schedule(a: &mut Active, cause: Cause, played: Duration, now: Instant) -> Option<StreamEnd> {
        match a.reconnect.schedule(cause, played, now) {
            Schedule::Retry { attempt, at } => {
                tracing::info!(
                    attempt,
                    ?cause,
                    delay_secs = at.saturating_duration_since(now).as_secs(),
                    "reconnecting"
                );
                None
            }
            Schedule::AlreadyPending => {
                tracing::debug!(?cause, "a reconnect is already pending");
                None
            }
            Schedule::GiveUp => {
                tracing::info!(?cause, "giving up after repeated failures");
                Some(StreamEnd::Failed(if a.req.kind.is_live() {
                    "The channel stopped and didn't come back. Try again, or pick another \
                     channel."
                        .to_owned()
                } else {
                    "The connection dropped and didn't come back.".to_owned()
                }))
            }
        }
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

    /// What to record for the current item: `(kind, id, position, duration)`.
    fn progress_record(
        &self,
    ) -> Option<(opensmarters_store::PlaybackKind, String, f64, Option<f64>)> {
        let a = self.active.as_ref()?;
        if a.finished {
            return None;
        }
        let pos = a.last_pos.filter(|p| *p >= 1.0)?;
        let duration = a
            .duration
            .or_else(|| self.engine.as_ref().and_then(|e| e.player.state().duration))
            .or(a.req.duration_hint);
        Some((
            a.req.kind.playback_kind()?,
            a.req.item.progress_id()?,
            pos,
            duration,
        ))
    }

    /// Saves the resume point now (VOD/episodes/catch-up). `final_save` is used on stop.
    fn save_progress(&mut self, final_save: bool) {
        let record = self.progress_record();
        let Some(a) = &mut self.active else {
            return;
        };
        a.last_save = Instant::now();
        let Some((kind, id, pos, duration)) = record else {
            return;
        };
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

    /// Saves the resume point on this thread (exit: the background pool may be gone
    /// before a queued write runs).
    fn save_progress_now(&mut self) {
        let Some((kind, id, pos, duration)) = self.progress_record() else {
            return;
        };
        let Some(a) = &self.active else { return };
        let snapshot = Self::snapshot(a);
        match a.session.store().playback().record_progress(
            a.session.profile_id(),
            kind,
            &id,
            pos,
            duration,
            &snapshot,
        ) {
            Ok(_) => tracing::debug!(pos, "progress saved at exit"),
            Err(e) => tracing::warn!(error = %e, "couldn't save the progress at exit"),
        }
    }

    fn mark_finished(&mut self) {
        self.finishes += 1;
        let Some(a) = &self.active else {
            return;
        };
        let (Some(kind), Some(id)) = (a.req.kind.playback_kind(), a.req.item.progress_id()) else {
            return;
        };
        let duration = a
            .duration
            .or_else(|| self.engine.as_ref().and_then(|e| e.player.state().duration))
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

    /// Counts finished items in this run (keys the next-episode countdown, so finishing
    /// the same episode twice starts a fresh countdown).
    pub fn finish_count(&self) -> u64 {
        self.finishes
    }

    /// Counts completed progress writes. Screens showing watch progress (Home's Continue
    /// watching, detail pages) reload when it changes.
    pub fn progress_epoch(&self) -> u64 {
        self.saves.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Paints the video into `rect` (black when nothing plays; the last frame while the
    /// next live channel or a reconnect opens).
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

    /// Selects an audio track (not remembered; see [`choose_audio`](Self::choose_audio)).
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

    /// Cycles to the next audio track (A key); its language is remembered.
    pub fn cycle_audio(&mut self) {
        let st = self.state();
        let tracks: Vec<i64> = st.audio_tracks().map(|t| t.id).collect();
        if tracks.is_empty() {
            return;
        }
        let cur = st.selected(TrackKind::Audio).map(|t| t.id);
        let i = cur
            .and_then(|c| tracks.iter().position(|t| *t == c))
            .map_or(0, |i| (i + 1) % tracks.len());
        self.choose_audio(TrackSelection::Id(tracks[i]));
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

    /// Zaps `delta` channels within the live list (`+1` = next). Stops at the ends of the
    /// list (the banner then says "First channel" / "Last channel").
    pub fn zap(&mut self, delta: isize) -> ZapOutcome {
        let Some(zap) = self.active.as_ref().and_then(|a| a.req.zap.clone()) else {
            return ZapOutcome::Unavailable;
        };
        match zap.step(delta) {
            Ok((index, channel)) => {
                let channel = channel.clone();
                self.zap_to(Arc::clone(&zap.channels), index, &channel)
            }
            Err(edge) => {
                self.banner = Some(Banner {
                    since: Instant::now(),
                    edge: Some(edge),
                });
                ZapOutcome::Edge(edge)
            }
        }
    }

    /// Zaps to the channel with `number` in the live list (`None` if it isn't there).
    pub fn zap_to_number(&mut self, number: u64) -> Option<String> {
        let a = self.active.as_ref()?;
        let zap = a.req.zap.clone()?;
        let (index, channel) = zap.find_number(number)?;
        match self.zap_to(Arc::clone(&zap.channels), index, &channel.clone()) {
            ZapOutcome::Zapped(name) => Some(name),
            _ => None,
        }
    }

    /// Zaps to `channels[index]`, which becomes the zap list (e.g. "All" after a channel
    /// number that isn't in the current category).
    pub fn zap_to_index(&mut self, channels: Arc<Vec<LiveChannel>>, index: usize) -> ZapOutcome {
        let Some(channel) = channels.get(index).cloned() else {
            return ZapOutcome::Unavailable;
        };
        self.zap_to(channels, index, &channel)
    }

    fn zap_to(
        &mut self,
        channels: Arc<Vec<LiveChannel>>,
        index: usize,
        channel: &LiveChannel,
    ) -> ZapOutcome {
        let Some(session) = self.active.as_ref().map(|a| Arc::clone(&a.session)) else {
            return ZapOutcome::Unavailable;
        };
        let ctx = ZapContext { channels, index };
        match session.live_request(channel, Some(ctx)) {
            Ok(req) => {
                let name = req.title.clone();
                self.play_after(session, req, ZAP_DEBOUNCE);
                ZapOutcome::Zapped(name)
            }
            Err(e) => {
                tracing::info!(error = %e, "zap target not playable");
                ZapOutcome::Unavailable
            }
        }
    }

    /// Saves progress synchronously and frees GL resources (call from `on_exit` with the
    /// GL context).
    pub fn destroy(&mut self, gl: &eframe::glow::Context) {
        self.save_progress_now();
        self.end_stream(StreamEnd::Stopped);
        if let Some(e) = &mut self.engine {
            e.video.destroy(gl);
        }
    }
}

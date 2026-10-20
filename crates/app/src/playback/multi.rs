// SPDX-License-Identifier: GPL-3.0-or-later
//! Multiscreen players (`cx.multi`): up to [`MAX_TILES`] live channels at once
//! (`Route::Multiscreen`, `screens::multiscreen`). Each tile is its own
//! [`PlaybackController`] (its own mpv core, video view and connection lease,
//! [`Purpose::Tile`]), created through the same path as the main player.
//!
//! Rules:
//! - **Never more than the budget.** [`capacity`] is the connection limit minus what
//!   recordings hold (they are never preempted; a download gives its slot to a tile), at
//!   most four. Below two, multiscreen doesn't open and [`Refusal`] explains why. A tile
//!   takes its lease when its stream opens (`acquire_for_viewing`), so a tile that can't
//!   get a connection shows `Busy::message()` the way the main player does.
//! - **Staggered starts.** Two tile streams never open less than [`STAGGER`] apart, also
//!   when zapping ([`next_start`]). Real panels count connections opened in a burst as
//!   stale for minutes and then answer 403 to everything.
//! - **Audio follows the focus.** Only the last focused tile has sound
//!   ([`follow_focus`]); M silences that one too. The others are muted.
//! - **Parental control.** A tile never plays a channel of a locked category (this
//!   session's PIN unlocks count). Every tile's zap list is built with [`playable`] from
//!   [`LockedCategories`], and [`MultiView::place`], [`MultiView::zap`] and
//!   [`MultiView::zap_to_number`] check the target again ([`sanitize`], [`step_playable`],
//!   [`number_playable`]), so channel up/down and number entry can't reach a locked
//!   channel even through a list built elsewhere.
//! - **Tuning:** the Small network buffer, the main player's hardware decoding, and the
//!   user's volume, aspect preset and audio language ([`TileSettings`]).
//! - **Engines are kept.** A tile engine is created on first use and reused. Leaving the
//!   screen stops the streams (freeing the connections) but keeps the idle mpv cores, so a
//!   render context is never freed outside a GL context. [`MultiView::destroy`] frees them
//!   at exit (`on_exit`, GL current).
//! - [`MultiView::is_active`] / [`MultiView::stop_all`] serve the sleep timer and profile
//!   changes. A tile whose session is no longer the open profile stops at the next
//!   [`MultiView::update`].

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_core::{CategoryId, Redactor};
use opensmarters_player::{GetProcAddress, LogLevel};
use opensmarters_store::ContentKind;

use super::{
    Buffering, PlaybackController, PlaybackItem, PlaybackRequest, PlaybackStatus, PlayerTuning,
    ZAP_DEBOUNCE, ZapContext,
};
use crate::services::Session;
use crate::services::catalog::{LiveCatalog, LiveChannel};
use crate::services::connections::Purpose;

/// At most this many tiles (2×2).
pub const MAX_TILES: usize = 4;
/// Minimum time between two tile streams opening.
pub const STAGGER: Duration = Duration::from_millis(800);

/// Why multiscreen can't open with the current connection budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The account (or the user's setting) allows one stream at a time.
    OneStream,
    /// Recordings hold the connections multiscreen would need.
    Recordings {
        /// Connections held by recordings.
        recordings: u32,
        /// The limit in effect.
        limit: u32,
    },
}

impl Refusal {
    /// A short headline.
    pub fn title(&self) -> &'static str {
        match self {
            Refusal::OneStream => "One stream at a time",
            Refusal::Recordings { .. } => "Your connections are in use",
        }
    }

    /// Plain-language explanation.
    pub fn message(&self) -> String {
        match self {
            Refusal::OneStream => "Your account allows one stream at a time. If your provider \
                                   allows more, raise \u{201c}Simultaneous streams\u{201d} in \
                                   Settings \u{203a} Playback."
                .to_owned(),
            Refusal::Recordings { recordings, limit } => {
                let who = if *recordings == 1 {
                    "A recording is using 1".to_owned()
                } else {
                    format!("Recordings are using {recordings}")
                };
                format!(
                    "{who} of your {limit} connections, and multiscreen needs two. Try \
                     again when a recording finishes."
                )
            }
        }
    }
}

/// How many tiles the budget allows (2..=[`MAX_TILES`]), or why multiscreen can't open.
/// `holders` are the app's current lease holders: recordings count against it, while the
/// main player (stopped when multiscreen opens), tiles and downloads (viewing takes their
/// slots) don't.
pub fn capacity(limit: u32, holders: &[Purpose]) -> Result<usize, Refusal> {
    if limit <= 1 {
        return Err(Refusal::OneStream);
    }
    let recordings = u32::try_from(
        holders
            .iter()
            .filter(|p| matches!(p, Purpose::Recording(_)))
            .count(),
    )
    .unwrap_or(u32::MAX);
    let free = limit.saturating_sub(recordings);
    if free < 2 {
        return Err(Refusal::Recordings { recordings, limit });
    }
    Ok(usize::try_from(free).unwrap_or(MAX_TILES).min(MAX_TILES))
}

/// How many slots the screen shows: the tiles plus one empty "add a channel" slot while
/// the budget allows another tile, at least two (1×2), never more than the budget or
/// [`MAX_TILES`]. Tiles already playing are never hidden, even if the budget shrank.
pub fn slot_count(tiles: usize, capacity: usize) -> usize {
    (tiles + 1)
        .max(2)
        .min(capacity.max(tiles))
        .min(MAX_TILES.max(tiles))
}

/// When a stream may open: not before `earliest`, and at least `gap` away from every
/// other open in `taken` (scheduled or recent).
pub fn next_start(earliest: Instant, taken: &[Instant], gap: Duration) -> Instant {
    let mut taken = taken.to_vec();
    taken.sort_unstable();
    let mut at = earliest;
    for t in taken {
        // Too close to `t` on either side: move just after it. `taken` is sorted and `at`
        // only grows, so earlier opens can't come back into range.
        if at < t + gap && t < at + gap {
            at = t + gap;
        }
    }
    at
}

/// The tile that has sound after the focus moved to `focused`: that tile if it plays
/// something, else the previous one (focus on the empty slot or a header button keeps
/// the sound where it was).
pub fn follow_focus(audio: usize, focused: usize, tiles: usize) -> usize {
    if focused < tiles {
        focused
    } else {
        audio.min(tiles.saturating_sub(1))
    }
}

/// Where an index (the tile with sound, the focused slot) goes after tile `removed` was
/// removed from a list that now has `len` tiles: later tiles move up one; the removed
/// one's index passes to its successor (or the new last tile).
pub fn after_remove(index: usize, removed: usize, len: usize) -> usize {
    let i = if index > removed { index - 1 } else { index };
    i.min(len.saturating_sub(1))
}

/// The tile that has sound: the last focused one, unless everything is muted.
pub fn audible(audio: usize, tiles: usize, muted: bool) -> Option<usize> {
    (!muted && audio < tiles).then_some(audio)
}

/// The live categories parental control locks right now (manual locks and keyword
/// matches, minus this session's PIN unlocks), to check many channels quickly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockedCategories(HashSet<CategoryId>);

impl LockedCategories {
    /// The locks of `session` for the categories in `catalog`. Recompute after a PIN
    /// unlock or a catalogue reload.
    pub fn of(session: &Session, catalog: &LiveCatalog) -> Self {
        if !session.parental_enabled() {
            return Self::default();
        }
        // Every category a channel names, also those the category list lacks (no name:
        // only a manual lock can apply).
        let mut ids: HashSet<&CategoryId> = catalog.categories.iter().map(|c| &c.id).collect();
        for ch in &catalog.channels {
            ids.extend(ch.categories.iter());
        }
        Self(
            ids.into_iter()
                .filter(|id| {
                    session.is_category_locked(
                        ContentKind::Live,
                        id,
                        catalog.category_name(id).unwrap_or(""),
                    )
                })
                .cloned()
                .collect(),
        )
    }

    /// Locks for exactly these categories (tests).
    pub fn from_ids(ids: impl IntoIterator<Item = CategoryId>) -> Self {
        Self(ids.into_iter().collect())
    }

    /// True if the channel is in a locked category.
    pub fn hides(&self, ch: &LiveChannel) -> bool {
        !self.0.is_empty() && ch.categories.iter().any(|c| self.0.contains(c))
    }
}

/// `channels` without the ones `locked` hides. Every zap list a tile gets is built with
/// this.
pub fn playable(
    channels: &[LiveChannel],
    locked: impl Fn(&LiveChannel) -> bool,
) -> Arc<Vec<LiveChannel>> {
    Arc::new(channels.iter().filter(|c| !locked(c)).cloned().collect())
}

/// A zap list positioned on channel `key` (`None` if the list doesn't have it).
pub fn zap_on(channels: &Arc<Vec<LiveChannel>>, key: &str) -> Option<ZapContext> {
    let index = channels.iter().position(|c| c.key == key)?;
    Some(ZapContext {
        channels: Arc::clone(channels),
        index,
    })
}

/// A zap list a tile may use: unchanged when it holds no locked channel, else rebuilt
/// with [`playable`] on the same channel. `None` when that channel itself is locked (or
/// the index is out of range).
pub fn sanitize(zap: ZapContext, locked: &dyn Fn(&LiveChannel) -> bool) -> Option<ZapContext> {
    let current = zap.channels.get(zap.index)?;
    if locked(current) {
        return None;
    }
    if !zap.channels.iter().any(locked) {
        return Some(zap);
    }
    let key = current.key.clone();
    zap_on(&playable(&zap.channels, locked), &key)
}

/// The channel `delta` steps away in the list (wrapping), passing over any channel
/// `locked` hides. `None` if no other playable channel is in the list.
pub fn step_playable<'a>(
    zap: &'a ZapContext,
    delta: isize,
    locked: &dyn Fn(&LiveChannel) -> bool,
) -> Option<(usize, &'a LiveChannel)> {
    let n = zap.channels.len();
    if n == 0 || delta == 0 {
        return None;
    }
    let n_i = isize::try_from(n).ok()?;
    let start = isize::try_from(zap.index).ok()?;
    let mut i = (start + delta).rem_euclid(n_i);
    for _ in 0..n {
        let idx = usize::try_from(i).ok()?;
        if let Some(ch) = zap.channels.get(idx)
            && !locked(ch)
        {
            return Some((idx, ch));
        }
        i = (i + delta.signum()).rem_euclid(n_i);
    }
    None
}

/// Channel `number` in the list, unless `locked` hides it.
pub fn number_playable<'a>(
    zap: &'a ZapContext,
    number: u64,
    locked: &dyn Fn(&LiveChannel) -> bool,
) -> Option<(usize, &'a LiveChannel)> {
    zap.find_number(number).filter(|(_, ch)| !locked(ch))
}

/// Why a channel couldn't go into a tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceError {
    /// Four tiles already (or no player could be created).
    Full,
    /// The channel is in a locked category.
    Locked,
    /// The channel's address was refused (plain-language message).
    NotPlayable(String),
}

impl PlaceError {
    /// Plain-language text for a toast.
    pub fn message(&self) -> String {
        match self {
            PlaceError::Full => "Multiscreen shows at most four channels at once.".to_owned(),
            PlaceError::Locked => "This channel is in a locked category. Open it in Live TV \
                                   with your PIN first."
                .to_owned(),
            PlaceError::NotPlayable(msg) => msg.clone(),
        }
    }
}

/// What new tile engines are created with (from the main player and the preferences).
#[derive(Debug, Clone)]
pub struct TileSettings {
    /// The egui context (repaints).
    pub egui: egui::Context,
    /// The app-wide redactor (mpv log lines).
    pub redactor: Redactor,
    /// Aspect preset (`VideoFit::PRESETS` index).
    pub fit_index: u8,
    /// Volume 0..=100.
    pub volume: u8,
    /// The main player's hardware decoding.
    pub hw_decoding: bool,
    /// Preferred audio language.
    pub audio_language: Option<String>,
}

struct Tile {
    /// Index into the engine pool (and the lease purpose `Tile(engine)`).
    engine: usize,
    session: Arc<Session>,
    /// The channel's request (its zap list gives the channel and channel up/down).
    req: PlaybackRequest,
    /// When its stream opens (scheduled) or last opened, for staggering.
    opens_at: Option<Instant>,
    /// When the channel last changed (the screen shows the tile's banner).
    changed_at: Instant,
    /// Stopped by [`MultiView::stop_all`]: kept with its channel until restarted.
    stopped: bool,
    /// The mute state last sent to mpv.
    sent_mute: Option<bool>,
}

impl Tile {
    fn channel(&self) -> Option<&LiveChannel> {
        self.req.zap.as_ref().and_then(|z| z.channels.get(z.index))
    }
}

/// The multiscreen tiles. See the [module docs](self).
pub struct MultiView {
    get_proc_address: Option<GetProcAddress>,
    mpv_log: LogLevel,
    settings: Option<TileSettings>,
    engines: Vec<PlaybackController>,
    tiles: Vec<Tile>,
    /// The tile with sound (see [`follow_focus`]).
    audio: usize,
    /// M: the audible tile is muted too.
    muted: bool,
    /// The main player's live channel, handed over by `screens::multiscreen::open`.
    carried: Option<(Arc<Session>, PlaybackRequest)>,
    /// When the main player's stream closed for multiscreen (staggers tile 1).
    main_closed_at: Option<Instant>,
}

impl std::fmt::Debug for MultiView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiView")
            .field("engines", &self.engines.len())
            .field("tiles", &self.tiles.len())
            .field("audio", &self.audio)
            .field("muted", &self.muted)
            .finish_non_exhaustive()
    }
}

impl Default for MultiView {
    fn default() -> Self {
        Self::new(None, LogLevel::Info)
    }
}

impl MultiView {
    /// Tile engines will render with `get_proc_address` (eframe's, as for the main
    /// player; `None` = video unavailable) and forward mpv logs at `mpv_log`.
    pub fn new(get_proc_address: Option<GetProcAddress>, mpv_log: LogLevel) -> Self {
        Self {
            get_proc_address,
            mpv_log,
            settings: None,
            engines: Vec::new(),
            tiles: Vec::new(),
            audio: 0,
            muted: false,
            carried: None,
            main_closed_at: None,
        }
    }

    /// Sets what new tiles use (the screen calls it when it opens). Existing engines pick
    /// up the volume, aspect and audio language at their next start.
    pub fn configure(&mut self, settings: TileSettings) {
        self.settings = Some(settings);
    }

    /// Hands over the main player's live channel for tile 1. The caller has just stopped
    /// the main player (its stream closed now).
    pub fn set_carried(&mut self, carried: Option<(Arc<Session>, PlaybackRequest)>) {
        self.carried = carried;
        self.main_closed_at = Some(Instant::now());
    }

    /// Takes the channel handed over by [`set_carried`](Self::set_carried).
    pub fn take_carried(&mut self) -> Option<(Arc<Session>, PlaybackRequest)> {
        self.carried.take()
    }

    /// Number of tiles (empty slots don't count).
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// No tile.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The tile with sound, if any ([`audible`]).
    pub fn audible(&self) -> Option<usize> {
        audible(self.audio, self.tiles.len(), self.muted)
    }

    /// The focus moved to `slot` (a tile, or the empty slot): the sound follows it.
    pub fn focus(&mut self, slot: usize) {
        self.audio = follow_focus(self.audio, slot, self.tiles.len());
    }

    /// M: mutes / unmutes the tile with sound. Returns true when muted.
    pub fn toggle_mute(&mut self) -> bool {
        self.muted = !self.muted;
        self.muted
    }

    /// Everything is muted (M).
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// The channel of tile `slot`, when known.
    pub fn channel(&self, slot: usize) -> Option<&LiveChannel> {
        self.tiles.get(slot).and_then(Tile::channel)
    }

    /// The request of tile `slot`.
    pub fn request(&self, slot: usize) -> Option<&PlaybackRequest> {
        self.tiles.get(slot).map(|t| &t.req)
    }

    /// The channel key of tile `slot`.
    pub fn channel_key(&self, slot: usize) -> Option<&str> {
        match self.request(slot).map(|r| &r.item) {
            Some(PlaybackItem::Live { key, .. }) => Some(key),
            _ => None,
        }
    }

    /// When tile `slot` last changed channel (or was added).
    pub fn changed_at(&self, slot: usize) -> Option<Instant> {
        self.tiles.get(slot).map(|t| t.changed_at)
    }

    /// Tile `slot` was stopped ([`stop_all`](Self::stop_all)) and waits for a restart.
    pub fn is_stopped(&self, slot: usize) -> bool {
        self.tiles.get(slot).is_some_and(|t| t.stopped)
    }

    /// Playback status of tile `slot` (`None` for an empty slot).
    pub fn status(&self, slot: usize) -> Option<PlaybackStatus> {
        let t = self.tiles.get(slot)?;
        self.engines.get(t.engine).map(PlaybackController::status)
    }

    /// The connection-limit notice of tile `slot`, if any.
    pub fn notice(&self, slot: usize) -> Option<String> {
        let t = self.tiles.get(slot)?;
        self.engines
            .get(t.engine)
            .and_then(PlaybackController::notice)
    }

    /// Paints tile `slot`'s video into `rect` (sized to the tile, so mpv renders at tile
    /// size).
    pub fn paint_video(&mut self, slot: usize, ui: &egui::Ui, rect: egui::Rect) {
        if let Some(t) = self.tiles.get(slot)
            && let Some(e) = self.engines.get_mut(t.engine)
        {
            e.paint_video(ui, rect);
        }
    }

    /// Plays the channel `zap` points at in `slot`: a new tile when `slot` is the empty
    /// slot (returns its index), else a channel change of that tile. The zap list is
    /// checked again ([`sanitize`]): a locked channel is refused and locked channels are
    /// dropped from the list.
    ///
    /// # Errors
    ///
    /// [`PlaceError`]: locked, no free player, or the address was refused.
    pub fn place(
        &mut self,
        slot: usize,
        session: Arc<Session>,
        zap: ZapContext,
        locked: &dyn Fn(&LiveChannel) -> bool,
    ) -> Result<usize, PlaceError> {
        let zap = sanitize(zap, locked).ok_or(PlaceError::Locked)?;
        let ch = zap
            .channels
            .get(zap.index)
            .cloned()
            .ok_or(PlaceError::Locked)?;
        let req = session
            .live_request(&ch, Some(zap))
            .map_err(|e| PlaceError::NotPlayable(e.user_message()))?;
        if let Some(t) = self.tiles.get_mut(slot) {
            t.session = session;
            t.req = req;
            self.schedule(slot, Duration::ZERO);
            return Ok(slot);
        }
        if self.tiles.len() >= MAX_TILES {
            return Err(PlaceError::Full);
        }
        let engine = self.free_engine().ok_or(PlaceError::Full)?;
        self.tiles.push(Tile {
            engine,
            session,
            req,
            opens_at: None,
            changed_at: Instant::now(),
            stopped: false,
            sent_mute: None,
        });
        let slot = self.tiles.len() - 1;
        self.schedule(slot, Duration::ZERO);
        Ok(slot)
    }

    /// Channel up/down in tile `slot` (its zap list, wrapping), passing over locked
    /// channels. Returns the channel name.
    pub fn zap(
        &mut self,
        slot: usize,
        delta: isize,
        locked: &dyn Fn(&LiveChannel) -> bool,
    ) -> Option<String> {
        let zap = self.tiles.get(slot)?.req.zap.clone()?;
        let (index, ch) = step_playable(&zap, delta, locked)?;
        let ch = ch.clone();
        self.switch(slot, &ch, zap.channels, index)
    }

    /// Tile `slot` to channel `number` of its own zap list. Returns the channel name, or
    /// `None` if the list has no such number or it is locked.
    pub fn zap_to_number(
        &mut self,
        slot: usize,
        number: u64,
        locked: &dyn Fn(&LiveChannel) -> bool,
    ) -> Option<String> {
        let zap = self.tiles.get(slot)?.req.zap.clone()?;
        let (index, ch) = number_playable(&zap, number, locked)?;
        let ch = ch.clone();
        self.switch(slot, &ch, zap.channels, index)
    }

    fn switch(
        &mut self,
        slot: usize,
        ch: &LiveChannel,
        channels: Arc<Vec<LiveChannel>>,
        index: usize,
    ) -> Option<String> {
        let t = self.tiles.get_mut(slot)?;
        match t
            .session
            .live_request(ch, Some(ZapContext { channels, index }))
        {
            Ok(req) => {
                let name = req.title.clone();
                t.req = req;
                // Debounced like the main player: zapping five times opens one stream.
                self.schedule(slot, ZAP_DEBOUNCE);
                Some(name)
            }
            Err(e) => {
                tracing::info!(error = %e, "tile zap target not playable");
                None
            }
        }
    }

    /// Removes tile `slot` (its stream stops and its connection is freed).
    pub fn remove(&mut self, slot: usize) {
        if slot >= self.tiles.len() {
            return;
        }
        let t = self.tiles.remove(slot);
        if let Some(e) = self.engines.get_mut(t.engine) {
            e.stop();
        }
        self.audio = after_remove(self.audio, slot, self.tiles.len());
        tracing::debug!(slot, tiles = self.tiles.len(), "multiscreen tile removed");
    }

    /// Starts tile `slot` again (after a stop or a failure).
    pub fn restart(&mut self, slot: usize) {
        if slot < self.tiles.len() {
            self.schedule(slot, Duration::ZERO);
        }
    }

    /// Restarts every stopped tile (returning to the screen), staggered.
    pub fn resume(&mut self) {
        for i in 0..self.tiles.len() {
            if self.tiles[i].stopped {
                self.schedule(i, Duration::ZERO);
            }
        }
    }

    /// Whether any tile is playing (or opening) a stream.
    pub fn is_active(&self) -> bool {
        self.tiles.iter().any(|t| {
            !t.stopped
                && self.engines.get(t.engine).is_some_and(|e| {
                    matches!(
                        e.status(),
                        PlaybackStatus::Opening
                            | PlaybackStatus::Playing
                            | PlaybackStatus::Paused
                            | PlaybackStatus::Buffering
                            | PlaybackStatus::Reconnecting { .. }
                    )
                })
        })
    }

    /// Stops every tile and frees their connections (sleep timer, profile switch, leaving
    /// the screen). The tiles keep their channels until [`resume`](Self::resume),
    /// [`restart`](Self::restart) or [`clear`](Self::clear).
    pub fn stop_all(&mut self) {
        for t in &mut self.tiles {
            if let Some(e) = self.engines.get_mut(t.engine)
                && e.request().is_some()
            {
                e.stop();
            }
            t.stopped = true;
        }
    }

    /// Stops and forgets every tile (a new multiscreen session).
    pub fn clear(&mut self) {
        self.stop_all();
        self.tiles.clear();
        self.audio = 0;
        self.muted = false;
    }

    /// Per-frame housekeeping (the screen calls it): player events, reconnects, stalls;
    /// tiles of another profile stop; the sound follows the focus.
    pub fn update(&mut self, current: Option<&Arc<Session>>) {
        let before = self.tiles.len();
        let engines = &mut self.engines;
        self.tiles.retain(|t| {
            let same = current.is_some_and(|s| Arc::ptr_eq(s, &t.session));
            if !same && let Some(e) = engines.get_mut(t.engine) {
                e.stop();
            }
            same
        });
        if self.tiles.len() != before {
            tracing::info!("multiscreen: the profile changed; its tiles stopped");
            self.audio = self.audio.min(self.tiles.len().saturating_sub(1));
        }
        for e in &mut self.engines {
            e.update();
        }
        let audible = self.audible();
        for (i, t) in self.tiles.iter_mut().enumerate() {
            let mute = audible != Some(i);
            if t.sent_mute != Some(mute) {
                if let Some(p) = self.engines.get(t.engine).and_then(|e| e.player()) {
                    let _ = p.set_mute(mute);
                }
                t.sent_mute = Some(mute);
            }
        }
    }

    /// Frees the tiles' render contexts. Call with the GL context current (app exit).
    pub fn destroy(&mut self, gl: &eframe::glow::Context) {
        self.stop_all();
        for e in &mut self.engines {
            e.destroy(gl);
        }
    }

    /// Opens (or reopens) tile `slot`'s stream at the next free start time.
    fn schedule(&mut self, slot: usize, min_delay: Duration) {
        let now = Instant::now();
        let taken: Vec<Instant> = self
            .tiles
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != slot)
            .filter_map(|(_, t)| t.opens_at)
            .chain(self.main_closed_at)
            .filter(|t| *t + STAGGER > now)
            .collect();
        let at = next_start(now + min_delay, &taken, STAGGER);
        let settings = self.settings.clone();
        let Some(t) = self.tiles.get_mut(slot) else {
            return;
        };
        t.opens_at = Some(at);
        t.changed_at = now;
        t.stopped = false;
        let Some(engine) = self.engines.get_mut(t.engine) else {
            return;
        };
        if let Some(s) = &settings {
            engine.set_track_prefs(s.audio_language.clone(), None);
            if engine.fit_index() != usize::from(s.fit_index) {
                engine.set_fit_index(usize::from(s.fit_index));
            }
        }
        let delay = at.saturating_duration_since(now);
        tracing::debug!(slot, ?delay, "multiscreen tile start");
        let session = Arc::clone(&t.session);
        if delay.is_zero() {
            engine.play(session, t.req.clone());
        } else {
            engine.play_after(session, t.req.clone(), delay);
        }
    }

    /// The lowest engine no tile uses, creating one if needed.
    fn free_engine(&mut self) -> Option<usize> {
        let used: Vec<usize> = self.tiles.iter().map(|t| t.engine).collect();
        if let Some(i) = (0..self.engines.len()).find(|i| !used.contains(i)) {
            return Some(i);
        }
        if self.engines.len() >= MAX_TILES {
            return None;
        }
        let s = self.settings.clone()?;
        let index = self.engines.len();
        let mut engine = PlaybackController::new(
            self.get_proc_address.clone(),
            &s.egui,
            s.redactor.clone(),
            s.fit_index,
            s.volume,
            self.mpv_log,
            // Tiles: the small buffer and no rewind window (less memory and disk per tile).
            PlayerTuning {
                hw_decoding: s.hw_decoding,
                buffering: Buffering::Small,
                rewind_minutes: 0,
            },
        );
        engine.set_connection_purpose(Purpose::Tile(u8::try_from(index).unwrap_or(u8::MAX)));
        engine.set_track_prefs(s.audio_language.clone(), None);
        tracing::info!(tile = index, "multiscreen tile engine created");
        self.engines.push(engine);
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_store::RecordingId;

    #[test]
    fn capacity_follows_the_budget() {
        assert_eq!(capacity(1, &[]), Err(Refusal::OneStream));
        assert_eq!(capacity(0, &[]), Err(Refusal::OneStream));
        assert_eq!(capacity(2, &[]), Ok(2));
        assert_eq!(capacity(3, &[]), Ok(3));
        assert_eq!(capacity(4, &[]), Ok(4));
        assert_eq!(capacity(16, &[]), Ok(MAX_TILES), "never more than four");
        // The main player, tiles and downloads don't count: they give way.
        let held = [
            Purpose::Watch,
            Purpose::Tile(0),
            Purpose::Download(RecordingId(1)),
        ];
        assert_eq!(capacity(3, &held), Ok(3));
        // Recordings are never preempted.
        let rec = [Purpose::Recording(RecordingId(7))];
        assert_eq!(capacity(3, &rec), Ok(2));
        assert_eq!(
            capacity(2, &rec),
            Err(Refusal::Recordings {
                recordings: 1,
                limit: 2
            })
        );
        let two = [
            Purpose::Recording(RecordingId(1)),
            Purpose::Recording(RecordingId(2)),
        ];
        assert_eq!(capacity(5, &two), Ok(3));
    }

    #[test]
    fn refusals_explain_themselves() {
        let one = Refusal::OneStream.message();
        assert!(one.contains("one stream at a time"), "{one}");
        assert!(one.contains("Settings \u{203a} Playback"), "{one}");
        assert_eq!(
            Refusal::Recordings {
                recordings: 1,
                limit: 2
            }
            .message(),
            "A recording is using 1 of your 2 connections, and multiscreen needs two. Try \
             again when a recording finishes."
        );
        assert!(
            Refusal::Recordings {
                recordings: 2,
                limit: 3
            }
            .message()
            .starts_with("Recordings are using 2 of your 3")
        );
    }

    #[test]
    fn slots_offer_one_more_tile_within_the_budget() {
        // Nothing playing yet: 1×2 with two empty slots.
        assert_eq!(slot_count(0, 4), 2);
        assert_eq!(slot_count(1, 2), 2);
        assert_eq!(slot_count(2, 2), 2, "full: no empty slot");
        assert_eq!(slot_count(2, 3), 3);
        assert_eq!(slot_count(3, 3), 3);
        assert_eq!(slot_count(3, 4), 4);
        assert_eq!(slot_count(4, 4), 4);
        // The budget shrank (a setting changed): playing tiles stay, no empty slot.
        assert_eq!(slot_count(3, 2), 3);
        // Refused: nothing to show.
        assert_eq!(slot_count(0, 0), 0);
    }

    #[test]
    fn starts_are_staggered() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        // Nothing else: start at once.
        assert_eq!(next_start(t0, &[], STAGGER), t0);
        // Another open just now: wait for the gap.
        assert_eq!(next_start(t0, &[t0], STAGGER), t0 + STAGGER);
        // An open scheduled shortly after: go after it.
        assert_eq!(
            next_start(t0, &[t0 + ms(300)], STAGGER),
            t0 + ms(300) + STAGGER
        );
        // Opens far enough away don't matter.
        assert_eq!(next_start(t0 + ms(900), &[t0], STAGGER), t0 + ms(900));
        assert_eq!(next_start(t0, &[t0 + ms(2000)], STAGGER), t0);
        // Four tiles at once: 0, 0.8, 1.6, 2.4 s, whatever the order.
        let mut taken: Vec<Instant> = Vec::new();
        for _ in 0..4 {
            let at = next_start(t0, &taken, STAGGER);
            taken.insert(0, at);
        }
        taken.sort_unstable();
        assert_eq!(
            taken,
            vec![t0, t0 + STAGGER, t0 + STAGGER * 2, t0 + STAGGER * 3]
        );
        // A gap between two opens that is too small is skipped.
        assert_eq!(
            next_start(t0, &[t0, t0 + ms(1200)], STAGGER),
            t0 + ms(1200) + STAGGER
        );
        // A gap that is big enough is used.
        assert_eq!(next_start(t0, &[t0, t0 + ms(2000)], STAGGER), t0 + STAGGER);
    }

    #[test]
    fn sound_follows_the_focus() {
        // Focus on a tile: it gets the sound.
        assert_eq!(follow_focus(0, 2, 4), 2);
        // Focus on the empty slot (index == tiles) or a button: the sound stays.
        assert_eq!(follow_focus(1, 3, 3), 1);
        assert_eq!(follow_focus(1, usize::MAX, 3), 1);
        // The sound never points past the tiles.
        assert_eq!(follow_focus(5, 9, 2), 1);
        assert_eq!(audible(1, 3, false), Some(1));
        assert_eq!(audible(1, 3, true), None, "M mutes every tile");
        assert_eq!(audible(0, 0, false), None, "no tile, no sound");
    }

    fn channel(n: u64, category: &str) -> LiveChannel {
        LiveChannel {
            key: n.to_string(),
            number: Some(n),
            name: format!("Channel {n}"),
            logo: None,
            epg_id: None,
            categories: vec![CategoryId::from(category)],
            archive: false,
            archive_days: None,
            added: None,
            is_adult: false,
            source: crate::services::catalog::ChannelSource::Xtream { stream_id: n },
        }
    }

    /// Channels 1–6; 3 and 4 are in the locked "adult" category.
    fn channels() -> Arc<Vec<LiveChannel>> {
        Arc::new(
            (1..=6)
                .map(|n| channel(n, if n == 3 || n == 4 { "adult" } else { "news" }))
                .collect(),
        )
    }

    fn numbers(list: &[LiveChannel]) -> Vec<u64> {
        list.iter().filter_map(|c| c.number).collect()
    }

    #[test]
    fn tile_lists_never_hold_locked_channels() {
        let locks = LockedCategories::from_ids([CategoryId::from("adult")]);
        let hide = |c: &LiveChannel| locks.hides(c);
        let all = channels();
        let ok = playable(&all, hide);
        assert_eq!(numbers(&ok), vec![1, 2, 5, 6]);
        assert!(
            zap_on(&ok, "3").is_none(),
            "a locked channel isn't in the list"
        );
        assert_eq!(zap_on(&ok, "5").map(|z| z.index), Some(2));
        // A list that still holds locked channels is rebuilt on the same channel...
        let z = sanitize(
            ZapContext {
                channels: Arc::clone(&all),
                index: 1,
            },
            &hide,
        )
        .unwrap_or_else(|| panic!("channel 2 is playable"));
        assert_eq!(numbers(&z.channels), vec![1, 2, 5, 6]);
        assert_eq!(z.channels[z.index].key, "2");
        // ...and a locked channel is refused.
        let on_locked = ZapContext {
            channels: Arc::clone(&all),
            index: 2,
        };
        assert!(sanitize(on_locked, &hide).is_none());
        // A clean list is kept as it is.
        let clean = ZapContext {
            channels: Arc::clone(&ok),
            index: 0,
        };
        let kept = sanitize(clean, &hide).unwrap_or_else(|| panic!("clean list"));
        assert!(Arc::ptr_eq(&kept.channels, &ok));
        // Without a PIN nothing is hidden.
        assert!(!LockedCategories::default().hides(&all[2]));
    }

    #[test]
    fn channel_up_down_and_numbers_skip_locked_channels() {
        let locks = LockedCategories::from_ids([CategoryId::from("adult")]);
        let hide = |c: &LiveChannel| locks.hides(c);
        // Even on a list that still holds locked channels (defence in depth).
        let at = |index: usize| ZapContext {
            channels: channels(),
            index,
        };
        let step = |index: usize, delta: isize| {
            step_playable(&at(index), delta, &hide).map(|(_, c)| c.number)
        };
        assert_eq!(step(1, 1), Some(Some(5)), "2 → 5, over 3 and 4");
        assert_eq!(step(4, -1), Some(Some(2)), "5 → 2, over 4 and 3");
        assert_eq!(step(5, 1), Some(Some(1)), "wraps");
        assert_eq!(step(1, 0), None);
        for index in 0..6 {
            for delta in [-7, -2, -1, 1, 2, 7] {
                if let Some((_, c)) = step_playable(&at(index), delta, &hide) {
                    assert!(!hide(c), "{index} {delta:+} reached channel {:?}", c.number);
                }
            }
        }
        assert!(number_playable(&at(0), 3, &hide).is_none());
        assert!(number_playable(&at(0), 4, &hide).is_none());
        assert_eq!(number_playable(&at(0), 5, &hide).map(|(i, _)| i), Some(4));
        // Everything locked: nowhere to go.
        let all_locked =
            LockedCategories::from_ids([CategoryId::from("adult"), CategoryId::from("news")]);
        assert!(step_playable(&at(0), 1, &|c| all_locked.hides(c)).is_none());
    }

    #[test]
    fn an_empty_view_is_inactive_and_safe_to_stop() {
        // The sleep timer and profile switches call these at any time.
        let mut view = MultiView::default();
        assert!(!view.is_active());
        view.stop_all();
        view.clear();
        view.resume();
        view.restart(3);
        view.remove(0);
        view.update(None);
        assert!(view.is_empty());
        assert_eq!(view.audible(), None);
        assert_eq!(view.status(0), None);
        assert!(view.toggle_mute());
        assert!(view.take_carried().is_none());
    }

    #[test]
    fn indices_survive_a_removal() {
        // Removing an earlier tile moves the index up.
        assert_eq!(after_remove(2, 0, 3), 1);
        // Removing the tile itself: its successor takes over...
        assert_eq!(after_remove(1, 1, 3), 1);
        // ...or the new last tile.
        assert_eq!(after_remove(3, 3, 3), 2);
        // Removing a later tile changes nothing.
        assert_eq!(after_remove(0, 2, 3), 0);
        // The last tile went away.
        assert_eq!(after_remove(0, 0, 0), 0);
    }
}

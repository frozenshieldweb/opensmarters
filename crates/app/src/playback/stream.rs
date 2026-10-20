// SPDX-License-Identifier: GPL-3.0-or-later
//! The controller's per-stream tuning state and its hooks (kept out of `mod.rs` so the
//! controller's state machine stays small): the buffering each load gets
//! ([`super::tuning`]), extra buffering for live channels that starve, the live rewind
//! position ([`super::timeshift`]), counters for the stream details overlay and one-shot
//! messages for toasts. (Holding the last picture during zaps and reconnects is the
//! controller's own: `VideoView::set_hold_last_frame` + the screens' scrim.)
//!
//! Call sites in `mod.rs`: `start` (load the remembered channels, a default
//! [`StreamState`]),
//! `open_stream` ([`PlaybackController::before_load`], [`prepare_load`]) and `update`
//! ([`PlaybackController::update_stream`]).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use opensmarters_player::{PlayerState, StreamTuning};
use opensmarters_store::ProfileId;

use super::timeshift::{self, Timeshift};
use super::tuning::{self, Buffering, ChannelBoosts, PlayerTuning};
use super::{Active, PlaybackController, PlaybackItem};
use crate::services::prefs::CHANNEL_BUFFERING;
use crate::services::{Services, Session};

/// Rebuffers after the start that give a live channel extra buffering next time.
pub const REBUFFERS_BEFORE_BOOST: u32 = 3;
/// One-shot messages older than this are dropped unseen.
const MESSAGE_TTL: Duration = Duration::from_secs(6);
/// A reconnect while this far behind live loses something worth mentioning.
const LOST_REWIND_SECS: f64 = 5.0;

/// Controller-wide state: channels with extra buffering, counters, messages.
#[derive(Debug, Default)]
pub struct Tuner {
    boosts: Arc<Mutex<ChannelBoosts>>,
    boosts_requested: bool,
    reconnects_total: u32,
    messages: Vec<(String, Instant)>,
}

/// Per-item state (a field of the controller's active item).
#[derive(Debug, Clone, Default)]
pub struct StreamState {
    /// The buffering of the current load.
    pub(super) tuning: StreamTuning,
    /// The channel's extra buffering level used for the current load.
    pub(super) level: u8,
    /// Reconnects of this item.
    reconnects: u32,
    /// Extra buffering was raised for this item already.
    boosted: bool,
    /// Playback advanced since the load (the start-up buffer is over).
    progressed: bool,
    last_pos: Option<f64>,
    /// Rebuffers since playback started, and whether it is rebuffering (starving) now.
    rebuffers: u32,
    starving: bool,
    /// Live: the viewer's position relative to live.
    pub(super) timeshift: Timeshift,
    /// Live: paused past the rewind window; say so on resume.
    past_window: bool,
    was_paused: bool,
}

/// Counters and settings for the stream details overlay.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StreamHealth {
    /// Reconnects of what is playing.
    pub reconnects: u32,
    /// Reconnects since the app started.
    pub reconnects_total: u32,
    /// The start buffer of the current stream.
    pub start_buffer: Duration,
    /// This channel starts with extra buffering because it stalled before.
    pub extra_buffering: bool,
}

/// The buffering for the load `load_current` is about to issue (and the per-load reset of
/// the item's state).
pub(super) fn prepare_load(
    tuner: &Tuner,
    settings: &PlayerTuning,
    a: &mut Active,
    disk: bool,
) -> StreamTuning {
    let s = &mut a.stream;
    s.level = match &a.req.item {
        PlaybackItem::Live { key, .. } => tuner
            .boosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .level(a.session.profile_id(), key),
        _ => 0,
    };
    s.tuning = tuning::stream_tuning(a.req.kind, settings, s.level, disk);
    s.timeshift.reset();
    s.past_window = false;
    s.progressed = false;
    s.last_pos = None;
    s.starving = false;
    tracing::debug!(
        start_buffer = ?s.tuning.start_buffer,
        disk = s.tuning.disk_cache,
        level = s.level,
        "stream buffering"
    );
    s.tuning
}

impl PlaybackController {
    /// Applies Settings › Playback at once: hardware decoding switches while playing, and
    /// the playing stream gets the new cache sizes and the buffer awaited after a stall.
    /// The start buffer and a disk-backed rewind window apply from the next channel or
    /// film.
    pub fn set_tuning(&mut self, settings: PlayerTuning) {
        let old = std::mem::replace(&mut self.tuning, settings);
        let Some(e) = &self.engine else { return };
        if old.hw_decoding != settings.hw_decoding {
            let _ = e.player.set_hwdec(settings.hwdec());
        }
        if old.buffering != settings.buffering || old.rewind_minutes != settings.rewind_minutes {
            let disk = e.player.config().has_cache_dir();
            if let Some(a) = &mut self.active {
                a.stream.tuning =
                    tuning::stream_tuning(a.req.kind, &settings, a.stream.level, disk);
                let _ = e.player.set_tuning(&a.stream.tuning);
            }
        }
    }

    /// Whether caches go to disk (the player has a cache folder).
    pub fn has_disk_cache(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|e| e.player.config().has_cache_dir())
    }

    /// Counters for the stream details overlay.
    pub fn health(&self) -> StreamHealth {
        let total = self.tune.reconnects_total;
        match &self.active {
            Some(a) => StreamHealth {
                reconnects: a.stream.reconnects,
                reconnects_total: total,
                start_buffer: a.stream.tuning.start_buffer,
                extra_buffering: a.stream.level > 0,
            },
            None => StreamHealth {
                reconnects_total: total,
                ..StreamHealth::default()
            },
        }
    }

    /// Live channels of `profile` that start with extra buffering.
    pub fn boosted_channels(&self, profile: ProfileId) -> usize {
        self.tune
            .boosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .count_for(profile)
    }

    /// Forgets the extra buffering of `profile`'s channels (Settings › Playback).
    pub fn reset_channel_buffering(&mut self, services: &Services, profile: ProfileId) {
        self.tune
            .boosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear_profile(profile);
        let prefix = format!("{profile}:");
        services.blocking("reset extra buffering", move |s| {
            let mut m = s.settings().get(&CHANNEL_BUFFERING)?;
            m.retain(|k, _| !k.starts_with(&prefix));
            s.settings().set(&CHANNEL_BUFFERING, &m)
        });
    }

    /// Messages for a toast, each returned once (stale ones are dropped).
    pub fn take_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.tune.messages)
            .into_iter()
            .filter(|(_, at)| at.elapsed() < MESSAGE_TTL)
            .map(|(m, _)| m)
            .collect()
    }

    pub(super) fn message(&mut self, text: impl Into<String>) {
        let messages = &mut self.tune.messages;
        messages.push((text.into(), Instant::now()));
        if messages.len() > 4 {
            messages.remove(0);
        }
        self.ctx.request_repaint();
    }

    /// Loads the channels with extra buffering once per controller (background).
    pub(super) fn load_boosts(&mut self, session: &Session) {
        if self.tune.boosts_requested {
            return;
        }
        self.tune.boosts_requested = true;
        let services = session.services();
        let store = Arc::clone(&services.store);
        let boosts = Arc::clone(&self.tune.boosts);
        services
            .rt()
            .spawn_blocking(move || match store.settings().get(&CHANNEL_BUFFERING) {
                Ok(map) => boosts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .merge(map),
                Err(e) => tracing::info!(error = %e, "couldn't read extra buffering"),
            });
    }

    /// Called first thing by `load_current`. On a reconnect (a scheduled retry): count it,
    /// give a live channel that ran dry more buffering, and say so if the viewer was
    /// behind live (the rewind buffer goes with the old stream).
    pub(super) fn before_load(&mut self, from_resume: bool) {
        let Some(a) = &mut self.active else { return };
        if from_resume || a.reconnect.attempts() == 0 {
            return;
        }
        a.stream.reconnects += 1;
        self.tune.reconnects_total += 1;
        let live = a.req.kind.is_live();
        let starved = a.stream.starving;
        let lost = live && a.stream.timeshift.behind() > LOST_REWIND_SECS;
        if live && starved {
            self.boost_current();
        }
        if lost {
            self.message("Back to live: the connection dropped, so the rewind buffer was lost.");
        }
    }

    /// Per frame, from `update`: rebuffers, the live rewind position (a long pause keeps
    /// the stream flowing) and the resume message.
    pub(super) fn update_stream(&mut self, st: &PlayerState, now: Instant) {
        let rewind_window = self.tuning.rewind_window().map(|w| w.as_secs_f64());
        let (Some(a), Some(e)) = (&mut self.active, &self.engine) else {
            return;
        };
        let s = &mut a.stream;
        if let Some(pos) = st.time_pos {
            if a.loaded_once && s.last_pos.is_some_and(|p| (p - pos).abs() > 0.2) {
                s.progressed = true;
            }
            s.last_pos = Some(pos);
        }
        let starving = s.progressed && st.paused_for_cache && !st.seeking;
        if starving && !s.starving {
            s.rebuffers += 1;
            tracing::debug!(count = s.rebuffers, "rebuffering");
        }
        s.starving = starving;
        let mut boost = a.req.kind.is_live() && s.rebuffers >= REBUFFERS_BEFORE_BOOST;
        let mut resumed_past_window = false;
        if a.req.kind.is_live() {
            let lim = timeshift::Limits {
                window: rewind_window,
                forward_cap: s.tuning.forward_bytes,
                lead: timeshift::live_lead(&s.tuning),
            };
            if let Some(target) = s.timeshift.tick(now, st, &lim) {
                tracing::debug!(target, "paused past the rewind window; moving forward");
                let _ = e.player.seek_absolute(target);
            }
            if s.timeshift.take_past_window() {
                s.past_window = true;
            }
            if s.was_paused && !st.paused && s.past_window {
                s.past_window = false;
                resumed_past_window = true;
            }
        }
        s.was_paused = st.paused;
        boost &= !s.boosted;
        if boost {
            self.boost_current();
        }
        if resumed_past_window {
            self.message(
                "You paused for longer than the rewind window, so playback continues from the \
                 oldest saved point.",
            );
        }
    }

    /// The live channel ran dry: it starts with more buffering from now on (Buffering:
    /// Auto), remembered for this profile.
    fn boost_current(&mut self) {
        if self.tuning.buffering != Buffering::Auto {
            return;
        }
        let Some(a) = &mut self.active else { return };
        let PlaybackItem::Live { key, .. } = &a.req.item else {
            return;
        };
        if a.stream.boosted || a.stream.level >= tuning::MAX_EXTRA_LEVEL {
            return;
        }
        a.stream.boosted = true;
        let profile = a.session.profile_id();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let level = self
            .tune
            .boosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bump(profile, key, now);
        if level == 0 {
            return;
        }
        tracing::info!(level, "channel ran dry; more buffering from now on");
        a.stream.level = level;
        if let Some(e) = &self.engine {
            let disk = e.player.config().has_cache_dir();
            a.stream.tuning = tuning::stream_tuning(a.req.kind, &self.tuning, level, disk);
            // The buffer awaited after the next stall grows at once.
            let _ = e.player.set_tuning(&a.stream.tuning);
        }
        if let Some(k) = tuning::boost_key(profile, key) {
            a.session
                .services()
                .blocking("remember extra buffering", move |s| {
                    let mut m = s.settings().get(&CHANNEL_BUFFERING)?;
                    tuning::bump(&mut m, &k, now);
                    s.settings().set(&CHANNEL_BUFFERING, &m)
                });
        }
    }
}

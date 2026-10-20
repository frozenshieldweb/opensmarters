// SPDX-License-Identifier: GPL-3.0-or-later
//! Pause and rewind live TV: how far behind live the viewer is, and seek targets that
//! stay inside what was received.
//!
//! A live stream plays with a seekable cache ([`super::tuning`]): the range around the
//! playback position (mpv's `demuxer-cache-state`) is the rewind window. "Behind live" is
//! what the viewer chose (pausing, rewinding, minus fast-forwarding), never the normal
//! start-up buffer or a rebuffer, so the OSD says "LIVE" until the viewer acts. It can't
//! exceed the media received after the playback position.
//!
//! Paused longer than the window: mpv stops reading once the cache ahead is full, and the
//! provider would then drop the connection. Instead, the paused position moves forward
//! ([`NUDGE`] at a time) so the stream keeps flowing and the oldest part is dropped;
//! playing again resumes at the oldest point still held, and the controller says so.
//!
//! The controller API (`live_view`, `live_seek`, `go_live`, `can_rewind_live`) is at the
//! end of this file; the per-frame part runs in [`super::stream`].

use std::time::Instant;

use opensmarters_player::{PlayerState, StreamTuning};

use super::PlaybackController;

/// Closer than this to live counts as live.
pub const AT_LIVE: f64 = 1.5;
/// While paused past the window, the paused position moves forward this far at a time.
pub const NUDGE: f64 = 30.0;
/// Seeks stay this far from the oldest buffered point (it may be dropped meanwhile).
const OLDEST_MARGIN: f64 = 1.0;
/// Pause past the window is detected when the cache ahead is this full.
const FULL: f64 = 0.92;

/// Limits for [`Timeshift::tick`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// The rewind window in seconds (`None` = rewind off).
    pub window: Option<f64>,
    /// The forward cache cap in bytes.
    pub forward_cap: u64,
    /// Distance to keep from the newest data (the start buffer).
    pub lead: f64,
}

/// The viewer's position relative to live, for one live stream. Reset it when a new
/// stream opens (zap, reconnect).
#[derive(Debug, Clone, Default)]
pub struct Timeshift {
    behind: f64,
    last_tick: Option<Instant>,
    last_nudge: Option<Instant>,
    past_window: bool,
}

/// What the live OSD shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveView {
    /// Seconds behind live (0 = live).
    pub behind: f64,
    /// Playback position (`time-pos`).
    pub pos: f64,
    /// Oldest position that can be seeked to.
    pub oldest: f64,
    /// The live point on the same timeline (`pos + behind`).
    pub live: f64,
    /// The rewind window in seconds (0 = rewind off).
    pub window: f64,
}

impl LiveView {
    /// Whether the viewer is at live.
    pub fn is_live(&self) -> bool {
        self.behind < AT_LIVE
    }

    /// Fractions (0..=1) along a bar spanning the window that ends at live: where the
    /// buffered part starts and where the playback position is.
    pub fn bar(&self) -> (f32, f32) {
        let span = self.window.max(self.live - self.oldest).max(1.0);
        let start = self.live - span;
        let f = |t: f64| (((t - start) / span) as f32).clamp(0.0, 1.0);
        (f(self.oldest), f(self.pos))
    }

    /// The position at fraction `frac` (0..=1) of the bar (a click on it).
    pub fn time_at(&self, frac: f64) -> f64 {
        let span = self.window.max(self.live - self.oldest).max(1.0);
        self.live - span + span * frac.clamp(0.0, 1.0)
    }
}

impl Timeshift {
    /// Back to live (new stream).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Seconds behind live.
    pub fn behind(&self) -> f64 {
        self.behind
    }

    /// Whether the viewer is at live.
    pub fn is_live(&self) -> bool {
        self.behind < AT_LIVE
    }

    /// True once if the paused position had to move forward since the last call.
    pub fn take_past_window(&mut self) -> bool {
        std::mem::take(&mut self.past_window)
    }

    /// Per frame: counts paused time as "behind" and keeps a long pause inside the window.
    /// Returns a position to seek to (still paused) when the paused position must move
    /// forward.
    pub fn tick(&mut self, now: Instant, st: &PlayerState, lim: &Limits) -> Option<f64> {
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        self.last_tick = Some(now);
        if !st.file_loaded {
            return None;
        }
        if st.paused {
            self.behind += dt;
        }
        let pos = st.time_pos?;
        let newest = st.cache.range_at(pos).map(|r| r.end)?;
        self.behind = self.behind.min((newest - pos).max(0.0));
        if !st.paused || st.seeking {
            return None;
        }
        let over_time = lim.window.is_some_and(|w| self.behind > w);
        #[allow(clippy::cast_precision_loss)]
        let over_bytes = st
            .cache
            .forward_bytes
            .is_some_and(|b| lim.forward_cap > 0 && b as f64 >= FULL * lim.forward_cap as f64);
        let due = self
            .last_nudge
            .is_none_or(|t| now.saturating_duration_since(t).as_secs_f64() >= 2.0);
        if !(over_time || over_bytes) || !due {
            return None;
        }
        let target = (pos + NUDGE).min(newest - lim.lead);
        if target <= pos + 0.5 {
            return None;
        }
        self.last_nudge = Some(now);
        self.past_window = true;
        self.on_seek(pos, target);
        Some(target)
    }

    /// The absolute position for a seek by `delta` seconds, kept inside the buffered range
    /// (at least `lead` seconds from the newest data). `None` if there is nowhere to go.
    pub fn seek_target(st: &PlayerState, delta: f64, lead: f64) -> Option<f64> {
        let pos = st.time_pos?;
        let range = st.cache.range_at(pos)?;
        let lo = range.start + OLDEST_MARGIN;
        let hi = range.end - lead;
        if !delta.is_finite() || hi <= lo {
            return None;
        }
        let target = (pos + delta).clamp(lo, hi);
        ((target - pos).abs() >= 0.25).then_some(target)
    }

    /// Where "Go live" seeks: forward by how far behind the viewer is, at most to `lead`
    /// seconds before the newest data. `None` when already there.
    pub fn live_target(&self, st: &PlayerState, lead: f64) -> Option<f64> {
        let pos = st.time_pos?;
        let range = st.cache.range_at(pos)?;
        let target = (pos + self.behind).min(range.end - lead);
        (target > pos + 0.25).then_some(target)
    }

    /// Records a seek the viewer made (or a nudge): moving back adds to "behind".
    pub fn on_seek(&mut self, from: f64, to: f64) {
        self.behind = (self.behind + from - to).max(0.0);
        if self.behind < AT_LIVE {
            self.behind = 0.0;
        }
    }

    /// The viewer is at live again ("Go live").
    pub fn set_live(&mut self) {
        self.behind = 0.0;
    }

    /// The OSD view, if the stream has a buffered range.
    pub fn view(&self, st: &PlayerState, window: Option<f64>) -> Option<LiveView> {
        let pos = st.time_pos?;
        let range = st.cache.range_at(pos)?;
        Some(LiveView {
            behind: self.behind,
            pos,
            oldest: range.start.min(pos),
            live: pos + self.behind,
            window: window.unwrap_or(0.0),
        })
    }
}

/// Distance kept from the newest data when moving forward on live TV: the stream's start
/// buffer, at least 1 s (so playback doesn't rebuffer at once).
pub fn live_lead(t: &StreamTuning) -> f64 {
    t.start_buffer.as_secs_f64().max(1.0)
}

/// What a live seek did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSeek {
    /// Moved.
    Moved,
    /// Already at live (nothing newer).
    AtLive,
    /// Already at the oldest point held.
    AtOldest,
    /// Nothing to seek in (nothing buffered yet, reconnecting, not live).
    Unavailable,
}

impl PlaybackController {
    /// The live rewind state for the OSD (live items with a buffered range only).
    pub fn live_view(&self) -> Option<LiveView> {
        let a = self.active.as_ref()?;
        if !a.req.kind.is_live() || a.retry_at.is_some() || a.load_at.is_some() {
            return None;
        }
        let window = self.tuning.rewind_window().map(|w| w.as_secs_f64());
        a.stream.timeshift.view(&self.state(), window)
    }

    /// Whether ←/→ move in time on live TV ("Rewind live TV" is on).
    pub fn can_rewind_live(&self) -> bool {
        self.tuning.rewind_minutes > 0
    }

    /// Seeks a live stream by `delta` seconds inside its buffer (the rewind window).
    pub fn live_seek(&mut self, delta: f64) -> LiveSeek {
        let (Some(e), Some(a)) = (&self.engine, &mut self.active) else {
            return LiveSeek::Unavailable;
        };
        if !a.req.kind.is_live() || a.retry_at.is_some() || a.load_at.is_some() {
            return LiveSeek::Unavailable;
        }
        let st = e.player.state();
        let Some(pos) = st.time_pos else {
            return LiveSeek::Unavailable;
        };
        let ts = &mut a.stream.timeshift;
        match Timeshift::seek_target(&st, delta, live_lead(&a.stream.tuning)) {
            Some(target) => {
                let _ = e.player.seek_absolute(target);
                ts.on_seek(pos, target);
                LiveSeek::Moved
            }
            None if delta > 0.0 => {
                // Nothing newer is held: this is live.
                ts.set_live();
                LiveSeek::AtLive
            }
            None if st.cache.range_at(pos).is_some() => LiveSeek::AtOldest,
            None => LiveSeek::Unavailable,
        }
    }

    /// Back to live: seeks forward by the delay and plays. Returns false when already
    /// live.
    pub fn go_live(&mut self) -> bool {
        let (Some(e), Some(a)) = (&self.engine, &mut self.active) else {
            return false;
        };
        let ts = &mut a.stream.timeshift;
        if !a.req.kind.is_live() || ts.is_live() {
            return false;
        }
        let st = e.player.state();
        if let Some(target) = ts.live_target(&st, live_lead(&a.stream.tuning)) {
            let _ = e.player.seek_absolute(target);
        }
        ts.set_live();
        let _ = e.player.play();
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opensmarters_player::{CacheState, SeekRange};

    use super::*;

    fn state(pos: f64, start: f64, end: f64, paused: bool) -> PlayerState {
        PlayerState {
            time_pos: Some(pos),
            paused,
            file_loaded: true,
            cache: CacheState {
                seekable_ranges: vec![SeekRange { start, end }],
                cache_end: Some(end),
                ..CacheState::default()
            },
            ..PlayerState::default()
        }
    }

    const LIM: Limits = Limits {
        window: Some(600.0),
        forward_cap: 1_000_000,
        lead: 1.0,
    };

    #[test]
    fn pausing_counts_as_behind_live() {
        let mut ts = Timeshift::default();
        let t0 = Instant::now();
        // Playing at live: the start-up buffer is not "behind".
        assert_eq!(ts.tick(t0, &state(10.0, 0.0, 12.0, false), &LIM), None);
        assert_eq!(
            ts.tick(
                t0 + Duration::from_secs(5),
                &state(15.0, 0.0, 17.0, false),
                &LIM
            ),
            None
        );
        assert!(ts.is_live());
        // Paused for 20 s while 20 s more arrived.
        let mut t = t0 + Duration::from_secs(5);
        for i in 1..=20 {
            t += Duration::from_secs(1);
            ts.tick(t, &state(15.0, 0.0, 17.0 + f64::from(i), true), &LIM);
        }
        assert!((ts.behind() - 20.0).abs() < 1e-6, "{}", ts.behind());
        assert!(!ts.is_live());
        // Playing again: stays 20 s behind.
        t += Duration::from_secs(3);
        ts.tick(t, &state(18.0, 0.0, 40.0, false), &LIM);
        assert!((ts.behind() - 20.0).abs() < 1e-6);
        // Never more than what was received after the position.
        ts.tick(t, &state(18.0, 0.0, 25.0, false), &LIM);
        assert!((ts.behind() - 7.0).abs() < 1e-6);
    }

    #[test]
    fn seeks_stay_inside_the_buffer() {
        let st = state(100.0, 40.0, 130.0, false);
        assert_eq!(Timeshift::seek_target(&st, -10.0, 1.0), Some(90.0));
        assert_eq!(
            Timeshift::seek_target(&st, -500.0, 1.0),
            Some(41.0),
            "oldest"
        );
        assert_eq!(
            Timeshift::seek_target(&st, 500.0, 1.0),
            Some(129.0),
            "live edge"
        );
        let at_edge = state(129.0, 40.0, 130.0, false);
        assert_eq!(Timeshift::seek_target(&at_edge, 10.0, 1.0), None);
        assert_eq!(Timeshift::seek_target(&st, f64::NAN, 1.0), None);
        let tiny = state(1.0, 0.0, 1.5, false);
        assert_eq!(Timeshift::seek_target(&tiny, -10.0, 1.0), None);
        assert_eq!(
            Timeshift::seek_target(&PlayerState::default(), -10.0, 1.0),
            None
        );

        let mut ts = Timeshift::default();
        ts.on_seek(100.0, 90.0);
        assert!((ts.behind() - 10.0).abs() < 1e-9);
        ts.on_seek(90.0, 99.5);
        assert_eq!(ts.behind(), 0.0, "within AT_LIVE counts as live");
    }

    #[test]
    fn go_live_moves_forward_by_the_delay() {
        let mut ts = Timeshift::default();
        ts.on_seek(100.0, 70.0);
        let st = state(70.0, 0.0, 102.0, false);
        assert_eq!(ts.live_target(&st, 1.0), Some(100.0));
        let short = state(70.0, 0.0, 90.0, false);
        assert_eq!(ts.live_target(&short, 1.0), Some(89.0), "not past the data");
        ts.set_live();
        assert_eq!(ts.live_target(&st, 1.0), None);
    }

    #[test]
    fn a_long_pause_moves_the_position_forward() {
        let mut ts = Timeshift::default();
        let lim = Limits {
            window: Some(60.0),
            ..LIM
        };
        let mut t = Instant::now();
        ts.tick(t, &state(10.0, 0.0, 11.0, true), &lim);
        let mut nudges = Vec::new();
        for i in 1..=80 {
            t += Duration::from_secs(1);
            let end = 11.0 + f64::from(i);
            if let Some(target) = ts.tick(t, &state(10.0, 0.0, end, true), &lim) {
                nudges.push((i, target));
                break;
            }
        }
        // The window (60 s) is exceeded after 61 s; the position moves 30 s forward.
        assert_eq!(nudges, vec![(61, 40.0)]);
        assert!(ts.take_past_window());
        assert!(!ts.take_past_window(), "reported once");
        assert!((ts.behind() - 31.0).abs() < 1e-6, "{}", ts.behind());

        // A full forward cache triggers it too (high bitrate), and not more than every 2 s.
        let mut ts = Timeshift::default();
        let mut st = state(10.0, 0.0, 50.0, true);
        st.cache.forward_bytes = Some(950_000);
        let t = Instant::now();
        assert_eq!(ts.tick(t, &st, &LIM), Some(40.0));
        let mut st2 = state(40.0, 0.0, 80.0, true);
        st2.cache.forward_bytes = Some(950_000);
        assert_eq!(ts.tick(t + Duration::from_secs(1), &st2, &LIM), None);
        assert_eq!(ts.tick(t + Duration::from_secs(3), &st2, &LIM), Some(70.0));
    }

    #[test]
    fn the_bar_ends_at_live() {
        let v = LiveView {
            behind: 150.0,
            pos: 450.0,
            oldest: 300.0,
            live: 600.0,
            window: 600.0,
        };
        let (buffered, pos) = v.bar();
        assert!((buffered - 0.5).abs() < 1e-6);
        assert!((pos - 0.75).abs() < 1e-6);
        assert!(!v.is_live());
        // More buffered than the window (low bitrate): the bar grows to fit.
        let wide = LiveView {
            oldest: -300.0,
            ..v
        };
        assert_eq!(wide.bar().0, 0.0);
    }
}

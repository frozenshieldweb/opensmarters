// SPDX-License-Identifier: GPL-3.0-or-later
//! Stream health, as pure logic the controller drives (no mpv, no clock of its own, so
//! every rule is unit-tested):
//!
//! - [`ProgressTracker`]: is `time-pos` advancing? A position counts as progress once it
//!   moved more than [`ADVANCE_EPSILON`] from the **reference** position (the last one
//!   that counted), not from the previous frame's: at 25–120 fps the per-frame deltas are
//!   far below the threshold, so comparing frame to frame would call every healthy stream
//!   stalled. A live stream is stalled after [`STALL_AFTER`] without progress (or
//!   [`OPEN_TIMEOUT`] before its first progress after a (re)load).
//! - [`Reconnector`]: one scheduled reconnect at a time (a stall and the old stream's
//!   end-of-file never schedule two), backoff [`RETRY_DELAYS`], give up after the kind's
//!   maximum. The attempt counter resets only after [`RESET_AFTER`] of continuous
//!   progress since the last (re)load, so a flapping stream backs off instead of
//!   reconnecting every 2 s forever (each reconnect is a new provider connection). A live
//!   stream the server **ends cleanly** after [`CLEAN_END_AFTER`] of playback starts over
//!   at the first delay (it is a stream that ends, not a failing connection).
//! - [`vod_end`]: an end-of-file of a film/episode/catch-up is *finished* only near the
//!   end; earlier it is a dropped connection, resumed from the last position.
//! - [`LoadGuard`]: ties mpv's events to the load that caused them, so a late
//!   `FileLoaded`/`EndFile` of the previous stream can't hit the new one.

use std::time::{Duration, Instant};

/// Reconnect delays for successive attempts.
pub const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(30),
];
/// Reconnect attempts for a live channel before giving up.
pub const LIVE_MAX_RETRIES: u32 = RETRY_DELAYS.len() as u32;
/// Reconnect attempts for films, episodes and catch-up (resumed from the last position).
pub const VOD_MAX_RETRIES: u32 = 3;
/// A live stream with no progress for this long (while not paused) is reconnected.
pub const STALL_AFTER: Duration = Duration::from_secs(10);
/// Before its first progress after a (re)load, a live stream gets this long.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// A film/episode/catch-up stream with no progress for this long (while not paused) is
/// reconnected from its last position. Longer than live: a seek or a slow server may
/// legitimately buffer for a while. Our libmpv fetches HTTP through libcurl, which has
/// no read timeout, so a server that stops sending never produces an error or an
/// end-of-file: this tracker is the only stall detector for every kind of stream.
pub const VOD_STALL_AFTER: Duration = Duration::from_secs(30);
/// Before its first progress after a (re)load, a film/episode/catch-up stream gets
/// this long (covers a stream that never opens).
pub const VOD_OPEN_TIMEOUT: Duration = Duration::from_secs(45);
/// Continuous progress for this long after a (re)load resets the attempt counter.
pub const RESET_AFTER: Duration = Duration::from_secs(30);
/// A live stream that ended cleanly after this much playback reconnects without
/// escalating the backoff.
pub const CLEAN_END_AFTER: Duration = Duration::from_secs(10);
/// A position change smaller than this (seconds) is not progress.
pub const ADVANCE_EPSILON: f64 = 0.2;
/// A pause in progress longer than this breaks a "continuous" run.
const RUN_GAP: Duration = Duration::from_secs(3);
/// End-of-file within this fraction of the duration means finished.
const FINISHED_FRACTION: f64 = 0.95;
/// End-of-file with less than this left (seconds) means finished.
const FINISHED_REMAINING: f64 = 30.0;

/// Watches the playback position. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct ProgressTracker {
    reference: Option<f64>,
    last_advance: Instant,
    progressed: bool,
    run_start: Option<Instant>,
}

impl ProgressTracker {
    /// A tracker for a stream (re)loaded at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            reference: None,
            last_advance: now,
            progressed: false,
            run_start: None,
        }
    }

    /// Starts over for a (re)load at `now`.
    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now);
    }

    /// Feeds the current position. `hold` = not expected to advance (paused by the user,
    /// waiting for the video view): it never counts as a stall, and ends a progress run.
    pub fn observe(&mut self, pos: Option<f64>, hold: bool, now: Instant) {
        if hold {
            self.last_advance = now;
            self.run_start = None;
            return;
        }
        let Some(pos) = pos.filter(|p| p.is_finite()) else {
            return;
        };
        match self.reference {
            // The first reading after a (re)load is the reference, not progress.
            None => self.reference = Some(pos),
            Some(r) if (pos - r).abs() <= ADVANCE_EPSILON => {}
            Some(_) => {
                if self.run_start.is_none() || now.duration_since(self.last_advance) > RUN_GAP {
                    self.run_start = Some(now);
                }
                self.reference = Some(pos);
                self.last_advance = now;
                self.progressed = true;
            }
        }
    }

    /// True once the position advanced since the last (re)load.
    pub fn has_progressed(&self) -> bool {
        self.progressed
    }

    /// How long the position hasn't advanced.
    pub fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_advance)
    }

    /// True if a live stream should be reconnected: no progress for [`STALL_AFTER`]
    /// ([`OPEN_TIMEOUT`] before the first progress).
    pub fn is_stalled(&self, now: Instant) -> bool {
        self.is_stalled_for(true, now)
    }

    /// True if the stream should be reconnected. Live: [`STALL_AFTER`] /
    /// [`OPEN_TIMEOUT`]; films, episodes and catch-up: [`VOD_STALL_AFTER`] /
    /// [`VOD_OPEN_TIMEOUT`]. Time spent paused by the user never counts (see
    /// [`ProgressTracker::observe`]).
    pub fn is_stalled_for(&self, live: bool, now: Instant) -> bool {
        let limit = match (live, self.progressed) {
            (true, true) => STALL_AFTER,
            (true, false) => OPEN_TIMEOUT,
            (false, true) => VOD_STALL_AFTER,
            (false, false) => VOD_OPEN_TIMEOUT,
        };
        self.idle_for(now) > limit
    }

    /// How long the position has been advancing without a break (zero if it isn't now).
    pub fn progressing_for(&self, now: Instant) -> Duration {
        match self.run_start {
            Some(start) if self.idle_for(now) <= RUN_GAP => now.saturating_duration_since(start),
            _ => Duration::ZERO,
        }
    }
}

/// Why a reconnect was needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// No progress (live).
    Stalled,
    /// The server ended the stream (live end-of-file, or VOD end-of-file far from the end).
    Ended,
    /// The stream failed (network error, refused).
    Failed,
}

/// What [`Reconnector::schedule`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Reconnect at `at` (attempt `attempt`, 1-based).
    Retry {
        /// Attempt number.
        attempt: u32,
        /// When to reload.
        at: Instant,
    },
    /// A reconnect is already pending; nothing changed.
    AlreadyPending,
    /// Out of attempts.
    GiveUp,
}

/// Reconnect scheduling and backoff. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct Reconnector {
    max: u32,
    attempts: u32,
    retry_at: Option<Instant>,
    cause: Option<Cause>,
}

impl Reconnector {
    /// A reconnector allowing `max` attempts in a row.
    pub fn new(max: u32) -> Self {
        Self {
            max,
            attempts: 0,
            retry_at: None,
            cause: None,
        }
    }

    /// Attempts used so far (the "(n)" in "Reconnecting… (n)").
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// When the pending reconnect fires.
    pub fn pending(&self) -> Option<Instant> {
        self.retry_at
    }

    /// Why the pending (or last) reconnect was scheduled.
    pub fn cause(&self) -> Option<Cause> {
        self.cause
    }

    /// Forgets attempts and any pending reconnect (user "Try again", a new item).
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.retry_at = None;
        self.cause = None;
    }

    /// Notes the current continuous progress: [`RESET_AFTER`] of it since the last
    /// (re)load means the stream is healthy again.
    pub fn note_progress(&mut self, progressing_for: Duration) {
        if self.retry_at.is_none() && progressing_for >= RESET_AFTER {
            self.attempts = 0;
            self.cause = None;
        }
    }

    /// Schedules a reconnect for `cause`. `played_for` is the continuous progress before
    /// the problem (for the clean-end rule). Never schedules a second one while one is
    /// pending.
    pub fn schedule(&mut self, cause: Cause, played_for: Duration, now: Instant) -> Schedule {
        if self.retry_at.is_some() {
            return Schedule::AlreadyPending;
        }
        if played_for >= RESET_AFTER || (cause == Cause::Ended && played_for >= CLEAN_END_AFTER) {
            self.attempts = 0;
        }
        if self.attempts >= self.max {
            self.retry_at = None;
            return Schedule::GiveUp;
        }
        let delay = RETRY_DELAYS[(self.attempts as usize).min(RETRY_DELAYS.len() - 1)];
        self.attempts += 1;
        let at = now + delay;
        self.retry_at = Some(at);
        self.cause = Some(cause);
        Schedule::Retry {
            attempt: self.attempts,
            at,
        }
    }

    /// True (once) when the pending reconnect is due; it is then no longer pending.
    pub fn take_due(&mut self, now: Instant) -> bool {
        match self.retry_at {
            Some(at) if now >= at => {
                self.retry_at = None;
                true
            }
            _ => false,
        }
    }
}

/// What an end-of-file of a film, episode or catch-up programme means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VodEnd {
    /// Watched to the end.
    Finished,
    /// The connection dropped part-way: resume from this position.
    Dropped {
        /// Last known position (seconds).
        at: f64,
    },
    /// It ended before anything played.
    Failed,
}

/// Classifies a VOD end-of-file from the last position and the duration: finished when
/// at least 95 % played or less than 30 s left (or the duration is unknown).
pub fn vod_end(last_pos: Option<f64>, duration: Option<f64>) -> VodEnd {
    let Some(pos) = last_pos.filter(|p| p.is_finite()) else {
        return VodEnd::Failed;
    };
    match duration.filter(|d| d.is_finite() && *d > 0.0) {
        Some(d) if pos < d * FINISHED_FRACTION && d - pos > FINISHED_REMAINING => {
            VodEnd::Dropped { at: pos }
        }
        _ => VodEnd::Finished,
    }
}

/// Which mpv events belong to the current load. See the [module docs](self).
///
/// Every load the controller issues produces exactly one `StartFile`; until all of them
/// have arrived, other events (`FileLoaded`, `PlaybackRestart`, `EndFile`) are the
/// previous stream's and are ignored. A stream the controller stops on purpose (before a
/// reconnect) is *abandoned*: its late events are ignored until the next load.
#[derive(Debug, Clone, Default)]
pub struct LoadGuard {
    starts_expected: u32,
    abandoned: bool,
    generation: u64,
}

impl LoadGuard {
    /// A load was issued to mpv.
    pub fn issued(&mut self) {
        self.starts_expected += 1;
        self.abandoned = false;
        self.generation += 1;
    }

    /// A load that was issued will never start (mpv refused it, or a held load was
    /// dropped).
    pub fn withdrawn(&mut self) {
        self.starts_expected = self.starts_expected.saturating_sub(1);
    }

    /// The current stream was stopped on purpose: ignore its remaining events until the
    /// next load.
    pub fn abandon(&mut self) {
        self.abandoned = true;
    }

    /// mpv lost events (queue overflow): trust what comes next.
    pub fn resync(&mut self) {
        self.starts_expected = 0;
    }

    /// Counts a `StartFile`.
    pub fn started(&mut self) {
        self.starts_expected = self.starts_expected.saturating_sub(1);
    }

    /// True if events now belong to the current load.
    pub fn is_current(&self) -> bool {
        !self.abandoned && self.starts_expected == 0
    }

    /// Increases with every load (diagnostics).
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    /// Simulates `seconds` of playback at `fps`, position advancing in real time.
    fn play(t: &mut ProgressTracker, start: Instant, from: f64, seconds: f64, fps: f64) -> Instant {
        let frames = (seconds * fps) as u64;
        let mut now = start;
        for i in 0..=frames {
            let dt = i as f64 / fps;
            now = start + secs(dt);
            t.observe(Some(from + dt), false, now);
        }
        now
    }

    #[test]
    fn healthy_stream_never_stalls_at_any_frame_rate() {
        for fps in [25.0, 60.0, 120.0, 240.0] {
            let t0 = Instant::now();
            let mut t = ProgressTracker::new(t0);
            let mut now = t0;
            // Sample "is it stalled?" every frame for 5 minutes.
            let frames = (300.0 * fps) as u64;
            for i in 0..=frames {
                let dt = i as f64 / fps;
                now = t0 + secs(dt);
                t.observe(Some(1000.0 + dt), false, now);
                assert!(!t.is_stalled(now), "stalled at {dt:.2}s ({fps} fps)");
            }
            assert!(t.has_progressed());
            assert!(t.progressing_for(now) >= secs(299.0));
        }
    }

    #[test]
    fn a_frozen_position_is_a_stall() {
        let t0 = Instant::now();
        let mut t = ProgressTracker::new(t0);
        let now = play(&mut t, t0, 0.0, 5.0, 60.0);
        // Frozen: the same position (with tiny jitter) for 10.5 s.
        let mut later = now;
        for i in 0..630 {
            later = now + secs(f64::from(i) / 60.0);
            t.observe(
                Some(5.0 + if i % 2 == 0 { 0.01 } else { 0.0 }),
                false,
                later,
            );
        }
        assert!(t.idle_for(later) > secs(10.0));
        assert!(t.is_stalled(later));
        assert_eq!(t.progressing_for(later), Duration::ZERO);
    }

    #[test]
    fn opening_gets_longer_than_a_stall() {
        let t0 = Instant::now();
        let mut t = ProgressTracker::new(t0);
        t.observe(None, false, t0 + secs(5.0));
        t.observe(Some(0.0), false, t0 + secs(12.0));
        assert!(
            !t.is_stalled(t0 + secs(12.0)),
            "no progress yet: open timeout"
        );
        assert!(t.is_stalled(t0 + secs(21.0)));
    }

    #[test]
    fn pausing_is_not_a_stall() {
        let t0 = Instant::now();
        let mut t = ProgressTracker::new(t0);
        let now = play(&mut t, t0, 0.0, 3.0, 30.0);
        for s in 0..60 {
            t.observe(Some(3.0), true, now + secs(f64::from(s)));
        }
        assert!(!t.is_stalled(now + secs(59.0)));
        assert_eq!(t.progressing_for(now + secs(59.0)), Duration::ZERO);
    }

    #[test]
    fn backoff_schedule_then_give_up() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(LIVE_MAX_RETRIES);
        let mut now = t0;
        let mut delays = Vec::new();
        loop {
            match r.schedule(Cause::Failed, Duration::ZERO, now) {
                Schedule::Retry { attempt, at } => {
                    assert_eq!(attempt, r.attempts());
                    delays.push((at - now).as_secs());
                    assert!(!r.take_due(at - secs(0.1)));
                    assert!(r.take_due(at));
                    assert!(!r.take_due(at), "fires once");
                    now = at;
                }
                Schedule::GiveUp => break,
                Schedule::AlreadyPending => panic!("nothing was pending"),
            }
        }
        assert_eq!(delays, vec![2, 5, 10, 30, 30]);
        let mut v = Reconnector::new(VOD_MAX_RETRIES);
        for _ in 0..3 {
            assert!(matches!(
                v.schedule(Cause::Ended, Duration::ZERO, now),
                Schedule::Retry { .. }
            ));
            assert!(v.take_due(now + secs(60.0)));
        }
        assert_eq!(
            v.schedule(Cause::Ended, Duration::ZERO, now),
            Schedule::GiveUp
        );
    }

    #[test]
    fn one_schedule_per_drop() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(LIVE_MAX_RETRIES);
        // The stall detector schedules, then the old stream's end-of-file arrives.
        assert!(matches!(
            r.schedule(Cause::Stalled, Duration::ZERO, t0),
            Schedule::Retry { attempt: 1, .. }
        ));
        assert_eq!(
            r.schedule(Cause::Failed, Duration::ZERO, t0 + secs(0.5)),
            Schedule::AlreadyPending
        );
        assert_eq!(r.attempts(), 1, "the counter moved by one, not two");
        assert_eq!(r.cause(), Some(Cause::Stalled));
    }

    #[test]
    fn counter_resets_only_after_sustained_progress() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(LIVE_MAX_RETRIES);
        let mut now = t0;
        // A flapping stream: plays 4 s, fails, again and again — it backs off.
        let mut delays = Vec::new();
        for _ in 0..4 {
            if let Schedule::Retry { at, .. } = r.schedule(Cause::Failed, secs(4.0), now) {
                delays.push((at - now).as_secs());
                assert!(r.take_due(at));
                r.note_progress(secs(4.0));
                now = at + secs(4.0);
            }
        }
        assert_eq!(delays, vec![2, 5, 10, 30]);
        // 29 s of progress is not enough...
        r.note_progress(secs(29.0));
        assert_eq!(r.attempts(), 4);
        // ...30 s is.
        r.note_progress(secs(30.0));
        assert_eq!(r.attempts(), 0);
        assert!(matches!(
            r.schedule(Cause::Failed, Duration::ZERO, now),
            Schedule::Retry { attempt: 1, .. }
        ));
        // While a reconnect is pending, progress readings (of the old stream) don't reset.
        r.note_progress(secs(40.0));
        assert_eq!(r.attempts(), 1);
    }

    #[test]
    fn a_live_stream_that_ends_cleanly_does_not_escalate() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(LIVE_MAX_RETRIES);
        let mut now = t0;
        // A 20 s clip that ends, forever: always the first delay, never gives up.
        for _ in 0..20 {
            match r.schedule(Cause::Ended, secs(20.0), now) {
                Schedule::Retry { attempt, at } => {
                    assert_eq!(attempt, 1);
                    assert_eq!(at - now, secs(2.0));
                    assert!(r.take_due(at));
                    now = at + secs(20.0);
                }
                other => panic!("{other:?}"),
            }
        }
        // But a stream that ends right after opening does back off.
        let mut q = Reconnector::new(LIVE_MAX_RETRIES);
        let mut attempts = Vec::new();
        for _ in 0..3 {
            if let Schedule::Retry { attempt, at } = q.schedule(Cause::Ended, secs(1.0), now) {
                attempts.push(attempt);
                assert!(q.take_due(at));
            }
        }
        assert_eq!(attempts, vec![1, 2, 3]);
    }

    #[test]
    fn vod_end_of_file_vs_early_drop() {
        assert_eq!(vod_end(Some(5_400.0), Some(5_420.0)), VodEnd::Finished);
        assert_eq!(
            vod_end(Some(5_330.0), Some(5_600.0)),
            VodEnd::Finished,
            "95 %"
        );
        assert_eq!(
            vod_end(Some(580.0), Some(600.0)),
            VodEnd::Finished,
            "< 30 s left"
        );
        assert_eq!(
            vod_end(Some(1_800.0), Some(5_400.0)),
            VodEnd::Dropped { at: 1_800.0 }
        );
        assert_eq!(
            vod_end(Some(100.0), None),
            VodEnd::Finished,
            "unknown duration"
        );
        assert_eq!(vod_end(None, Some(5_400.0)), VodEnd::Failed);
        assert_eq!(vod_end(Some(f64::NAN), Some(5_400.0)), VodEnd::Failed);
    }

    #[test]
    fn load_guard_ignores_the_previous_streams_events() {
        let mut g = LoadGuard::default();
        assert!(g.is_current());
        // Load A, then quickly B: A's FileLoaded/EndFile arrive before B's StartFile.
        g.issued();
        g.issued();
        assert!(!g.is_current());
        g.started(); // A
        assert!(!g.is_current(), "A's FileLoaded must be ignored");
        g.started(); // B
        assert!(g.is_current());
        // mpv refused a load: it will never start.
        g.issued();
        g.withdrawn();
        assert!(g.is_current());
        // Lost events: resync.
        g.issued();
        g.resync();
        assert!(g.is_current());
        // A stream stopped before a reconnect: its late EndFile is ignored until the
        // reconnect's load starts.
        g.abandon();
        assert!(!g.is_current());
        g.issued();
        assert!(!g.is_current());
        g.started();
        assert!(g.is_current());
        assert!(g.generation() >= 5);
    }

    #[test]
    fn vod_stall_uses_longer_thresholds() {
        let t0 = Instant::now();
        let mut t = ProgressTracker::new(t0);
        t.observe(Some(100.0), false, t0);
        t.observe(Some(101.0), false, t0 + Duration::from_secs(1));
        // A live stream would be stalled after 10 s; a film keeps buffering until 30 s.
        let later = t0 + Duration::from_secs(1) + Duration::from_secs(20);
        assert!(t.is_stalled_for(true, later));
        assert!(!t.is_stalled_for(false, later));
        assert!(t.is_stalled_for(false, later + Duration::from_secs(11)));
    }

    #[test]
    fn vod_that_never_opens_times_out() {
        let t0 = Instant::now();
        let t = ProgressTracker::new(t0);
        assert!(!t.is_stalled_for(false, t0 + Duration::from_secs(40)));
        assert!(t.is_stalled_for(false, t0 + Duration::from_secs(46)));
    }

    #[test]
    fn user_pause_is_never_a_vod_stall() {
        let t0 = Instant::now();
        let mut t = ProgressTracker::new(t0);
        t.observe(Some(10.0), false, t0);
        t.observe(Some(11.0), false, t0 + Duration::from_secs(1));
        let mut now = t0 + Duration::from_secs(1);
        for _ in 0..120 {
            now += Duration::from_secs(1);
            t.observe(Some(11.0), true, now); // paused by the user
        }
        assert!(!t.is_stalled_for(false, now));
    }
}

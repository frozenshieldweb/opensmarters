//! Timestamps and an injectable clock.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

/// A point in time: milliseconds since the Unix epoch, UTC.
///
/// Stored as an `INTEGER` in SQLite and serialised as a plain number. Convert to local
/// time in the UI layer (e.g. with `chrono`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// The current system time.
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// Builds a timestamp from milliseconds since the Unix epoch.
    pub const fn from_unix_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// Builds a timestamp from whole seconds since the Unix epoch (saturating).
    pub const fn from_unix_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(1000))
    }

    /// Milliseconds since the Unix epoch.
    pub const fn as_unix_millis(self) -> i64 {
        self.0
    }

    /// Whole seconds since the Unix epoch (rounded towards negative infinity).
    pub const fn as_unix_secs(self) -> i64 {
        self.0.div_euclid(1000)
    }

    /// Converts a [`SystemTime`]; times outside the `i64` millisecond range saturate.
    pub fn from_system_time(t: SystemTime) -> Self {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Self(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
            Err(e) => Self(
                i64::try_from(e.duration().as_millis())
                    .map(|ms| -ms)
                    .unwrap_or(i64::MIN),
            ),
        }
    }

    /// Converts to a [`SystemTime`].
    pub fn to_system_time(self) -> SystemTime {
        let magnitude = Duration::from_millis(self.0.unsigned_abs());
        if self.0 >= 0 {
            UNIX_EPOCH.checked_add(magnitude).unwrap_or_else(|| {
                // Year 9999: representable on every platform.
                UNIX_EPOCH
                    .checked_add(Duration::from_secs(253_402_300_799))
                    .unwrap_or(UNIX_EPOCH)
            })
        } else {
            UNIX_EPOCH.checked_sub(magnitude).unwrap_or(UNIX_EPOCH)
        }
    }

    /// `self + d`, saturating at the representable range.
    pub fn saturating_add(self, d: Duration) -> Self {
        Self(self.0.saturating_add(duration_to_millis(d)))
    }

    /// `self - d`, saturating at the representable range.
    pub fn saturating_sub(self, d: Duration) -> Self {
        Self(self.0.saturating_sub(duration_to_millis(d)))
    }

    /// The duration from `earlier` to `self`, or zero if `earlier` is later.
    pub fn duration_since(self, earlier: Self) -> Duration {
        let ms = self.0.saturating_sub(earlier.0);
        Duration::from_millis(u64::try_from(ms).unwrap_or(0))
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({}ms)", self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0))
    }
}

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        i64::column_result(value).map(Self)
    }
}

/// Converts a duration to whole milliseconds, saturating at `i64::MAX`.
pub(crate) fn duration_to_millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// A source of the current time.
///
/// The store reads the time only through this trait, so tests (including the app's) can
/// drive time-dependent behaviour — PIN lockouts, cache TTLs, recency ordering — with a
/// [`ManualClock`].
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current time.
    fn now(&self) -> Timestamp;
}

/// The real system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that only moves when told to. Intended for tests.
#[derive(Debug)]
pub struct ManualClock {
    ms: AtomicI64,
}

impl ManualClock {
    /// A clock stopped at `start`.
    pub fn new(start: Timestamp) -> Self {
        Self {
            ms: AtomicI64::new(start.as_unix_millis()),
        }
    }

    /// Sets the current time (may go backwards).
    pub fn set(&self, t: Timestamp) {
        self.ms.store(t.as_unix_millis(), Ordering::SeqCst);
    }

    /// Moves the clock forward by `d`.
    pub fn advance(&self, d: Duration) {
        let delta = duration_to_millis(d);
        let _ = self
            .ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |ms| {
                Some(ms.saturating_add(delta))
            });
    }

    /// Moves the clock backward by `d` (to simulate a user changing the system time).
    pub fn rewind(&self, d: Duration) {
        let delta = duration_to_millis(d);
        let _ = self
            .ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |ms| {
                Some(ms.saturating_sub(delta))
            });
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.ms.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_time_round_trip() {
        let t = Timestamp::from_unix_millis(1_700_000_123_456);
        assert_eq!(Timestamp::from_system_time(t.to_system_time()), t);
        let neg = Timestamp::from_unix_millis(-5_000);
        assert_eq!(Timestamp::from_system_time(neg.to_system_time()), neg);
        assert_eq!(neg.as_unix_secs(), -5);
        assert_eq!(Timestamp::from_unix_millis(-1).as_unix_secs(), -1);
        // Out-of-range values saturate instead of panicking.
        let _ = Timestamp::from_unix_millis(i64::MAX).to_system_time();
        let _ = Timestamp::from_unix_millis(i64::MIN).to_system_time();
    }

    #[test]
    fn saturating_arithmetic() {
        let t = Timestamp::from_unix_millis(i64::MAX - 1);
        assert_eq!(
            t.saturating_add(Duration::from_secs(10)).as_unix_millis(),
            i64::MAX
        );
        let a = Timestamp::from_unix_secs(10);
        let b = Timestamp::from_unix_secs(4);
        assert_eq!(a.duration_since(b), Duration::from_secs(6));
        assert_eq!(b.duration_since(a), Duration::ZERO);
    }

    #[test]
    fn manual_clock_moves_both_ways() {
        let c = ManualClock::new(Timestamp::from_unix_secs(100));
        c.advance(Duration::from_secs(5));
        assert_eq!(c.now(), Timestamp::from_unix_secs(105));
        c.rewind(Duration::from_secs(10));
        assert_eq!(c.now(), Timestamp::from_unix_secs(95));
    }
}

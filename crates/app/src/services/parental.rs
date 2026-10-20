// SPDX-License-Identifier: GPL-3.0-or-later
//! App-side parental control helpers (the PIN itself, its Argon2id hash, rate limiting,
//! keywords and category locks are in `opensmarters_store::Parental`):
//!
//! - [`PIN_LENGTH`]: how many digits the PIN has (4–8), so PIN pads show the right number
//!   of dots and submit on the last digit. Not secret in any useful sense (the pad shows
//!   it anyway); stripped from exports and kept local on import ([`super::backup`]).
//! - Plain-language messages for wrong PINs and lockouts ([`wrong_pin_text`],
//!   [`lockout_text`]).
//! - "Forgot PIN": [`verify_account_secret`] checks that the person knows the password of
//!   one of this computer's Xtream accounts (or the address of an M3U playlist), compared
//!   in constant time against the keychain, rate-limited by [`ResetLimiter`]. Only then
//!   may the app call `force_remove_pin`. [`forgot_pin_attempt`] does all of it in one
//!   blocking call with the limiter **persisted in the store** ([`RESET_LIMITER`], SR-14),
//!   so restarting the app gives no new tries. This stops a child at the keyboard; like
//!   any local control, it does not stop someone who can delete the app's data.

use std::time::{Duration, Instant};

use opensmarters_core::secrecy::{ExposeSecret, SecretString};
use opensmarters_store::settings::Setting;
use opensmarters_store::{ProfileSource, Store, StoreError, Timestamp};
use serde::{Deserialize, Serialize};

/// Number of digits of the current PIN (4–8). Default 4 (older databases, the demo PIN).
pub const PIN_LENGTH: Setting<u8> = Setting::new("app.parental.pin_length", || 4);

/// The persisted "forgot PIN" limiter (machine-local: never exported, ignored on import).
pub const RESET_LIMITER: Setting<ResetLimiterState> =
    Setting::new("app.parental.reset_limiter", ResetLimiterState::default);

/// What [`RESET_LIMITER`] stores.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetLimiterState {
    /// Failed tries since the last success.
    pub failures: u32,
    /// No try before this wall-clock time (Unix ms); 0 = no wait.
    pub locked_until_ms: i64,
}

/// Shortest PIN.
pub const MIN_PIN: usize = 4;
/// Longest PIN.
pub const MAX_PIN: usize = 8;

/// The stored PIN length, clamped to 4–8.
pub fn pin_length(store: &Store) -> usize {
    store
        .settings()
        .get(&PIN_LENGTH)
        .map_or(MIN_PIN, |n| usize::from(n).clamp(MIN_PIN, MAX_PIN))
}

/// Records the PIN length after a PIN was set or changed (`None` after removal).
///
/// # Errors
///
/// Database errors.
pub fn set_pin_length(store: &Store, len: Option<usize>) -> Result<(), StoreError> {
    match len {
        Some(n) => {
            let n = u8::try_from(n.clamp(MIN_PIN, MAX_PIN)).unwrap_or(4);
            store.settings().set(&PIN_LENGTH, &n)
        }
        None => store.settings().reset(&PIN_LENGTH),
    }
}

/// True if `pin` is 4–8 ASCII digits (the store's rule).
pub fn is_valid_pin(pin: &SecretString) -> bool {
    let p = pin.expose_secret();
    (MIN_PIN..=MAX_PIN).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())
}

/// "Wrong PIN. 2 attempts left" (before the next lockout).
pub fn wrong_pin_text(remaining_before_lockout: u32) -> String {
    match remaining_before_lockout {
        0 => "Wrong PIN.".to_owned(),
        1 => "Wrong PIN. 1 attempt left".to_owned(),
        n => format!("Wrong PIN. {n} attempts left"),
    }
}

/// "Try again in 0:27" (minutes:seconds; hours when ≥ 1 h). Rounds up so it never shows
/// 0:00 while still locked.
pub fn lockout_text(remaining: Duration) -> String {
    let s = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
    if s >= 3600 {
        format!(
            "Try again in {}:{:02}:{:02}",
            s / 3600,
            (s / 60) % 60,
            s % 60
        )
    } else {
        format!("Try again in {}:{:02}", s / 60, s % 60)
    }
}

/// Time left until `until` (zero if past).
pub fn remaining_until(until: Timestamp, now: Timestamp) -> Duration {
    if until > now {
        until.duration_since(now)
    } else {
        Duration::ZERO
    }
}

/// Constant-time equality for secrets (the length is not hidden).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Result of a "forgot PIN" check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetProof {
    /// The text matches an account password or playlist address on this computer.
    Matched,
    /// It doesn't.
    NoMatch,
    /// No profile has a password or playlist address to check (M3U files only).
    NothingToCheck,
}

/// Checks `attempt` against every Xtream password and M3U playlist address stored in the
/// keychain (constant-time per comparison). Blocking (keychain): call off the UI thread.
///
/// # Errors
///
/// Database or keychain errors.
pub fn verify_account_secret(
    store: &Store,
    attempt: &SecretString,
) -> Result<ResetProof, StoreError> {
    let attempt = attempt.expose_secret();
    let mut checked = false;
    let mut matched = false;
    for p in store.profiles().list()? {
        let secret = match &p.source {
            ProfileSource::Xtream { .. } => store.profiles().password(p.id)?,
            ProfileSource::M3uUrl => store.profiles().m3u_url(p.id)?,
            ProfileSource::M3uFile { .. } => None,
        };
        if let Some(secret) = secret {
            checked = true;
            // Passwords exactly; playlist addresses ignoring surrounding spaces.
            let (a, b) = match p.source {
                ProfileSource::M3uUrl => (attempt.trim(), secret.expose_secret().trim()),
                _ => (attempt, secret.expose_secret()),
            };
            // No early exit: every stored secret is compared.
            matched |= !a.is_empty() && ct_eq(a.as_bytes(), b.as_bytes());
        }
    }
    Ok(match (checked, matched) {
        (_, true) => ResetProof::Matched,
        (true, false) => ResetProof::NoMatch,
        (false, false) => ResetProof::NothingToCheck,
    })
}

/// Rate limit for "forgot PIN" attempts: [`ResetLimiter::MAX_TRIES`] tries, then a wait
/// that doubles each time (60 s, 120 s, … up to 1 h). The screen keeps one in memory for
/// the countdown; [`forgot_pin_attempt`] loads and saves the authoritative copy from the
/// store ([`ResetLimiter::load`] / [`ResetLimiter::save`]).
#[derive(Debug, Default)]
pub struct ResetLimiter {
    failures: u32,
    locked_until: Option<Instant>,
}

impl ResetLimiter {
    /// Free tries before the first wait.
    pub const MAX_TRIES: u32 = 3;
    /// Longest wait.
    pub const MAX_WAIT: Duration = Duration::from_secs(3600);

    /// The limiter as persisted, converted to this process's clock. A wait never comes
    /// back longer than [`ResetLimiter::MAX_WAIT`] (moving the clock backwards cannot
    /// lengthen it).
    pub fn from_state(state: ResetLimiterState, now_wall: Timestamp, now: Instant) -> Self {
        let until = Timestamp::from_unix_millis(state.locked_until_ms);
        let left = remaining_until(until, now_wall).min(Self::MAX_WAIT);
        Self {
            failures: state.failures,
            locked_until: (!left.is_zero()).then(|| now + left),
        }
    }

    /// What to persist.
    pub fn to_state(&self, now_wall: Timestamp, now: Instant) -> ResetLimiterState {
        let left = self
            .locked_until
            .and_then(|t| t.checked_duration_since(now))
            .unwrap_or_default();
        ResetLimiterState {
            failures: self.failures,
            locked_until_ms: if left.is_zero() {
                0
            } else {
                now_wall.saturating_add(left).as_unix_millis()
            },
        }
    }

    /// Reads the persisted limiter.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn load(store: &Store) -> Result<Self, StoreError> {
        let state = store.settings().get(&RESET_LIMITER)?;
        Ok(Self::from_state(state, store.clock().now(), Instant::now()))
    }

    /// Persists the limiter.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn save(&self, store: &Store) -> Result<(), StoreError> {
        store.settings().set(
            &RESET_LIMITER,
            &self.to_state(store.clock().now(), Instant::now()),
        )
    }

    /// Mirrors a wait reported by [`forgot_pin_attempt`] (for the countdown).
    pub fn wait_for(&mut self, wait: Duration, now: Instant) {
        self.failures = self.failures.max(Self::MAX_TRIES);
        self.locked_until = Some(now + wait.min(Self::MAX_WAIT));
    }

    /// Time left before the next try is allowed.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.locked_until
            .and_then(|t| t.checked_duration_since(now))
            .filter(|d| !d.is_zero())
    }

    /// Records a failed try (starting a wait after [`Self::MAX_TRIES`]).
    pub fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= Self::MAX_TRIES {
            let doublings = (self.failures - Self::MAX_TRIES).min(6);
            let secs = (60u64 << doublings).min(3600);
            self.locked_until = Some(now + Duration::from_secs(secs));
        }
    }

    /// Clears the counter after a success.
    pub fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// Result of [`forgot_pin_attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgotPinResult {
    /// The text was checked ([`ResetProof::Matched`]: the PIN is removed).
    Checked(ResetProof),
    /// Too many tries: nothing was checked; try again after this long.
    Wait(Duration),
}

/// "Forgot PIN" in one blocking call (off the UI thread): refuses while the persisted
/// limiter says wait; otherwise checks `attempt` ([`verify_account_secret`]), removes the
/// PIN when it matches, and records the failure or success in the store — so the limit
/// survives restarts (SR-14).
///
/// # Errors
///
/// Database or keychain errors.
pub fn forgot_pin_attempt(
    store: &Store,
    attempt: &SecretString,
) -> Result<ForgotPinResult, StoreError> {
    let now = Instant::now();
    let mut limiter = ResetLimiter::load(store)?;
    if let Some(wait) = limiter.wait(now) {
        return Ok(ForgotPinResult::Wait(wait));
    }
    let proof = verify_account_secret(store, attempt)?;
    match proof {
        ResetProof::Matched => {
            limiter.succeeded();
            store.parental().force_remove_pin()?;
            set_pin_length(store, None)?;
        }
        ResetProof::NoMatch => limiter.failed(now),
        ResetProof::NothingToCheck => {}
    }
    limiter.save(store)?;
    Ok(ForgotPinResult::Checked(proof))
}

/// The persisted "forgot PIN" wait, if any (to show the countdown when the dialog opens).
///
/// # Errors
///
/// Database errors.
pub fn forgot_pin_wait(store: &Store) -> Result<Option<Duration>, StoreError> {
    Ok(ResetLimiter::load(store)?.wait(Instant::now()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use opensmarters_store::{
        MemorySecretStore, NewProfile, NewProfileSource, ProfileSettings, StoreOptions,
    };

    use super::*;

    fn store() -> Store {
        Store::open(
            StoreOptions::in_memory().with_pin_hash(opensmarters_store::PinHashParams {
                m_cost_kib: 64,
                t_cost: 1,
                p_cost: 1,
            }),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap()
    }

    #[test]
    fn messages() {
        assert_eq!(wrong_pin_text(2), "Wrong PIN. 2 attempts left");
        assert_eq!(wrong_pin_text(1), "Wrong PIN. 1 attempt left");
        assert_eq!(lockout_text(Duration::from_secs(27)), "Try again in 0:27");
        assert_eq!(
            lockout_text(Duration::from_millis(26_200)),
            "Try again in 0:27"
        );
        assert_eq!(lockout_text(Duration::from_secs(90)), "Try again in 1:30");
        assert_eq!(
            lockout_text(Duration::from_secs(3600)),
            "Try again in 1:00:00"
        );
        assert_eq!(lockout_text(Duration::ZERO), "Try again in 0:00");
    }

    #[test]
    fn pin_rules_and_length_setting() {
        assert!(is_valid_pin(&SecretString::from("1234")));
        assert!(is_valid_pin(&SecretString::from("12345678")));
        assert!(!is_valid_pin(&SecretString::from("123")));
        assert!(!is_valid_pin(&SecretString::from("12a4")));
        let s = store();
        assert_eq!(pin_length(&s), 4);
        set_pin_length(&s, Some(6)).unwrap();
        assert_eq!(pin_length(&s), 6);
        set_pin_length(&s, Some(42)).unwrap();
        assert_eq!(pin_length(&s), 8);
        set_pin_length(&s, None).unwrap();
        assert_eq!(pin_length(&s), 4);
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn forgot_pin_checks_account_secrets() {
        let s = store();
        assert_eq!(
            verify_account_secret(&s, &SecretString::from("x")).unwrap(),
            ResetProof::NothingToCheck
        );
        s.profiles()
            .create(NewProfile {
                name: "Home".into(),
                source: NewProfileSource::Xtream {
                    server_url: "http://panel.example.invalid".into(),
                    username: "demo".into(),
                    password: SecretString::from("synthetic-pass-1"),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap();
        s.profiles()
            .create(NewProfile {
                name: "List".into(),
                source: NewProfileSource::M3uUrl {
                    url: SecretString::from("http://lists.example.invalid/get.m3u"),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap();
        let check = |t: &str| verify_account_secret(&s, &SecretString::from(t)).unwrap();
        assert_eq!(check("synthetic-pass-1"), ResetProof::Matched);
        assert_eq!(check("synthetic-pass-"), ResetProof::NoMatch);
        assert_eq!(
            check(" synthetic-pass-1"),
            ResetProof::NoMatch,
            "passwords exactly"
        );
        assert_eq!(
            check("  http://lists.example.invalid/get.m3u "),
            ResetProof::Matched
        );
        assert_eq!(check(""), ResetProof::NoMatch);
    }

    #[test]
    fn reset_limiter_waits_after_three_failures() {
        let mut l = ResetLimiter::default();
        let t0 = Instant::now();
        l.failed(t0);
        l.failed(t0);
        assert!(l.wait(t0).is_none());
        l.failed(t0);
        assert_eq!(l.wait(t0), Some(Duration::from_secs(60)));
        assert!(l.wait(t0 + Duration::from_secs(61)).is_none());
        l.failed(t0);
        assert_eq!(l.wait(t0), Some(Duration::from_secs(120)));
        l.succeeded();
        assert!(l.wait(t0).is_none());
    }

    #[test]
    fn forgot_pin_limit_survives_a_restart() {
        let clock = Arc::new(opensmarters_store::ManualClock::new(
            Timestamp::from_unix_millis(1_000_000),
        ));
        let s = Store::open(
            StoreOptions::in_memory()
                .with_clock(clock.clone())
                .with_pin_hash(opensmarters_store::PinHashParams {
                    m_cost_kib: 64,
                    t_cost: 1,
                    p_cost: 1,
                }),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        s.profiles()
            .create(NewProfile {
                name: "Home".into(),
                source: NewProfileSource::Xtream {
                    server_url: "http://panel.example.invalid".into(),
                    username: "demo".into(),
                    password: SecretString::from("synthetic-pass-2"),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap();
        s.parental().set_pin(&SecretString::from("1234")).unwrap();
        let wrong = SecretString::from("nope");
        for _ in 0..ResetLimiter::MAX_TRIES {
            assert_eq!(
                forgot_pin_attempt(&s, &wrong).unwrap(),
                ForgotPinResult::Checked(ResetProof::NoMatch)
            );
        }
        // "Restart": nothing in memory, the wait comes from the store.
        let wait = forgot_pin_wait(&s).unwrap().unwrap();
        assert!(wait > Duration::from_secs(55) && wait <= Duration::from_secs(60));
        let right = SecretString::from("synthetic-pass-2");
        assert!(matches!(
            forgot_pin_attempt(&s, &right).unwrap(),
            ForgotPinResult::Wait(_)
        ));
        assert!(s.parental().is_pin_set().unwrap(), "refused while waiting");
        clock.advance(Duration::from_secs(61));
        assert_eq!(
            forgot_pin_attempt(&s, &right).unwrap(),
            ForgotPinResult::Checked(ResetProof::Matched)
        );
        assert!(!s.parental().is_pin_set().unwrap());
        assert_eq!(forgot_pin_wait(&s).unwrap(), None);
        assert_eq!(
            s.settings().get(&RESET_LIMITER).unwrap(),
            ResetLimiterState::default()
        );
    }

    #[test]
    fn persisted_wait_is_capped_when_the_clock_goes_back() {
        let now = Instant::now();
        let state = ResetLimiterState {
            failures: 9,
            locked_until_ms: 10_000_000_000,
        };
        let l = ResetLimiter::from_state(state, Timestamp::from_unix_millis(0), now);
        assert_eq!(l.wait(now), Some(ResetLimiter::MAX_WAIT));
        let back = l.to_state(Timestamp::from_unix_millis(5_000), now);
        assert_eq!(back.failures, 9);
        assert_eq!(back.locked_until_ms, 5_000 + 3_600_000);
    }

    #[test]
    fn remaining_time() {
        let now = Timestamp::from_unix_millis(10_000);
        assert_eq!(
            remaining_until(Timestamp::from_unix_millis(37_000), now),
            Duration::from_secs(27)
        );
        assert_eq!(
            remaining_until(Timestamp::from_unix_millis(5_000), now),
            Duration::ZERO
        );
    }
}

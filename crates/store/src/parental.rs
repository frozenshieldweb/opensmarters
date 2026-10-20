//! Parental control: Argon2id-hashed PIN with persisted rate limiting, the category
//! keyword list, and manual per-profile category locks.
//!
//! # PIN
//!
//! - 4–8 ASCII digits, passed as [`SecretString`]; never stored, logged or `Debug`-printed.
//! - Stored as an Argon2id **PHC string** (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`) with a
//!   random 128-bit salt. Default parameters ([`PinHashParams::default`]): m = 64 MiB,
//!   t = 3, p = 4 — the second recommended option of RFC 9106 §4. Verification uses the
//!   parameters recorded in the PHC string and compares in constant time; after a
//!   successful check a hash made with different parameters is transparently upgraded.
//! - Implausible stored parameters (m > 1 GiB, t > 64, p > 64) are rejected instead of
//!   evaluated, so a tampered database cannot make the app allocate unbounded memory.
//!
//! # Rate limiting ([`LockoutPolicy`], persisted across restarts)
//!
//! The first `max_attempts − 1` (default 4) consecutive failures return
//! [`PinOutcome::Wrong`] with the number of attempts left. The failure that reaches
//! `max_attempts` (5) locks verification for `base_lockout` (30 s); while locked,
//! [`Parental::verify_pin`] returns [`PinOutcome::LockedOut`] without evaluating the PIN.
//! Every further failure doubles the lockout (60 s, 120 s, …) up to `max_lockout` (1 h). A
//! correct PIN resets everything. If the system clock is moved backwards during a
//! lockout, the remaining time is capped at the lockout's own length rather than growing.
//!
//! This protects against a child guessing at the keyboard. It does not stop someone who
//! can edit files in the user's profile — nothing local can.
//!
//! # Keywords and locks
//!
//! The keyword list (seeded with `adult`, `xxx`, `porn`, `18+`, `erotic`) is for the app's
//! whole-word, case-insensitive category matcher. Manual per-profile [`CategoryLock`]s
//! override it either way.

use std::collections::HashMap;
use std::time::Duration;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use rusqlite::{OptionalExtension, params};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, StoreError, map_fk};
use crate::ids::{ContentKind, ProfileId};
use crate::text::{has_control, validate_item_id};
use crate::time::{Timestamp, duration_to_millis};

/// Keywords seeded on a new database and restored by [`Parental::reset_keywords`].
pub const DEFAULT_KEYWORDS: &[&str] = &["adult", "xxx", "porn", "18+", "erotic"];

pub(crate) const MAX_KEYWORDS: usize = 200;
const MAX_KEYWORD_CHARS: usize = 64;
/// Upper bounds accepted when *verifying* a stored hash.
const MAX_STORED_M_COST_KIB: u32 = 1024 * 1024;
const MAX_STORED_T_COST: u32 = 64;
const MAX_STORED_P_COST: u32 = 64;

/// Argon2id cost parameters for new PIN hashes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PinHashParams {
    /// Memory in KiB. Default: 65 536 (64 MiB).
    pub m_cost_kib: u32,
    /// Iterations. Default: 3.
    pub t_cost: u32,
    /// Lanes. Default: 4.
    pub p_cost: u32,
}

impl Default for PinHashParams {
    /// RFC 9106 §4, second recommended option: Argon2id, m = 2^16 KiB, t = 3, p = 4.
    fn default() -> Self {
        Self {
            m_cost_kib: 64 * 1024,
            t_cost: 3,
            p_cost: 4,
        }
    }
}

impl PinHashParams {
    fn argon2(&self) -> Result<Argon2<'static>> {
        let params = Params::new(self.m_cost_kib, self.t_cost, self.p_cost, None)
            .map_err(|_| StoreError::PinHash("invalid Argon2 parameters"))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

/// Failed-attempt policy for the PIN.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LockoutPolicy {
    /// Consecutive failures that trigger the first lockout. Default: 5.
    pub max_attempts: u32,
    /// First lockout length; doubles with each further failure. Default: 30 s.
    pub base_lockout: Duration,
    /// Lockout cap. Default: 1 h.
    pub max_lockout: Duration,
}

impl Default for LockoutPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_lockout: Duration::from_secs(30),
            max_lockout: Duration::from_secs(3600),
        }
    }
}

impl LockoutPolicy {
    /// Lockout length after `failures` consecutive failures (`None` below the threshold).
    pub fn lockout_for(&self, failures: u32) -> Option<Duration> {
        let max_attempts = self.max_attempts.max(1);
        if failures < max_attempts {
            return None;
        }
        let doublings = (failures - max_attempts).min(32);
        let factor = 1u32 << doublings.min(31);
        Some(
            self.base_lockout
                .checked_mul(factor)
                .unwrap_or(self.max_lockout)
                .min(self.max_lockout),
        )
    }
}

/// Result of a PIN check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinOutcome {
    /// The PIN is correct.
    Ok,
    /// The PIN is wrong; this many more failures are allowed before a lockout.
    Wrong {
        /// Failures left before the next one locks.
        remaining_before_lockout: u32,
    },
    /// Verification is locked until `until` (either already locked, or this failure
    /// triggered the lockout). The PIN was not evaluated if it was already locked.
    LockedOut {
        /// When the next attempt will be accepted.
        until: Timestamp,
    },
}

impl PinOutcome {
    /// True for [`PinOutcome::Ok`].
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Snapshot of the PIN state for the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PinStatus {
    /// Whether a PIN is set (parental control enabled).
    pub is_set: bool,
    /// Consecutive failures so far.
    pub failed_attempts: u32,
    /// Lockout end, if currently locked.
    pub locked_until: Option<Timestamp>,
}

/// A manual lock decision for one category.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryLock {
    /// Always locked (requires the PIN), regardless of keywords.
    Locked,
    /// Never locked, even if a keyword matches.
    Unlocked,
}

impl CategoryLock {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Unlocked => "unlocked",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "locked" => Some(Self::Locked),
            "unlocked" => Some(Self::Unlocked),
            _ => None,
        }
    }
}
crate::ids::sql_text_enum!(CategoryLock);

fn is_valid_pin(pin: &SecretString) -> bool {
    let p = pin.expose_secret();
    (4..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())
}

/// Persisted PIN row.
struct PinRow {
    phc: Option<String>,
    failed_attempts: u32,
    locked_until: Option<Timestamp>,
    lockout_ms: Option<i64>,
}

pub(crate) fn normalize_keyword(k: &str) -> Result<String> {
    let k = k.trim().to_lowercase();
    if k.is_empty() {
        return Err(StoreError::invalid("keyword", "must not be empty"));
    }
    if k.chars().count() > MAX_KEYWORD_CHARS || has_control(&k) {
        return Err(StoreError::invalid(
            "keyword",
            "must be at most 64 printable characters",
        ));
    }
    Ok(k)
}

/// Parental-control operations. Obtain with [`Store::parental`].
///
/// PIN operations run Argon2 (≈ 0.1–0.3 s with default parameters in release builds) —
/// call them off the UI thread. They are serialised so concurrent attempts are counted
/// correctly; other store operations are not blocked while a hash is computed.
#[derive(Clone, Copy, Debug)]
pub struct Parental<'a> {
    pub(crate) store: &'a Store,
}

impl Parental<'_> {
    fn load(&self) -> Result<PinRow> {
        Ok(self.store.conn().query_row(
            "SELECT phc, failed_attempts, locked_until, lockout_ms FROM parental_pin WHERE id = 1",
            [],
            |r| {
                Ok(PinRow {
                    phc: r.get(0)?,
                    failed_attempts: r.get(1)?,
                    locked_until: r.get(2)?,
                    lockout_ms: r.get(3)?,
                })
            },
        )?)
    }

    fn hash(&self, pin: &SecretString) -> Result<String> {
        let argon = self.store.pin_hash.argon2()?;
        let hash = argon
            .hash_password(pin.expose_secret().as_bytes())
            .map_err(|_| StoreError::PinHash("hashing failed"))?;
        Ok(hash.to_string())
    }

    fn write_hash(&self, phc: Option<&str>) -> Result<()> {
        self.store.conn().execute(
            "UPDATE parental_pin SET phc = ?1, failed_attempts = 0, locked_until = NULL,
                    lockout_ms = NULL, updated_at = ?2 WHERE id = 1",
            params![phc, self.store.now()],
        )?;
        Ok(())
    }

    /// True if a PIN is set.
    pub fn is_pin_set(&self) -> Result<bool> {
        Ok(self.load()?.phc.is_some())
    }

    /// Current PIN state (for showing "locked for N s").
    pub fn status(&self) -> Result<PinStatus> {
        let row = self.load()?;
        let now = self.store.now();
        Ok(PinStatus {
            is_set: row.phc.is_some(),
            failed_attempts: row.failed_attempts,
            locked_until: row.locked_until.filter(|t| *t > now),
        })
    }

    /// Sets the first PIN. Fails with [`StoreError::PinAlreadySet`] if one exists (use
    /// [`change_pin`](Self::change_pin)).
    pub fn set_pin(&self, pin: &SecretString) -> Result<()> {
        if !is_valid_pin(pin) {
            return Err(StoreError::InvalidPinFormat);
        }
        let _gate = self.store.pin_gate();
        if self.load()?.phc.is_some() {
            return Err(StoreError::PinAlreadySet);
        }
        let phc = self.hash(pin)?;
        self.write_hash(Some(&phc))?;
        tracing::info!("parental PIN set");
        Ok(())
    }

    /// Checks a PIN, applying and updating the rate limit. Fails with
    /// [`StoreError::PinNotSet`] if no PIN is set. Malformed input counts as a failure.
    pub fn verify_pin(&self, pin: &SecretString) -> Result<PinOutcome> {
        let _gate = self.store.pin_gate();
        self.verify_locked(pin)
    }

    /// Verification body; the caller holds the PIN gate.
    fn verify_locked(&self, pin: &SecretString) -> Result<PinOutcome> {
        let row = self.load()?;
        let Some(phc) = row.phc else {
            return Err(StoreError::PinNotSet);
        };
        let now = self.store.now();
        if let Some(until) = row.locked_until
            && until > now
        {
            // Clock moved backwards? Never wait longer than the lockout itself.
            let len = Duration::from_millis(row.lockout_ms.unwrap_or(0).max(0).unsigned_abs());
            let capped = now.saturating_add(len);
            if until > capped {
                self.store.conn().execute(
                    "UPDATE parental_pin SET locked_until = ?1 WHERE id = 1",
                    [capped],
                )?;
                return Ok(PinOutcome::LockedOut { until: capped });
            }
            return Ok(PinOutcome::LockedOut { until });
        }

        let parsed =
            PasswordHash::new(&phc).map_err(|_| StoreError::PinHash("stored hash is malformed"))?;
        let correct = is_valid_pin(pin) && self.check(pin, &parsed)?;

        if correct {
            let stale = self.needs_rehash(&parsed);
            if stale {
                let upgraded = self.hash(pin)?;
                self.write_hash(Some(&upgraded))?;
                tracing::info!("parental PIN hash upgraded to current parameters");
            } else if row.failed_attempts > 0 || row.locked_until.is_some() {
                self.store.conn().execute(
                    "UPDATE parental_pin SET failed_attempts = 0, locked_until = NULL,
                            lockout_ms = NULL WHERE id = 1",
                    [],
                )?;
            }
            return Ok(PinOutcome::Ok);
        }

        let failures = row.failed_attempts.saturating_add(1);
        let policy = self.store.lockout;
        let outcome = match policy.lockout_for(failures) {
            Some(len) => {
                let until = now.saturating_add(len);
                self.store.conn().execute(
                    "UPDATE parental_pin SET failed_attempts = ?1, locked_until = ?2,
                            lockout_ms = ?3, updated_at = ?4 WHERE id = 1",
                    params![failures, until, duration_to_millis(len), now],
                )?;
                tracing::warn!(
                    failures,
                    lockout_secs = len.as_secs(),
                    "parental PIN locked out"
                );
                PinOutcome::LockedOut { until }
            }
            None => {
                self.store.conn().execute(
                    "UPDATE parental_pin SET failed_attempts = ?1, locked_until = NULL,
                            lockout_ms = NULL, updated_at = ?2 WHERE id = 1",
                    params![failures, now],
                )?;
                PinOutcome::Wrong {
                    remaining_before_lockout: policy.max_attempts.max(1) - failures,
                }
            }
        };
        Ok(outcome)
    }

    /// Constant-time Argon2 verification with the stored parameters (bounded).
    fn check(&self, pin: &SecretString, parsed: &PasswordHash) -> Result<bool> {
        if parsed.algorithm.as_str() != "argon2id" {
            return Err(StoreError::PinHash("stored hash is not Argon2id"));
        }
        let params = Params::try_from(parsed)
            .map_err(|_| StoreError::PinHash("stored hash has invalid parameters"))?;
        if params.m_cost() > MAX_STORED_M_COST_KIB
            || params.t_cost() > MAX_STORED_T_COST
            || params.p_cost() > MAX_STORED_P_COST
        {
            return Err(StoreError::PinHash(
                "stored hash has implausible parameters",
            ));
        }
        match Argon2::default().verify_password(pin.expose_secret().as_bytes(), parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
            Err(_) => Err(StoreError::PinHash("verification failed")),
        }
    }

    fn needs_rehash(&self, parsed: &PasswordHash) -> bool {
        let want = self.store.pin_hash;
        match Params::try_from(parsed) {
            Ok(p) => {
                p.m_cost() != want.m_cost_kib
                    || p.t_cost() != want.t_cost
                    || p.p_cost() != want.p_cost
                    || parsed.version != Some(Version::V0x13 as u32)
            }
            Err(_) => true,
        }
    }

    /// Changes the PIN after verifying the current one (rate-limited). The new PIN's
    /// format is checked first, so a typo in it does not cost an attempt. Returns the
    /// verification outcome; the PIN is changed only on [`PinOutcome::Ok`].
    pub fn change_pin(&self, current: &SecretString, new: &SecretString) -> Result<PinOutcome> {
        if !is_valid_pin(new) {
            return Err(StoreError::InvalidPinFormat);
        }
        let _gate = self.store.pin_gate();
        let outcome = self.verify_locked(current)?;
        if outcome.is_ok() {
            let phc = self.hash(new)?;
            self.write_hash(Some(&phc))?;
            tracing::info!("parental PIN changed");
        }
        Ok(outcome)
    }

    /// Removes the PIN (disables parental control) after verifying it (rate-limited).
    pub fn remove_pin(&self, current: &SecretString) -> Result<PinOutcome> {
        let _gate = self.store.pin_gate();
        let outcome = self.verify_locked(current)?;
        if outcome.is_ok() {
            self.write_hash(None)?;
            tracing::info!("parental PIN removed");
        }
        Ok(outcome)
    }

    /// Removes the PIN **without** verification ("forgot PIN"). The app must first
    /// authenticate the user another way (e.g. re-entering an account password).
    pub fn force_remove_pin(&self) -> Result<()> {
        let _gate = self.store.pin_gate();
        self.write_hash(None)?;
        tracing::warn!("parental PIN force-removed");
        Ok(())
    }

    /// The keyword list, sorted.
    pub fn keywords(&self) -> Result<Vec<String>> {
        let conn = self.store.conn();
        let mut stmt =
            conn.prepare_cached("SELECT keyword FROM parental_keywords ORDER BY keyword")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces the keyword list. Keywords are trimmed, lower-cased and de-duplicated;
    /// at most 200 of up to 64 characters each.
    pub fn set_keywords<S: AsRef<str>>(&self, keywords: &[S]) -> Result<()> {
        let mut normalized: Vec<String> = keywords
            .iter()
            .map(|k| normalize_keyword(k.as_ref()))
            .collect::<Result<_>>()?;
        normalized.sort();
        normalized.dedup();
        if normalized.len() > MAX_KEYWORDS {
            return Err(StoreError::invalid("keywords", "too many keywords"));
        }
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM parental_keywords", [])?;
        {
            let mut stmt = tx.prepare("INSERT INTO parental_keywords (keyword) VALUES (?1)")?;
            for k in &normalized {
                stmt.execute([k])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Adds one keyword. Returns `false` if it was already present.
    pub fn add_keyword(&self, keyword: &str) -> Result<bool> {
        let k = normalize_keyword(keyword)?;
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let count: i64 =
            tx.query_row("SELECT COUNT(*) FROM parental_keywords", [], |r| r.get(0))?;
        let n = tx.execute(
            "INSERT INTO parental_keywords (keyword) VALUES (?1) ON CONFLICT DO NOTHING",
            [&k],
        )?;
        if n > 0 && usize::try_from(count).unwrap_or(usize::MAX) >= MAX_KEYWORDS {
            return Err(StoreError::invalid("keywords", "too many keywords"));
        }
        tx.commit()?;
        Ok(n > 0)
    }

    /// Removes one keyword (matched after normalisation). Returns `false` if absent.
    pub fn remove_keyword(&self, keyword: &str) -> Result<bool> {
        let Ok(k) = normalize_keyword(keyword) else {
            return Ok(false);
        };
        let n = self
            .store
            .conn()
            .execute("DELETE FROM parental_keywords WHERE keyword = ?1", [&k])?;
        Ok(n > 0)
    }

    /// Restores [`DEFAULT_KEYWORDS`].
    pub fn reset_keywords(&self) -> Result<()> {
        self.set_keywords(DEFAULT_KEYWORDS)
    }

    /// Sets (`Some`) or clears (`None`) a manual lock on a category.
    pub fn set_category_lock(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        category_id: &str,
        lock: Option<CategoryLock>,
    ) -> Result<()> {
        let category_id = validate_item_id("category_id", category_id)?;
        let conn = self.store.conn();
        match lock {
            Some(state) => {
                conn.execute(
                    "INSERT INTO category_locks (profile_id, kind, category_id, state)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (profile_id, kind, category_id) DO UPDATE SET state = excluded.state",
                    params![profile, kind, category_id, state],
                )
                .map_err(map_fk(profile))?;
            }
            None => {
                conn.execute(
                    "DELETE FROM category_locks WHERE profile_id = ?1 AND kind = ?2 AND category_id = ?3",
                    params![profile, kind, category_id],
                )?;
            }
        }
        Ok(())
    }

    /// The manual lock on one category, if any.
    pub fn category_lock(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        category_id: &str,
    ) -> Result<Option<CategoryLock>> {
        Ok(self
            .store
            .conn()
            .prepare_cached(
                "SELECT state FROM category_locks
                 WHERE profile_id = ?1 AND kind = ?2 AND category_id = ?3",
            )?
            .query_row(params![profile, kind, category_id], |r| r.get(0))
            .optional()?)
    }

    /// All manual locks of one section, keyed by category id.
    pub fn category_locks(
        &self,
        profile: ProfileId,
        kind: ContentKind,
    ) -> Result<HashMap<String, CategoryLock>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT category_id, state FROM category_locks WHERE profile_id = ?1 AND kind = ?2",
        )?;
        let rows = stmt.query_map(params![profile, kind], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockout_schedule() {
        let p = LockoutPolicy::default();
        assert_eq!(p.lockout_for(4), None);
        assert_eq!(p.lockout_for(5), Some(Duration::from_secs(30)));
        assert_eq!(p.lockout_for(6), Some(Duration::from_secs(60)));
        assert_eq!(p.lockout_for(7), Some(Duration::from_secs(120)));
        assert_eq!(p.lockout_for(11), Some(Duration::from_secs(1920)));
        assert_eq!(p.lockout_for(12), Some(Duration::from_secs(3600)));
        assert_eq!(p.lockout_for(u32::MAX), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn pin_format() {
        for ok in ["1234", "00000000", "987654"] {
            assert!(is_valid_pin(&SecretString::from(ok)), "{ok}");
        }
        for bad in ["123", "123456789", "12a4", " 1234", "１２３４", ""] {
            assert!(!is_valid_pin(&SecretString::from(bad)), "{bad:?}");
        }
    }

    #[test]
    fn keyword_normalisation() {
        assert_eq!(normalize_keyword("  ADULT ").ok().as_deref(), Some("adult"));
        assert!(normalize_keyword("   ").is_err());
        assert!(normalize_keyword(&"x".repeat(65)).is_err());
    }
}

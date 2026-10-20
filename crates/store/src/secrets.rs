//! Secret storage: the OS keychain, behind a small trait.
//!
//! Everything credential-like lives here and **only** here — never in SQLite:
//!
//! | Key | Value |
//! |---|---|
//! | `profile/<uuid>/password` | Xtream password |
//! | `profile/<uuid>/m3u_url` | M3U playlist URL (they often embed `username=…&password=…`) |
//! | `profile/<uuid>/epg_url` | Per-profile EPG URL override (`xmltv.php` URLs embed credentials) |
//! | `tmdb_api_key` | The user's own TMDB API key (opt-in) |
//! | `cache_key` | Random 256-bit master key for the encrypted cache (hex) |
//!
//! [`KeyringSecretStore`] is the production backend (Windows Credential Manager, macOS
//! Keychain, Secret Service on Linux/BSD; service name `opensmarters`).
//! [`MemorySecretStore`] is for tests.
//!
//! Values are [`SecretString`]s: their `Debug` output is redacted and they are zeroised on
//! drop. Neither the trait nor its errors ever print a value.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::ids::ProfileId;

/// Name of a secret in the [`SecretStore`].
///
/// Construct with the typed helpers ([`SecretKey::profile_password`], …) or
/// [`SecretKey::custom`] for app-specific secrets.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretKey(String);

impl SecretKey {
    const MAX_LEN: usize = 128;

    /// `profile/<id>/password` — an Xtream profile's password.
    pub fn profile_password(id: ProfileId) -> Self {
        Self(format!("profile/{id}/password"))
    }

    /// `profile/<id>/m3u_url` — an M3U-URL profile's playlist URL.
    pub fn profile_m3u_url(id: ProfileId) -> Self {
        Self(format!("profile/{id}/m3u_url"))
    }

    /// `profile/<id>/epg_url` — a profile's EPG (XMLTV) URL override.
    pub fn profile_epg_url(id: ProfileId) -> Self {
        Self(format!("profile/{id}/epg_url"))
    }

    /// Every key that may belong to a profile (deleted together with the profile).
    pub fn all_for_profile(id: ProfileId) -> [Self; 3] {
        [
            Self::profile_password(id),
            Self::profile_m3u_url(id),
            Self::profile_epg_url(id),
        ]
    }

    /// `tmdb_api_key` — the user's TMDB API key.
    pub fn tmdb_api_key() -> Self {
        Self("tmdb_api_key".to_owned())
    }

    /// `cache_key` — master key of the encrypted cache. Managed by the store.
    pub fn cache_master_key() -> Self {
        Self("cache_key".to_owned())
    }

    /// An app-defined key: 1–128 chars of `[A-Za-z0-9._/-]`, not starting with
    /// `profile/` (reserved).
    pub fn custom(name: &str) -> Result<Self, SecretError> {
        let ok_chars = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
        if name.is_empty()
            || name.len() > Self::MAX_LEN
            || !ok_chars
            || name.starts_with("profile/")
        {
            return Err(SecretError::InvalidKey);
        }
        Ok(Self(name.to_owned()))
    }

    /// The key as a string, e.g. `profile/…/password`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", self.0)
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors from a [`SecretStore`]. Never contain secret material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// No usable OS keychain on this system (e.g. no Secret Service on a headless Linux).
    #[error("no OS keychain is available: {0}")]
    Unavailable(String),
    /// The keychain exists but refused access (locked, denied by the user/policy).
    #[error("the OS keychain is locked or access was denied: {0}")]
    AccessDenied(String),
    /// The value exceeds the platform's size limit (2560 bytes on Windows).
    #[error("the secret is too long for the OS keychain (limit {limit} bytes)")]
    TooLong {
        /// Platform limit in bytes.
        limit: u32,
    },
    /// The stored value is not valid UTF-8 or not in the expected format.
    #[error("the stored secret is malformed")]
    Corrupt,
    /// The key name is not acceptable.
    #[error("invalid secret key name")]
    InvalidKey,
    /// Any other platform failure.
    #[error("OS keychain error: {0}")]
    Platform(String),
}

/// A key–value store for secrets.
///
/// Implementations must be thread-safe, must never log or `Debug`-print values, and
/// `delete` must be idempotent (deleting a missing key is `Ok`).
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// Creates or replaces the secret under `key`.
    fn set(&self, key: &SecretKey, value: &SecretString) -> Result<(), SecretError>;
    /// Reads the secret under `key`; `Ok(None)` if there is none.
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError>;
    /// Removes the secret under `key`; `Ok(())` if there was none.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError>;
}

// ---------------------------------------------------------------------------------------
// OS keychain backend
// ---------------------------------------------------------------------------------------

/// The OS keychain via the [`keyring`] crate.
///
/// - **Windows:** Windows Credential Manager, one *generic credential* per key with the
///   target name `<key>.opensmarters` (e.g. `profile/<uuid>/password.opensmarters`),
///   visible under *Control Panel → Credential Manager → Windows Credentials*.
///   Values are stored as UTF-8 bytes; the platform limit is 2560 bytes per value.
/// - **macOS:** the login Keychain (service `opensmarters`, account = key).
/// - **Linux/BSD:** the Secret Service (GNOME Keyring, KWallet) over D-Bus.
///
/// Calls block (they are IPC to the OS), so call them off the UI thread. All keychain
/// operations in the process are serialised through one global lock: on Windows,
/// concurrent Credential Manager calls from different threads — even on *different*
/// credentials — were observed to lose writes (a value just written read back as missing,
/// and a deleted credential reappearing).
pub struct KeyringSecretStore {
    service: String,
}

/// Process-wide serialisation of keychain calls (see [`KeyringSecretStore`]).
static KEYCHAIN_LOCK: Mutex<()> = Mutex::new(());

fn keychain_guard() -> std::sync::MutexGuard<'static, ()> {
    KEYCHAIN_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

impl fmt::Debug for KeyringSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringSecretStore")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl KeyringSecretStore {
    /// The keychain service name used by the app.
    pub const SERVICE: &'static str = "opensmarters";

    /// The production store (service `opensmarters`). Fails with
    /// [`SecretError::Unavailable`] if the platform keychain cannot be initialised.
    pub fn new() -> Result<Self, SecretError> {
        Self::with_service(Self::SERVICE)
    }

    /// A store under a different service name (useful to isolate experiments).
    pub fn with_service(service: impl Into<String>) -> Result<Self, SecretError> {
        if let Err(e) = keyring::Entry::store_status() {
            return Err(SecretError::Unavailable(safe_keyring_message(e)));
        }
        Ok(Self {
            service: service.into(),
        })
    }

    fn entry(&self, key: &SecretKey) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, key.as_str()).map_err(map_keyring_error)
    }
}

impl SecretStore for KeyringSecretStore {
    fn set(&self, key: &SecretKey, value: &SecretString) -> Result<(), SecretError> {
        let _g = keychain_guard();
        self.entry(key)?
            .set_secret(value.expose_secret().as_bytes())
            .map_err(map_keyring_error)
    }

    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
        let _g = keychain_guard();
        match self.entry(key)?.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let s = std::str::from_utf8(&bytes).map_err(|_| SecretError::Corrupt)?;
                Ok(Some(SecretString::from(s)))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring_error(e)),
        }
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        let _g = keychain_guard();
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_keyring_error(e)),
        }
    }
}

/// Converts a keyring error without ever copying secret bytes (some variants carry the
/// raw credential blob).
fn map_keyring_error(e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::NoStorageAccess(inner) => SecretError::AccessDenied(inner.to_string()),
        keyring::Error::NoDefaultStore => {
            SecretError::Unavailable("credential store could not be initialised".to_owned())
        }
        keyring::Error::TooLong(_, limit) => SecretError::TooLong { limit },
        keyring::Error::BadEncoding(mut bytes) => {
            zeroize::Zeroize::zeroize(&mut bytes);
            SecretError::Corrupt
        }
        keyring::Error::BadDataFormat(mut bytes, _) => {
            zeroize::Zeroize::zeroize(&mut bytes);
            SecretError::Corrupt
        }
        keyring::Error::Invalid(attr, _) if attr == "platform" => {
            SecretError::Unavailable("unsupported platform".to_owned())
        }
        keyring::Error::Invalid(_, _) => SecretError::InvalidKey,
        keyring::Error::PlatformFailure(inner) => SecretError::Platform(inner.to_string()),
        other => SecretError::Platform(safe_keyring_message(&other)),
    }
}

/// A message for keyring errors that is safe to log.
fn safe_keyring_message(e: &keyring::Error) -> String {
    match e {
        keyring::Error::BadEncoding(_) | keyring::Error::BadDataFormat(_, _) => {
            "malformed credential".to_owned()
        }
        keyring::Error::Ambiguous(_) => "more than one matching credential".to_owned(),
        // Remaining variants carry only platform messages / attribute names.
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------------------
// In-memory backend
// ---------------------------------------------------------------------------------------

/// A process-local secret store for tests. Values are zeroised on drop.
///
/// [`MemorySecretStore::set_failing`] makes every call fail, to exercise error paths.
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<String, SecretString>>,
    failing: AtomicBool,
}

impl fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemorySecretStore")
            .field("entries", &self.len())
            .finish_non_exhaustive()
    }
}

impl MemorySecretStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored secrets.
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// True if nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if `key` has a value.
    pub fn contains(&self, key: &SecretKey) -> bool {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(key.as_str())
    }

    /// Names of the stored keys (sorted). Values are not exposed.
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// When `true`, every operation fails with [`SecretError::Platform`].
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    fn check(&self) -> Result<(), SecretError> {
        if self.failing.load(Ordering::SeqCst) {
            Err(SecretError::Platform("injected failure".to_owned()))
        } else {
            Ok(())
        }
    }
}

impl SecretStore for MemorySecretStore {
    fn set(&self, key: &SecretKey, value: &SecretString) -> Result<(), SecretError> {
        self.check()?;
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.as_str().to_owned(), value.clone());
        Ok(())
    }

    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
        self.check()?;
        Ok(self
            .map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key.as_str())
            .cloned())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.check()?;
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key.as_str());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        let id = ProfileId::new_random();
        assert_eq!(
            SecretKey::profile_password(id).as_str(),
            format!("profile/{id}/password")
        );
        assert_eq!(
            SecretKey::profile_m3u_url(id).as_str(),
            format!("profile/{id}/m3u_url")
        );
        assert_eq!(SecretKey::tmdb_api_key().as_str(), "tmdb_api_key");
        assert!(SecretKey::custom("app/token").is_ok());
        assert!(SecretKey::custom("profile/x").is_err());
        assert!(SecretKey::custom("has space").is_err());
        assert!(SecretKey::custom("").is_err());
    }

    #[test]
    fn memory_store_round_trip_and_redaction() {
        let s = MemorySecretStore::new();
        let k = SecretKey::tmdb_api_key();
        assert!(s.get(&k).ok().flatten().is_none());
        s.set(&k, &SecretString::from("hunter2-memory")).ok();
        let got = s.get(&k).ok().flatten();
        assert_eq!(
            got.as_ref().map(|v| v.expose_secret()),
            Some("hunter2-memory")
        );
        assert!(!format!("{s:?}").contains("hunter2"));
        assert!(!format!("{got:?}").contains("hunter2"));
        assert!(s.delete(&k).is_ok());
        assert!(s.delete(&k).is_ok(), "delete is idempotent");
        assert!(s.is_empty());
    }

    #[test]
    fn memory_store_failure_injection() {
        let s = MemorySecretStore::new();
        s.set_failing(true);
        assert!(
            s.set(&SecretKey::tmdb_api_key(), &SecretString::from("x"))
                .is_err()
        );
        s.set_failing(false);
        assert!(
            s.set(&SecretKey::tmdb_api_key(), &SecretString::from("x"))
                .is_ok()
        );
    }
}

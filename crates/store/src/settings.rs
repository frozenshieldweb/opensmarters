//! Typed global application settings.
//!
//! Each setting is a [`Setting<T>`] constant: a key plus a default. Values are stored as
//! JSON in the `settings` table; a missing or unreadable value yields the default (an
//! unreadable value is logged by key name, never by content). The app can declare its
//! own settings with [`Setting::new`].
//!
//! ```
//! # use opensmarters_store::{Store, StoreOptions, MemorySecretStore, settings::{self, TimeFormat}};
//! # use std::sync::Arc;
//! # let store = Store::open(StoreOptions::in_memory(), Arc::new(MemorySecretStore::new()))?;
//! let s = store.settings();
//! assert_eq!(s.get(&settings::TIME_FORMAT)?, TimeFormat::H24);
//! s.set(&settings::TIME_FORMAT, &TimeFormat::H12)?;
//! assert_eq!(s.get(&settings::TIME_FORMAT)?, TimeFormat::H12);
//! # Ok::<(), opensmarters_store::StoreError>(())
//! ```
//!
//! **Never store secrets here** — the TMDB API key, for example, has dedicated keychain
//! accessors ([`AppSettings::tmdb_api_key`]).

use std::fmt;

use rusqlite::{OptionalExtension, params};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Store;
use crate::error::{Result, StoreError};
use crate::ids::ProfileId;
use crate::secrets::SecretKey;

/// A typed setting: a stable key and a default value.
pub struct Setting<T> {
    key: &'static str,
    default: fn() -> T,
}

impl<T> Setting<T> {
    /// Declares a setting. `key` must be unique, ASCII and stable across releases.
    pub const fn new(key: &'static str, default: fn() -> T) -> Self {
        Self { key, default }
    }

    /// The storage key.
    pub const fn key(&self) -> &'static str {
        self.key
    }

    /// The default value.
    pub fn default_value(&self) -> T {
        (self.default)()
    }
}

impl<T> fmt::Debug for Setting<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Setting").field("key", &self.key).finish()
    }
}

/// Clock display format.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub enum TimeFormat {
    /// 24-hour clock (default).
    #[default]
    #[serde(rename = "24h")]
    H24,
    /// 12-hour clock with AM/PM (midnight is 12:00 AM).
    #[serde(rename = "12h")]
    H12,
}

/// 12/24-hour clock. Default: 24 h.
pub const TIME_FORMAT: Setting<TimeFormat> = Setting::new("time_format", TimeFormat::default);
/// UI language as a BCP 47 tag (e.g. `"fr-CA"`); `None` follows the system. Default: `None`.
pub const LANGUAGE: Setting<Option<String>> = Setting::new("language", || None);
/// UI scale factor. Default: `1.0`. The app should clamp it to a sane range.
pub const UI_SCALE: Setting<f32> = Setting::new("ui_scale", || 1.0);
/// The profile that was active last ([`Profiles::mark_used`](crate::Profiles::mark_used)
/// maintains it; deleting that profile clears it). Default: `None`.
pub const LAST_PROFILE: Setting<Option<ProfileId>> = Setting::new("last_profile", || None);
/// Whether the user opted in to TMDB enrichment. The key itself is in the keychain.
/// Default: `false`.
pub const TMDB_ENABLED: Setting<bool> = Setting::new("tmdb_enabled", || false);
/// Cache size cap in bytes; use [`Cache::set_max_bytes`](crate::Cache::set_max_bytes) to
/// change it (that also evicts). Default: 512 MiB.
pub const CACHE_MAX_BYTES: Setting<u64> =
    Setting::new("cache_max_bytes", || DEFAULT_CACHE_MAX_BYTES);

/// Default cache cap: 512 MiB.
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn validate_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err(StoreError::invalid(
            "setting key",
            "must be 1-128 chars of [A-Za-z0-9_.-]",
        ));
    }
    Ok(())
}

/// Global settings operations. Obtain with [`Store::settings`].
#[derive(Clone, Copy, Debug)]
pub struct AppSettings<'a> {
    pub(crate) store: &'a Store,
}

impl AppSettings<'_> {
    /// Reads a setting, falling back to its default if unset or unreadable.
    pub fn get<T: DeserializeOwned>(&self, setting: &Setting<T>) -> Result<T> {
        validate_key(setting.key)?;
        let raw: Option<String> = self
            .store
            .conn()
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
            .query_row([setting.key], |r| r.get(0))
            .optional()?;
        Ok(match raw {
            None => setting.default_value(),
            Some(json) => match serde_json::from_str(&json) {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!(key = setting.key, "unreadable setting; using default");
                    setting.default_value()
                }
            },
        })
    }

    /// Writes a setting.
    pub fn set<T: Serialize>(&self, setting: &Setting<T>, value: &T) -> Result<()> {
        validate_key(setting.key)?;
        let json = serde_json::to_string(value)?;
        self.store.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![setting.key, json],
        )?;
        if setting.key == CACHE_MAX_BYTES.key {
            self.store.reload_cache_cap()?;
        }
        Ok(())
    }

    /// Removes a stored value so the default applies again.
    pub fn reset<T>(&self, setting: &Setting<T>) -> Result<()> {
        validate_key(setting.key)?;
        self.store
            .conn()
            .execute("DELETE FROM settings WHERE key = ?1", [setting.key])?;
        if setting.key == CACHE_MAX_BYTES.key {
            self.store.reload_cache_cap()?;
        }
        Ok(())
    }

    /// The user's TMDB API key from the keychain.
    pub fn tmdb_api_key(&self) -> Result<Option<SecretString>> {
        Ok(self.store.secrets.get(&SecretKey::tmdb_api_key())?)
    }

    /// Stores (`Some`) or removes (`None`) the TMDB API key in the keychain.
    pub fn set_tmdb_api_key(&self, key: Option<&SecretString>) -> Result<()> {
        let k = SecretKey::tmdb_api_key();
        match key {
            Some(v) => {
                let t = v.expose_secret().trim();
                if t.is_empty() || t.len() > 512 || t.chars().any(char::is_control) {
                    return Err(StoreError::invalid(
                        "tmdb_api_key",
                        "must be 1-512 printable characters",
                    ));
                }
                self.store.secrets.set(&k, &SecretString::from(t))?;
            }
            None => self.store.secrets.delete(&k)?,
        }
        Ok(())
    }
}

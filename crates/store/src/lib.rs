//! # opensmarters-store
//!
//! Local persistence for opensmarters: a SQLite database for everything that is **not**
//! secret, and the OS keychain for everything that is.
//!
//! | What | Where | API |
//! |---|---|---|
//! | Profiles (name, kind, server URL, username, M3U file path, per-profile settings) | SQLite | [`Store::profiles`] → [`Profiles`] |
//! | Xtream passwords, M3U URLs, EPG URL overrides | keychain | [`Profiles::password`], [`Profiles::m3u_url`], [`Profiles::epg_url_override`] |
//! | Favourites (live / movies / series) with display snapshots | SQLite | [`Store::favourites`] → [`Favourites`] |
//! | Resume points, watch history, continue watching, up-next inputs | SQLite | [`Store::playback`] → [`Playback`] |
//! | Recently watched live channels | SQLite | [`Store::recent_live`] → [`RecentLive`] |
//! | Recordings and downloads (jobs, progress, file names; never URLs) | SQLite | [`Store::recordings`] → [`Recordings`] |
//! | Programme reminders | SQLite | [`Store::reminders`] → [`Reminders`] |
//! | Response cache (TTL, size cap, LRU eviction) — **encrypted** | SQLite + key in keychain | [`Store::cache`] → [`Cache`] |
//! | Global settings (time format, language, UI scale, last profile, TMDB opt-in, cache cap) | SQLite | [`Store::settings`] → [`AppSettings`] |
//! | TMDB API key | keychain | [`AppSettings::tmdb_api_key`] |
//! | Parental PIN (Argon2id), lockout state, keywords, category locks | SQLite | [`Store::parental`] → [`Parental`] |
//! | JSON export / import of the non-secret parts (backup, migration) | — | [`Store::export_json`], [`Store::import_json`] ([`export`]) |
//!
//! ## Quick start
//!
//! ```
//! use std::sync::Arc;
//! use opensmarters_store::{
//!     ContentKind, ItemSnapshot, MemorySecretStore, NewProfile, NewProfileSource,
//!     ProfileSettings, Store, StoreOptions,
//! };
//! use secrecy::{ExposeSecret, SecretString};
//!
//! // In the app: `Store::open_default(Arc::new(KeyringSecretStore::new()?))`.
//! let store = Store::open(StoreOptions::in_memory(), Arc::new(MemorySecretStore::new()))?;
//!
//! let profile = store.profiles().create(NewProfile {
//!     name: "Home".into(),
//!     source: NewProfileSource::Xtream {
//!         server_url: "http://panel.example.invalid:8080".into(),
//!         username: "demo".into(),
//!         password: SecretString::from("not-a-real-password"),
//!     },
//!     settings: ProfileSettings::default(),
//! })?;
//! store.profiles().mark_used(profile.id)?;
//!
//! let pw = store.profiles().password(profile.id)?.expect("stored in the keychain");
//! assert_eq!(pw.expose_secret(), "not-a-real-password");
//!
//! store.favourites().add(profile.id, ContentKind::Live, "1001", &ItemSnapshot::named("News"))?;
//! assert!(store.favourites().contains(profile.id, ContentKind::Live, "1001")?);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Threading model
//!
//! The API is synchronous. [`Store`] is `Send + Sync`: share it as `Arc<Store>` and call it
//! from worker threads or `tokio::task::spawn_blocking`, **never from the UI thread** —
//! SQLite writes `fsync`, keychain calls are IPC (and may show an OS unlock prompt on
//! Linux/macOS), and PIN checks run Argon2 (~0.1–0.3 s).
//!
//! Internally there is one SQLite connection behind a `Mutex`; each method holds it only
//! for its own statements/transaction. Keychain calls, Argon2 hashing and cache
//! decryption happen **outside** that lock. PIN operations are additionally serialised
//! among themselves so failed attempts are counted exactly, and all OS keychain calls in
//! the process go through one global lock (concurrent Windows Credential Manager calls
//! were observed to lose writes). A poisoned lock is recovered
//! (SQLite rolls back the interrupted transaction), never propagated as a panic.
//! Multiple processes may open the same file: WAL mode plus `busy_timeout` (5 s default)
//! handle that, and migrations take an exclusive write lock.
//!
//! ## Files
//!
//! Default database: [`default_db_path`] = `ProjectDirs("org", "opensmarters",
//! "opensmarters").data_local_dir()/opensmarters.db`, i.e. on Windows
//! `%LOCALAPPDATA%\opensmarters\opensmarters\data\opensmarters.db` (with `-wal`/`-shm`
//! siblings while open). *Local*, not *Roaming*: the cache can be large and history is
//! private to the machine. Override with [`StoreOptions::at_path`] for tests or a
//! portable install. On Unix the directory is created `0700` and the file `0600`.
//!
//! Keychain entries use service `opensmarters`; see [`KeyringSecretStore`] for the exact
//! Windows Credential Manager target names.
//!
//! Connection hardening: WAL, `synchronous=NORMAL`, `foreign_keys=ON`,
//! `secure_delete=FAST`, `SQLITE_DBCONFIG_DEFENSIVE`, `trusted_schema=OFF`, incremental
//! auto-vacuum, STRICT tables.
//!
//! ## Secrets never touch SQLite
//!
//! Enforced by construction (no column exists for them) and by validation: Xtream server
//! URLs carrying `user:pass@`, a query string or fragment are rejected; favourites /
//! history / lock ids that look like URLs are rejected; cache values *and* cache keys are
//! encrypted/hashed. An integration test (`tests/leak.rs`) scans the raw database, WAL and
//! SHM bytes for planted secrets. Provider display strings in snapshots are sanitised and
//! length-capped; the app should still avoid passing credential-bearing image URLs.
//!
//! ## Schema versions
//!
//! Forward-only migrations tracked in `PRAGMA user_version` (currently
//! [`SCHEMA_VERSION`]); a database from a newer build is refused with
//! [`StoreError::SchemaTooNew`].

#![forbid(unsafe_code)]

mod cache;
mod db;
mod error;
pub mod export;
mod favourites;
mod ids;
mod parental;
mod playback;
mod profiles;
mod recent;
pub mod recordings;
mod reminders;
mod secrets;
pub mod settings;
mod text;
mod time;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::Connection;

pub use cache::{Cache, CacheScope, CachedValue};
pub use db::SCHEMA_VERSION;
pub use error::{Result, StoreError};
pub use export::ImportSummary;
pub use favourites::{Favourite, Favourites, ItemSnapshot};
pub use ids::{ContentKind, ProfileId};
pub use parental::{
    CategoryLock, DEFAULT_KEYWORDS, LockoutPolicy, Parental, PinHashParams, PinOutcome, PinStatus,
};
pub use playback::{
    Playback, PlaybackKind, PlaybackProgress, PlaybackSnapshot, ResumeThresholds, next_episode,
};
pub use profiles::{
    LiveContainer, NewProfile, NewProfileSource, Profile, ProfileKind, ProfileSettings,
    ProfileSource, ProfileUpdate, Profiles, TimeshiftFormat, UserAgent,
};
pub use recent::{RecentChannel, RecentLive};
pub use recordings::{
    NewRecording, Recording, RecordingId, RecordingKind, RecordingState, Recordings,
};
pub use reminders::{NewReminder, Reminder, ReminderAction, ReminderId, Reminders};
pub use secrets::{KeyringSecretStore, MemorySecretStore, SecretError, SecretKey, SecretStore};
pub use settings::AppSettings;
pub use time::{Clock, ManualClock, SystemClock, Timestamp};

/// File name of the database inside the data directory.
pub const DB_FILE_NAME: &str = "opensmarters.db";

/// The default database location for the current user:
/// `ProjectDirs::from("org", "opensmarters", "opensmarters").data_local_dir()` +
/// [`DB_FILE_NAME`] — on Windows
/// `%LOCALAPPDATA%\opensmarters\opensmarters\data\opensmarters.db`.
pub fn default_db_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("org", "opensmarters", "opensmarters")
        .ok_or(StoreError::NoDataDir)?;
    Ok(dirs.data_local_dir().join(DB_FILE_NAME))
}

/// Where the database lives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DbLocation {
    /// [`default_db_path`].
    #[default]
    Default,
    /// An explicit file (tests, portable mode). Parent directories are created.
    Path(PathBuf),
    /// A private in-memory database, gone when the store is dropped.
    Memory,
}

/// Options for [`Store::open`]. Start from [`StoreOptions::default`],
/// [`StoreOptions::at_path`] or [`StoreOptions::in_memory`] and adjust fields.
#[derive(Clone, Debug)]
pub struct StoreOptions {
    /// Database location.
    pub location: DbLocation,
    /// Time source (inject a [`ManualClock`] in tests).
    pub clock: Arc<dyn Clock>,
    /// Resume / continue-watching thresholds.
    pub resume: ResumeThresholds,
    /// PIN failed-attempt policy.
    pub lockout: LockoutPolicy,
    /// Argon2id parameters for new PIN hashes.
    pub pin_hash: PinHashParams,
    /// How long to wait for another process's write lock. Default: 5 s.
    pub busy_timeout: Duration,
    /// Recent live channels kept per profile. Default: 50.
    pub recent_live_limit: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            location: DbLocation::Default,
            clock: Arc::new(SystemClock),
            resume: ResumeThresholds::default(),
            lockout: LockoutPolicy::default(),
            pin_hash: PinHashParams::default(),
            busy_timeout: Duration::from_secs(5),
            recent_live_limit: 50,
        }
    }
}

impl StoreOptions {
    /// Defaults, with the database at `path`.
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self {
            location: DbLocation::Path(path.into()),
            ..Self::default()
        }
    }

    /// Defaults, with an in-memory database.
    pub fn in_memory() -> Self {
        Self {
            location: DbLocation::Memory,
            ..Self::default()
        }
    }

    /// Replaces the clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Replaces the Argon2 parameters (tests can use cheap ones).
    #[must_use]
    pub fn with_pin_hash(mut self, params: PinHashParams) -> Self {
        self.pin_hash = params;
        self
    }
}

/// The store: a SQLite connection plus a [`SecretStore`]. See the [crate docs](crate) for
/// the threading model. Cheap per-domain handles are returned by [`profiles`](Self::profiles),
/// [`favourites`](Self::favourites), [`playback`](Self::playback),
/// [`recent_live`](Self::recent_live), [`recordings`](Self::recordings),
/// [`reminders`](Self::reminders), [`cache`](Self::cache), [`settings`](Self::settings) and
/// [`parental`](Self::parental).
pub struct Store {
    conn: Mutex<Connection>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    path: Option<PathBuf>,
    resume: ResumeThresholds,
    lockout: LockoutPolicy,
    pin_hash: PinHashParams,
    recent_live_limit: usize,
    cache_max_bytes: AtomicU64,
    cache_crypto: Mutex<Option<Arc<cache::CacheCrypto>>>,
    pin_gate: Mutex<()>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (creating and migrating if needed) the database described by `options`, using
    /// `secrets` for credentials.
    pub fn open(options: StoreOptions, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let (mut conn, path) = match &options.location {
            DbLocation::Default => {
                let p = default_db_path()?;
                (db::open_file(&p, options.busy_timeout)?, Some(p))
            }
            DbLocation::Path(p) => (db::open_file(p, options.busy_timeout)?, Some(p.clone())),
            DbLocation::Memory => (db::open_memory()?, None),
        };
        db::migrate(&mut conn)?;
        let store = Self {
            conn: Mutex::new(conn),
            secrets,
            clock: options.clock,
            path,
            resume: options.resume,
            lockout: options.lockout,
            pin_hash: options.pin_hash,
            recent_live_limit: options.recent_live_limit.max(1),
            cache_max_bytes: AtomicU64::new(settings::DEFAULT_CACHE_MAX_BYTES),
            cache_crypto: Mutex::new(None),
            pin_gate: Mutex::new(()),
        };
        store.reload_cache_cap()?;
        Ok(store)
    }

    /// Opens the database at [`default_db_path`] with default options.
    pub fn open_default(secrets: Arc<dyn SecretStore>) -> Result<Self> {
        Self::open(StoreOptions::default(), secrets)
    }

    /// The database file, or `None` for an in-memory store.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The secret store in use.
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// The clock in use.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Profiles and their secrets.
    pub fn profiles(&self) -> Profiles<'_> {
        Profiles { store: self }
    }

    /// Favourites.
    pub fn favourites(&self) -> Favourites<'_> {
        Favourites { store: self }
    }

    /// Resume points and history.
    pub fn playback(&self) -> Playback<'_> {
        Playback { store: self }
    }

    /// Recently watched live channels.
    pub fn recent_live(&self) -> RecentLive<'_> {
        RecentLive { store: self }
    }

    /// Recordings and downloads.
    pub fn recordings(&self) -> Recordings<'_> {
        Recordings { store: self }
    }

    /// Programme reminders.
    pub fn reminders(&self) -> Reminders<'_> {
        Reminders { store: self }
    }

    /// The encrypted response cache.
    pub fn cache(&self) -> Cache<'_> {
        Cache { store: self }
    }

    /// Global settings.
    pub fn settings(&self) -> AppSettings<'_> {
        AppSettings { store: self }
    }

    /// Parental control.
    pub fn parental(&self) -> Parental<'_> {
        Parental { store: self }
    }

    /// Reclaims free pages and truncates the WAL (e.g. after clearing the cache).
    pub fn compact(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute_batch("PRAGMA incremental_vacuum;")?;
        if self.path.is_some() {
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        }
        Ok(())
    }

    /// Checkpoints the WAL and closes the database, reporting any error (dropping the
    /// store also closes it, silently).
    pub fn close(self) -> Result<()> {
        let conn = self
            .conn
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        if self.path.is_some() {
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        }
        conn.close().map_err(|(_, e)| StoreError::Db(e))
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn pin_gate(&self) -> MutexGuard<'_, ()> {
        self.pin_gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn store_is_send_and_sync() {
        assert_send_sync::<Store>();
        assert_send_sync::<KeyringSecretStore>();
        assert_send_sync::<MemorySecretStore>();
    }

    #[test]
    fn default_path_is_under_local_data() {
        if let Ok(p) = default_db_path() {
            assert!(p.ends_with(Path::new("data").join(DB_FILE_NAME)) || p.ends_with(DB_FILE_NAME));
            assert!(p.to_string_lossy().contains("opensmarters"));
        }
    }
}

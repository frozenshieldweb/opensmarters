//! Error type for the store.

use crate::ids::ProfileId;
use crate::secrets::SecretError;

/// Result alias used throughout the crate.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Everything that can go wrong in the store.
///
/// No variant ever carries a secret (password, URL, PIN) — error messages are safe to log.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// An SQLite error.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// A filesystem error (creating the data directory, setting permissions, …).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The per-user data directory could not be determined (no home directory).
    #[error("could not determine the per-user data directory")]
    NoDataDir,

    /// The database was written by a newer build of opensmarters. Migrations are
    /// forward-only, so this build refuses to touch it.
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew {
        /// `PRAGMA user_version` found in the file.
        found: u32,
        /// Latest version this build knows.
        supported: u32,
    },

    /// A value failed validation before it reached the database.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// Which input was rejected.
        field: &'static str,
        /// Why.
        reason: &'static str,
    },

    /// No profile with this id exists.
    #[error("profile {0} not found")]
    ProfileNotFound(ProfileId),

    /// The operation does not apply to this kind of profile (e.g. a password on an M3U
    /// file profile).
    #[error("operation not supported for this profile kind")]
    WrongProfileKind,

    /// A keychain operation failed.
    #[error("secret store error: {0}")]
    Secret(#[from] SecretError),

    /// No parental PIN has been set.
    #[error("no parental PIN is set")]
    PinNotSet,

    /// A parental PIN is already set; use `change_pin`.
    #[error("a parental PIN is already set")]
    PinAlreadySet,

    /// The new PIN is not 4–8 ASCII digits.
    #[error("the PIN must be 4 to 8 digits")]
    InvalidPinFormat,

    /// Hashing or parsing the PIN hash failed, or the stored hash is malformed or
    /// has implausible parameters.
    #[error("parental PIN hash error: {0}")]
    PinHash(&'static str),

    /// A stored setting could not be (de)serialised.
    #[error("setting serialisation error: {0}")]
    Json(#[from] serde_json::Error),

    /// The cache could not encrypt or decrypt a value.
    #[error("cache crypto error: {0}")]
    CacheCrypto(&'static str),
}

impl StoreError {
    pub(crate) const fn invalid(field: &'static str, reason: &'static str) -> Self {
        Self::Invalid { field, reason }
    }
}

/// True if the error is an SQLite foreign-key violation (the referenced profile is gone).
pub(crate) fn is_foreign_key_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(err, _)
            if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    )
}

/// Maps a foreign-key violation on a profile-owned row to [`StoreError::ProfileNotFound`].
pub(crate) fn map_fk(profile: ProfileId) -> impl FnOnce(rusqlite::Error) -> StoreError {
    move |e| {
        if is_foreign_key_violation(&e) {
            StoreError::ProfileNotFound(profile)
        } else {
            StoreError::Db(e)
        }
    }
}

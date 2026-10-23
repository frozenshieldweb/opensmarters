// SPDX-License-Identifier: GPL-3.0-or-later
//! The server's own SQLite database (`<data>/opensmarters-web.db`).
//!
//! STRICT tables, foreign keys on, forward-only migrations (`PRAGMA user_version`; a newer
//! schema is refused), `trusted_schema=OFF`, `SQLITE_DBCONFIG_DEFENSIVE`,
//! `secure_delete=FAST`. **No secret is ever stored in clear**: provider passwords and
//! playlist / EPG addresses are in `accounts.secret` (sealed, see [`crate::crypto`]),
//! session and invite tokens are stored as keyed hashes, passwords as Argon2id PHC
//! strings. Item snapshots (names, artwork addresses) are sealed too.
//!
//! Every query that reaches provider accounts, favourites or progress is scoped by the
//! signed-in user (accounts by `user_id`; favourites and progress by an account that was
//! first looked up with the user's id).
//!
//! All calls run on the blocking pool through [`Db::call`].

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OptionalExtension, params};

/// Current schema version.
pub const SCHEMA_VERSION: i64 = 2;

const MIGRATION_1: &str = "
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    name_key TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0,
    disabled INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER
) STRICT;
CREATE TABLE sessions (
    token_hash BLOB PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX sessions_user ON sessions(user_id);
CREATE TABLE invites (
    token_hash BLOB PRIMARY KEY,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
) STRICT;
CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('xtream', 'm3u')),
    host TEXT NOT NULL,
    server TEXT,
    username TEXT,
    secret BLOB NOT NULL,
    status TEXT NOT NULL DEFAULT 'unknown',
    expires_at INTEGER,
    max_connections INTEGER,
    active_connections INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX accounts_user ON accounts(user_id);
CREATE TABLE favourites (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('channel', 'movie', 'series')),
    item TEXT NOT NULL,
    snapshot BLOB,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, kind, item)
) STRICT;
CREATE TABLE progress (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('movie', 'episode')),
    item TEXT NOT NULL,
    series_id TEXT,
    position REAL NOT NULL,
    duration REAL NOT NULL,
    finished INTEGER NOT NULL DEFAULT 0,
    snapshot BLOB,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, kind, item)
) STRICT;
CREATE INDEX progress_recent ON progress(account_id, finished, updated_at);
CREATE TABLE settings (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    value TEXT NOT NULL
) STRICT;
";

/// Version 2: per account, whether any of its addresses is unencrypted (`NULL` until it
/// is next checked: rows from version 1) and the catch-up link format that worked last.
const MIGRATION_2: &str = "
ALTER TABLE accounts ADD COLUMN insecure INTEGER CHECK (insecure IN (0, 1));
ALTER TABLE accounts ADD COLUMN timeshift_format TEXT CHECK (timeshift_format IN ('standard', 'smarters'));
";

/// Database errors (internal; the API shows a generic message).
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// SQLite error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The database was written by a newer version.
    #[error("the database schema ({0}) is newer than this server understands ({SCHEMA_VERSION})")]
    TooNew(i64),
    /// I/O around the database file.
    #[error("{0}")]
    Io(String),
    /// The blocking task failed.
    #[error("database task failed")]
    Task,
}

/// The database handle (cheap to clone).
#[derive(Debug, Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

/// The current time in Unix seconds.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Db {
    /// Opens (creating and migrating) the database in `dir`.
    ///
    /// # Errors
    ///
    /// [`DbError`].
    pub fn open(dir: &Path) -> Result<Self, DbError> {
        let path = dir.join("opensmarters-web.db");
        let conn = Connection::open(&path)?;
        restrict_permissions(&path);
        Self::init(conn)
    }

    /// An in-memory database (tests).
    ///
    /// # Errors
    ///
    /// [`DbError`].
    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, DbError> {
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        conn.pragma_update(None, "trusted_schema", "OFF")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "FAST")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL is unavailable for in-memory databases; ignore the answer.
        let _: Option<String> = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .optional()?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(DbError::TooNew(version));
        }
        if version < 1 {
            conn.execute_batch(&format!(
                "BEGIN; {MIGRATION_1} PRAGMA user_version = 1; COMMIT;"
            ))?;
        }
        if version < 2 {
            conn.execute_batch(&format!(
                "BEGIN; {MIGRATION_2} PRAGMA user_version = 2; COMMIT;"
            ))?;
        }
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` with the connection on the blocking pool.
    ///
    /// # Errors
    ///
    /// What `f` returns, or [`DbError::Task`].
    pub async fn call<T, F>(&self, f: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut guard)
        })
        .await
        .map_err(|_| DbError::Task)?
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

// ------------------------------------------------------------------------------------
// Users and sessions
// ------------------------------------------------------------------------------------

/// A user row.
#[derive(Debug, Clone)]
pub struct User {
    /// Id.
    pub id: String,
    /// Display / sign-in name.
    pub name: String,
    /// Administrator.
    pub is_admin: bool,
    /// Disabled by the administrator.
    pub disabled: bool,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub last_seen_at: Option<i64>,
}

fn user_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get("id")?,
        name: r.get("name")?,
        is_admin: r.get::<_, i64>("is_admin")? != 0,
        disabled: r.get::<_, i64>("disabled")? != 0,
        created_at: r.get("created_at")?,
        last_seen_at: r.get("last_seen_at")?,
    })
}

const USER_COLUMNS: &str = "id, name, is_admin, disabled, created_at, last_seen_at";

/// The case-insensitive uniqueness key of a name.
pub fn name_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Number of users.
///
/// # Errors
///
/// [`DbError`].
pub fn user_count(c: &Connection) -> Result<i64, DbError> {
    Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
}

/// Inserts a user; `Ok(false)` if the name is taken.
///
/// # Errors
///
/// [`DbError`].
pub fn insert_user(c: &Connection, user: &User, password_hash: &str) -> Result<bool, DbError> {
    let n = c.execute(
        "INSERT INTO users (id, name, name_key, password_hash, is_admin, disabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6) ON CONFLICT (name_key) DO NOTHING",
        params![
            user.id,
            user.name,
            name_key(&user.name),
            password_hash,
            i64::from(user.is_admin),
            user.created_at
        ],
    )?;
    Ok(n == 1)
}

/// A user and their password hash, by name (case-insensitive).
///
/// # Errors
///
/// [`DbError`].
pub fn user_by_name(c: &Connection, name: &str) -> Result<Option<(User, String)>, DbError> {
    Ok(c.query_row(
        &format!("SELECT {USER_COLUMNS}, password_hash FROM users WHERE name_key = ?1"),
        [name_key(name)],
        |r| Ok((user_from_row(r)?, r.get::<_, String>("password_hash")?)),
    )
    .optional()?)
}

/// A user and their password hash, by id.
///
/// # Errors
///
/// [`DbError`].
pub fn user_by_id(c: &Connection, id: &str) -> Result<Option<(User, String)>, DbError> {
    Ok(c.query_row(
        &format!("SELECT {USER_COLUMNS}, password_hash FROM users WHERE id = ?1"),
        [id],
        |r| Ok((user_from_row(r)?, r.get::<_, String>("password_hash")?)),
    )
    .optional()?)
}

/// Every user, oldest first.
///
/// # Errors
///
/// [`DbError`].
pub fn list_users(c: &Connection) -> Result<Vec<User>, DbError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY created_at, name_key"
    ))?;
    let rows = stmt.query_map([], user_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Replaces a password hash.
///
/// # Errors
///
/// [`DbError`].
pub fn set_password_hash(c: &Connection, user_id: &str, hash: &str) -> Result<(), DbError> {
    c.execute(
        "UPDATE users SET password_hash = ?2 WHERE id = ?1",
        params![user_id, hash],
    )?;
    Ok(())
}

/// Sets `disabled`; `Ok(false)` if there is no such user.
///
/// # Errors
///
/// [`DbError`].
pub fn set_disabled(c: &Connection, user_id: &str, disabled: bool) -> Result<bool, DbError> {
    Ok(c.execute(
        "UPDATE users SET disabled = ?2 WHERE id = ?1",
        params![user_id, i64::from(disabled)],
    )? == 1)
}

/// Deletes a user (accounts, favourites, progress, sessions and settings go with it).
///
/// # Errors
///
/// [`DbError`].
pub fn delete_user(c: &Connection, user_id: &str) -> Result<bool, DbError> {
    Ok(c.execute("DELETE FROM users WHERE id = ?1", [user_id])? == 1)
}

/// Stores a new session.
///
/// # Errors
///
/// [`DbError`].
pub fn insert_session(
    c: &Connection,
    token_hash: &[u8; 32],
    user_id: &str,
    now: i64,
    expires_at: i64,
) -> Result<(), DbError> {
    c.execute(
        "INSERT INTO sessions (token_hash, user_id, created_at, last_used_at, expires_at)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        params![token_hash.as_slice(), user_id, now, expires_at],
    )?;
    Ok(())
}

/// A live session and its (enabled) user.
#[derive(Debug, Clone)]
pub struct SessionUser {
    /// The user.
    pub user: User,
    /// Last use (Unix seconds).
    pub last_used_at: i64,
}

/// Looks up a valid session (not expired, user not disabled).
///
/// # Errors
///
/// [`DbError`].
pub fn session_user(
    c: &Connection,
    token_hash: &[u8; 32],
    now: i64,
) -> Result<Option<SessionUser>, DbError> {
    Ok(c.query_row(
        "SELECT u.id, u.name, u.is_admin, u.disabled, u.created_at, u.last_seen_at, s.last_used_at
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND u.disabled = 0",
        params![token_hash.as_slice(), now],
        |r| {
            Ok(SessionUser {
                user: user_from_row(r)?,
                last_used_at: r.get("last_used_at")?,
            })
        },
    )
    .optional()?)
}

/// Slides a session's expiry and records the user as seen.
///
/// # Errors
///
/// [`DbError`].
pub fn touch_session(
    c: &Connection,
    token_hash: &[u8; 32],
    user_id: &str,
    now: i64,
    expires_at: i64,
) -> Result<(), DbError> {
    c.execute(
        "UPDATE sessions SET last_used_at = ?2, expires_at = ?3 WHERE token_hash = ?1",
        params![token_hash.as_slice(), now, expires_at],
    )?;
    c.execute(
        "UPDATE users SET last_seen_at = ?2 WHERE id = ?1",
        params![user_id, now],
    )?;
    Ok(())
}

/// Ends one session.
///
/// # Errors
///
/// [`DbError`].
pub fn delete_session(c: &Connection, token_hash: &[u8; 32]) -> Result<(), DbError> {
    c.execute(
        "DELETE FROM sessions WHERE token_hash = ?1",
        [token_hash.as_slice()],
    )?;
    Ok(())
}

/// Ends every session of a user.
///
/// # Errors
///
/// [`DbError`].
pub fn delete_user_sessions(c: &Connection, user_id: &str) -> Result<(), DbError> {
    c.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    Ok(())
}

/// Removes expired sessions and invites.
///
/// # Errors
///
/// [`DbError`].
pub fn purge_expired(c: &Connection, now: i64) -> Result<(), DbError> {
    c.execute("DELETE FROM sessions WHERE expires_at <= ?1", [now])?;
    c.execute(
        "DELETE FROM invites WHERE expires_at <= ?1 OR used_at IS NOT NULL",
        [now - 7 * 24 * 3600],
    )?;
    Ok(())
}

// ------------------------------------------------------------------------------------
// Invites
// ------------------------------------------------------------------------------------

/// Stores an invite.
///
/// # Errors
///
/// [`DbError`].
pub fn insert_invite(
    c: &Connection,
    token_hash: &[u8; 32],
    created_by: &str,
    now: i64,
    expires_at: i64,
) -> Result<(), DbError> {
    c.execute(
        "INSERT INTO invites (token_hash, created_by, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![token_hash.as_slice(), created_by, now, expires_at],
    )?;
    Ok(())
}

/// Marks an unused, unexpired invite as used; `Ok(false)` when there is no such invite.
///
/// # Errors
///
/// [`DbError`].
pub fn use_invite(c: &Connection, token_hash: &[u8; 32], now: i64) -> Result<bool, DbError> {
    Ok(c.execute(
        "UPDATE invites SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
        params![token_hash.as_slice(), now],
    )? == 1)
}

// ------------------------------------------------------------------------------------
// Provider accounts
// ------------------------------------------------------------------------------------

/// A provider account row (the secret stays sealed).
#[derive(Debug, Clone)]
pub struct AccountRow {
    /// Id.
    pub id: String,
    /// Owner.
    pub user_id: String,
    /// Display name.
    pub name: String,
    /// `xtream` or `m3u`.
    pub kind: String,
    /// Host name only.
    pub host: String,
    /// Xtream server URL (no credentials).
    pub server: Option<String>,
    /// Xtream user name.
    pub username: Option<String>,
    /// Sealed secret JSON.
    pub secret: Vec<u8>,
    /// `active`, `expired`, `banned`, `disabled` or `unknown`.
    pub status: String,
    /// Unix seconds.
    pub expires_at: Option<i64>,
    /// Provider limit.
    pub max_connections: Option<i64>,
    /// Provider's count at the last check.
    pub active_connections: Option<i64>,
    /// Unix seconds.
    pub created_at: i64,
    /// An address of this account (panel, streams, playlist, guide) is plain `http`;
    /// `None` until it is checked (rows from schema version 1).
    pub insecure: Option<bool>,
    /// The catch-up link format that worked last (`standard` or `smarters`).
    pub timeshift_format: Option<String>,
}

const ACCOUNT_COLUMNS: &str = "id, user_id, name, kind, host, server, username, secret, status, \
     expires_at, max_connections, active_connections, created_at, insecure, timeshift_format";

fn account_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        id: r.get("id")?,
        user_id: r.get("user_id")?,
        name: r.get("name")?,
        kind: r.get("kind")?,
        host: r.get("host")?,
        server: r.get("server")?,
        username: r.get("username")?,
        secret: r.get("secret")?,
        status: r.get("status")?,
        expires_at: r.get("expires_at")?,
        max_connections: r.get("max_connections")?,
        active_connections: r.get("active_connections")?,
        created_at: r.get("created_at")?,
        insecure: r.get("insecure")?,
        timeshift_format: r.get("timeshift_format")?,
    })
}

/// A user's accounts, oldest first.
///
/// # Errors
///
/// [`DbError`].
pub fn list_accounts(c: &Connection, user_id: &str) -> Result<Vec<AccountRow>, DbError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE user_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([user_id], account_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One account **of this user**.
///
/// # Errors
///
/// [`DbError`].
pub fn get_account(
    c: &Connection,
    user_id: &str,
    account_id: &str,
) -> Result<Option<AccountRow>, DbError> {
    Ok(c.query_row(
        &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1 AND user_id = ?2"),
        params![account_id, user_id],
        account_from_row,
    )
    .optional()?)
}

/// Every account of every user (stream accounting, demo set-up).
///
/// # Errors
///
/// [`DbError`].
pub fn all_accounts(c: &Connection) -> Result<Vec<AccountRow>, DbError> {
    let mut stmt = c.prepare(&format!("SELECT {ACCOUNT_COLUMNS} FROM accounts"))?;
    let rows = stmt.query_map([], account_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Number of accounts of a user.
///
/// # Errors
///
/// [`DbError`].
pub fn account_count(c: &Connection, user_id: &str) -> Result<i64, DbError> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM accounts WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )?)
}

/// Inserts or replaces an account row (scoped by its `user_id`).
///
/// # Errors
///
/// [`DbError`].
pub fn upsert_account(c: &Connection, a: &AccountRow) -> Result<(), DbError> {
    let now = now();
    c.execute(
        "INSERT INTO accounts (id, user_id, name, kind, host, server, username, secret, status,
                               expires_at, max_connections, active_connections, created_at, updated_at,
                               insecure, timeshift_format)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT (id) DO UPDATE SET
             name = excluded.name, kind = excluded.kind, host = excluded.host,
             server = excluded.server, username = excluded.username, secret = excluded.secret,
             status = excluded.status, expires_at = excluded.expires_at,
             max_connections = excluded.max_connections,
             active_connections = excluded.active_connections, updated_at = excluded.updated_at,
             insecure = excluded.insecure, timeshift_format = excluded.timeshift_format
         WHERE accounts.user_id = excluded.user_id",
        params![
            a.id,
            a.user_id,
            a.name,
            a.kind,
            a.host,
            a.server,
            a.username,
            a.secret,
            a.status,
            a.expires_at,
            a.max_connections,
            a.active_connections,
            a.created_at,
            now,
            a.insecure,
            a.timeshift_format,
        ],
    )?;
    Ok(())
}

/// Records whether an account **of this user** has an unencrypted address.
///
/// # Errors
///
/// [`DbError`].
pub fn set_account_insecure(
    c: &Connection,
    user_id: &str,
    account_id: &str,
    insecure: bool,
) -> Result<(), DbError> {
    c.execute(
        "UPDATE accounts SET insecure = ?3 WHERE id = ?1 AND user_id = ?2",
        params![account_id, user_id, insecure],
    )?;
    Ok(())
}

/// Remembers the catch-up link format that worked for an account **of this user**
/// (`standard` or `smarters`).
///
/// # Errors
///
/// [`DbError`].
pub fn set_timeshift_format(
    c: &Connection,
    user_id: &str,
    account_id: &str,
    format: &str,
) -> Result<(), DbError> {
    c.execute(
        "UPDATE accounts SET timeshift_format = ?3 WHERE id = ?1 AND user_id = ?2",
        params![account_id, user_id, format],
    )?;
    Ok(())
}

/// Deletes an account **of this user** (favourites and progress go with it).
///
/// # Errors
///
/// [`DbError`].
pub fn delete_account(c: &Connection, user_id: &str, account_id: &str) -> Result<bool, DbError> {
    Ok(c.execute(
        "DELETE FROM accounts WHERE id = ?1 AND user_id = ?2",
        params![account_id, user_id],
    )? == 1)
}

// ------------------------------------------------------------------------------------
// Favourites and progress (account ids come from a user-scoped lookup)
// ------------------------------------------------------------------------------------

/// A favourite.
#[derive(Debug, Clone)]
pub struct Favourite {
    /// `channel`, `movie` or `series`.
    pub kind: String,
    /// Item id / key.
    pub item: String,
    /// Sealed snapshot.
    pub snapshot: Option<Vec<u8>>,
}

/// An account's favourites, in the order they were added.
///
/// # Errors
///
/// [`DbError`].
pub fn list_favourites(c: &Connection, account_id: &str) -> Result<Vec<Favourite>, DbError> {
    let mut stmt = c.prepare(
        "SELECT kind, item, snapshot FROM favourites WHERE account_id = ?1 ORDER BY created_at, rowid",
    )?;
    let rows = stmt.query_map([account_id], |r| {
        Ok(Favourite {
            kind: r.get(0)?,
            item: r.get(1)?,
            snapshot: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Number of favourites of an account.
///
/// # Errors
///
/// [`DbError`].
pub fn favourite_count(c: &Connection, account_id: &str) -> Result<i64, DbError> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM favourites WHERE account_id = ?1",
        [account_id],
        |r| r.get(0),
    )?)
}

/// Adds (or refreshes the snapshot of) a favourite.
///
/// # Errors
///
/// [`DbError`].
pub fn put_favourite(
    c: &Connection,
    account_id: &str,
    kind: &str,
    item: &str,
    snapshot: Option<&[u8]>,
) -> Result<(), DbError> {
    c.execute(
        "INSERT INTO favourites (account_id, kind, item, snapshot, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (account_id, kind, item) DO UPDATE SET snapshot = excluded.snapshot",
        params![account_id, kind, item, snapshot, now()],
    )?;
    Ok(())
}

/// Removes a favourite.
///
/// # Errors
///
/// [`DbError`].
pub fn delete_favourite(
    c: &Connection,
    account_id: &str,
    kind: &str,
    item: &str,
) -> Result<(), DbError> {
    c.execute(
        "DELETE FROM favourites WHERE account_id = ?1 AND kind = ?2 AND item = ?3",
        params![account_id, kind, item],
    )?;
    Ok(())
}

/// A resume point.
#[derive(Debug, Clone)]
pub struct Progress {
    /// `movie` or `episode`.
    pub kind: String,
    /// Item id.
    pub item: String,
    /// Series of an episode.
    pub series_id: Option<String>,
    /// Seconds.
    pub position: f64,
    /// Seconds.
    pub duration: f64,
    /// Watched to the end (≥ 95 %).
    pub finished: bool,
    /// Sealed snapshot (name, artwork).
    pub snapshot: Option<Vec<u8>>,
    /// Unix seconds.
    pub updated_at: i64,
}

fn progress_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Progress> {
    Ok(Progress {
        kind: r.get(0)?,
        item: r.get(1)?,
        series_id: r.get(2)?,
        position: r.get(3)?,
        duration: r.get(4)?,
        finished: r.get::<_, i64>(5)? != 0,
        snapshot: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

const PROGRESS_COLUMNS: &str =
    "kind, item, series_id, position, duration, finished, snapshot, updated_at";

/// Saves a resume point.
///
/// # Errors
///
/// [`DbError`].
pub fn put_progress(c: &Connection, account_id: &str, p: &Progress) -> Result<(), DbError> {
    c.execute(
        "INSERT INTO progress (account_id, kind, item, series_id, position, duration, finished, snapshot, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (account_id, kind, item) DO UPDATE SET
             series_id = excluded.series_id, position = excluded.position,
             duration = excluded.duration, finished = excluded.finished,
             snapshot = COALESCE(excluded.snapshot, progress.snapshot),
             updated_at = excluded.updated_at",
        params![
            account_id,
            p.kind,
            p.item,
            p.series_id,
            p.position,
            p.duration,
            i64::from(p.finished),
            p.snapshot,
            p.updated_at
        ],
    )?;
    // Keep the table bounded per account.
    c.execute(
        "DELETE FROM progress WHERE account_id = ?1 AND rowid NOT IN (
             SELECT rowid FROM progress WHERE account_id = ?1 ORDER BY updated_at DESC LIMIT 5000)",
        [account_id],
    )?;
    Ok(())
}

/// One resume point.
///
/// # Errors
///
/// [`DbError`].
pub fn get_progress(
    c: &Connection,
    account_id: &str,
    kind: &str,
    item: &str,
) -> Result<Option<Progress>, DbError> {
    Ok(c.query_row(
        &format!(
            "SELECT {PROGRESS_COLUMNS} FROM progress WHERE account_id = ?1 AND kind = ?2 AND item = ?3"
        ),
        params![account_id, kind, item],
        progress_from_row,
    )
    .optional()?)
}

/// Resume points of a series' episodes.
///
/// # Errors
///
/// [`DbError`].
pub fn series_progress(
    c: &Connection,
    account_id: &str,
    series_id: &str,
) -> Result<Vec<Progress>, DbError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {PROGRESS_COLUMNS} FROM progress WHERE account_id = ?1 AND kind = 'episode' AND series_id = ?2"
    ))?;
    let rows = stmt.query_map(params![account_id, series_id], progress_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Unfinished resume points, newest first.
///
/// # Errors
///
/// [`DbError`].
pub fn continue_watching(
    c: &Connection,
    account_id: &str,
    limit: i64,
) -> Result<Vec<Progress>, DbError> {
    let mut stmt = c.prepare(&format!(
        "SELECT {PROGRESS_COLUMNS} FROM progress WHERE account_id = ?1 AND finished = 0
         ORDER BY updated_at DESC, rowid DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![account_id, limit], progress_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

// ------------------------------------------------------------------------------------
// Settings
// ------------------------------------------------------------------------------------

/// A user's settings JSON.
///
/// # Errors
///
/// [`DbError`].
pub fn get_settings(c: &Connection, user_id: &str) -> Result<Option<String>, DbError> {
    Ok(c.query_row(
        "SELECT value FROM settings WHERE user_id = ?1",
        [user_id],
        |r| r.get(0),
    )
    .optional()?)
}

/// Saves a user's settings JSON.
///
/// # Errors
///
/// [`DbError`].
pub fn put_settings(c: &Connection, user_id: &str, value: &str) -> Result<(), DbError> {
    c.execute(
        "INSERT INTO settings (user_id, value) VALUES (?1, ?2)
         ON CONFLICT (user_id) DO UPDATE SET value = excluded.value",
        params![user_id, value],
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn migrations_and_scoping() {
        let db = Db::open_in_memory().unwrap();
        let c = db.conn.lock().unwrap();
        let mk = |id: &str, name: &str| User {
            id: id.into(),
            name: name.into(),
            is_admin: false,
            disabled: false,
            created_at: 1,
            last_seen_at: None,
        };
        assert!(insert_user(&c, &mk("u1", "Alice"), "h").unwrap());
        assert!(
            !insert_user(&c, &mk("u2", "alice"), "h").unwrap(),
            "names are case-insensitive"
        );
        assert!(insert_user(&c, &mk("u2", "bob"), "h").unwrap());
        let a = AccountRow {
            id: "a1".into(),
            user_id: "u1".into(),
            name: "P".into(),
            kind: "xtream".into(),
            host: "h".into(),
            server: Some("http://h".into()),
            username: Some("x".into()),
            secret: vec![1, 2, 3],
            status: "active".into(),
            expires_at: None,
            max_connections: Some(1),
            active_connections: None,
            created_at: 1,
            insecure: Some(true),
            timeshift_format: None,
        };
        upsert_account(&c, &a).unwrap();
        assert!(get_account(&c, "u1", "a1").unwrap().is_some());
        // The flags are scoped by the owner too.
        set_timeshift_format(&c, "u2", "a1", "smarters").unwrap();
        set_account_insecure(&c, "u2", "a1", false).unwrap();
        let got = get_account(&c, "u1", "a1").unwrap().unwrap();
        assert_eq!((got.insecure, got.timeshift_format), (Some(true), None));
        set_timeshift_format(&c, "u1", "a1", "smarters").unwrap();
        set_account_insecure(&c, "u1", "a1", false).unwrap();
        let got = get_account(&c, "u1", "a1").unwrap().unwrap();
        assert_eq!(
            (got.insecure, got.timeshift_format.as_deref()),
            (Some(false), Some("smarters"))
        );
        assert!(
            set_timeshift_format(&c, "u1", "a1", "other").is_err(),
            "only the two known formats"
        );
        assert!(get_account(&c, "u2", "a1").unwrap().is_none());
        // Another user can't take the row over through an upsert with the same id.
        upsert_account(
            &c,
            &AccountRow {
                user_id: "u2".into(),
                name: "Hijack".into(),
                ..a.clone()
            },
        )
        .unwrap();
        assert_eq!(get_account(&c, "u1", "a1").unwrap().unwrap().name, "P");
        assert!(!delete_account(&c, "u2", "a1").unwrap());
        put_favourite(&c, "a1", "channel", "1001", None).unwrap();
        assert_eq!(favourite_count(&c, "a1").unwrap(), 1);
        assert!(delete_user(&c, "u1").unwrap());
        assert_eq!(favourite_count(&c, "a1").unwrap(), 0, "cascade");
    }

    #[test]
    fn version_1_databases_gain_the_account_flags() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "BEGIN; {MIGRATION_1} PRAGMA user_version = 1; COMMIT;"
        ))
        .unwrap();
        conn.execute_batch(
            "INSERT INTO users (id, name, name_key, password_hash, created_at) VALUES ('u1', 'A', 'a', 'h', 1);
             INSERT INTO accounts (id, user_id, name, kind, host, secret, created_at, updated_at)
                 VALUES ('a1', 'u1', 'P', 'm3u', 'h', x'00', 1, 1);",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        let c = db.conn.lock().unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let a = get_account(&c, "u1", "a1").unwrap().unwrap();
        assert_eq!(
            (a.insecure, a.timeshift_format),
            (None, None),
            "not known yet"
        );
    }
}

//! Opening the SQLite database, connection hardening and schema migrations.
//!
//! Migrations are forward-only SQL scripts applied in order; `PRAGMA user_version` records
//! how many have been applied. Each runs in its own `BEGIN IMMEDIATE` transaction together
//! with the `user_version` bump, so a crash mid-migration leaves the previous version
//! intact and two app instances cannot migrate concurrently. A database whose version is
//! newer than this build is refused ([`StoreError::SchemaTooNew`]) rather than modified.
//!
//! To change the schema, **append** a new script to [`MIGRATIONS`] (never edit a shipped
//! one) and add a fixture for the previous version to the migration tests.

use std::path::Path;
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::error::{Result, StoreError};

/// The ordered list of migrations. Index `i` upgrades version `i` to `i + 1`.
pub(crate) const MIGRATIONS: &[&str] = &[MIGRATION_1, MIGRATION_2];

/// Schema version written by this build.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Version 1: initial schema.
///
/// Conventions: timestamps are INTEGER milliseconds since the Unix epoch (UTC); profile ids
/// are hyphenated lowercase UUID text; all tables are STRICT; every profile-owned row
/// cascades on profile deletion. **No column ever holds a password, an M3U URL, an EPG
/// URL, an API key or a PIN.**
const MIGRATION_1: &str = r#"
CREATE TABLE profiles (
    id                TEXT    PRIMARY KEY NOT NULL,
    kind              TEXT    NOT NULL CHECK (kind IN ('xtream', 'm3u_url', 'm3u_file')),
    name              TEXT    NOT NULL,
    server_url        TEXT,                -- Xtream only; credential-free (validated)
    username          TEXT,                -- Xtream only
    file_path         TEXT,                -- M3U file only
    sort_order        INTEGER NOT NULL DEFAULT 0,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    last_used_at      INTEGER,
    live_container    TEXT    NOT NULL DEFAULT 'ts' CHECK (live_container IN ('ts', 'm3u8')),
    timeshift_format  TEXT    NOT NULL DEFAULT 'standard'
                              CHECK (timeshift_format IN ('standard', 'smarters_compat')),
    user_agent        TEXT    NOT NULL DEFAULT 'default'
                              CHECK (user_agent IN ('default', 'smarters_compat', 'custom')),
    user_agent_custom TEXT,
    has_epg_override  INTEGER NOT NULL DEFAULT 0 CHECK (has_epg_override IN (0, 1)),
    CHECK ((kind = 'xtream') = (server_url IS NOT NULL AND username IS NOT NULL)),
    CHECK ((kind = 'm3u_file') = (file_path IS NOT NULL)),
    CHECK ((user_agent = 'custom') = (user_agent_custom IS NOT NULL))
) STRICT;

CREATE INDEX profiles_order ON profiles (sort_order, created_at);

CREATE TABLE favourites (
    profile_id  TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    kind        TEXT    NOT NULL CHECK (kind IN ('live', 'movie', 'series')),
    item_id     TEXT    NOT NULL,
    position    INTEGER NOT NULL,
    added_at    INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    icon_url    TEXT,
    category_id TEXT,
    PRIMARY KEY (profile_id, kind, item_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX favourites_order ON favourites (profile_id, kind, position);

CREATE TABLE playback (
    profile_id          TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    kind                TEXT    NOT NULL CHECK (kind IN ('movie', 'episode', 'catchup')),
    item_id             TEXT    NOT NULL,
    position_secs       REAL    NOT NULL CHECK (position_secs >= 0),
    duration_secs       REAL    CHECK (duration_secs IS NULL OR duration_secs > 0),
    finished            INTEGER NOT NULL DEFAULT 0 CHECK (finished IN (0, 1)),
    ever_finished       INTEGER NOT NULL DEFAULT 0 CHECK (ever_finished IN (0, 1)),
    updated_at          INTEGER NOT NULL,
    seq                 INTEGER NOT NULL,  -- monotonic recency counter (ties in updated_at)
    title               TEXT    NOT NULL,
    poster_url          TEXT,
    series_id           TEXT,
    season              INTEGER,
    episode             INTEGER,
    container_extension TEXT,
    PRIMARY KEY (profile_id, kind, item_id)
) STRICT;

CREATE INDEX playback_recent ON playback (profile_id, seq DESC);
CREATE INDEX playback_series ON playback (profile_id, series_id) WHERE series_id IS NOT NULL;

CREATE TABLE recent_live (
    profile_id   TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    stream_id    TEXT    NOT NULL,
    last_watched INTEGER NOT NULL,
    seq          INTEGER NOT NULL,
    name         TEXT    NOT NULL,
    icon_url     TEXT,
    category_id  TEXT,
    PRIMARY KEY (profile_id, stream_id)
) STRICT;

CREATE INDEX recent_live_order ON recent_live (profile_id, seq DESC);

-- Encrypted cache. key_id = keyed BLAKE2b(scope, key): cache keys are not stored in clear.
-- value = XChaCha20-Poly1305(nonce || ciphertext); the key lives in the OS keychain.
CREATE TABLE cache (
    key_id      BLOB    PRIMARY KEY NOT NULL,
    profile_id  TEXT    REFERENCES profiles (id) ON DELETE CASCADE,  -- NULL = global
    value       BLOB    NOT NULL,
    size        INTEGER NOT NULL,
    fetched_at  INTEGER NOT NULL,
    ttl_ms      INTEGER NOT NULL CHECK (ttl_ms >= 0),
    last_access INTEGER NOT NULL
) STRICT;

CREATE INDEX cache_profile ON cache (profile_id);
CREATE INDEX cache_lru ON cache (last_access);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL              -- JSON
) STRICT;

-- Single-row table holding the Argon2id PHC hash and the persisted rate-limit state.
CREATE TABLE parental_pin (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    phc             TEXT,
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    locked_until    INTEGER,
    lockout_ms      INTEGER,
    updated_at      INTEGER NOT NULL
) STRICT;

INSERT INTO parental_pin (id, phc, failed_attempts, updated_at) VALUES (1, NULL, 0, 0);

CREATE TABLE parental_keywords (
    keyword TEXT PRIMARY KEY NOT NULL
) STRICT;

INSERT INTO parental_keywords (keyword) VALUES ('adult'), ('xxx'), ('porn'), ('18+'), ('erotic');

CREATE TABLE category_locks (
    profile_id  TEXT NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('live', 'movie', 'series')),
    category_id TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('locked', 'unlocked')),
    PRIMARY KEY (profile_id, kind, category_id)
) STRICT, WITHOUT ROWID;
"#;

/// Version 2: recordings / downloads and programme reminders (Phase 7).
///
/// `recordings.file_name` is a bare file name relative to the user's recordings folder
/// (validated: no separators, no `..`); the folder itself is an app setting. Rows never
/// hold a stream URL: the app rebuilds it from the ids and the profile's credentials.
const MIGRATION_2: &str = r#"
CREATE TABLE recordings (
    id                  INTEGER PRIMARY KEY,
    profile_id          TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    kind                TEXT    NOT NULL CHECK (kind IN ('movie', 'episode', 'live', 'clip')),
    state               TEXT    NOT NULL CHECK (state IN ('scheduled', 'queued', 'active',
                                                          'paused', 'done', 'failed',
                                                          'cancelled')),
    item_id             TEXT    NOT NULL,  -- movie stream_id, episode id or live stream_id
    series_id           TEXT,
    season              INTEGER,
    episode             INTEGER,
    container_extension TEXT,
    title               TEXT    NOT NULL,
    subtitle            TEXT,              -- channel or series name
    icon_url            TEXT,
    start_at            INTEGER,           -- live: programme start (ms)
    end_at              INTEGER,           -- live: programme end (ms)
    file_name           TEXT,              -- relative to the recordings folder
    bytes_done          INTEGER NOT NULL DEFAULT 0 CHECK (bytes_done >= 0),
    bytes_total         INTEGER CHECK (bytes_total IS NULL OR bytes_total >= 0),
    duration_secs       REAL    CHECK (duration_secs IS NULL OR duration_secs >= 0),
    position_secs       REAL    CHECK (position_secs IS NULL OR position_secs >= 0),
    error               TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    CHECK (start_at IS NULL OR end_at IS NULL OR end_at > start_at),
    CHECK (state <> 'scheduled' OR (start_at IS NOT NULL AND end_at IS NOT NULL))
) STRICT;

CREATE INDEX recordings_profile ON recordings (profile_id, state, start_at);

CREATE TABLE reminders (
    id           INTEGER PRIMARY KEY,
    profile_id   TEXT    NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    stream_id    TEXT    NOT NULL,         -- live channel key
    start_at     INTEGER NOT NULL,         -- programme start (ms)
    end_at       INTEGER NOT NULL,
    title        TEXT    NOT NULL,
    channel_name TEXT    NOT NULL,
    icon_url     TEXT,
    action       TEXT    NOT NULL DEFAULT 'notify' CHECK (action IN ('notify', 'switch')),
    fired_at     INTEGER,
    created_at   INTEGER NOT NULL,
    CHECK (end_at > start_at),
    UNIQUE (profile_id, stream_id, start_at)
) STRICT;

CREATE INDEX reminders_due ON reminders (profile_id, start_at);
"#;

/// Opens (creating if needed) a database file and prepares the connection.
pub(crate) fn open_file(path: &Path, busy_timeout: Duration) -> Result<Connection> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        create_private_dir(dir)?;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    restrict_file_permissions(path)?;
    configure(&conn, busy_timeout, true)?;
    Ok(conn)
}

/// Opens a private in-memory database (tests, ephemeral sessions).
pub(crate) fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn, Duration::from_secs(1), false)?;
    Ok(conn)
}

/// Connection hardening and performance settings. Must run outside a transaction.
fn configure(conn: &Connection, busy_timeout: Duration, wal: bool) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    // Refuse schema tricks from a tampered file: no writable_schema, no unsafe functions
    // in triggers/views.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    // Only takes effect on a brand-new file (before the first table exists).
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    if wal {
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            tracing::warn!(%mode, "SQLite did not switch to WAL mode");
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Truncate the WAL back to 16 MiB after checkpoints (large cache writes grow it).
        conn.pragma_update(None, "journal_size_limit", 16 * 1024 * 1024)?;
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    // Overwrite deleted content where it costs no extra I/O (watch history privacy).
    conn.pragma_update(None, "secure_delete", "FAST")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

/// Current `PRAGMA user_version`.
pub(crate) fn user_version(conn: &Connection) -> Result<u32> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    u32::try_from(v).map_err(|_| StoreError::SchemaTooNew {
        found: u32::MAX,
        supported: SCHEMA_VERSION,
    })
}

/// Applies all pending migrations.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    migrate_to(conn, SCHEMA_VERSION)
}

/// Applies migrations up to and including version `target`.
pub(crate) fn migrate_to(conn: &mut Connection, target: u32) -> Result<()> {
    let found = user_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().take(target as usize) {
        let to_version = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Re-check inside the write lock: another process may have migrated meanwhile.
        if user_version(&tx)? >= to_version {
            continue;
        }
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", to_version)?;
        tx.commit()?;
        tracing::info!(version = to_version, "applied database migration");
    }
    Ok(())
}

fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// On Unix, make the database readable by the owner only (SQLite gives -wal/-shm the same
/// mode). On Windows the per-user AppData ACL already restricts access to the user.
fn restrict_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Normalised schema (tables, indexes, triggers) for comparisons.
    fn schema(conn: &Connection) -> Vec<(String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT type, name, COALESCE(sql, '') FROM sqlite_schema \
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Inserts representative rows valid at `version`, so upgrades are tested with data.
    fn seed_fixture(conn: &Connection, version: u32) {
        match version {
            0 => {}
            1 => {
                conn.execute_batch(
                    "INSERT INTO profiles (id, kind, name, server_url, username, created_at, updated_at)
                     VALUES ('00000000-0000-4000-8000-000000000001', 'xtream', 'Fixture',
                             'http://panel.example.invalid:8080', 'fixture_user', 1, 1);
                     INSERT INTO favourites (profile_id, kind, item_id, position, added_at, name)
                     VALUES ('00000000-0000-4000-8000-000000000001', 'live', '42', 0, 1, 'Ch 42');",
                )
                .unwrap();
            }
            2 => {
                seed_fixture(conn, 1);
                conn.execute_batch(
                    "INSERT INTO recordings (profile_id, kind, state, item_id, title, start_at,
                                             end_at, created_at, updated_at)
                     VALUES ('00000000-0000-4000-8000-000000000001', 'live', 'scheduled', '42',
                             'News', 1000, 2000, 1, 1);
                     INSERT INTO reminders (profile_id, stream_id, start_at, end_at, title,
                                            channel_name, created_at)
                     VALUES ('00000000-0000-4000-8000-000000000001', '42', 1000, 2000, 'News',
                             'Ch 42', 1);",
                )
                .unwrap();
            }
            v => panic!("add a fixture for schema version {v}"),
        }
    }

    #[test]
    fn migrates_from_empty() {
        let mut conn = open_memory().unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        // Idempotent.
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        let fk: Vec<String> = conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(fk.is_empty());
    }

    #[test]
    fn migrates_from_every_prior_version_with_data() {
        let mut fresh = open_memory().unwrap();
        migrate(&mut fresh).unwrap();
        let expected = schema(&fresh);

        for start in 0..SCHEMA_VERSION {
            let mut conn = open_memory().unwrap();
            migrate_to(&mut conn, start).unwrap();
            assert_eq!(user_version(&conn).unwrap(), start);
            seed_fixture(&conn, start);
            migrate(&mut conn).unwrap();
            assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
            assert_eq!(
                schema(&conn),
                expected,
                "schema after upgrading from v{start}"
            );
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
            if start >= 1 {
                let n: i64 = conn
                    .query_row("SELECT COUNT(*) FROM favourites", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(n, 1, "data survives the upgrade from v{start}");
            }
        }
    }

    #[test]
    fn refuses_newer_schema() {
        let mut conn = open_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        match migrate(&mut conn) {
            Err(StoreError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
    }

    #[test]
    fn file_database_uses_wal_and_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("t.db");
        let mut conn = open_file(&path, Duration::from_secs(1)).unwrap();
        migrate(&mut conn).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let av: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
            .unwrap();
        assert_eq!(av, 2, "incremental auto-vacuum on new files");
    }
}

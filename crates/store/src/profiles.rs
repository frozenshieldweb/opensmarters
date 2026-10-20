//! Profiles (accounts): Xtream Codes logins, M3U URLs and local M3U files.
//!
//! The database holds only non-secret fields. Passwords, M3U URLs and EPG URL overrides
//! go to the [`SecretStore`](crate::SecretStore) under keys derived from the profile id,
//! and are deleted with the profile.

use std::path::PathBuf;

use rusqlite::{OptionalExtension, Row, params};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::Store;
use crate::error::{Result, StoreError};
use crate::ids::{ProfileId, sql_text_enum};
use crate::secrets::{SecretError, SecretKey};
use crate::settings::LAST_PROFILE;
use crate::text::{MAX_PROFILE_NAME_CHARS, MAX_URL_BYTES, has_control, validate_name};
use crate::time::Timestamp;

const MAX_USERNAME_CHARS: usize = 256;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_USER_AGENT_CHARS: usize = 512;

/// What kind of source a profile reads from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// Xtream Codes API login (server URL + username + password).
    Xtream,
    /// M3U playlist downloaded from a URL (the URL is a secret).
    M3uUrl,
    /// Local M3U file.
    M3uFile,
}

impl ProfileKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Xtream => "xtream",
            Self::M3uUrl => "m3u_url",
            Self::M3uFile => "m3u_file",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "xtream" => Some(Self::Xtream),
            "m3u_url" => Some(Self::M3uUrl),
            "m3u_file" => Some(Self::M3uFile),
            _ => None,
        }
    }
}
sql_text_enum!(ProfileKind);

/// The non-secret description of where a profile's content comes from.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileSource {
    /// Xtream Codes. The password is in the keychain ([`Profiles::password`]).
    Xtream {
        /// Panel URL as entered (normalised by the caller), without credentials.
        server_url: String,
        /// Account username.
        username: String,
    },
    /// M3U from a URL. The URL is in the keychain ([`Profiles::m3u_url`]).
    M3uUrl,
    /// Local M3U file.
    M3uFile {
        /// Path to the file.
        path: PathBuf,
    },
}

impl ProfileSource {
    /// The kind of this source.
    pub const fn kind(&self) -> ProfileKind {
        match self {
            Self::Xtream { .. } => ProfileKind::Xtream,
            Self::M3uUrl => ProfileKind::M3uUrl,
            Self::M3uFile { .. } => ProfileKind::M3uFile,
        }
    }
}

/// Source of a profile being created, including its secret.
#[derive(Clone, Debug)]
pub enum NewProfileSource {
    /// Xtream Codes login.
    Xtream {
        /// Panel URL: `http(s)://host[:port][/path]`, no credentials, query or fragment.
        server_url: String,
        /// Account username.
        username: String,
        /// Account password (goes to the keychain).
        password: SecretString,
    },
    /// M3U playlist URL (goes to the keychain).
    M3uUrl {
        /// The playlist URL.
        url: SecretString,
    },
    /// Local M3U file.
    M3uFile {
        /// Path to the file.
        path: PathBuf,
    },
}

/// Input for [`Profiles::create`].
#[derive(Clone, Debug)]
pub struct NewProfile {
    /// Display name (1–128 chars, trimmed).
    pub name: String,
    /// Where the content comes from.
    pub source: NewProfileSource,
    /// Per-profile playback settings.
    pub settings: ProfileSettings,
}

/// Container for live streams (and timeshift).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveContainer {
    /// MPEG-TS (`.ts`), the Xtream default.
    #[default]
    Ts,
    /// HLS (`.m3u8`).
    M3u8,
}

impl LiveContainer {
    /// File extension without the dot (`"ts"` / `"m3u8"`).
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Ts => "ts",
            Self::M3u8 => "m3u8",
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.extension()
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "ts" => Some(Self::Ts),
            "m3u8" => Some(Self::M3u8),
            _ => None,
        }
    }
}
sql_text_enum!(LiveContainer);

/// Timeshift (catch-up) URL time format. See `docs/PROTOCOL.md` → Catch-up.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeshiftFormat {
    /// `YYYY-MM-DD:HH-MM` with whole minutes (what most clients send).
    #[default]
    Standard,
    /// IPTV Smarters' `YYYY-MM-DD:HH:MM:SS` form, as a per-account fallback.
    SmartersCompat,
}

impl TimeshiftFormat {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::SmartersCompat => "smarters_compat",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "standard" => Some(Self::Standard),
            "smarters_compat" => Some(Self::SmartersCompat),
            _ => None,
        }
    }
}
sql_text_enum!(TimeshiftFormat);

/// User-Agent used for API requests and streams of a profile.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserAgent {
    /// `opensmarters/<version>` (chosen by the app).
    #[default]
    Default,
    /// The "IPTV Smarters compatible" preset (`IPTVSmartersPro`).
    SmartersCompat,
    /// A user-supplied string (1–512 chars, no control characters).
    Custom(String),
}

/// Per-profile settings.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileSettings {
    /// `.ts` or `.m3u8` for live/timeshift. Offer only what `allowed_output_formats` permits.
    pub live_container: LiveContainer,
    /// Catch-up URL time format.
    pub timeshift_format: TimeshiftFormat,
    /// User-Agent preset or custom value.
    pub user_agent: UserAgent,
}

impl ProfileSettings {
    pub(crate) fn validated(mut self) -> Result<Self> {
        if let UserAgent::Custom(ua) = &self.user_agent {
            let ua = validate_name("user_agent", ua, MAX_USER_AGENT_CHARS)?;
            self.user_agent = UserAgent::Custom(ua);
        }
        Ok(self)
    }

    pub(crate) fn user_agent_columns(&self) -> (&'static str, Option<&str>) {
        match &self.user_agent {
            UserAgent::Default => ("default", None),
            UserAgent::SmartersCompat => ("smarters_compat", None),
            UserAgent::Custom(s) => ("custom", Some(s.as_str())),
        }
    }
}

/// A stored profile (no secrets).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Profile {
    /// Stable id.
    pub id: ProfileId,
    /// Display name.
    pub name: String,
    /// Source description.
    pub source: ProfileSource,
    /// Per-profile settings.
    pub settings: ProfileSettings,
    /// Whether an EPG URL override is stored in the keychain
    /// ([`Profiles::epg_url_override`]).
    pub has_epg_url_override: bool,
    /// Position in the profile list (ascending).
    pub sort_order: i64,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last modification time.
    pub updated_at: Timestamp,
    /// Last time the profile was switched to ([`Profiles::mark_used`]).
    pub last_used_at: Option<Timestamp>,
}

impl Profile {
    /// The profile's kind.
    pub const fn kind(&self) -> ProfileKind {
        self.source.kind()
    }
}

/// Changes for [`Profiles::update`]. `None` leaves a field unchanged. Setting a field that
/// does not apply to the profile's kind fails with [`StoreError::WrongProfileKind`].
#[derive(Clone, Debug, Default)]
pub struct ProfileUpdate {
    /// New display name.
    pub name: Option<String>,
    /// New Xtream server URL (Xtream only).
    pub server_url: Option<String>,
    /// New Xtream username (Xtream only).
    pub username: Option<String>,
    /// New M3U file path (M3U file only).
    pub file_path: Option<PathBuf>,
    /// New per-profile settings.
    pub settings: Option<ProfileSettings>,
}

const PROFILE_COLUMNS: &str = "id, kind, name, server_url, username, file_path, sort_order, \
     created_at, updated_at, last_used_at, live_container, timeshift_format, user_agent, \
     user_agent_custom, has_epg_override";

fn profile_from_row(row: &Row<'_>) -> rusqlite::Result<Profile> {
    let kind: ProfileKind = row.get(1)?;
    let source = match kind {
        ProfileKind::Xtream => ProfileSource::Xtream {
            server_url: row.get(3)?,
            username: row.get(4)?,
        },
        ProfileKind::M3uUrl => ProfileSource::M3uUrl,
        ProfileKind::M3uFile => ProfileSource::M3uFile {
            path: PathBuf::from(row.get::<_, String>(5)?),
        },
    };
    let ua_kind: String = row.get(12)?;
    let ua_custom: Option<String> = row.get(13)?;
    let user_agent = match (ua_kind.as_str(), ua_custom) {
        ("smarters_compat", _) => UserAgent::SmartersCompat,
        ("custom", Some(s)) => UserAgent::Custom(s),
        _ => UserAgent::Default,
    };
    Ok(Profile {
        id: row.get(0)?,
        name: row.get(2)?,
        source,
        sort_order: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        last_used_at: row.get(9)?,
        settings: ProfileSettings {
            live_container: row.get(10)?,
            timeshift_format: row.get(11)?,
            user_agent,
        },
        has_epg_url_override: row.get(14)?,
    })
}

/// Validates an Xtream server URL and returns it trimmed, without trailing slashes.
///
/// The URL must be absolute `http`/`https` with a host, and must not carry credentials
/// (`user:pass@`), a query string or a fragment — anything credential-bearing belongs in
/// the keychain, and a server URL never needs a query.
pub(crate) fn validate_server_url(s: &str) -> Result<String> {
    const F: &str = "server_url";
    let t = s.trim();
    if t.is_empty() {
        return Err(StoreError::invalid(F, "must not be empty"));
    }
    if t.len() > MAX_URL_BYTES {
        return Err(StoreError::invalid(F, "is too long"));
    }
    if has_control(t) {
        return Err(StoreError::invalid(
            F,
            "must not contain control characters",
        ));
    }
    let url = Url::parse(t).map_err(|_| StoreError::invalid(F, "is not a valid absolute URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(StoreError::invalid(F, "must use http or https"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(StoreError::invalid(F, "must have a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(StoreError::invalid(F, "must not embed credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(StoreError::invalid(
            F,
            "must not contain a query string or fragment",
        ));
    }
    Ok(t.trim_end_matches('/').to_owned())
}

pub(crate) fn validate_username(s: &str) -> Result<String> {
    validate_name("username", s, MAX_USERNAME_CHARS)
}

pub(crate) fn validate_path(p: &std::path::Path) -> Result<String> {
    let s = p
        .to_str()
        .ok_or(StoreError::invalid("file_path", "must be valid Unicode"))?;
    if s.trim().is_empty() {
        return Err(StoreError::invalid("file_path", "must not be empty"));
    }
    if s.len() > MAX_PATH_BYTES {
        return Err(StoreError::invalid("file_path", "is too long"));
    }
    if has_control(s) {
        return Err(StoreError::invalid(
            "file_path",
            "must not contain control characters",
        ));
    }
    Ok(s.to_owned())
}

fn validate_password(p: &SecretString) -> Result<()> {
    let s = p.expose_secret();
    if s.is_empty() {
        return Err(StoreError::invalid("password", "must not be empty"));
    }
    if s.len() > MAX_PASSWORD_BYTES {
        return Err(StoreError::invalid("password", "is too long"));
    }
    Ok(())
}

/// Validates a secret URL (M3U / EPG) and returns a trimmed copy.
fn validate_secret_url(field: &'static str, u: &SecretString) -> Result<SecretString> {
    let s = u.expose_secret().trim();
    if s.is_empty() {
        return Err(StoreError::invalid(field, "must not be empty"));
    }
    if s.len() > MAX_URL_BYTES {
        return Err(StoreError::invalid(field, "is too long"));
    }
    if has_control(s) {
        return Err(StoreError::invalid(
            field,
            "must not contain control characters",
        ));
    }
    Ok(SecretString::from(s))
}

/// Profile operations. Obtain with [`Store::profiles`].
#[derive(Clone, Copy, Debug)]
pub struct Profiles<'a> {
    pub(crate) store: &'a Store,
}

impl Profiles<'_> {
    /// Creates a profile. Its secret (password or M3U URL) is written to the keychain
    /// first; if the database insert then fails the secret is removed again.
    pub fn create(&self, new: NewProfile) -> Result<Profile> {
        let name = validate_name("name", &new.name, MAX_PROFILE_NAME_CHARS)?;
        let settings = new.settings.validated()?;
        let id = ProfileId::new_random();

        let (kind, server_url, username, file_path, secret) = match &new.source {
            NewProfileSource::Xtream {
                server_url,
                username,
                password,
            } => {
                validate_password(password)?;
                (
                    ProfileKind::Xtream,
                    Some(validate_server_url(server_url)?),
                    Some(validate_username(username)?),
                    None,
                    Some((SecretKey::profile_password(id), password.clone())),
                )
            }
            NewProfileSource::M3uUrl { url } => (
                ProfileKind::M3uUrl,
                None,
                None,
                None,
                Some((
                    SecretKey::profile_m3u_url(id),
                    validate_secret_url("m3u_url", url)?,
                )),
            ),
            NewProfileSource::M3uFile { path } => (
                ProfileKind::M3uFile,
                None,
                None,
                Some(validate_path(path)?),
                None,
            ),
        };

        // Keychain first, without holding the database lock (it is IPC and may block).
        if let Some((key, value)) = &secret {
            self.store.secrets.set(key, value)?;
        }

        let now = self.store.now();
        let (ua_kind, ua_custom) = settings.user_agent_columns();
        let inserted = self.store.conn().execute(
            "INSERT INTO profiles (id, kind, name, server_url, username, file_path, sort_order,
                                   created_at, updated_at, live_container, timeshift_format,
                                   user_agent, user_agent_custom)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                     (SELECT COALESCE(MAX(sort_order) + 1, 0) FROM profiles),
                     ?7, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                kind,
                name,
                server_url,
                username,
                file_path,
                now,
                settings.live_container,
                settings.timeshift_format,
                ua_kind,
                ua_custom
            ],
        );
        if let Err(e) = inserted {
            if let Some((key, _)) = &secret
                && let Err(cleanup) = self.store.secrets.delete(key)
            {
                tracing::warn!(error = %cleanup, "could not remove secret after failed profile insert");
            }
            return Err(e.into());
        }
        tracing::debug!(profile = %id, kind = kind.as_str(), "profile created");
        self.get(id)?.ok_or(StoreError::ProfileNotFound(id))
    }

    /// Looks up a profile.
    pub fn get(&self, id: ProfileId) -> Result<Option<Profile>> {
        let conn = self.store.conn();
        let sql = format!("SELECT {PROFILE_COLUMNS} FROM profiles WHERE id = ?1");
        Ok(conn
            .prepare_cached(&sql)?
            .query_row([id], profile_from_row)
            .optional()?)
    }

    /// All profiles in display order (`sort_order`, then creation time).
    pub fn list(&self) -> Result<Vec<Profile>> {
        let conn = self.store.conn();
        let sql =
            format!("SELECT {PROFILE_COLUMNS} FROM profiles ORDER BY sort_order, created_at, id");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], profile_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Applies an update and returns the new state. Changing the server URL, username or
    /// M3U file path also drops the profile's cache entries (they describe the old source).
    pub fn update(&self, id: ProfileId, update: ProfileUpdate) -> Result<Profile> {
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let sql = format!("SELECT {PROFILE_COLUMNS} FROM profiles WHERE id = ?1");
        let current = tx
            .query_row(&sql, [id], profile_from_row)
            .optional()?
            .ok_or(StoreError::ProfileNotFound(id))?;

        let name = match &update.name {
            Some(n) => validate_name("name", n, MAX_PROFILE_NAME_CHARS)?,
            None => current.name.clone(),
        };
        let (server_url, username, file_path) = match &current.source {
            ProfileSource::Xtream {
                server_url,
                username,
            } => {
                if update.file_path.is_some() {
                    return Err(StoreError::WrongProfileKind);
                }
                (
                    Some(match &update.server_url {
                        Some(u) => validate_server_url(u)?,
                        None => server_url.clone(),
                    }),
                    Some(match &update.username {
                        Some(u) => validate_username(u)?,
                        None => username.clone(),
                    }),
                    None,
                )
            }
            ProfileSource::M3uUrl => {
                if update.server_url.is_some()
                    || update.username.is_some()
                    || update.file_path.is_some()
                {
                    return Err(StoreError::WrongProfileKind);
                }
                (None, None, None)
            }
            ProfileSource::M3uFile { path } => {
                if update.server_url.is_some() || update.username.is_some() {
                    return Err(StoreError::WrongProfileKind);
                }
                let p = update.file_path.as_ref().unwrap_or(path);
                (None, None, Some(validate_path(p)?))
            }
        };
        let settings = match update.settings {
            Some(s) => s.validated()?,
            None => current.settings.clone(),
        };
        let (ua_kind, ua_custom) = settings.user_agent_columns();
        tx.execute(
            "UPDATE profiles SET name = ?2, server_url = ?3, username = ?4, file_path = ?5,
                    live_container = ?6, timeshift_format = ?7, user_agent = ?8,
                    user_agent_custom = ?9, updated_at = ?10
             WHERE id = ?1",
            params![
                id,
                name,
                server_url,
                username,
                file_path,
                settings.live_container,
                settings.timeshift_format,
                ua_kind,
                ua_custom,
                self.store.now()
            ],
        )?;
        let updated = tx.query_row(&sql, [id], profile_from_row)?;
        if updated.source != current.source {
            tx.execute("DELETE FROM cache WHERE profile_id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Deletes a profile, its keychain secrets and (by cascade) its favourites, history,
    /// recent channels, cache entries and category locks. Returns `false` if it did not
    /// exist.
    ///
    /// Secrets are deleted first: if the keychain refuses (e.g. it is locked) to delete a
    /// secret the profile is expected to have, the profile is kept so the user can retry,
    /// and no orphaned credential is left behind. Keys the profile should not have (an
    /// M3U-file profile has no password) are still deleted best-effort, but failures there
    /// do not block. A system without any keychain ([`SecretError::Unavailable`]) cannot
    /// hold secrets, so that case never blocks.
    pub fn delete(&self, id: ProfileId) -> Result<bool> {
        let expected: Vec<SecretKey> = match self.get(id)? {
            Some(p) => {
                let mut keys = Vec::new();
                match p.kind() {
                    ProfileKind::Xtream => keys.push(SecretKey::profile_password(id)),
                    ProfileKind::M3uUrl => keys.push(SecretKey::profile_m3u_url(id)),
                    ProfileKind::M3uFile => {}
                }
                if p.has_epg_url_override {
                    keys.push(SecretKey::profile_epg_url(id));
                }
                keys
            }
            None => Vec::new(),
        };
        for key in SecretKey::all_for_profile(id) {
            match self.store.secrets.delete(&key) {
                Ok(()) | Err(SecretError::Unavailable(_)) => {}
                Err(e) if expected.contains(&key) => return Err(e.into()),
                Err(e) => {
                    tracing::warn!(error = %e, "could not delete an unexpected profile secret");
                }
            }
        }
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let n = tx.execute("DELETE FROM profiles WHERE id = ?1", [id])?;
        tx.execute(
            "DELETE FROM settings WHERE key = ?1 AND value = ?2",
            params![LAST_PROFILE.key(), serde_json::to_string(&Some(id))?],
        )?;
        tx.commit()?;
        if n > 0 {
            tracing::debug!(profile = %id, "profile deleted");
        }
        Ok(n > 0)
    }

    /// Records that the user switched to this profile: updates `last_used_at` and the
    /// global [`LAST_PROFILE`] setting.
    pub fn mark_used(&self, id: ProfileId) -> Result<()> {
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE profiles SET last_used_at = ?2 WHERE id = ?1",
            params![id, self.store.now()],
        )?;
        if n == 0 {
            return Err(StoreError::ProfileNotFound(id));
        }
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![LAST_PROFILE.key(), serde_json::to_string(&Some(id))?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reorders profiles: the given ids first, in that order; any others keep their
    /// relative order after them. Unknown ids are ignored.
    pub fn reorder(&self, ids: &[ProfileId]) -> Result<()> {
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let existing: Vec<ProfileId> = tx
            .prepare("SELECT id FROM profiles ORDER BY sort_order, created_at, id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut order: Vec<ProfileId> = Vec::with_capacity(existing.len());
        for id in ids {
            if existing.contains(id) && !order.contains(id) {
                order.push(*id);
            }
        }
        for id in &existing {
            if !order.contains(id) {
                order.push(*id);
            }
        }
        {
            let mut stmt = tx.prepare("UPDATE profiles SET sort_order = ?2 WHERE id = ?1")?;
            for (pos, id) in order.iter().enumerate() {
                stmt.execute(params![id, i64::try_from(pos).unwrap_or(i64::MAX)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn kind_of(&self, id: ProfileId) -> Result<ProfileKind> {
        self.store
            .conn()
            .prepare_cached("SELECT kind FROM profiles WHERE id = ?1")?
            .query_row([id], |r| r.get(0))
            .optional()?
            .ok_or(StoreError::ProfileNotFound(id))
    }

    fn require_kind(&self, id: ProfileId, kind: ProfileKind) -> Result<()> {
        if self.kind_of(id)? == kind {
            Ok(())
        } else {
            Err(StoreError::WrongProfileKind)
        }
    }

    /// The Xtream password from the keychain (`None` if missing there).
    pub fn password(&self, id: ProfileId) -> Result<Option<SecretString>> {
        self.require_kind(id, ProfileKind::Xtream)?;
        Ok(self.store.secrets.get(&SecretKey::profile_password(id))?)
    }

    /// Replaces the Xtream password in the keychain.
    pub fn set_password(&self, id: ProfileId, password: &SecretString) -> Result<()> {
        validate_password(password)?;
        self.require_kind(id, ProfileKind::Xtream)?;
        self.store
            .secrets
            .set(&SecretKey::profile_password(id), password)?;
        self.touch(id)
    }

    /// The M3U playlist URL from the keychain.
    pub fn m3u_url(&self, id: ProfileId) -> Result<Option<SecretString>> {
        self.require_kind(id, ProfileKind::M3uUrl)?;
        Ok(self.store.secrets.get(&SecretKey::profile_m3u_url(id))?)
    }

    /// Replaces the M3U playlist URL in the keychain and drops the profile's cache
    /// entries (they describe the old playlist).
    pub fn set_m3u_url(&self, id: ProfileId, url: &SecretString) -> Result<()> {
        let url = validate_secret_url("m3u_url", url)?;
        self.require_kind(id, ProfileKind::M3uUrl)?;
        self.store
            .secrets
            .set(&SecretKey::profile_m3u_url(id), &url)?;
        self.store
            .conn()
            .execute("DELETE FROM cache WHERE profile_id = ?1", [id])?;
        self.touch(id)
    }

    /// The EPG (XMLTV) URL override from the keychain, if one is set.
    pub fn epg_url_override(&self, id: ProfileId) -> Result<Option<SecretString>> {
        self.kind_of(id)?;
        Ok(self.store.secrets.get(&SecretKey::profile_epg_url(id))?)
    }

    /// Sets (`Some`) or clears (`None`) the EPG URL override. Kept in the keychain because
    /// `xmltv.php` URLs embed credentials.
    pub fn set_epg_url_override(&self, id: ProfileId, url: Option<&SecretString>) -> Result<()> {
        self.kind_of(id)?;
        let key = SecretKey::profile_epg_url(id);
        match url {
            Some(u) => {
                let u = validate_secret_url("epg_url", u)?;
                self.store.secrets.set(&key, &u)?;
            }
            None => self.store.secrets.delete(&key)?,
        }
        self.store.conn().execute(
            "UPDATE profiles SET has_epg_override = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, url.is_some(), self.store.now()],
        )?;
        Ok(())
    }

    fn touch(&self, id: ProfileId) -> Result<()> {
        self.store.conn().execute(
            "UPDATE profiles SET updated_at = ?2 WHERE id = ?1",
            params![id, self.store.now()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_validation() {
        assert_eq!(
            validate_server_url(" http://panel.example.invalid:8080/ ")
                .ok()
                .as_deref(),
            Some("http://panel.example.invalid:8080")
        );
        assert!(validate_server_url("https://panel.example.invalid/sub").is_ok());
        for bad in [
            "",
            "panel.example.invalid:8080",
            "ftp://panel.example.invalid",
            "http://user:pass@panel.example.invalid",
            "http://user@panel.example.invalid",
            "http://panel.example.invalid/player_api.php?username=u&password=p",
            "http://panel.example.invalid/#frag",
            "http://panel.example.invalid/a\tb",
            "http://panel.example.invalid/a\nb",
            "file:///etc/passwd",
        ] {
            assert!(
                validate_server_url(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn kinds_round_trip() {
        for k in [
            ProfileKind::Xtream,
            ProfileKind::M3uUrl,
            ProfileKind::M3uFile,
        ] {
            assert_eq!(ProfileKind::parse(k.as_str()), Some(k));
        }
        for c in [LiveContainer::Ts, LiveContainer::M3u8] {
            assert_eq!(LiveContainer::parse(c.as_str()), Some(c));
        }
        for t in [TimeshiftFormat::Standard, TimeshiftFormat::SmartersCompat] {
            assert_eq!(TimeshiftFormat::parse(t.as_str()), Some(t));
        }
    }
}

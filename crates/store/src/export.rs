//! Export and import of **non-secret** data as JSON (backup, moving to another machine).
//!
//! Included: profiles (without passwords / M3U URLs / EPG overrides), their favourites and
//! manual category locks, all global settings, and the parental keyword list.
//! Excluded: every keychain secret, the parental PIN hash and lockout state, watch
//! history, recent channels and the cache.
//!
//! Import treats the file as untrusted input: every field goes through the same
//! validation as the regular API, provider strings are sanitised, sizes are capped, and
//! the whole import runs in one transaction (all or nothing). It merges: profiles whose
//! id already exists are skipped, favourites/locks are added if absent, settings in the
//! file overwrite local ones, keywords are added to the local list. Imported Xtream and
//! M3U-URL profiles have no secret yet; [`ImportSummary::needs_secrets`] lists them so the
//! app can ask for the password / URL.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, StoreError};
use crate::favourites::ItemSnapshot;
use crate::ids::{ContentKind, ProfileId};
use crate::parental::{CategoryLock, MAX_KEYWORDS, normalize_keyword};
use crate::profiles::{
    ProfileKind, ProfileSettings, ProfileSource, validate_path, validate_server_url,
    validate_username,
};
use crate::settings::validate_key;
use crate::text::{MAX_PROFILE_NAME_CHARS, validate_item_id, validate_name};
use crate::time::Timestamp;

/// `format` field of an export document.
pub const EXPORT_FORMAT: &str = "opensmarters-export";
/// Current export document version.
pub const EXPORT_VERSION: u32 = 1;

const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMPORT_PROFILES: usize = 1000;
const MAX_IMPORT_FAVOURITES_PER_PROFILE: usize = 100_000;
const MAX_IMPORT_SETTINGS: usize = 10_000;
const MAX_SETTING_JSON_BYTES: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
struct ExportDoc {
    format: String,
    version: u32,
    exported_at: Timestamp,
    #[serde(default)]
    profiles: Vec<ExportProfile>,
    #[serde(default)]
    settings: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    parental_keywords: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct ExportProfile {
    id: ProfileId,
    name: String,
    source: ProfileSource,
    #[serde(default)]
    settings: ProfileSettings,
    #[serde(default)]
    sort_order: i64,
    created_at: Timestamp,
    #[serde(default)]
    favourites: Vec<ExportFavourite>,
    #[serde(default)]
    category_locks: Vec<ExportLock>,
}

#[derive(Serialize, Deserialize)]
struct ExportFavourite {
    kind: ContentKind,
    item_id: String,
    position: i64,
    added_at: Timestamp,
    #[serde(default)]
    snapshot: ItemSnapshot,
}

#[derive(Serialize, Deserialize)]
struct ExportLock {
    kind: ContentKind,
    category_id: String,
    state: CategoryLock,
}

/// What [`Store::import_json`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Profiles created.
    pub profiles_added: Vec<ProfileId>,
    /// Profiles skipped because their id already exists.
    pub profiles_skipped: usize,
    /// Created profiles that need a password / M3U URL before they can be used.
    pub needs_secrets: Vec<ProfileId>,
    /// Favourites added.
    pub favourites_added: usize,
    /// Category locks added.
    pub category_locks_added: usize,
    /// Settings written.
    pub settings_applied: usize,
    /// Keywords added to the local list.
    pub keywords_added: usize,
}

impl Store {
    /// Serialises all non-secret data (see the [module docs](crate::export)) as pretty
    /// JSON. Contains no password, URL secret, API key or PIN hash.
    pub fn export_json(&self) -> Result<String> {
        let profiles = self.profiles().list()?;
        let mut out = Vec::with_capacity(profiles.len());
        for p in profiles {
            let mut favourites = Vec::new();
            let mut category_locks = Vec::new();
            for kind in ContentKind::ALL {
                favourites.extend(self.favourites().list(p.id, kind)?.into_iter().map(|f| {
                    ExportFavourite {
                        kind: f.kind,
                        item_id: f.item_id,
                        position: f.position,
                        added_at: f.added_at,
                        snapshot: f.snapshot,
                    }
                }));
                let mut locks: Vec<_> = self
                    .parental()
                    .category_locks(p.id, kind)?
                    .into_iter()
                    .map(|(category_id, state)| ExportLock {
                        kind,
                        category_id,
                        state,
                    })
                    .collect();
                locks.sort_by(|a, b| a.category_id.cmp(&b.category_id));
                category_locks.extend(locks);
            }
            out.push(ExportProfile {
                id: p.id,
                name: p.name,
                source: p.source,
                settings: p.settings,
                sort_order: p.sort_order,
                created_at: p.created_at,
                favourites,
                category_locks,
            });
        }
        let settings = {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut map = BTreeMap::new();
            for row in rows {
                let (k, v) = row?;
                match serde_json::from_str(&v) {
                    Ok(value) => {
                        map.insert(k, value);
                    }
                    Err(_) => tracing::warn!(key = %k, "skipping unreadable setting in export"),
                }
            }
            map
        };
        let doc = ExportDoc {
            format: EXPORT_FORMAT.to_owned(),
            version: EXPORT_VERSION,
            exported_at: self.now(),
            profiles: out,
            settings,
            parental_keywords: self.parental().keywords()?,
        };
        Ok(serde_json::to_string_pretty(&doc)?)
    }

    /// Imports a document produced by [`export_json`](Self::export_json) (see the
    /// [module docs](crate::export) for the merge rules). Untrusted input: validated
    /// throughout; nothing is written unless the whole document is valid.
    pub fn import_json(&self, json: &str) -> Result<ImportSummary> {
        if json.len() > MAX_IMPORT_BYTES {
            return Err(StoreError::invalid("import", "file is too large"));
        }
        let doc: ExportDoc = serde_json::from_str(json)?;
        if doc.format != EXPORT_FORMAT {
            return Err(StoreError::invalid("import", "not an opensmarters export"));
        }
        if doc.version == 0 || doc.version > EXPORT_VERSION {
            return Err(StoreError::invalid("import", "unsupported export version"));
        }
        if doc.profiles.len() > MAX_IMPORT_PROFILES || doc.settings.len() > MAX_IMPORT_SETTINGS {
            return Err(StoreError::invalid("import", "too many entries"));
        }

        let now = self.now();
        let mut summary = ImportSummary::default();
        let mut conn = self.conn();
        let tx = conn.transaction()?;

        for p in &doc.profiles {
            if p.favourites.len() > MAX_IMPORT_FAVOURITES_PER_PROFILE {
                return Err(StoreError::invalid("import", "too many favourites"));
            }
            let exists: Option<i64> = tx
                .query_row("SELECT 1 FROM profiles WHERE id = ?1", [p.id], |r| r.get(0))
                .optional()?;
            if exists.is_some() {
                summary.profiles_skipped += 1;
            } else {
                let name = validate_name("name", &p.name, MAX_PROFILE_NAME_CHARS)?;
                let settings = p.settings.clone().validated()?;
                let (server_url, username, file_path): (
                    Option<String>,
                    Option<String>,
                    Option<String>,
                ) = match &p.source {
                    ProfileSource::Xtream {
                        server_url,
                        username,
                    } => (
                        Some(validate_server_url(server_url)?),
                        Some(validate_username(username)?),
                        None,
                    ),
                    ProfileSource::M3uUrl => (None, None, None),
                    ProfileSource::M3uFile { path } => (None, None, Some(validate_path(path)?)),
                };
                let kind = p.source.kind();
                let (ua_kind, ua_custom) = settings.user_agent_columns();
                tx.execute(
                    "INSERT INTO profiles (id, kind, name, server_url, username, file_path,
                                           sort_order, created_at, updated_at, live_container,
                                           timeshift_format, user_agent, user_agent_custom)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        p.id,
                        kind,
                        name,
                        server_url,
                        username,
                        file_path,
                        p.sort_order,
                        p.created_at,
                        now,
                        settings.live_container,
                        settings.timeshift_format,
                        ua_kind,
                        ua_custom
                    ],
                )?;
                summary.profiles_added.push(p.id);
                if matches!(kind, ProfileKind::Xtream | ProfileKind::M3uUrl) {
                    summary.needs_secrets.push(p.id);
                }
            }

            for f in &p.favourites {
                let item_id = validate_item_id("item_id", &f.item_id)?;
                let snap = f.snapshot.sanitized();
                summary.favourites_added += tx.execute(
                    "INSERT INTO favourites (profile_id, kind, item_id, position, added_at, name,
                                             icon_url, category_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT DO NOTHING",
                    params![
                        p.id,
                        f.kind,
                        item_id,
                        f.position,
                        f.added_at,
                        snap.name,
                        snap.icon_url,
                        snap.category_id
                    ],
                )?;
            }
            for l in &p.category_locks {
                let category_id = validate_item_id("category_id", &l.category_id)?;
                summary.category_locks_added += tx.execute(
                    "INSERT INTO category_locks (profile_id, kind, category_id, state)
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
                    params![p.id, l.kind, category_id, l.state],
                )?;
            }
        }

        for (key, value) in &doc.settings {
            validate_key(key)?;
            let json = serde_json::to_string(value)?;
            if json.len() > MAX_SETTING_JSON_BYTES {
                return Err(StoreError::invalid("import", "setting value is too large"));
            }
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, json],
            )?;
            summary.settings_applied += 1;
        }

        let existing: i64 =
            tx.query_row("SELECT COUNT(*) FROM parental_keywords", [], |r| r.get(0))?;
        let mut count = usize::try_from(existing).unwrap_or(usize::MAX);
        for k in &doc.parental_keywords {
            let k = normalize_keyword(k)?;
            let n = tx.execute(
                "INSERT INTO parental_keywords (keyword) VALUES (?1) ON CONFLICT DO NOTHING",
                [&k],
            )?;
            count += n;
            if count > MAX_KEYWORDS {
                return Err(StoreError::invalid("import", "too many keywords"));
            }
            summary.keywords_added += n;
        }

        tx.commit()?;
        drop(conn);
        self.reload_cache_cap()?;
        Ok(summary)
    }
}

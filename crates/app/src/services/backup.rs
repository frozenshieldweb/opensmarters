// SPDX-License-Identifier: GPL-3.0-or-later
//! Backup export/import (Settings › Storage & data) on top of the store's
//! `export_json` / `import_json`, which already exclude every secret (passwords, M3U
//! addresses, the TMDB key, the PIN hash) and history.
//!
//! On top of that the app:
//! - drops machine-local settings ([`LOCAL_ONLY_KEYS`]: PIN length, window position,
//!   search history, last profile, update-check state) from exports, and ignores them in
//!   imported files, so
//!   a backup never changes how the local PIN pad works or where the window opens;
//! - caps the file size ([`MAX_BACKUP_BYTES`]) before parsing;
//! - lists the imported profiles that need a password / playlist address
//!   ([`ImportReport::needs_secrets`]).
//!
//! Blocking (store): call off the UI thread.

use opensmarters_store::{ProfileId, ProfileKind, Store};

use crate::error::AppError;

/// Settings that belong to this computer only (never exported, ignored on import).
pub const LOCAL_ONLY_KEYS: &[&str] = &[
    "app.parental.pin_length",
    // A backup must never reset (or impose) the "forgot PIN" wait.
    "app.parental.reset_limiter",
    "app.window",
    "app.search_history",
    "last_profile",
    // Update check: last check time, latest version seen, the version dismissed.
    "app.updates",
];

/// Largest backup file read (the store refuses more than 64 MiB anyway).
pub const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;

/// A suggested file name: `opensmarters-backup-2026-10-20.json`.
pub fn suggested_file_name(date: opensmarters_core::chrono::NaiveDate) -> String {
    format!("opensmarters-backup-{}.json", date.format("%Y-%m-%d"))
}

fn strip_local(doc: &mut serde_json::Value) {
    if let Some(settings) = doc.get_mut("settings").and_then(|s| s.as_object_mut()) {
        for k in LOCAL_ONLY_KEYS {
            settings.remove(*k);
        }
    }
}

/// The backup document (pretty JSON, no secrets, no machine-local settings).
///
/// # Errors
///
/// Database errors.
pub fn export(store: &Store) -> Result<String, AppError> {
    let raw = store.export_json()?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| AppError::msg("The backup couldn't be built."))?;
    strip_local(&mut doc);
    serde_json::to_string_pretty(&doc).map_err(|_| AppError::msg("The backup couldn't be built."))
}

/// One imported profile that needs its password / playlist address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsSecret {
    /// The new profile.
    pub id: ProfileId,
    /// Its name.
    pub name: String,
    /// Xtream (password) or M3U URL (playlist address).
    pub kind: ProfileKind,
}

/// What an import did, for the summary dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Profiles created.
    pub profiles_added: usize,
    /// Profiles already on this computer (skipped).
    pub profiles_skipped: usize,
    /// Favourites added.
    pub favourites_added: usize,
    /// Settings applied.
    pub settings_applied: usize,
    /// Words added to the parental list.
    pub keywords_added: usize,
    /// New profiles that need their password / playlist address.
    pub needs_secrets: Vec<NeedsSecret>,
}

/// Imports a backup document (validated by the store; all or nothing).
///
/// # Errors
///
/// Not an opensmarters backup, too large, invalid data, database errors.
pub fn import(store: &Store, json: &str) -> Result<ImportReport, AppError> {
    if json.len() as u64 > MAX_BACKUP_BYTES {
        return Err(AppError::msg("That file is too large to be a backup."));
    }
    let mut doc: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| AppError::msg("That file isn't an opensmarters backup."))?;
    strip_local(&mut doc);
    let cleaned = serde_json::to_string(&doc)
        .map_err(|_| AppError::msg("That file isn't an opensmarters backup."))?;
    let summary = store.import_json(&cleaned).map_err(|e| {
        tracing::warn!(error = %e, "backup import refused");
        AppError::msg(
            "That backup couldn't be imported: it isn't an opensmarters backup, or it is \
             damaged or from a newer version. Nothing was changed.",
        )
    })?;
    let mut needs = Vec::new();
    for id in &summary.needs_secrets {
        if let Some(p) = store.profiles().get(*id)? {
            needs.push(NeedsSecret {
                id: p.id,
                name: p.name.clone(),
                kind: p.kind(),
            });
        }
    }
    Ok(ImportReport {
        profiles_added: summary.profiles_added.len(),
        profiles_skipped: summary.profiles_skipped,
        favourites_added: summary.favourites_added,
        settings_applied: summary.settings_applied,
        keywords_added: summary.keywords_added,
        needs_secrets: needs,
    })
}

/// Reads a backup file (size-capped).
///
/// # Errors
///
/// Unreadable, too large, not UTF-8.
pub fn read_file(path: &std::path::Path) -> Result<String, AppError> {
    let meta = std::fs::metadata(path).map_err(|_| AppError::msg("The file can't be opened."))?;
    if !meta.is_file() {
        return Err(AppError::msg("That isn't a file."));
    }
    if meta.len() > MAX_BACKUP_BYTES {
        return Err(AppError::msg("That file is too large to be a backup."));
    }
    let bytes = std::fs::read(path).map_err(|_| AppError::msg("The file can't be read."))?;
    String::from_utf8(bytes).map_err(|_| AppError::msg("That file isn't an opensmarters backup."))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use opensmarters_core::secrecy::SecretString;
    use opensmarters_store::{
        ContentKind, ItemSnapshot, MemorySecretStore, NewProfile, NewProfileSource, PinHashParams,
        ProfileSettings, StoreOptions, settings,
    };

    use super::*;
    use crate::services::parental::{pin_length, set_pin_length};

    const XTREAM_PASSWORD: &str = "synthetic-Pa55word-zq81";
    const M3U_URL: &str =
        "http://lists.example.invalid/get.php?username=u7&password=synthetic-m3u-pw";
    const TMDB_KEY: &str = "0123456789abcdef0123456789abcdef";
    const PIN: &str = "482915";

    fn store() -> Store {
        Store::open(
            StoreOptions::in_memory().with_pin_hash(PinHashParams {
                m_cost_kib: 64,
                t_cost: 1,
                p_cost: 1,
            }),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap()
    }

    fn populated() -> (Store, ProfileId) {
        let s = store();
        let p = s
            .profiles()
            .create(NewProfile {
                name: "Living room".into(),
                source: NewProfileSource::Xtream {
                    server_url: "http://panel.example.invalid:8080".into(),
                    username: "demo-user".into(),
                    password: SecretString::from(XTREAM_PASSWORD),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap();
        s.profiles()
            .create(NewProfile {
                name: "Playlist".into(),
                source: NewProfileSource::M3uUrl {
                    url: SecretString::from(M3U_URL),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap();
        s.favourites()
            .add(
                p.id,
                ContentKind::Live,
                "1001",
                &ItemSnapshot::named("News One"),
            )
            .unwrap();
        s.parental().set_pin(&SecretString::from(PIN)).unwrap();
        set_pin_length(&s, Some(PIN.len())).unwrap();
        s.parental().add_keyword("casino").unwrap();
        s.settings()
            .set_tmdb_api_key(Some(&SecretString::from(TMDB_KEY)))
            .unwrap();
        s.settings().set(&settings::TMDB_ENABLED, &true).unwrap();
        s.settings()
            .set(&crate::services::prefs::TV_MODE, &true)
            .unwrap();
        crate::services::search_history::record(&s, p.id, "late night quiz").unwrap();
        s.settings()
            .set(
                &crate::services::updates::STATE,
                &crate::services::updates::UpdateState {
                    last_success: Some(1_800_000_000),
                    latest_seen: Some("7.7.7".into()),
                    dismissed: Some("7.7.7".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        s.settings()
            .set(&crate::services::prefs::CHECK_UPDATES_AUTOMATICALLY, &true)
            .unwrap();
        s.profiles().mark_used(p.id).unwrap();
        (s, p.id)
    }

    #[test]
    fn export_never_contains_secrets_or_local_settings() {
        let (s, _) = populated();
        let json = export(&s).unwrap();
        for secret in [
            XTREAM_PASSWORD,
            "synthetic-m3u-pw",
            "lists.example.invalid",
            TMDB_KEY,
            PIN,
            "argon2",
            "late night quiz",
            "app.parental.pin_length",
            "app.search_history",
            "last_profile",
            "app.updates",
            "7.7.7",
            "1800000000",
        ] {
            assert!(!json.contains(secret), "export leaks {secret:?}");
        }
        // The user's choice to check automatically is a preference: it travels.
        assert!(json.contains("app.check_updates_automatically"));
        // Non-secret data is there.
        assert!(json.contains("Living room"));
        assert!(json.contains("News One"));
        assert!(json.contains("casino"));
        assert!(json.contains("app.tv_mode"));
    }

    #[test]
    fn exported_file_on_disk_has_no_secrets() {
        let (s, _) = populated();
        let dir = std::env::temp_dir().join(format!("os-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(suggested_file_name(
            opensmarters_core::chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        ));
        std::fs::write(&path, export(&s).unwrap()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for secret in [XTREAM_PASSWORD, "synthetic-m3u-pw", TMDB_KEY] {
            assert!(!text.contains(secret));
        }
        assert!(path.ends_with("opensmarters-backup-2026-10-20.json"));
        let back = read_file(&path).unwrap();
        assert_eq!(back.len(), bytes.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_lists_profiles_needing_secrets_and_keeps_local_pin_length() {
        let (src, _) = populated();
        let json = export(&src).unwrap();

        let dst = store();
        dst.parental().set_pin(&SecretString::from("1234")).unwrap();
        set_pin_length(&dst, Some(4)).unwrap();
        let report = import(&dst, &json).unwrap();
        assert_eq!(report.profiles_added, 2);
        assert_eq!(report.favourites_added, 1);
        assert!(report.keywords_added >= 1);
        let mut names: Vec<_> = report
            .needs_secrets
            .iter()
            .map(|n| n.name.clone())
            .collect();
        names.sort();
        assert_eq!(names, vec!["Living room", "Playlist"]);
        // Secrets are not there; the local PIN (and its length) are untouched.
        let p = &report.needs_secrets[0];
        assert!(dst.profiles().password(p.id).unwrap().is_none());
        assert!(dst.settings().tmdb_api_key().unwrap().is_none());
        assert_eq!(pin_length(&dst), 4);
        assert!(
            dst.parental()
                .verify_pin(&SecretString::from("1234"))
                .unwrap()
                .is_ok()
        );
        // Settings came across.
        assert!(
            dst.settings()
                .get(&crate::services::prefs::TV_MODE)
                .unwrap()
        );

        // Importing again skips existing profiles.
        let again = import(&dst, &json).unwrap();
        assert_eq!(again.profiles_added, 0);
        assert_eq!(again.profiles_skipped, 2);
        assert!(again.needs_secrets.is_empty());
    }

    #[test]
    fn hostile_imports_are_refused_or_neutralised() {
        let dst = store();
        set_pin_length(&dst, Some(6)).unwrap();
        assert!(import(&dst, "not json").is_err());
        assert!(import(&dst, r#"{"format":"something-else","version":1}"#).is_err());
        // Local-only keys in a hand-made file are ignored.
        let doc = r#"{"format":"opensmarters-export","version":1,"exported_at":0,
            "settings":{"app.parental.pin_length":4,"app.window":{"x":1},
                "app.updates":{"dismissed":"9.9.9"}},"profiles":[],
            "parental_keywords":[]}"#;
        let r = import(&dst, doc).unwrap();
        assert_eq!(r.settings_applied, 0);
        assert_eq!(pin_length(&dst), 6);
        assert_eq!(
            dst.settings()
                .get(&crate::services::updates::STATE)
                .unwrap()
                .dismissed,
            None,
            "a backup can't dismiss an update"
        );
    }
}

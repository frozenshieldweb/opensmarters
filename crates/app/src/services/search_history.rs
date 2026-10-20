// SPDX-License-Identifier: GPL-3.0-or-later
//! Recent searches per profile (Search shows them as chips; Settings › Storage clears
//! them). Stored in the settings table under one key ([`SEARCH_HISTORY`]), most recent
//! first, at most [`MAX_PER_PROFILE`] per profile. Machine-local: never exported.
//!
//! Blocking (store): call off the UI thread.

use std::collections::BTreeMap;

use opensmarters_store::settings::Setting;
use opensmarters_store::{ProfileId, Store, StoreError};

/// Profile id (UUID text) → queries, most recent first.
pub const SEARCH_HISTORY: Setting<BTreeMap<String, Vec<String>>> =
    Setting::new("app.search_history", BTreeMap::new);

/// Queries kept per profile.
pub const MAX_PER_PROFILE: usize = 20;
/// Longest query kept (characters).
pub const MAX_QUERY_CHARS: usize = 100;

fn key(profile: ProfileId) -> String {
    profile.as_uuid().to_string()
}

/// The profile's recent searches, most recent first.
///
/// # Errors
///
/// Database errors.
pub fn load(store: &Store, profile: ProfileId) -> Result<Vec<String>, StoreError> {
    Ok(store
        .settings()
        .get(&SEARCH_HISTORY)?
        .remove(&key(profile))
        .unwrap_or_default())
}

/// Records a search (trimmed; blank and control-character queries are ignored; an
/// existing entry moves to the front, ignoring case).
///
/// # Errors
///
/// Database errors.
pub fn record(store: &Store, profile: ProfileId, query: &str) -> Result<(), StoreError> {
    let q: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
    if q.is_empty() || q.chars().any(char::is_control) {
        return Ok(());
    }
    let s = store.settings();
    let mut all = s.get(&SEARCH_HISTORY)?;
    let list = all.entry(key(profile)).or_default();
    list.retain(|x| !x.eq_ignore_ascii_case(&q) && x.to_lowercase() != q.to_lowercase());
    list.insert(0, q);
    list.truncate(MAX_PER_PROFILE);
    s.set(&SEARCH_HISTORY, &all)
}

/// Forgets the profile's searches. Returns how many were removed.
///
/// # Errors
///
/// Database errors.
pub fn clear(store: &Store, profile: ProfileId) -> Result<usize, StoreError> {
    let s = store.settings();
    let mut all = s.get(&SEARCH_HISTORY)?;
    let n = all.remove(&key(profile)).map_or(0, |v| v.len());
    if n > 0 {
        s.set(&SEARCH_HISTORY, &all)?;
    }
    Ok(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use opensmarters_store::{MemorySecretStore, StoreOptions};

    use super::*;

    #[test]
    fn records_dedupes_caps_and_clears_per_profile() {
        let store = Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        let a = ProfileId::new_random();
        let b = ProfileId::new_random();
        record(&store, a, "news").unwrap();
        record(&store, a, "  Sport ").unwrap();
        record(&store, a, "NEWS").unwrap();
        record(&store, a, "   ").unwrap();
        record(&store, a, "bad\u{7}").unwrap();
        record(&store, b, "films").unwrap();
        assert_eq!(load(&store, a).unwrap(), vec!["NEWS", "Sport"]);
        for i in 0..30 {
            record(&store, a, &format!("q{i}")).unwrap();
        }
        let l = load(&store, a).unwrap();
        assert_eq!(l.len(), MAX_PER_PROFILE);
        assert_eq!(l[0], "q29");
        assert_eq!(clear(&store, a).unwrap(), MAX_PER_PROFILE);
        assert!(load(&store, a).unwrap().is_empty());
        assert_eq!(
            load(&store, b).unwrap(),
            vec!["films"],
            "other profiles untouched"
        );
        assert_eq!(clear(&store, a).unwrap(), 0);
    }
}

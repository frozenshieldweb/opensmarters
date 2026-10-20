// SPDX-License-Identifier: GPL-3.0-or-later
//! Deleting a profile together with its machine-local app data.
//!
//! `store.profiles().delete(id)` removes the profile, its keychain secrets and
//! (by cascade) its favourites, history, caches and category locks. Two app settings are
//! keyed by profile id outside those tables, and the delete dialog promises they go too:
//! the recent searches ([`search_history`]) and the remembered Movies/Series browse
//! category (`app.vod_last_category`, `"<profile>:<movie|series>"`).
//!
//! Every place that deletes a profile should call [`delete_profile`] (Settings ›
//! Profiles does; the profile picker's "Manage profiles" should too).
//!
//! Blocking (store): call off the UI thread.

use opensmarters_store::{ProfileId, Store, StoreError};

use crate::services::search_history;
use crate::services::session::LAST_VOD_CATEGORY;

/// Deletes profile `id` and forgets its machine-local data. Returns `false` if the
/// profile did not exist.
///
/// # Errors
///
/// The store's delete errors (the profile is then kept). Failing to forget the local data
/// afterwards is only logged: the profile itself is gone by then.
pub fn delete_profile(store: &Store, id: ProfileId) -> Result<bool, StoreError> {
    let existed = store.profiles().delete(id)?;
    if let Err(e) = forget_local_data(store, id) {
        tracing::warn!(error = %e, "could not forget a deleted profile's local data");
    }
    Ok(existed)
}

/// Forgets the recent searches and remembered browse categories of profile `id`.
///
/// # Errors
///
/// Database errors.
pub fn forget_local_data(store: &Store, id: ProfileId) -> Result<(), StoreError> {
    search_history::clear(store, id)?;
    let settings = store.settings();
    let mut map = settings.get(&LAST_VOD_CATEGORY)?;
    let prefix = format!("{id}:");
    let before = map.len();
    map.retain(|k, _| !k.starts_with(&prefix));
    if map.len() != before {
        settings.set(&LAST_VOD_CATEGORY, &map)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use opensmarters_store::{MemorySecretStore, StoreOptions};

    use super::*;

    #[test]
    fn forgets_only_that_profile() {
        let store = Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        let a = ProfileId::new_random();
        let b = ProfileId::new_random();
        search_history::record(&store, a, "news").unwrap();
        search_history::record(&store, b, "films").unwrap();
        let mut map = BTreeMap::new();
        map.insert(format!("{a}:movie"), "3".to_owned());
        map.insert(format!("{a}:series"), "4".to_owned());
        map.insert(format!("{b}:movie"), "5".to_owned());
        store.settings().set(&LAST_VOD_CATEGORY, &map).unwrap();

        forget_local_data(&store, a).unwrap();
        assert!(search_history::load(&store, a).unwrap().is_empty());
        assert_eq!(search_history::load(&store, b).unwrap(), ["films"]);
        let left = store.settings().get(&LAST_VOD_CATEGORY).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(
            left.get(&format!("{b}:movie")).map(String::as_str),
            Some("5")
        );
        // Nothing to forget: fine.
        forget_local_data(&store, a).unwrap();
        // An unknown profile: nothing deleted, no error.
        assert!(!delete_profile(&store, ProfileId::new_random()).unwrap());
    }
}

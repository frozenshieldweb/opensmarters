// SPDX-License-Identifier: GPL-3.0-or-later
//! The open profile's category management choices (hidden categories, order, region
//! filter; see [`crate::services::categories`]), cached per section so screens can read
//! them every frame.
//!
//! - [`Session::category_prefs`]: a shared snapshot (`Arc`, no copy).
//! - [`Session::category_prefs_generation`]: bumped on every change; screens rebuild a
//!   list when it moves (like the favourites generation).
//! - [`Session::change_category_prefs`]: applies a change in memory at once (optimistic),
//!   queues the store write on the FIFO writer, then re-reads what the store kept (a
//!   refused write, e.g. over the row cap, is undone that way and reported as a toast).
//! - [`Session::reload_category_prefs`]: re-reads everything (after a backup import).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use opensmarters_core::Category;
use opensmarters_store::{CategoryPrefs, ContentKind, ProfileId, Store, StoreError};

use super::Session;
use crate::services::categories::HiddenCategories;

/// Per-session cache of the category choices.
#[derive(Debug, Default)]
pub(super) struct CategoryState {
    prefs: RwLock<HashMap<ContentKind, Arc<CategoryPrefs>>>,
    generation: AtomicU64,
    /// Bumped by every optimistic change: a re-read requested before a newer change is
    /// not applied (the newer change's own re-read follows).
    edits: AtomicU64,
}

impl CategoryState {
    /// The state loaded at sign-in.
    pub(super) fn new(prefs: HashMap<ContentKind, CategoryPrefs>) -> Self {
        Self {
            prefs: RwLock::new(prefs.into_iter().map(|(k, p)| (k, Arc::new(p))).collect()),
            generation: AtomicU64::new(0),
            edits: AtomicU64::new(0),
        }
    }

    fn get(&self, kind: ContentKind) -> Arc<CategoryPrefs> {
        self.prefs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&kind)
            .cloned()
            .unwrap_or_default()
    }

    fn set_all(&self, fresh: HashMap<ContentKind, CategoryPrefs>) {
        let mut map = self.prefs.write().unwrap_or_else(PoisonError::into_inner);
        let default = CategoryPrefs::default();
        let same = ContentKind::ALL.iter().all(|k| {
            map.get(k).map_or(&default, |p| p.as_ref()) == fresh.get(k).unwrap_or(&default)
        });
        if !same {
            *map = fresh.into_iter().map(|(k, p)| (k, Arc::new(p))).collect();
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Reads every section's choices of `profile` (sign-in, reloads).
pub(super) fn read_category_prefs(
    s: &Store,
    profile: ProfileId,
) -> Result<HashMap<ContentKind, CategoryPrefs>, StoreError> {
    let mut out = HashMap::new();
    for kind in ContentKind::ALL {
        out.insert(kind, s.categories().prefs(profile, kind)?);
    }
    Ok(out)
}

impl Session {
    /// The viewer's category choices for `kind` (shared snapshot; defaults when none).
    pub fn category_prefs(&self, kind: ContentKind) -> Arc<CategoryPrefs> {
        self.categories.get(kind)
    }

    /// Bumped on every change of the category choices: rebuild filtered lists when it
    /// moves.
    pub fn category_prefs_generation(&self) -> u64 {
        self.categories.generation.load(Ordering::Relaxed)
    }

    /// The categories of `kind` hidden among `categories` right now.
    pub fn hidden_categories(
        &self,
        kind: ContentKind,
        categories: &[Category],
    ) -> HiddenCategories {
        HiddenCategories::of(categories, &self.category_prefs(kind))
    }

    /// Changes the choices of `kind`: `change` applies in memory at once, `write` is
    /// queued on the store writer (`what` names it in an error toast), then the stored
    /// state is read back.
    pub fn change_category_prefs<C, W>(
        &self,
        kind: ContentKind,
        what: &'static str,
        change: C,
        write: W,
    ) where
        C: FnOnce(&mut CategoryPrefs),
        W: FnOnce(&Store, ProfileId) -> Result<(), StoreError> + Send + 'static,
    {
        {
            let mut map = self
                .categories
                .prefs
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let entry = map.entry(kind).or_default();
            change(Arc::make_mut(entry));
        }
        self.categories.generation.fetch_add(1, Ordering::Relaxed);
        let edit = self.categories.edits.fetch_add(1, Ordering::Relaxed) + 1;
        let profile = self.profile.id;
        self.services.blocking(what, move |s| write(s, profile));
        let Some(me) = self.me.upgrade() else { return };
        self.services.spawn(async move {
            let fresh = me
                .services
                .write("read category settings", move |s| {
                    read_category_prefs(s, profile)
                })
                .await;
            if let Ok(fresh) = fresh
                && me.categories.edits.load(Ordering::Relaxed) == edit
            {
                me.categories.set_all(fresh);
                me.services.egui().request_repaint();
            }
        });
    }

    /// Re-reads the category choices from the store (behind the writes queued before).
    pub async fn reload_category_prefs(&self) {
        let profile = self.profile.id;
        let edit = self.categories.edits.load(Ordering::Relaxed);
        if let Ok(fresh) = self
            .services
            .write("read category settings", move |s| {
                read_category_prefs(s, profile)
            })
            .await
            && self.categories.edits.load(Ordering::Relaxed) == edit
        {
            self.categories.set_all(fresh);
        }
    }
}

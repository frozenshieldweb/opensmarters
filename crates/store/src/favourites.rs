//! Per-profile favourites for live channels, movies and series.
//!
//! Keyed by `(profile, kind, item_id)` — ids are compared as exact strings, so `"10"` and
//! `10` from a loosely typed panel must be normalised to the same string by the caller.
//! Removing an id that is not a favourite is a no-op (the original app removed the *last*
//! favourite in that case).
//!
//! Each favourite carries a small display snapshot so the Favourites view renders without
//! refetching. Snapshot strings come from the provider and are sanitised (control
//! characters stripped, lengths capped); over-long URLs are dropped.
//!
//! **Item ids must not be URLs.** For M3U entries use a stable non-secret id (`tvg-id`, or
//! a hash of name + group) — M3U stream URLs embed credentials. URLs are rejected.

use std::collections::HashSet;

use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, map_fk};
use crate::ids::{ContentKind, ProfileId};
use crate::text::{
    MAX_ITEM_ID_BYTES, MAX_TITLE_CHARS, MAX_URL_BYTES, sanitize_display, sanitize_opt,
    validate_item_id,
};
use crate::time::Timestamp;

/// Display data captured when an item is favourited or watched.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemSnapshot {
    /// Display name.
    pub name: String,
    /// Logo / poster URL from the provider.
    pub icon_url: Option<String>,
    /// Provider category id, for "go to category".
    pub category_id: Option<String>,
}

impl ItemSnapshot {
    /// Convenience constructor with only a name.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub(crate) fn sanitized(&self) -> Self {
        Self {
            name: sanitize_display(&self.name, MAX_TITLE_CHARS),
            icon_url: sanitize_opt(self.icon_url.as_deref(), MAX_URL_BYTES),
            category_id: sanitize_opt(self.category_id.as_deref(), MAX_ITEM_ID_BYTES),
        }
    }
}

/// A stored favourite.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Favourite {
    /// Section.
    pub kind: ContentKind,
    /// Provider id (`stream_id` / `series_id`, or the app's stable M3U id).
    pub item_id: String,
    /// Position within `(profile, kind)`; lists are returned in ascending order.
    pub position: i64,
    /// When it was first added.
    pub added_at: Timestamp,
    /// Display snapshot.
    pub snapshot: ItemSnapshot,
}

fn favourite_from_row(row: &Row<'_>) -> rusqlite::Result<Favourite> {
    Ok(Favourite {
        kind: row.get(0)?,
        item_id: row.get(1)?,
        position: row.get(2)?,
        added_at: row.get(3)?,
        snapshot: ItemSnapshot {
            name: row.get(4)?,
            icon_url: row.get(5)?,
            category_id: row.get(6)?,
        },
    })
}

/// Favourites operations. Obtain with [`Store::favourites`].
#[derive(Clone, Copy, Debug)]
pub struct Favourites<'a> {
    pub(crate) store: &'a Store,
}

impl Favourites<'_> {
    /// Adds a favourite at the end of the list. If it already exists its snapshot is
    /// refreshed and its position kept. Returns `true` if it was newly added.
    pub fn add(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        item_id: &str,
        snapshot: &ItemSnapshot,
    ) -> Result<bool> {
        let item_id = validate_item_id("item_id", item_id)?;
        let snap = snapshot.sanitized();
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let updated = tx.execute(
            "UPDATE favourites SET name = ?4, icon_url = ?5, category_id = ?6
             WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![
                profile,
                kind,
                item_id,
                snap.name,
                snap.icon_url,
                snap.category_id
            ],
        )?;
        if updated == 0 {
            tx.execute(
                "INSERT INTO favourites (profile_id, kind, item_id, position, added_at,
                                         name, icon_url, category_id)
                 VALUES (?1, ?2, ?3,
                         (SELECT COALESCE(MAX(position) + 1, 0) FROM favourites
                          WHERE profile_id = ?1 AND kind = ?2),
                         ?4, ?5, ?6, ?7)",
                params![
                    profile,
                    kind,
                    item_id,
                    self.store.now(),
                    snap.name,
                    snap.icon_url,
                    snap.category_id
                ],
            )
            .map_err(map_fk(profile))?;
        }
        tx.commit()?;
        Ok(updated == 0)
    }

    /// Removes a favourite. Returns `false` (and changes nothing) if it was not one.
    pub fn remove(&self, profile: ProfileId, kind: ContentKind, item_id: &str) -> Result<bool> {
        let n = self.store.conn().execute(
            "DELETE FROM favourites WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![profile, kind, item_id],
        )?;
        Ok(n > 0)
    }

    /// Adds the favourite if absent, removes it if present. Returns the new state.
    pub fn toggle(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        item_id: &str,
        snapshot: &ItemSnapshot,
    ) -> Result<bool> {
        if self.remove(profile, kind, item_id)? {
            Ok(false)
        } else {
            self.add(profile, kind, item_id, snapshot)?;
            Ok(true)
        }
    }

    /// True if the item is a favourite.
    pub fn contains(&self, profile: ProfileId, kind: ContentKind, item_id: &str) -> Result<bool> {
        let n: i64 = self.store.conn().prepare_cached(
            "SELECT COUNT(*) FROM favourites WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
        )?
        .query_row(params![profile, kind, item_id], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// Favourites of one section, in list order.
    pub fn list(&self, profile: ProfileId, kind: ContentKind) -> Result<Vec<Favourite>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT kind, item_id, position, added_at, name, icon_url, category_id
             FROM favourites WHERE profile_id = ?1 AND kind = ?2
             ORDER BY position, added_at, item_id",
        )?;
        let rows = stmt.query_map(params![profile, kind], favourite_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The ids of one section's favourites, for fast membership tests while rendering
    /// a channel list.
    pub fn ids(&self, profile: ProfileId, kind: ContentKind) -> Result<HashSet<String>> {
        let conn = self.store.conn();
        let mut stmt = conn
            .prepare_cached("SELECT item_id FROM favourites WHERE profile_id = ?1 AND kind = ?2")?;
        let rows = stmt.query_map(params![profile, kind], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Reorders a section: the given ids first, in that order; others keep their relative
    /// order after them. Unknown ids are ignored.
    pub fn reorder<S: AsRef<str>>(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        ordered_ids: &[S],
    ) -> Result<()> {
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let existing: Vec<String> = tx
            .prepare(
                "SELECT item_id FROM favourites WHERE profile_id = ?1 AND kind = ?2
                 ORDER BY position, added_at, item_id",
            )?
            .query_map(params![profile, kind], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let existing_set: HashSet<&str> = existing.iter().map(String::as_str).collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut order: Vec<&str> = Vec::with_capacity(existing.len());
        for id in ordered_ids {
            let id = id.as_ref();
            if let Some(&known) = existing_set.get(id)
                && seen.insert(known)
            {
                order.push(known);
            }
        }
        for id in &existing {
            if seen.insert(id.as_str()) {
                order.push(id.as_str());
            }
        }
        {
            let mut stmt = tx.prepare(
                "UPDATE favourites SET position = ?4
                 WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
            )?;
            for (pos, id) in order.iter().enumerate() {
                stmt.execute(params![
                    profile,
                    kind,
                    id,
                    i64::try_from(pos).unwrap_or(i64::MAX)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Moves one favourite to `index` (clamped to the list length). Returns `false` if it
    /// is not a favourite.
    pub fn move_to(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        item_id: &str,
        index: usize,
    ) -> Result<bool> {
        let mut ids: Vec<String> = self
            .list(profile, kind)?
            .into_iter()
            .map(|f| f.item_id)
            .collect();
        let Some(from) = ids.iter().position(|i| i == item_id) else {
            return Ok(false);
        };
        let id = ids.remove(from);
        ids.insert(index.min(ids.len()), id);
        self.reorder(profile, kind, &ids)?;
        Ok(true)
    }

    /// Removes all favourites of a profile, or only one section. Returns how many.
    pub fn clear(&self, profile: ProfileId, kind: Option<ContentKind>) -> Result<usize> {
        let conn = self.store.conn();
        Ok(match kind {
            Some(k) => conn.execute(
                "DELETE FROM favourites WHERE profile_id = ?1 AND kind = ?2",
                params![profile, k],
            )?,
            None => conn.execute("DELETE FROM favourites WHERE profile_id = ?1", [profile])?,
        })
    }
}

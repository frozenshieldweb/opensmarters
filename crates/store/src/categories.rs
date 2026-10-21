//! Category management for one profile and section (Live TV, movies, series): which
//! categories the viewer hides, the order they put them in, and the region filter.
//!
//! # Storage
//!
//! - `category_prefs (profile_id, kind, category_id, hidden, position)`: `hidden` = 1
//!   hides the category; `position` orders it (lowest first; `NULL` = provider order,
//!   after the ordered ones). A row that is neither hidden nor ordered is deleted, so the
//!   table only holds what the viewer changed.
//! - `category_regions (profile_id, kind, tag)`: the region tags the viewer selected (the
//!   short prefix of a category name, e.g. `FR` in `"FR | Sports"`), or `""` for
//!   categories without one. No rows = every region.
//!
//! Defaults change nothing: no rows means every category shown, in provider order.
//!
//! # Hostile input
//!
//! Category ids come from the provider. They are validated like every other item id
//! (non-empty, at most 256 bytes, no control characters, never a URL) and only ever reach
//! SQL as bound parameters. A profile and section keeps at most [`MAX_CATEGORY_ROWS`]
//! rows in each table (a hostile list of a million categories cannot grow the
//! database); a change that would go beyond is refused as a whole. Region tags are 1–4
//! ASCII capital letters or digits, or empty.
//!
//! ```
//! use std::sync::Arc;
//! use opensmarters_store::{ContentKind, MemorySecretStore, NewProfile, NewProfileSource,
//!     ProfileSettings, Store, StoreOptions};
//! # use secrecy::SecretString;
//! let store = Store::open(StoreOptions::in_memory(), Arc::new(MemorySecretStore::new()))?;
//! # let p = store.profiles().create(NewProfile {
//! #     name: "Home".into(),
//! #     source: NewProfileSource::Xtream {
//! #         server_url: "http://panel.example.invalid".into(),
//! #         username: "u".into(),
//! #         password: SecretString::from("pw"),
//! #     },
//! #     settings: ProfileSettings::default(),
//! # })?.id;
//! let cats = store.categories();
//! cats.set_hidden(p, ContentKind::Live, "12", true)?;
//! cats.set_order(p, ContentKind::Live, &["7", "3"])?;
//! cats.set_regions(p, ContentKind::Live, &["FR", ""])?;
//! let prefs = cats.prefs(p, ContentKind::Live)?;
//! assert!(prefs.hidden.contains("12"));
//! assert_eq!(prefs.order, ["7", "3"]);
//! assert!(prefs.regions.contains("FR"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::HashSet;

use rusqlite::{Transaction, params};

use crate::Store;
use crate::error::{Result, StoreError, map_fk};
use crate::ids::{ContentKind, ProfileId};
use crate::text::validate_item_id;

/// Rows kept per profile and section in each table (hidden/ordered categories, regions).
pub const MAX_CATEGORY_ROWS: usize = 5_000;
/// Longest region tag (characters).
pub const MAX_REGION_TAG_CHARS: usize = 4;

/// What the viewer changed about one section's categories (see the [module docs](self)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CategoryPrefs {
    /// Categories hidden by the viewer.
    pub hidden: HashSet<String>,
    /// Categories in the viewer's order (by position); the others follow in provider order.
    pub order: Vec<String>,
    /// Region tags shown (`""` = categories without a tag). Empty = every region.
    pub regions: HashSet<String>,
}

impl CategoryPrefs {
    /// True when nothing was changed (all shown, provider order, no region filter).
    pub fn is_default(&self) -> bool {
        self.hidden.is_empty() && self.order.is_empty() && self.regions.is_empty()
    }
}

/// True when `id` can be stored (the same rule every write applies): callers leave out a
/// provider's unusable ids instead of failing a whole change because of one.
pub fn is_valid_id(id: &str) -> bool {
    validate_item_id("category_id", id).is_ok()
}

/// Checks a region tag: empty (untagged categories) or 1–4 ASCII capital letters/digits.
pub(crate) fn validate_region(tag: &str) -> Result<&str> {
    let ok = tag.len() <= MAX_REGION_TAG_CHARS
        && tag
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if ok {
        Ok(tag)
    } else {
        Err(StoreError::invalid(
            "region",
            "must be empty or 1 to 4 capital letters or digits",
        ))
    }
}

/// Validates every id (all or nothing) and drops repeats, keeping the first.
fn valid_ids<S: AsRef<str>>(ids: &[S]) -> Result<Vec<&str>> {
    if ids.len() > MAX_CATEGORY_ROWS {
        return Err(too_many());
    }
    let mut seen = HashSet::with_capacity(ids.len());
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let id = validate_item_id("category_id", id.as_ref())?;
        if seen.insert(id) {
            out.push(id);
        }
    }
    Ok(out)
}

fn valid_regions<S: AsRef<str>>(tags: &[S]) -> Result<Vec<&str>> {
    if tags.len() > MAX_CATEGORY_ROWS {
        return Err(too_many());
    }
    let mut seen = HashSet::with_capacity(tags.len());
    let mut out = Vec::with_capacity(tags.len());
    for t in tags {
        let t = validate_region(t.as_ref())?;
        if seen.insert(t) {
            out.push(t);
        }
    }
    Ok(out)
}

fn too_many() -> StoreError {
    StoreError::invalid(
        "categories",
        "too many categories (at most 5000 per section)",
    )
}

fn set_hidden_tx(
    tx: &Transaction<'_>,
    profile: ProfileId,
    kind: ContentKind,
    id: &str,
    hidden: bool,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO category_prefs (profile_id, kind, category_id, hidden)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (profile_id, kind, category_id) DO UPDATE SET hidden = excluded.hidden",
    )?
    .execute(params![profile, kind, id, hidden])
    .map_err(map_fk(profile))?;
    Ok(())
}

fn set_position_tx(
    tx: &Transaction<'_>,
    profile: ProfileId,
    kind: ContentKind,
    id: &str,
    position: i64,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO category_prefs (profile_id, kind, category_id, position)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (profile_id, kind, category_id) DO UPDATE SET position = excluded.position",
    )?
    .execute(params![profile, kind, id, position])
    .map_err(map_fk(profile))?;
    Ok(())
}

fn insert_region_tx(
    tx: &Transaction<'_>,
    profile: ProfileId,
    kind: ContentKind,
    tag: &str,
) -> Result<usize> {
    tx.prepare_cached(
        "INSERT INTO category_regions (profile_id, kind, tag) VALUES (?1, ?2, ?3)
         ON CONFLICT DO NOTHING",
    )?
    .execute(params![profile, kind, tag])
    .map_err(map_fk(profile))
}

/// Deletes rows that are neither hidden nor ordered, then enforces the row cap.
fn finish_tx(tx: &Transaction<'_>, profile: ProfileId, kind: ContentKind) -> Result<()> {
    tx.execute(
        "DELETE FROM category_prefs
         WHERE profile_id = ?1 AND kind = ?2 AND hidden = 0 AND position IS NULL",
        params![profile, kind],
    )?;
    check_cap(tx, profile, kind)
}

/// Fails when a profile's section holds more rows than [`MAX_CATEGORY_ROWS`] in either
/// table (the caller's transaction is then rolled back).
pub(crate) fn check_cap(tx: &Transaction<'_>, profile: ProfileId, kind: ContentKind) -> Result<()> {
    for sql in [
        "SELECT COUNT(*) FROM category_prefs WHERE profile_id = ?1 AND kind = ?2",
        "SELECT COUNT(*) FROM category_regions WHERE profile_id = ?1 AND kind = ?2",
    ] {
        let n: i64 = tx.query_row(sql, params![profile, kind], |r| r.get(0))?;
        if usize::try_from(n).unwrap_or(usize::MAX) > MAX_CATEGORY_ROWS {
            return Err(too_many());
        }
    }
    Ok(())
}

/// One stored row of `category_prefs` (export).
pub(crate) struct PrefRow {
    pub(crate) category_id: String,
    pub(crate) hidden: bool,
    pub(crate) position: Option<i64>,
}

/// Category management. Obtain with [`Store::categories`].
#[derive(Clone, Copy, Debug)]
pub struct Categories<'a> {
    pub(crate) store: &'a Store,
}

impl Categories<'_> {
    /// The viewer's choices for one section (defaults when nothing was changed).
    pub fn prefs(&self, profile: ProfileId, kind: ContentKind) -> Result<CategoryPrefs> {
        let mut prefs = CategoryPrefs::default();
        for row in self.rows(profile, kind)? {
            if row.hidden {
                prefs.hidden.insert(row.category_id.clone());
            }
            if row.position.is_some() {
                prefs.order.push(row.category_id);
            }
        }
        prefs.regions = self.regions(profile, kind)?.into_iter().collect();
        Ok(prefs)
    }

    /// The stored rows, ordered ones first (by position), then the hidden-only ones.
    pub(crate) fn rows(&self, profile: ProfileId, kind: ContentKind) -> Result<Vec<PrefRow>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT category_id, hidden, position FROM category_prefs
             WHERE profile_id = ?1 AND kind = ?2
             ORDER BY position IS NULL, position, category_id",
        )?;
        let rows = stmt.query_map(params![profile, kind], |r| {
            Ok(PrefRow {
                category_id: r.get(0)?,
                hidden: r.get(1)?,
                position: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The selected region tags, sorted.
    pub(crate) fn regions(&self, profile: ProfileId, kind: ContentKind) -> Result<Vec<String>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT tag FROM category_regions WHERE profile_id = ?1 AND kind = ?2 ORDER BY tag",
        )?;
        let rows = stmt.query_map(params![profile, kind], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Hides (`true`) or shows one category.
    pub fn set_hidden(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        category_id: &str,
        hidden: bool,
    ) -> Result<()> {
        self.set_hidden_many(profile, kind, &[category_id], hidden)
    }

    /// Hides or shows several categories at once (all or nothing).
    pub fn set_hidden_many<S: AsRef<str>>(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        category_ids: &[S],
        hidden: bool,
    ) -> Result<()> {
        let ids = valid_ids(category_ids)?;
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        for id in ids {
            set_hidden_tx(&tx, profile, kind, id, hidden)?;
        }
        finish_tx(&tx, profile, kind)?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the viewer's order: these categories first, in this order (repeats
    /// ignored); every other category follows in provider order. An empty list goes back
    /// to provider order. Hidden flags are kept.
    pub fn set_order<S: AsRef<str>>(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        category_ids: &[S],
    ) -> Result<()> {
        let ids = valid_ids(category_ids)?;
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE category_prefs SET position = NULL WHERE profile_id = ?1 AND kind = ?2",
            params![profile, kind],
        )?;
        for (pos, id) in ids.into_iter().enumerate() {
            set_position_tx(
                &tx,
                profile,
                kind,
                id,
                i64::try_from(pos).unwrap_or(i64::MAX),
            )?;
        }
        finish_tx(&tx, profile, kind)?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the region filter (`""` = categories without a tag; an empty list shows
    /// every region).
    pub fn set_regions<S: AsRef<str>>(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        tags: &[S],
    ) -> Result<()> {
        let tags = valid_regions(tags)?;
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM category_regions WHERE profile_id = ?1 AND kind = ?2",
            params![profile, kind],
        )?;
        for t in tags {
            insert_region_tx(&tx, profile, kind, t)?;
        }
        check_cap(&tx, profile, kind)?;
        tx.commit()?;
        Ok(())
    }

    /// Back to the defaults for one section: every category shown, provider order, no
    /// region filter.
    pub fn reset(&self, profile: ProfileId, kind: ContentKind) -> Result<()> {
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM category_prefs WHERE profile_id = ?1 AND kind = ?2",
            params![profile, kind],
        )?;
        tx.execute(
            "DELETE FROM category_regions WHERE profile_id = ?1 AND kind = ?2",
            params![profile, kind],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces everything stored for one section with `prefs` in one transaction (the
    /// editor's Undo). Validated like the other calls.
    pub fn replace(
        &self,
        profile: ProfileId,
        kind: ContentKind,
        prefs: &CategoryPrefs,
    ) -> Result<()> {
        let order = valid_ids(&prefs.order)?;
        let hidden: Vec<&String> = {
            let mut h: Vec<&String> = prefs.hidden.iter().collect();
            h.sort();
            h
        };
        let hidden = valid_ids(&hidden)?;
        let regions: Vec<&String> = prefs.regions.iter().collect();
        let regions = valid_regions(&regions)?;
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM category_prefs WHERE profile_id = ?1 AND kind = ?2",
            params![profile, kind],
        )?;
        tx.execute(
            "DELETE FROM category_regions WHERE profile_id = ?1 AND kind = ?2",
            params![profile, kind],
        )?;
        for (pos, id) in order.into_iter().enumerate() {
            set_position_tx(
                &tx,
                profile,
                kind,
                id,
                i64::try_from(pos).unwrap_or(i64::MAX),
            )?;
        }
        for id in hidden {
            set_hidden_tx(&tx, profile, kind, id, true)?;
        }
        for t in regions {
            insert_region_tx(&tx, profile, kind, t)?;
        }
        finish_tx(&tx, profile, kind)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_tags() {
        for ok in ["", "F", "FR", "UK", "DE", "4K", "ABCD", "B2B"] {
            assert!(validate_region(ok).is_ok(), "{ok:?}");
        }
        for bad in ["fr", "ABCDE", "F R", "É", "FR|", "\u{0}", "Fr"] {
            assert!(validate_region(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ids_are_validated_and_deduplicated() {
        assert!(is_valid_id("12"));
        assert!(!is_valid_id("http://h.example.invalid/x"));
        let ids = valid_ids(&["1", "2", "1"]).unwrap_or_default();
        assert_eq!(ids, ["1", "2"]);
        assert!(valid_ids(&["ok", "http://h.example.invalid/x"]).is_err());
        assert!(valid_ids(&[""]).is_err());
        let many: Vec<String> = (0..=MAX_CATEGORY_ROWS).map(|i| i.to_string()).collect();
        assert!(valid_ids(&many).is_err());
    }
}

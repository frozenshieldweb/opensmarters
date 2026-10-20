// SPDX-License-Identifier: GPL-3.0-or-later
//! Parental locks over **every** category of an item.
//!
//! A channel, film or series can sit in several categories (`category_ids` on newer
//! panels, several M3U groups). It is locked when *any* of them is locked: checking only
//! the primary category would let an item of a locked category through via another one
//! (security review SR-15). Keyword locks match category **names**, so callers pass the
//! names they know ([`CategoryNames`]); with no names only manual (per-id) locks apply,
//! which is why Search loads the category lists before it lists films and series while
//! parental control is on.

use std::collections::HashMap;

use opensmarters_core::{Category, CategoryId};
use opensmarters_store::ContentKind;

use crate::services::Session;

/// Category names by id.
#[derive(Debug, Clone, Default)]
pub struct CategoryNames(HashMap<CategoryId, String>);

impl CategoryNames {
    /// Indexes `categories`.
    pub fn new<'a>(categories: impl IntoIterator<Item = &'a Category>) -> Self {
        Self(
            categories
                .into_iter()
                .map(|c| (c.id.clone(), c.name.clone()))
                .collect(),
        )
    }

    /// The name of `id` (`""` when unknown).
    pub fn name(&self, id: &CategoryId) -> &str {
        self.0.get(id).map_or("", String::as_str)
    }

    /// True when no name is known.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The categories of an item: the primary one first, then the others, without repeats.
pub fn item_categories<'a>(
    primary: Option<&'a CategoryId>,
    all: &'a [CategoryId],
) -> Vec<&'a CategoryId> {
    let mut out: Vec<&CategoryId> = Vec::with_capacity(all.len() + 1);
    for id in primary.into_iter().chain(all) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// True when `is_locked` says yes for any of `ids`.
pub fn any_locked<'a>(
    ids: impl IntoIterator<Item = &'a CategoryId>,
    is_locked: impl FnMut(&CategoryId) -> bool,
) -> bool {
    ids.into_iter().any(is_locked)
}

/// True when the item (in categories `ids`) is locked for this session.
///
/// TODO(integration): switch to `Session::is_item_locked(kind, ids)` once it lands
/// (batch B); this local version checks the same thing with the names it is given.
pub fn is_item_locked(
    session: &Session,
    kind: ContentKind,
    ids: &[&CategoryId],
    names: &CategoryNames,
) -> bool {
    session.parental_enabled()
        && any_locked(ids.iter().copied(), |id| {
            session.is_category_locked(kind, id, names.name(id))
        })
}

/// The categories among `ids` that are locked for this session (to unlock them for the
/// session after the PIN was entered).
pub fn locked_among(
    session: &Session,
    kind: ContentKind,
    ids: &[&CategoryId],
    names: &CategoryNames,
) -> Vec<CategoryId> {
    if !session.parental_enabled() {
        return Vec::new();
    }
    ids.iter()
        .filter(|id| session.is_category_locked(kind, id, names.name(id)))
        .map(|id| (*id).clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> CategoryId {
        CategoryId::from(s.to_owned())
    }

    #[test]
    fn every_category_counts() {
        let primary = id("1");
        let all = vec![id("1"), id("7"), id("9")];
        let ids = item_categories(Some(&primary), &all);
        assert_eq!(
            ids.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            ["1", "7", "9"],
            "primary first, no repeats"
        );
        // Only a secondary category is locked: the item is locked.
        assert!(any_locked(ids.iter().copied(), |c| c.as_str() == "9"));
        assert!(!any_locked(ids.iter().copied(), |c| c.as_str() == "2"));
        // No category at all: never locked.
        assert!(item_categories(None, &[]).is_empty());
        assert!(!any_locked(item_categories(None, &[]), |_| true));
    }

    #[test]
    fn names_for_keyword_locks() {
        let cats = vec![
            Category {
                id: id("5"),
                name: "Adult Night".into(),
                parent_id: None,
            },
            Category {
                id: id("6"),
                name: "Kids".into(),
                parent_id: None,
            },
        ];
        let names = CategoryNames::new(&cats);
        assert_eq!(names.name(&id("5")), "Adult Night");
        assert_eq!(names.name(&id("404")), "");
        assert!(!names.is_empty());
        assert!(CategoryNames::default().is_empty());
        let keyword = |c: &CategoryId| names.name(c).to_lowercase().contains("adult");
        assert!(any_locked([&id("6"), &id("5")], keyword));
        assert!(!any_locked([&id("6")], keyword));
    }
}

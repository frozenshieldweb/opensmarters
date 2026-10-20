// SPDX-License-Identifier: GPL-3.0-or-later
//! The search engine behind the Search screen: a pre-normalised corpus (diacritics- and
//! case-insensitive, never a regex) with ranking and incremental narrowing.
//!
//! Ranking, best first: exact name → name starts with the query → a word starts with
//! the query → anywhere; then shorter names, then the catalogue order. Names are ranked
//! without their provider tag (`"FR | Title"` ranks as `"Title"`) but still match on it,
//! so typing `fr` finds tagged items.
//!
//! **Incremental:** substring matches only shrink as the query grows, so a query that
//! extends the previous one is searched within the previous matches.

use opensmarters_core::catalog::normalize_for_search;

use crate::services::catalog::split_tag;

/// Result groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Live channels.
    Channels,
    /// Films.
    Movies,
    /// Series.
    Series,
    /// EPG programmes.
    Programmes,
}

impl Kind {
    /// Every group, in display order.
    pub const ALL: [Kind; 4] = [Kind::Channels, Kind::Movies, Kind::Series, Kind::Programmes];

    /// Group title.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Channels => "Channels",
            Kind::Movies => "Films",
            Kind::Series => "Series",
            Kind::Programmes => "Programmes",
        }
    }

    fn slot(self) -> usize {
        match self {
            Kind::Channels => 0,
            Kind::Movies => 1,
            Kind::Series => 2,
            Kind::Programmes => 3,
        }
    }
}

/// One searchable name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Normalised full name.
    full: String,
    /// Normalised name without the provider tag.
    short: String,
}

impl Entry {
    fn new(name: &str) -> Self {
        let full = normalize_for_search(name);
        let short = normalize_for_search(split_tag(name).1);
        Self { full, short }
    }
}

/// Match quality (lower is better), `None` when `needle` is not in `hay`.
pub fn rank(hay: &str, needle: &str) -> Option<u8> {
    if needle.is_empty() {
        return Some(3);
    }
    if hay == needle {
        return Some(0);
    }
    if hay.starts_with(needle) {
        return Some(1);
    }
    let mut any = false;
    for (i, _) in hay.match_indices(needle) {
        any = true;
        let before = hay[..i].chars().next_back();
        if before.is_none_or(|c| !c.is_alphanumeric()) {
            return Some(2);
        }
    }
    any.then_some(3)
}

/// The searchable names of one session, by group (indices refer to the caller's lists).
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    groups: [Vec<Entry>; 4],
}

impl Corpus {
    /// Adds names of a group, in catalogue order.
    pub fn add<'a>(&mut self, kind: Kind, names: impl IntoIterator<Item = &'a str>) {
        self.groups[kind.slot()].extend(names.into_iter().map(Entry::new));
    }

    /// Items in a group.
    pub fn len(&self, kind: Kind) -> usize {
        self.groups[kind.slot()].len()
    }

    /// True when nothing is searchable.
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(Vec::is_empty)
    }

    /// Searches `query`, narrowing `previous` when the query extends its query.
    pub fn search(&self, query: &str, previous: Option<&Hits>) -> Hits {
        let needle = normalize_for_search(query);
        let narrow = previous.filter(|p| {
            !p.needle.is_empty() && needle.starts_with(&p.needle) && p.sizes == self.sizes()
        });
        let mut groups: [Vec<u32>; 4] = Default::default();
        for kind in Kind::ALL {
            let entries = &self.groups[kind.slot()];
            let mut scored: Vec<(u8, usize, u32)> = Vec::new();
            let mut consider = |i: u32| {
                let Some(e) = entries.get(i as usize) else {
                    return;
                };
                let r =
                    rank(&e.short, &needle).or_else(|| rank(&e.full, &needle).map(|r| r.max(2)));
                if let Some(r) = r {
                    scored.push((r, e.short.len(), i));
                }
            };
            if needle.is_empty() {
                continue;
            }
            match narrow {
                Some(p) => p.groups[kind.slot()]
                    .iter()
                    .copied()
                    .for_each(&mut consider),
                None => (0..entries.len() as u32).for_each(&mut consider),
            }
            scored.sort_unstable();
            groups[kind.slot()] = scored.into_iter().map(|(_, _, i)| i).collect();
        }
        Hits {
            needle,
            sizes: self.sizes(),
            groups,
        }
    }

    fn sizes(&self) -> [usize; 4] {
        [
            self.groups[0].len(),
            self.groups[1].len(),
            self.groups[2].len(),
            self.groups[3].len(),
        ]
    }
}

/// Search results: per group, item indices best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits {
    needle: String,
    sizes: [usize; 4],
    groups: [Vec<u32>; 4],
}

impl Hits {
    /// The normalised query.
    pub fn needle(&self) -> &str {
        &self.needle
    }

    /// Matches of a group, best first.
    pub fn of(&self, kind: Kind) -> &[u32] {
        &self.groups[kind.slot()]
    }

    /// Total matches.
    pub fn total(&self) -> usize {
        self.groups.iter().map(Vec::len).sum()
    }
}

/// Recent searches: newest first, case-insensitive de-duplication, at most `cap`.
pub fn push_recent(list: &mut Vec<String>, query: &str, cap: usize) {
    let q = query.trim();
    if q.is_empty() {
        return;
    }
    let key = normalize_for_search(q);
    list.retain(|s| normalize_for_search(s) != key);
    list.insert(0, crate::ui::text::cap(q, 80).into_owned());
    list.truncate(cap);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks() {
        assert_eq!(rank("news", "news"), Some(0));
        assert_eq!(rank("news 24", "news"), Some(1));
        assert_eq!(rank("sky news", "news"), Some(2));
        assert_eq!(rank("tvnews", "news"), Some(3));
        assert_eq!(rank("sport", "news"), None);
        // A later word-start beats an earlier mid-word hit.
        assert_eq!(rank("kanews (news)", "news"), Some(2));
    }

    #[test]
    fn search_is_ranked_accent_insensitive_and_tag_aware() {
        let mut c = Corpus::default();
        c.add(
            Kind::Movies,
            [
                "Le Café des Étoiles",
                "FR | Café",
                "Cafeteria Nights",
                "Decafe",
                "The Café",
                "Unrelated",
            ],
        );
        let h = c.search("CAFE", None);
        let names: Vec<u32> = h.of(Kind::Movies).to_vec();
        // Exact (tag stripped) → prefix → word start (shorter first) → anywhere.
        assert_eq!(names, vec![1, 2, 4, 0, 3]);
        // Typing the tag finds tagged items.
        assert_eq!(c.search("fr", None).of(Kind::Movies), &[1]);
        // No regex: special characters are plain text.
        assert!(c.search("(.*)", None).of(Kind::Movies).is_empty());
        assert_eq!(h.total(), 5);
        assert!(c.search("   ", None).total() == 0);
    }

    #[test]
    fn incremental_narrowing_matches_a_full_search() {
        let mut c = Corpus::default();
        let names: Vec<String> = (0..500).map(|i| format!("Channel {i} Sport")).collect();
        c.add(Kind::Channels, names.iter().map(String::as_str));
        c.add(Kind::Programmes, ["Sports Night", "Match of the Day"]);
        let first = c.search("spo", None);
        let narrowed = c.search("sport", Some(&first));
        let full = c.search("sport", None);
        assert_eq!(narrowed, full);
        assert_eq!(full.of(Kind::Programmes), &[0]);
        // A different corpus (sizes changed) is never narrowed.
        let mut c2 = c.clone();
        c2.add(Kind::Series, ["Sport Stories"]);
        assert_eq!(c2.search("sport", Some(&first)).of(Kind::Series), &[0]);
    }

    #[test]
    fn recent_searches() {
        let mut r = vec!["news".to_owned(), "Café".to_owned()];
        push_recent(&mut r, "  cafe ", 3);
        assert_eq!(r, ["cafe", "news"]);
        push_recent(&mut r, "sport", 2);
        assert_eq!(r, ["sport", "cafe"]);
        push_recent(&mut r, "   ", 2);
        assert_eq!(r.len(), 2);
    }
}

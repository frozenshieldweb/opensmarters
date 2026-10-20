// SPDX-License-Identifier: GPL-3.0-or-later
//! Catalogue helpers: pseudo-categories, search and sorting.
//!
//! - [`CategorySelection`] models *All* / *Favourites* / a real category as a type
//!   (the original sent the magic strings `ALL` / `FAVOURITE` to the server).
//!   [`CategorySelection::server_category`] tells the Xtream client what to request.
//! - [`SearchQuery`] / [`SearchIndex`]: case-insensitive, **diacritics-insensitive
//!   substring** search. User input is never turned into a regex, so `(`, `[` or `c++`
//!   are just characters.
//! - [`SortOrder`]: natural, case-insensitive name order ("Channel 2" < "Channel 10");
//!   numbers compared numerically; added/rating newest/highest first; **missing values
//!   always last**. Sorting is stable and never mutates shared data unless asked
//!   ([`sorted_indices`] leaves the input alone).
//!
//! Works on anything implementing [`CatalogItem`]: [`LiveStream`], [`VodStream`],
//! [`Series`] and [`M3uEntry`] are provided.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::hash::Hash;

use chrono::{DateTime, Utc};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::m3u::M3uEntry;
use crate::model::{CategoryId, LiveStream, Series, VodStream};

/// What the user selected in the category list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum CategorySelection {
    /// Everything (pseudo-category).
    #[default]
    All,
    /// The account's favourites (pseudo-category, filtered locally).
    Favourites,
    /// A real category.
    Category(CategoryId),
}

impl CategorySelection {
    /// The category to request from the server: `None` (= omit `category_id`) for All
    /// and Favourites.
    pub fn server_category(&self) -> Option<&CategoryId> {
        match self {
            CategorySelection::Category(id) => Some(id),
            CategorySelection::All | CategorySelection::Favourites => None,
        }
    }

    /// `true` for the two pseudo-categories.
    pub fn is_pseudo(&self) -> bool {
        !matches!(self, CategorySelection::Category(_))
    }
}

/// A list item that can be searched, sorted and filtered.
pub trait CatalogItem {
    /// Key used for favourites (stream id, series id, or URL for M3U).
    type Key: Eq + Hash;

    /// Favourite key.
    fn key(&self) -> Self::Key;
    /// Display name.
    fn name(&self) -> &str;
    /// Channel number, if any.
    fn number(&self) -> Option<u64> {
        None
    }
    /// When it was added / last modified.
    fn added(&self) -> Option<DateTime<Utc>> {
        None
    }
    /// Rating out of 5.
    fn rating(&self) -> Option<f64> {
        None
    }
    /// Whether the item belongs to `category`.
    fn in_category(&self, category: &CategoryId) -> bool;
}

impl CatalogItem for LiveStream {
    type Key = u64;

    fn key(&self) -> u64 {
        self.stream_id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn number(&self) -> Option<u64> {
        self.num
    }
    fn added(&self) -> Option<DateTime<Utc>> {
        self.added
    }
    fn in_category(&self, category: &CategoryId) -> bool {
        self.category_id.as_ref() == Some(category) || self.category_ids.contains(category)
    }
}

impl CatalogItem for VodStream {
    type Key = u64;

    fn key(&self) -> u64 {
        self.stream_id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn number(&self) -> Option<u64> {
        self.num
    }
    fn added(&self) -> Option<DateTime<Utc>> {
        self.added
    }
    fn rating(&self) -> Option<f64> {
        self.rating_out_of_5()
    }
    fn in_category(&self, category: &CategoryId) -> bool {
        self.category_id.as_ref() == Some(category) || self.category_ids.contains(category)
    }
}

impl CatalogItem for Series {
    type Key = u64;

    fn key(&self) -> u64 {
        self.series_id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn number(&self) -> Option<u64> {
        self.num
    }
    fn added(&self) -> Option<DateTime<Utc>> {
        self.last_modified
    }
    fn rating(&self) -> Option<f64> {
        self.rating_out_of_5()
    }
    fn in_category(&self, category: &CategoryId) -> bool {
        self.category_id.as_ref() == Some(category) || self.category_ids.contains(category)
    }
}

impl CatalogItem for M3uEntry {
    type Key = String;

    fn key(&self) -> String {
        self.url.clone()
    }
    fn name(&self) -> &str {
        &self.title
    }
    fn number(&self) -> Option<u64> {
        self.tvg_chno.map(u64::from)
    }
    fn in_category(&self, category: &CategoryId) -> bool {
        self.group_title.as_deref() == Some(category.as_str())
    }
}

/// Items matching a category selection, in input order. `favourites` is only consulted
/// for [`CategorySelection::Favourites`].
pub fn filter_by_selection<'a, T: CatalogItem>(
    items: &'a [T],
    selection: &CategorySelection,
    favourites: &HashSet<T::Key>,
) -> Vec<&'a T> {
    match selection {
        CategorySelection::All => items.iter().collect(),
        CategorySelection::Favourites => items
            .iter()
            .filter(|i| favourites.contains(&i.key()))
            .collect(),
        CategorySelection::Category(id) => items.iter().filter(|i| i.in_category(id)).collect(),
    }
}

// ---------------------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------------------

/// Letters that do not decompose under NFKD but should match their ASCII base.
fn fold_special(c: char, out: &mut String) {
    match c {
        'ß' => out.push_str("ss"),
        'æ' => out.push_str("ae"),
        'œ' => out.push_str("oe"),
        'ø' => out.push('o'),
        'đ' | 'ð' => out.push('d'),
        'ł' => out.push('l'),
        'ħ' => out.push('h'),
        'ı' => out.push('i'),
        'þ' => out.push_str("th"),
        _ => out.push(c),
    }
}

/// Normalises text for search and sorting: NFKD, combining marks removed, lower-cased,
/// a few special letters folded (`ß`→`ss`, `ø`→`o`, …), whitespace collapsed.
pub fn normalize_for_search(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.nfkd().filter(|c| !is_combining_mark(*c)) {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        last_space = false;
        for lc in c.to_lowercase() {
            fold_special(lc, &mut out);
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// A normalised search query (plain substring; never a regex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    needle: String,
}

impl SearchQuery {
    /// Normalises `query`. Leading/trailing whitespace is ignored.
    pub fn new(query: &str) -> Self {
        SearchQuery {
            needle: normalize_for_search(query),
        }
    }

    /// `true` for an empty query (matches everything).
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Whether `text` contains the query (case- and diacritics-insensitive).
    pub fn matches(&self, text: &str) -> bool {
        self.is_empty() || normalize_for_search(text).contains(&self.needle)
    }

    /// Whether an already-normalised text contains the query.
    pub fn matches_normalized(&self, normalized: &str) -> bool {
        self.is_empty() || normalized.contains(&self.needle)
    }
}

/// Pre-normalised names for fast repeated searches over a large list.
#[derive(Debug, Clone, Default)]
pub struct SearchIndex {
    names: Vec<String>,
}

impl SearchIndex {
    /// Indexes the names of `items`.
    pub fn new<T: CatalogItem>(items: &[T]) -> Self {
        SearchIndex {
            names: items
                .iter()
                .map(|i| normalize_for_search(i.name()))
                .collect(),
        }
    }

    /// Indices of the items whose name matches, in order.
    pub fn search(&self, query: &SearchQuery) -> Vec<usize> {
        self.names
            .iter()
            .enumerate()
            .filter(|(_, n)| query.matches_normalized(n))
            .map(|(i, _)| i)
            .collect()
    }

    /// Number of indexed items.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// `true` when nothing is indexed.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Items whose name matches `query`, in input order.
pub fn search<'a, T: CatalogItem>(items: &'a [T], query: &SearchQuery) -> Vec<&'a T> {
    items.iter().filter(|i| query.matches(i.name())).collect()
}

// ---------------------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------------------

/// Sort options (the original's `default`, `atoz`, `ztoa`, `channelnum_asc`,
/// `channelnum_desc`, `topadded`, `toprated`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortOrder {
    /// Server order.
    #[default]
    Server,
    /// Name A→Z (natural, case- and diacritics-insensitive).
    NameAsc,
    /// Name Z→A.
    NameDesc,
    /// Number ascending (missing last).
    NumberAsc,
    /// Number descending (missing last).
    NumberDesc,
    /// Most recently added first (missing last).
    RecentlyAdded,
    /// Highest rated first (missing last).
    TopRated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Chunk {
    Text(String),
    /// Digits without leading zeros, plus the original length for tie-breaking.
    Num(String, usize),
}

/// A precomputed natural-order key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaturalKey {
    chunks: Vec<Chunk>,
}

impl NaturalKey {
    /// Builds the key for `s`.
    pub fn new(s: &str) -> Self {
        let norm = normalize_for_search(s);
        let mut chunks = Vec::new();
        let mut text = String::new();
        let mut digits = String::new();
        for c in norm.chars() {
            if c.is_ascii_digit() {
                if !text.is_empty() {
                    chunks.push(Chunk::Text(std::mem::take(&mut text)));
                }
                digits.push(c);
            } else {
                if !digits.is_empty() {
                    chunks.push(num_chunk(std::mem::take(&mut digits)));
                }
                text.push(c);
            }
        }
        if !text.is_empty() {
            chunks.push(Chunk::Text(text));
        }
        if !digits.is_empty() {
            chunks.push(num_chunk(digits));
        }
        NaturalKey { chunks }
    }
}

fn num_chunk(digits: String) -> Chunk {
    let len = digits.len();
    let trimmed = digits.trim_start_matches('0');
    Chunk::Num(
        if trimmed.is_empty() {
            "0".to_owned()
        } else {
            trimmed.to_owned()
        },
        len,
    )
}

fn cmp_chunk(a: &Chunk, b: &Chunk) -> Ordering {
    match (a, b) {
        (Chunk::Num(x, xl), Chunk::Num(y, yl)) => x
            .len()
            .cmp(&y.len())
            .then_with(|| x.cmp(y))
            .then_with(|| xl.cmp(yl)),
        (Chunk::Text(x), Chunk::Text(y)) => x.cmp(y),
        // Numbers sort before text ("2 Broke" < "Abc").
        (Chunk::Num(..), Chunk::Text(_)) => Ordering::Less,
        (Chunk::Text(_), Chunk::Num(..)) => Ordering::Greater,
    }
}

impl Ord for NaturalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        for (a, b) in self.chunks.iter().zip(&other.chunks) {
            let o = cmp_chunk(a, b);
            if o != Ordering::Equal {
                return o;
            }
        }
        self.chunks.len().cmp(&other.chunks.len())
    }
}

impl PartialOrd for NaturalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Natural, case- and diacritics-insensitive comparison of two names.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    NaturalKey::new(a)
        .cmp(&NaturalKey::new(b))
        .then_with(|| a.cmp(b))
}

/// Compares two optional values, `None` last regardless of direction.
fn missing_last<T>(a: Option<T>, b: Option<T>, cmp: impl Fn(&T, &T) -> Ordering) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => cmp(&x, &y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The permutation that sorts `items` by `order` (stable). The input is not touched,
/// so cached server lists stay in server order.
pub fn sorted_indices<T: CatalogItem>(items: &[T], order: SortOrder) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..items.len()).collect();
    match order {
        SortOrder::Server => {}
        SortOrder::NameAsc | SortOrder::NameDesc => {
            let keys: Vec<NaturalKey> = items.iter().map(|i| NaturalKey::new(i.name())).collect();
            idx.sort_by(|&a, &b| {
                let o = keys[a]
                    .cmp(&keys[b])
                    .then_with(|| items[a].name().cmp(items[b].name()));
                if order == SortOrder::NameDesc {
                    o.reverse()
                } else {
                    o
                }
            });
        }
        SortOrder::NumberAsc => {
            idx.sort_by(|&a, &b| {
                missing_last(items[a].number(), items[b].number(), |x, y| x.cmp(y))
            });
        }
        SortOrder::NumberDesc => {
            idx.sort_by(|&a, &b| {
                missing_last(items[a].number(), items[b].number(), |x, y| y.cmp(x))
            });
        }
        SortOrder::RecentlyAdded => {
            idx.sort_by(|&a, &b| missing_last(items[a].added(), items[b].added(), |x, y| y.cmp(x)));
        }
        SortOrder::TopRated => {
            idx.sort_by(|&a, &b| {
                missing_last(items[a].rating(), items[b].rating(), |x, y| y.total_cmp(x))
            });
        }
    }
    idx
}

/// Sorts `items` in place by `order` (stable).
pub fn sort_items<T: CatalogItem>(items: &mut Vec<T>, order: SortOrder) {
    if order == SortOrder::Server {
        return;
    }
    let idx = sorted_indices(items, order);
    let mut slots: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.extend(
        idx.into_iter()
            .filter_map(|i| slots.get_mut(i).and_then(Option::take)),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct It {
        id: u64,
        name: &'static str,
        num: Option<u64>,
        added: Option<i64>,
        rating: Option<f64>,
        cat: &'static str,
    }

    impl CatalogItem for It {
        type Key = u64;
        fn key(&self) -> u64 {
            self.id
        }
        fn name(&self) -> &str {
            self.name
        }
        fn number(&self) -> Option<u64> {
            self.num
        }
        fn added(&self) -> Option<DateTime<Utc>> {
            self.added.and_then(|s| DateTime::from_timestamp(s, 0))
        }
        fn rating(&self) -> Option<f64> {
            self.rating
        }
        fn in_category(&self, category: &CategoryId) -> bool {
            category.as_str() == self.cat
        }
    }

    fn it(
        id: u64,
        name: &'static str,
        num: Option<u64>,
        added: Option<i64>,
        rating: Option<f64>,
    ) -> It {
        It {
            id,
            name,
            num,
            added,
            rating,
            cat: if id & 1 == 0 { "even" } else { "odd" },
        }
    }

    fn items() -> Vec<It> {
        vec![
            it(1, "Channel 10", Some(10), Some(300), Some(4.0)),
            it(2, "channel 2", Some(2), None, Some(4.5)),
            it(3, "Éclair", None, Some(100), None),
            it(4, "eclair", Some(9), Some(200), Some(1.0)),
            it(5, "Zulu", Some(1), Some(400), None),
            it(6, "Channel 02", Some(3), None, Some(4.5)),
        ]
    }

    fn names(v: &[It], idx: &[usize]) -> Vec<&'static str> {
        idx.iter().map(|&i| v[i].name).collect()
    }

    #[test]
    fn sort_orders() {
        let v = items();
        assert_eq!(
            names(&v, &sorted_indices(&v, SortOrder::Server)),
            vec![
                "Channel 10",
                "channel 2",
                "Éclair",
                "eclair",
                "Zulu",
                "Channel 02"
            ]
        );
        assert_eq!(
            names(&v, &sorted_indices(&v, SortOrder::NameAsc)),
            vec![
                "channel 2",
                "Channel 02",
                "Channel 10",
                "eclair",
                "Éclair",
                "Zulu"
            ]
        );
        assert_eq!(
            names(&v, &sorted_indices(&v, SortOrder::NameDesc)),
            vec![
                "Zulu",
                "Éclair",
                "eclair",
                "Channel 10",
                "Channel 02",
                "channel 2"
            ]
        );
        let nums: Vec<_> = sorted_indices(&v, SortOrder::NumberAsc)
            .iter()
            .map(|&i| v[i].num)
            .collect();
        assert_eq!(
            nums,
            vec![Some(1), Some(2), Some(3), Some(9), Some(10), None]
        );
        let nums: Vec<_> = sorted_indices(&v, SortOrder::NumberDesc)
            .iter()
            .map(|&i| v[i].num)
            .collect();
        assert_eq!(
            nums,
            vec![Some(10), Some(9), Some(3), Some(2), Some(1), None]
        );
        let added: Vec<_> = sorted_indices(&v, SortOrder::RecentlyAdded)
            .iter()
            .map(|&i| v[i].id)
            .collect();
        assert_eq!(added, vec![5, 1, 4, 3, 2, 6]); // missing last, stable
        let rated: Vec<_> = sorted_indices(&v, SortOrder::TopRated)
            .iter()
            .map(|&i| v[i].id)
            .collect();
        assert_eq!(rated, vec![2, 6, 1, 4, 3, 5]);
    }

    #[test]
    fn sort_in_place() {
        let mut v = items();
        sort_items(&mut v, SortOrder::NumberAsc);
        assert_eq!(
            v.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![5, 2, 6, 4, 1, 3]
        );
        let mut empty: Vec<It> = Vec::new();
        sort_items(&mut empty, SortOrder::NameAsc);
        assert!(empty.is_empty());
    }

    #[test]
    fn natural_ordering() {
        assert_eq!(natural_cmp("Channel 2", "Channel 10"), Ordering::Less);
        assert_eq!(natural_cmp("a9", "a10"), Ordering::Less);
        assert_eq!(natural_cmp("10", "9"), Ordering::Greater);
        assert_eq!(natural_cmp("abc", "ABD"), Ordering::Less);
        assert_eq!(natural_cmp("2 Broke", "Abc"), Ordering::Less);
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
        assert_eq!(natural_cmp("Ünïcode", "unicode 2"), Ordering::Less);
        assert_eq!(
            natural_cmp("a99999999999999999999999", "a100000000000000000000000"),
            Ordering::Less
        );
    }

    #[test]
    fn search_is_plain_and_accent_insensitive() {
        let v = items();
        let q = SearchQuery::new("ECLA");
        assert_eq!(
            search(&v, &q).iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![3, 4]
        );
        let q = SearchQuery::new("éclair");
        assert_eq!(search(&v, &q).len(), 2);
        for hostile in ["(", "[", "c++", ".*", "\\", "a{2,}"] {
            let q = SearchQuery::new(hostile);
            assert!(search(&v, &q).is_empty(), "{hostile}");
        }
        let q = SearchQuery::new("   ");
        assert!(q.is_empty());
        assert_eq!(search(&v, &q).len(), v.len());
        let index = SearchIndex::new(&v);
        assert_eq!(index.len(), 6);
        assert_eq!(index.search(&SearchQuery::new("channel  1")), vec![0]);
        assert!(SearchQuery::new("strasse").matches("Straße"));
        assert!(SearchQuery::new("oresund").matches("Øresund"));
        assert!(SearchQuery::new("ｆｕｌｌ").matches("Full width"));
        assert!(SearchQuery::new("c++").matches("Learn C++"));
    }

    #[test]
    fn selections() {
        let v = items();
        let favs: HashSet<u64> = [3, 5].into_iter().collect();
        assert_eq!(
            filter_by_selection(&v, &CategorySelection::All, &favs).len(),
            6
        );
        let f: Vec<u64> = filter_by_selection(&v, &CategorySelection::Favourites, &favs)
            .iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(f, vec![3, 5]);
        let c: Vec<u64> =
            filter_by_selection(&v, &CategorySelection::Category("even".into()), &favs)
                .iter()
                .map(|i| i.id)
                .collect();
        assert_eq!(c, vec![2, 4, 6]);
        assert_eq!(CategorySelection::All.server_category(), None);
        assert_eq!(CategorySelection::Favourites.server_category(), None);
        let id = CategoryId::new("7");
        assert_eq!(
            CategorySelection::Category(id.clone()).server_category(),
            Some(&id)
        );
        assert!(CategorySelection::Favourites.is_pseudo());
    }

    #[test]
    fn model_impls() {
        let live: crate::Listing<LiveStream> = crate::de::parse_list(
            br#"[{"stream_id":1,"name":"B","num":"2","category_ids":[7]},{"stream_id":2,"name":"a","num":1,"category_id":"7"}]"#,
        )
        .unwrap();
        let idx = sorted_indices(&live.items, SortOrder::NameAsc);
        assert_eq!(idx, vec![1, 0]);
        let sel = CategorySelection::Category(CategoryId::new("7"));
        assert_eq!(
            filter_by_selection(&live.items, &sel, &HashSet::new()).len(),
            2
        );
        let pl = crate::m3u::parse_str(
            "#EXTM3U\n#EXTINF:-1 tvg-chno=\"5\" group-title=\"G\",Five\nhttp://a/5\n",
            &crate::m3u::M3uLimits::default(),
        )
        .unwrap();
        assert_eq!(pl.entries[0].number(), Some(5));
        assert!(pl.entries[0].in_category(&CategoryId::new("G")));
        assert_eq!(pl.entries[0].key(), "http://a/5");
    }
}

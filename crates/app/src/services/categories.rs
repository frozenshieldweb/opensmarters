// SPDX-License-Identifier: GPL-3.0-or-later
//! Category management rules (Settings › Categories), one pure module used by every
//! screen that lists categories or their items. Unit-tested.
//!
//! The viewer's choices per profile and section are a [`CategoryPrefs`] (hidden ids, the
//! viewer's order, the selected region tags), kept by the store and cached by the session
//! (`Session::category_prefs`). From them:
//!
//! - [`region_of`]: the region tag of a category **name** (`"FR | Sports"` → `FR`), with
//!   [`split_tag`]; `None` = untagged (the filter value `""`).
//! - [`arrange`]: the categories a browse screen lists, in the viewer's order (ordered ids
//!   first, then the rest in provider order, so new provider categories show up at the
//!   end), without the hidden ones and, while a region filter is set, without the
//!   categories of other regions. [`editor_order`] is the same order with every category
//!   (the editor).
//! - [`HiddenCategories`] / [`hides_item`]: an item is hidden when it has at least one
//!   category and **none** of them is visible (a channel in a hidden and a visible
//!   category stays; items without a category stay).
//! - [`regions`]: the editor's region chips (tags with category counts).
//! - [`move_in_view`], [`move_to_top`], [`minimal_order`]: reordering in the editor.
//!
//! Hidden is a viewing choice, not a lock: parental locks are checked separately and
//! never relaxed here (a hidden locked category simply isn't listed).

use std::collections::{HashMap, HashSet};

use opensmarters_core::{Category, CategoryId};
pub use opensmarters_store::CategoryPrefs;

use super::catalog::split_tag;

/// The region tag of a category name (`"UK | News"` → `Some("UK")`), `None` if untagged.
pub fn region_of(name: &str) -> Option<&str> {
    split_tag(name).0
}

/// The region filter value of a category name: its tag, or `""` when untagged.
fn region_key(name: &str) -> &str {
    region_of(name).unwrap_or("")
}

/// True when the region filter lets a category with this name through (no filter = all).
pub fn region_selected(name: &str, prefs: &CategoryPrefs) -> bool {
    prefs.regions.is_empty() || prefs.regions.contains(region_key(name))
}

/// How one category shows in the browse screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Listed.
    Shown,
    /// Hidden by the viewer.
    Hidden,
    /// Hidden because the region filter doesn't include its region.
    HiddenByRegion,
}

/// Whether `category` is shown, hidden by the viewer or hidden by the region filter (the
/// region filter wins: the editor then says "Hidden by region").
pub fn visibility(category: &Category, prefs: &CategoryPrefs) -> Visibility {
    if !region_selected(&category.name, prefs) {
        Visibility::HiddenByRegion
    } else if prefs.hidden.contains(category.id.as_str()) {
        Visibility::Hidden
    } else {
        Visibility::Shown
    }
}

/// Every category in the viewer's order: the ids of `prefs.order` first (in that order;
/// ids the provider no longer lists are skipped), then the others in provider order.
/// Repeated ids (a hostile list) appear once.
pub fn editor_order<'a>(categories: &'a [Category], prefs: &CategoryPrefs) -> Vec<&'a Category> {
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(categories.len());
    for (i, c) in categories.iter().enumerate() {
        index.entry(c.id.as_str()).or_insert(i);
    }
    let mut taken: HashSet<&str> = HashSet::with_capacity(categories.len());
    let mut out = Vec::with_capacity(categories.len());
    for id in &prefs.order {
        if let Some(&i) = index.get(id.as_str())
            && let Some(c) = categories.get(i)
            && taken.insert(c.id.as_str())
        {
            out.push(c);
        }
    }
    for c in categories {
        if taken.insert(c.id.as_str()) {
            out.push(c);
        }
    }
    out
}

/// The categories a browse screen lists: [`editor_order`] without the hidden ones (by
/// the viewer or the region filter).
pub fn arrange<'a>(categories: &'a [Category], prefs: &CategoryPrefs) -> Vec<&'a Category> {
    if prefs.is_default() {
        return editor_order(categories, prefs);
    }
    editor_order(categories, prefs)
        .into_iter()
        .filter(|c| visibility(c, prefs) == Visibility::Shown)
        .collect()
}

/// True when an item with these categories must stay out of the lists: it has at least
/// one category and every one of them is hidden. Items without a category stay.
pub fn hides_item<'a>(
    item_categories: impl IntoIterator<Item = &'a CategoryId>,
    hidden: &HashSet<CategoryId>,
) -> bool {
    if hidden.is_empty() {
        return false;
    }
    let mut any = false;
    for c in item_categories {
        if !hidden.contains(c) {
            return false;
        }
        any = true;
    }
    any
}

/// The hidden categories of one section (by the viewer, or by the region filter), to
/// check many items quickly. Build it once per list ([`HiddenCategories::of`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HiddenCategories(HashSet<CategoryId>);

impl HiddenCategories {
    /// The categories `prefs` hides among `categories` (the viewer's hidden ids count even
    /// when the provider doesn't list them right now).
    pub fn of(categories: &[Category], prefs: &CategoryPrefs) -> Self {
        let mut set: HashSet<CategoryId> = prefs
            .hidden
            .iter()
            .map(|id| CategoryId::from(id.as_str()))
            .collect();
        if !prefs.regions.is_empty() {
            for c in categories {
                if !region_selected(&c.name, prefs) {
                    set.insert(c.id.clone());
                }
            }
        }
        Self(set)
    }

    /// Exactly these hidden ids (tests).
    pub fn from_ids(ids: impl IntoIterator<Item = CategoryId>) -> Self {
        Self(ids.into_iter().collect())
    }

    /// Nothing is hidden.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The category is hidden.
    pub fn contains(&self, id: &CategoryId) -> bool {
        self.0.contains(id)
    }

    /// The hidden ids, sorted (a cheap key to know when a filtered list must change).
    pub fn sorted_ids(&self) -> Vec<CategoryId> {
        let mut ids: Vec<CategoryId> = self.0.iter().cloned().collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids
    }

    /// An item with these categories is hidden ([`hides_item`]).
    pub fn hides<'a>(&self, item_categories: impl IntoIterator<Item = &'a CategoryId>) -> bool {
        hides_item(item_categories, &self.0)
    }
}

/// One region chip of the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The tag (`"FR"`), or `""` for categories without one ("No tag").
    pub tag: String,
    /// Categories with this tag.
    pub count: usize,
}

/// The editor's region chips: the tags found among the category names with their
/// category counts, most categories first (then by name), and "No tag" (`""`) last when
/// some categories have none. Empty when no category has a tag, unless a filter is set:
/// selected tags always appear (with 0 when the provider no longer has them) so they can
/// be cleared.
pub fn regions(categories: &[Category], selected: &HashSet<String>) -> Vec<Region> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut untagged = 0;
    let mut seen: HashSet<&str> = HashSet::with_capacity(categories.len());
    for c in categories {
        if !seen.insert(c.id.as_str()) {
            continue;
        }
        match region_of(&c.name) {
            Some(t) => *counts.entry(t).or_default() += 1,
            None => untagged += 1,
        }
    }
    if counts.is_empty() && selected.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Region> = counts
        .into_iter()
        .map(|(tag, count)| Region {
            tag: tag.to_owned(),
            count,
        })
        .collect();
    for t in selected {
        if !t.is_empty() && !out.iter().any(|r| &r.tag == t) {
            out.push(Region {
                tag: t.clone(),
                count: 0,
            });
        }
    }
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
    if untagged > 0 || selected.contains("") {
        out.push(Region {
            tag: String::new(),
            count: untagged,
        });
    }
    out
}

/// The label of a region tag ("No tag" for `""`).
pub fn region_label(tag: &str) -> &str {
    if tag.is_empty() { "No tag" } else { tag }
}

/// "FR, CA, No tag": the selected regions, sorted (tags first).
pub fn regions_summary(prefs: &CategoryPrefs) -> String {
    let mut tags: Vec<&str> = prefs.regions.iter().map(String::as_str).collect();
    tags.sort_by(|a, b| a.is_empty().cmp(&b.is_empty()).then_with(|| a.cmp(b)));
    tags.into_iter()
        .map(region_label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Moves `id` one place up (`delta < 0`) or down among `view` (the rows on screen, e.g.
/// filtered) inside `order` (every id): it lands just before / after its neighbour in
/// `view`. Returns false when it can't move (already first/last, or not on screen).
pub fn move_in_view(order: &mut Vec<String>, view: &[String], id: &str, delta: isize) -> bool {
    if delta == 0 {
        return false;
    }
    let Some(pos) = view.iter().position(|v| v == id) else {
        return false;
    };
    let neighbour = if delta < 0 {
        pos.checked_sub(1)
    } else {
        Some(pos + 1)
    };
    let Some(neighbour) = neighbour.and_then(|n| view.get(n)) else {
        return false;
    };
    let Some(from) = order.iter().position(|o| o == id) else {
        return false;
    };
    let item = order.remove(from);
    let Some(at) = order.iter().position(|o| o == neighbour) else {
        order.insert(from, item);
        return false;
    };
    order.insert(if delta < 0 { at } else { at + 1 }, item);
    true
}

/// Moves `id` to the front of `order`. Returns false when it is already first or absent.
pub fn move_to_top(order: &mut Vec<String>, id: &str) -> bool {
    match order.iter().position(|o| o == id) {
        Some(0) | None => false,
        Some(from) => {
            let item = order.remove(from);
            order.insert(0, item);
            true
        }
    }
}

/// The shortest prefix of `order` (every category, in the viewer's order) that gives the
/// same list when the remaining categories follow in provider order: what the store keeps
/// as positions. Moving one category to the top stores one row, not the whole list.
pub fn minimal_order<'a>(order: &'a [String], provider: &[Category]) -> &'a [String] {
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(provider.len());
    for (i, c) in provider.iter().enumerate() {
        index.entry(c.id.as_str()).or_insert(i);
    }
    let pos = |id: &String| index.get(id.as_str()).copied();
    let mut k = order.len();
    while k > 0 {
        let Some(a) = pos(&order[k - 1]) else { break };
        if let Some(next) = order.get(k)
            && pos(next).is_none_or(|b| a > b)
        {
            break;
        }
        k -= 1;
    }
    &order[..k]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: &str, name: &str) -> Category {
        Category {
            id: CategoryId::from(id),
            name: name.to_owned(),
            parent_id: None,
        }
    }

    fn sample() -> Vec<Category> {
        vec![
            cat("1", "UK | News"),
            cat("2", "FR | Sports"),
            cat("3", "[CA] Kids"),
            cat("4", "Documentary"),
            cat("5", "FR | Cinema"),
            cat("6", "DE: Kino"),
        ]
    }

    fn ids(v: &[&Category]) -> Vec<String> {
        v.iter().map(|c| c.id.as_str().to_owned()).collect()
    }

    fn prefs(hidden: &[&str], order: &[&str], regions: &[&str]) -> CategoryPrefs {
        CategoryPrefs {
            hidden: hidden.iter().map(|s| (*s).to_owned()).collect(),
            order: order.iter().map(|s| (*s).to_owned()).collect(),
            regions: regions.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn regions_come_from_category_names() {
        assert_eq!(region_of("FR | Sports"), Some("FR"));
        assert_eq!(region_of("[CA] Hockey"), Some("CA"));
        assert_eq!(region_of("DE: Kino"), Some("DE"));
        assert_eq!(region_of("Documentary"), None);
        assert_eq!(region_of("News: World"), None, "not a tag (lower case)");
    }

    #[test]
    fn defaults_change_nothing() {
        let cats = sample();
        let p = CategoryPrefs::default();
        assert_eq!(ids(&arrange(&cats, &p)), ["1", "2", "3", "4", "5", "6"]);
        assert!(HiddenCategories::of(&cats, &p).is_empty());
        assert!(cats.iter().all(|c| visibility(c, &p) == Visibility::Shown));
    }

    #[test]
    fn arrange_orders_hides_and_filters_by_region() {
        let cats = sample();
        // Ordered ids first (unknown ones skipped), then provider order.
        let p = prefs(&[], &["5", "gone", "2"], &[]);
        assert_eq!(ids(&arrange(&cats, &p)), ["5", "2", "1", "3", "4", "6"]);
        // Hidden ones drop out; the editor keeps them in place.
        let p = prefs(&["2", "4"], &["5", "2"], &[]);
        assert_eq!(ids(&arrange(&cats, &p)), ["5", "1", "3", "6"]);
        assert_eq!(
            ids(&editor_order(&cats, &p)),
            ["5", "2", "1", "3", "4", "6"]
        );
        assert_eq!(visibility(&cats[1], &p), Visibility::Hidden);
        // Region filter: only FR and untagged ("").
        let p = prefs(&["2"], &[], &["FR", ""]);
        assert_eq!(ids(&arrange(&cats, &p)), ["4", "5"]);
        assert_eq!(visibility(&cats[0], &p), Visibility::HiddenByRegion);
        assert_eq!(visibility(&cats[1], &p), Visibility::Hidden);
        let hidden = HiddenCategories::of(&cats, &p);
        for id in ["1", "2", "3", "6"] {
            assert!(hidden.contains(&CategoryId::from(id)), "{id}");
        }
        assert!(!hidden.contains(&CategoryId::from("5")));
    }

    #[test]
    fn repeated_ids_appear_once() {
        let cats = vec![cat("1", "A"), cat("2", "B"), cat("1", "A again")];
        assert_eq!(ids(&arrange(&cats, &CategoryPrefs::default())), ["1", "2"]);
        assert_eq!(
            ids(&arrange(&cats, &prefs(&[], &["1", "1"], &[]))),
            ["1", "2"]
        );
    }

    #[test]
    fn items_hide_only_when_every_category_is_hidden() {
        let hidden: HashSet<CategoryId> = ["2", "3"].into_iter().map(CategoryId::from).collect();
        let c = |v: &[&str]| v.iter().map(|s| CategoryId::from(*s)).collect::<Vec<_>>();
        assert!(hides_item(&c(&["2"]), &hidden));
        assert!(hides_item(&c(&["2", "3"]), &hidden));
        assert!(
            !hides_item(&c(&["2", "1"]), &hidden),
            "also in a visible one"
        );
        assert!(!hides_item(&c(&[]), &hidden), "no category: stays");
        assert!(!hides_item(&c(&["9"]), &hidden), "unknown category: stays");
        assert!(!hides_item(&c(&["2"]), &HashSet::new()));
        let h = HiddenCategories::from_ids(c(&["2"]));
        assert!(h.hides(&c(&["2"])));
    }

    #[test]
    fn region_chips_count_categories() {
        let cats = sample();
        let r = regions(&cats, &HashSet::new());
        let pairs: Vec<(&str, usize)> = r.iter().map(|r| (r.tag.as_str(), r.count)).collect();
        assert_eq!(
            pairs,
            [("FR", 2), ("CA", 1), ("DE", 1), ("UK", 1), ("", 1)],
            "most first, then by name; No tag last"
        );
        // No tags at all: no chips.
        let plain = vec![cat("1", "News"), cat("2", "Sports")];
        assert!(regions(&plain, &HashSet::new()).is_empty());
        // A selected tag the provider dropped still shows (so it can be cleared).
        let selected: HashSet<String> = ["JP".to_owned()].into_iter().collect();
        let r = regions(&plain, &selected);
        assert_eq!(
            r.first().map(|r| (r.tag.as_str(), r.count)),
            Some(("JP", 0))
        );
        assert_eq!(region_label(""), "No tag");
        assert_eq!(
            regions_summary(&prefs(&[], &[], &["", "FR", "CA"])),
            "CA, FR, No tag"
        );
    }

    #[test]
    fn moving_in_a_filtered_view() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        let mut order = s(&["a", "b", "c", "d", "e"]);
        // The view shows a, c, e (filtered): c moves before a.
        assert!(move_in_view(&mut order, &s(&["a", "c", "e"]), "c", -1));
        assert_eq!(order, s(&["c", "a", "b", "d", "e"]));
        // Down: after its neighbour in the view.
        assert!(move_in_view(&mut order, &s(&["c", "a", "e"]), "a", 1));
        assert_eq!(order, s(&["c", "b", "d", "e", "a"]));
        assert!(!move_in_view(&mut order, &s(&["c", "a"]), "c", -1), "first");
        assert!(!move_in_view(&mut order, &s(&["c", "a"]), "a", 1), "last");
        assert!(
            !move_in_view(&mut order, &s(&["c"]), "zz", 1),
            "not on screen"
        );
        assert!(move_to_top(&mut order, "e"));
        assert_eq!(order, s(&["e", "c", "b", "d", "a"]));
        assert!(!move_to_top(&mut order, "e"));
    }

    #[test]
    fn minimal_order_keeps_only_the_moved_prefix() {
        let cats = sample(); // provider: 1 2 3 4 5 6
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
        assert!(minimal_order(&s(&["1", "2", "3", "4", "5", "6"]), &cats).is_empty());
        assert_eq!(
            minimal_order(&s(&["5", "1", "2", "3", "4", "6"]), &cats),
            s(&["5"])
        );
        assert_eq!(
            minimal_order(&s(&["1", "3", "2", "4", "5", "6"]), &cats),
            s(&["1", "3"])
        );
        assert_eq!(
            minimal_order(&s(&["2", "1", "3", "4", "6", "5"]), &cats),
            s(&["2", "1", "3", "4", "6"])
        );
        // The prefix plus provider order gives the order back.
        let order = s(&["4", "1", "6", "2", "3", "5"]);
        let p = CategoryPrefs {
            order: minimal_order(&order, &cats).to_vec(),
            ..CategoryPrefs::default()
        };
        assert_eq!(ids(&arrange(&cats, &p)), order);
    }
}

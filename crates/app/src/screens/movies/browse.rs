// SPDX-License-Identifier: GPL-3.0-or-later
//! The poster-grid browse screen shared by Movies and Series (§8.4), generic over
//! [`BrowseKind`].
//!
//! - **Per-category loading.** Real panels list tens of thousands of movies and "All"
//!   can take ~20 s, so the screen opens on the route's category, else the category last
//!   browsed with this profile, else the first one. **"All" is last** in the list and only
//!   loads when picked (with a progress panel and Cancel).
//! - **Off the UI thread:** the list's search index is built in the loader task; search
//!   (plain, accent-insensitive, debounced 200 ms) and sorting run in a `Loader` too.
//!   Sorted orders are cached per list.
//! - **Virtualised:** `VirtualGrid` builds only visible rows; posters load lazily.
//! - **Parental control:** locked categories show a lock and ask for the PIN (unlocked for
//!   the session); their items stay out of "All".
//! - Favourites show a heart on the poster; movies show their watch progress.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, Id, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use opensmarters_core::catalog::{CatalogItem, SearchIndex, SearchQuery, sorted_indices};
use opensmarters_core::{Category, CategoryId};
use opensmarters_store::{ContentKind, PlaybackProgress};

use crate::error::{AppError, AppResult};
use crate::focus;
use crate::nav::Route;
use crate::screens::{AppCtx, shell};
use crate::services::vod::{self, VodSort};
use crate::services::{ImageSize, Session};
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons::{self, Style};
use crate::ui::layout::Breakpoint;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{
    Button, Chip, Dropdown, EmptyState, MenuRow, PinPrompt, PinPromptResult, PosterCard,
    SearchField, VirtualGrid,
};
use crate::ui::{paint, text};

/// Search waits this long after the last keystroke on big lists.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(200);
/// Lists up to this size are filtered without a debounce.
const INSTANT_SEARCH_MAX: usize = 2_000;
/// Width of the categories column (wide layout).
const CATS_W: f32 = 248.0;

/// What differs between Movies and Series.
pub(crate) trait BrowseKind: 'static {
    /// List item.
    type Item: CatalogItem<Key = u64> + Clone + Send + Sync + 'static;
    /// Favourites / parental kind.
    const CONTENT: ContentKind;
    /// Screen title ("Movies").
    const TITLE: &'static str;
    /// Plural noun ("movies").
    const PLURAL: &'static str;
    /// Singular noun ("movie").
    const SINGULAR: &'static str;
    /// Id prefix ("movies").
    const KEY: &'static str;

    /// The provider's categories.
    fn categories(s: Arc<Session>) -> impl Future<Output = AppResult<Arc<Vec<Category>>>> + Send;
    /// The items of a category (`None` = all).
    fn list(
        s: Arc<Session>,
        category: Option<CategoryId>,
    ) -> impl Future<Output = AppResult<Arc<Vec<Self::Item>>>> + Send;
    /// Poster URL.
    fn poster(item: &Self::Item) -> Option<&str>;
    /// Meta line under the poster.
    fn meta(item: &Self::Item) -> String;
    /// Categories of an item (primary + extra).
    fn item_categories(item: &Self::Item) -> Vec<&CategoryId>;
    /// The detail route of an item id.
    fn detail(id: u64) -> Route;
    /// Movies show watch progress on posters.
    const PROGRESS: bool;
}

/// The selected category.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Selection {
    Category(CategoryId),
    All,
}

impl Selection {
    fn server_category(&self) -> Option<CategoryId> {
        match self {
            Selection::Category(id) => Some(id.clone()),
            Selection::All => None,
        }
    }
}

static SERIAL: AtomicU64 = AtomicU64::new(1);

/// A loaded list with its search index and cached sort orders.
struct Listing<T> {
    serial: u64,
    selection: Selection,
    items: Arc<Vec<T>>,
    index: SearchIndex,
    orders: Mutex<HashMap<VodSort, Arc<Vec<usize>>>>,
}

impl<T> std::fmt::Debug for Listing<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("selection", &self.selection)
            .field("len", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl<T: CatalogItem<Key = u64>> Listing<T> {
    fn order(&self, sort: VodSort) -> Arc<Vec<usize>> {
        let mut cache = self.orders.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            cache
                .entry(sort)
                .or_insert_with(|| Arc::new(sorted_indices(&self.items, sort.order()))),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ViewKey {
    serial: u64,
    query: String,
    sort: VodSort,
    locked: Vec<CategoryId>,
}

/// The visible items (indices into the listing), filtered and sorted.
#[derive(Debug)]
struct View {
    key: ViewKey,
    indices: Arc<Vec<usize>>,
}

/// Filters by `query` and locked categories, in `sort` order (runs off the UI thread).
fn compute_view<T: CatalogItem<Key = u64>>(
    listing: &Listing<T>,
    query: &str,
    sort: VodSort,
    locked: &HashSet<CategoryId>,
    categories_of: impl Fn(&T) -> Vec<&CategoryId>,
) -> Vec<usize> {
    let n = listing.items.len();
    let q = SearchQuery::new(query);
    let mut keep = vec![q.is_empty(); n];
    if !q.is_empty() {
        for i in listing.index.search(&q) {
            if let Some(k) = keep.get_mut(i) {
                *k = true;
            }
        }
    }
    if !locked.is_empty() {
        for (i, item) in listing.items.iter().enumerate() {
            if keep[i] && categories_of(item).iter().any(|c| locked.contains(*c)) {
                keep[i] = false;
            }
        }
    }
    let order = listing.order(sort);
    order.iter().copied().filter(|&i| keep[i]).collect()
}

/// A locked category waiting for the PIN ([`PinPrompt`] does the pad, the lockout and
/// the off-thread check).
#[derive(Debug)]
struct PinGate {
    selection: Selection,
    name: String,
    prompt: PinPrompt,
}

/// The browse screen state. See the [module docs](self).
pub(crate) struct Browse<K: BrowseKind> {
    route_category: Option<String>,
    categories: Loader<Arc<Vec<Category>>>,
    remembered: Loader<Option<CategoryId>>,
    selection: Option<Selection>,
    previous: Option<Selection>,
    list: Loader<Arc<Listing<K::Item>>>,
    list_started: Option<Instant>,
    view: Loader<View>,
    view_pending_key: Option<ViewKey>,
    query: String,
    query_changed: Option<Instant>,
    applied_query: String,
    sort: VodSort,
    cat_filter: String,
    pin: Option<PinGate>,
    progress: Loader<Arc<HashMap<u64, PlaybackProgress>>>,
    progress_epoch: u64,
    scroll_to: Option<usize>,
    _kind: PhantomData<K>,
}

impl<K: BrowseKind> std::fmt::Debug for Browse<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browse")
            .field("kind", &K::KEY)
            .field("selection", &self.selection)
            .field("sort", &self.sort)
            .finish_non_exhaustive()
    }
}

fn card_id(key: &str, id: u64) -> Id {
    Id::unique((key, "poster", id))
}

impl<K: BrowseKind> Browse<K> {
    /// A browse screen, optionally opened on a category id (or `"all"`).
    pub(crate) fn new(category: Option<String>) -> Self {
        Self {
            route_category: category.filter(|c| !c.trim().is_empty()),
            categories: Loader::new(),
            remembered: Loader::new(),
            selection: None,
            previous: None,
            list: Loader::new(),
            list_started: None,
            view: Loader::new(),
            view_pending_key: None,
            query: String::new(),
            query_changed: None,
            applied_query: String::new(),
            sort: VodSort::Server,
            cat_filter: String::new(),
            pin: None,
            progress: Loader::new(),
            progress_epoch: 0,
            scroll_to: None,
            _kind: PhantomData,
        }
    }

    fn id(&self, what: &str) -> Id {
        Id::unique((K::KEY, what))
    }

    fn sort_id(&self) -> Id {
        self.id("sort")
    }

    fn load_categories(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        if !session.is_xtream() {
            return;
        }
        self.categories
            .start(cx.rt(), &cx.egui, move |_| K::categories(session));
    }

    fn load_progress(&mut self, cx: &AppCtx<'_>) {
        if !K::PROGRESS {
            return;
        }
        let Some(session) = cx.session() else { return };
        self.progress_epoch = cx.playback.progress_epoch();
        self.progress.start(cx.rt(), &cx.egui, move |_| async move {
            session.movie_progress_map().await.map(Arc::new)
        });
    }

    fn load_list(&mut self, cx: &AppCtx<'_>, sel: Selection) {
        let Some(session) = cx.session() else { return };
        let category = sel.server_category();
        self.list_started = Some(Instant::now());
        self.list.start(cx.rt(), &cx.egui, move |_| async move {
            let items = K::list(session, category).await?;
            // Index the names off the async workers (35 000 names on big panels).
            tokio::task::spawn_blocking(move || {
                let index = SearchIndex::new(&items);
                Arc::new(Listing {
                    serial: SERIAL.fetch_add(1, Ordering::Relaxed),
                    selection: sel,
                    items,
                    index,
                    orders: Mutex::new(HashMap::new()),
                })
            })
            .await
            .map_err(|_| AppError::Cancelled)
        });
    }

    /// Selects a category, asking for the PIN when it's locked.
    fn select(&mut self, cx: &mut AppCtx<'_>, sel: Selection, name: &str) {
        let Some(session) = cx.session() else { return };
        if let Selection::Category(id) = &sel
            && session.is_category_locked(K::CONTENT, id, name)
        {
            self.pin = Some(PinGate {
                selection: sel,
                name: name.to_owned(),
                prompt: PinPrompt::new(self.id("pin")),
            });
            return;
        }
        self.apply_selection(cx, sel);
    }

    fn apply_selection(&mut self, cx: &mut AppCtx<'_>, sel: Selection) {
        if self.selection.as_ref() == Some(&sel) {
            return;
        }
        if let (Some(session), Selection::Category(id)) = (cx.session(), &sel) {
            session.remember_vod_category(K::CONTENT, id);
        }
        self.previous = self.selection.take();
        self.selection = Some(sel.clone());
        self.scroll_to = Some(0);
        self.load_list(cx, sel);
    }

    /// Picks the opening category once the categories (and the remembered one) are in.
    fn decide_initial(&mut self, cx: &mut AppCtx<'_>) {
        if self.selection.is_some() || self.pin.is_some() {
            return;
        }
        let Some(cats) = self.categories.value().cloned() else {
            return;
        };
        if !self.remembered.has_finished() && self.remembered.is_loading() {
            return;
        }
        let Some(session) = cx.session() else { return };
        let unlocked =
            |id: &CategoryId, name: &str| !session.is_category_locked(K::CONTENT, id, name);
        let first_unlocked = cats
            .iter()
            .find(|c| unlocked(&c.id, &c.name))
            .map(|c| Selection::Category(c.id.clone()));
        if let Some(route) = self.route_category.take() {
            if route.eq_ignore_ascii_case("all") {
                self.apply_selection(cx, Selection::All);
                return;
            }
            if let Some(c) = cats.iter().find(|c| c.id.as_str() == route) {
                let (sel, name) = (Selection::Category(c.id.clone()), c.name.clone());
                if !unlocked(&c.id, &c.name) {
                    // Open behind the PIN on the first unlocked category.
                    if let Some(fallback) = first_unlocked.clone() {
                        self.apply_selection(cx, fallback);
                    }
                }
                self.select(cx, sel, &name);
                return;
            }
        }
        let remembered = self
            .remembered
            .value()
            .cloned()
            .flatten()
            .and_then(|id| cats.iter().find(|c| c.id == id))
            .filter(|c| unlocked(&c.id, &c.name))
            .map(|c| Selection::Category(c.id.clone()));
        let sel = remembered.or(first_unlocked).unwrap_or(Selection::All);
        self.apply_selection(cx, sel);
    }

    fn locked_categories(&self, session: &Session) -> Vec<CategoryId> {
        if !session.parental_enabled() || self.selection != Some(Selection::All) {
            return Vec::new();
        }
        self.categories
            .value()
            .map(|cats| {
                cats.iter()
                    .filter(|c| session.is_category_locked(K::CONTENT, &c.id, &c.name))
                    .map(|c| c.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The listing for the current selection, if loaded.
    fn current_listing(&self) -> Option<Arc<Listing<K::Item>>> {
        let l = self.list.value()?;
        (Some(&l.selection) == self.selection.as_ref()).then(|| Arc::clone(l))
    }

    /// (Re)computes the view when the list, query, sort or locks change.
    fn update_view(&mut self, cx: &AppCtx<'_>, session: &Session) {
        let Some(listing) = self.current_listing() else {
            return;
        };
        // Debounced search on big lists.
        if self.query != self.applied_query {
            let due = listing.items.len() <= INSTANT_SEARCH_MAX
                || self
                    .query_changed
                    .is_some_and(|t| t.elapsed() >= SEARCH_DEBOUNCE);
            if due {
                self.applied_query = self.query.clone();
                self.scroll_to = Some(0);
            } else if let Some(t) = self.query_changed {
                cx.egui
                    .request_repaint_after(SEARCH_DEBOUNCE.saturating_sub(t.elapsed()));
            }
        }
        let key = ViewKey {
            serial: listing.serial,
            query: self.applied_query.clone(),
            sort: self.sort,
            locked: self.locked_categories(session),
        };
        let current = self.view.value().map(|v| &v.key);
        let pending = self.view.is_loading();
        if current == Some(&key) || (pending && self.view_pending_key.as_ref() == Some(&key)) {
            return;
        }
        self.view_pending_key = Some(key.clone());
        self.view.start_blocking(cx.rt(), &cx.egui, move || {
            let locked: HashSet<CategoryId> = key.locked.iter().cloned().collect();
            let indices = compute_view(&listing, &key.query, key.sort, &locked, K::item_categories);
            Ok(View {
                key,
                indices: Arc::new(indices),
            })
        });
    }
}

// ---------------------------------------------------------------------------------------
// Screen lifecycle (called by `MoviesScreen` / `SeriesScreen`)
// ---------------------------------------------------------------------------------------

impl<K: BrowseKind> Browse<K> {
    pub(crate) fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if self.categories.is_idle()
            || (self.categories.error().is_some() && !self.categories.is_loading())
        {
            self.load_categories(cx);
        }
        if self.remembered.is_idle()
            && let Some(session) = cx.session()
        {
            self.remembered
                .start(cx.rt(), &cx.egui, move |_| async move {
                    Ok(session.last_vod_category(K::CONTENT).await)
                });
        }
        if K::PROGRESS
            && (self.progress.is_idle() || self.progress_epoch != cx.playback.progress_epoch())
        {
            self.load_progress(cx);
        }
    }

    pub(crate) fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if self.pin.take().is_some() {
            if self.selection.is_none() {
                self.decide_initial(cx);
            }
            return true;
        }
        Dropdown::close(&cx.egui, self.sort_id())
    }

    pub(crate) fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.categories.poll();
        self.remembered.poll();
        self.list.poll();
        self.view.poll();
        self.progress.poll();
        if K::PROGRESS && cx.playback.progress_epoch() != self.progress_epoch {
            self.load_progress(cx);
        }
        let Some(session) = cx.session() else { return };
        self.decide_initial(cx);
        self.update_view(cx, &session);

        let content = cx.layout.content;
        let m = cx.layout.margin;
        let wide = cx.layout.breakpoint == Breakpoint::Wide;
        let painter = ui.painter().clone();
        shell::status_cluster(cx, ui);

        if !session.is_xtream() {
            shell::page_title(cx, ui, K::TITLE);
            let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(
                pos2(content.left() + m, content.top() + 96.0),
                pos2(content.right() - m, content.bottom() - 24.0),
            )));
            let body = format!(
                "M3U playlists don't separate {} from live channels. Add an Xtream Codes \
                 login to browse them.",
                K::PLURAL
            );
            let title = format!("{} need an Xtream Codes account", K::TITLE);
            if EmptyState::new(self.id("m3u"), Illustration::EmptyGuide, &title, &body)
                .action("Go to Live TV", false)
                .show(&mut child)
                .is_some_and(|f| f.activated)
            {
                cx.nav.section(Route::live());
            }
            return;
        }

        let cats_arc = self.categories.value().cloned();
        let cats: Option<&[Category]> = cats_arc.as_ref().map(|c| c.as_slice());
        // Left: categories column (wide) or the page title + chips (medium/TV).
        let right_left;
        let mut top;
        if wide {
            let cats_rect = Rect::from_min_size(content.min, vec2(CATS_W, content.height()));
            painter.rect_filled(cats_rect, 0.0, color::SURFACE_1);
            painter.vline(
                cats_rect.right() - 0.5,
                cats_rect.y_range(),
                Stroke::new(1.0, color::STROKE_SUBTLE),
            );
            painter.text(
                pos2(cats_rect.left() + 24.0, content.top() + 64.0),
                Align2::LEFT_CENTER,
                K::TITLE,
                font::heading(),
                color::TEXT,
            );
            let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(
                pos2(cats_rect.left() + 24.0, content.top() + 106.0),
                pos2(cats_rect.right() - 24.0, cats_rect.bottom() - 24.0),
            )));
            let pick = focus::scope(&mut child, self.id("cats"), |ui| {
                self.categories_column(cx, ui, cats, &session)
            });
            if let Some((sel, name)) = pick {
                self.select(cx, sel, &name);
            }
            right_left = cats_rect.right() + m;
            top = content.top() + 64.0;
            // Category title + count.
            let title = self.selection_name(cats);
            let tr = text::line(
                &painter,
                pos2(right_left, top),
                Align2::LEFT_CENTER,
                &title,
                font::heading(),
                color::TEXT,
                (content.right() - m - right_left - 360.0).max(120.0),
            );
            if let Some(n) = self.visible_count() {
                painter.text(
                    pos2(tr.right() + 12.0, top + 4.0),
                    Align2::LEFT_CENTER,
                    count_label(n, K::SINGULAR, K::PLURAL),
                    font::caption(),
                    color::TEXT_3,
                );
            }
            top += 42.0;
        } else {
            let tr = shell::page_title(cx, ui, K::TITLE);
            if let Some(n) = self.visible_count() {
                painter.text(
                    pos2(tr.right() + 12.0, tr.center().y + 4.0),
                    Align2::LEFT_CENTER,
                    count_label(n, K::SINGULAR, K::PLURAL),
                    font::caption(),
                    color::TEXT_3,
                );
            }
            right_left = content.left() + m;
            let chips = Rect::from_min_max(
                pos2(content.left() + m - 8.0, content.top() + 88.0),
                pos2(content.right() - m + 8.0, content.top() + 140.0),
            );
            let mut child = ui.new_child(UiBuilder::new().max_rect(chips));
            let pick = focus::scope(&mut child, self.id("chips"), |ui| {
                self.category_chips(ui, cats, &session)
            });
            if let Some((sel, name)) = pick {
                self.select(cx, sel, &name);
            }
            top = content.top() + 148.0;
        }
        let right = Rect::from_min_max(
            pos2(right_left, top),
            pos2(content.right() - m, content.bottom()),
        );

        // Toolbar: in-category search + sort.
        let toolbar = Rect::from_min_size(right.min, vec2(right.width(), 44.0));
        self.toolbar(ui, toolbar, cats);

        let body = Rect::from_min_max(
            pos2(right.left() - 16.0, toolbar.bottom() + 8.0),
            pos2(right.right() + 16.0, content.bottom()),
        );
        self.body(cx, ui, body, &session);
        self.pin_dialog(cx, ui);
    }
}

fn count_label(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

// ---------------------------------------------------------------------------------------
// Parts
// ---------------------------------------------------------------------------------------

impl<K: BrowseKind> Browse<K> {
    fn selection_name(&self, cats: Option<&[Category]>) -> String {
        match &self.selection {
            Some(Selection::All) => format!("All {}", K::PLURAL),
            Some(Selection::Category(id)) => cats
                .and_then(|c| c.iter().find(|c| &c.id == id))
                .map_or_else(
                    || K::TITLE.to_owned(),
                    |c| text::cap(&c.name, 80).into_owned(),
                ),
            None => K::TITLE.to_owned(),
        }
    }

    fn visible_count(&self) -> Option<usize> {
        let listing = self.current_listing()?;
        let v = self.view.value()?;
        (v.key.serial == listing.serial).then(|| v.indices.len())
    }

    fn categories_column(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        cats: Option<&[Category]>,
        session: &Arc<Session>,
    ) -> Option<(Selection, String)> {
        ui.spacing_mut().item_spacing.y = 0.0;
        SearchField::new(self.id("cat-search"), "Find a category")
            .size(ui.available_width(), 52.0)
            .show(ui, &mut self.cat_filter);
        ui.add_space(20.0);
        let Some(cats) = cats else {
            for i in 0..8 {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(
                        r.min + vec2(12.0, 17.0),
                        vec2(90.0 + (i % 3) as f32 * 30.0, 14.0),
                    ),
                    radius::XS,
                );
            }
            return None;
        };
        let q = SearchQuery::new(&self.cat_filter);
        let mut picked = None;
        egui::ScrollArea::vertical()
            .id_salt(self.id("cats-scroll"))
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for c in cats {
                    if !q.matches(&c.name) {
                        continue;
                    }
                    let sel = Selection::Category(c.id.clone());
                    let locked = session.is_category_locked(K::CONTENT, &c.id, &c.name);
                    let name = text::cap(&c.name, 80);
                    let f = MenuRow::new(Id::unique((K::KEY, "cat", c.id.as_str())), &name)
                        .locked(locked)
                        .selected(self.selection.as_ref() == Some(&sel))
                        .show(ui);
                    if f.activated {
                        picked = Some((sel, c.name.clone()));
                    }
                }
                // "All" last: it loads the whole catalogue.
                let all = format!("All {}", K::PLURAL);
                if q.is_empty() || q.matches(&all) {
                    let (r, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                    ui.painter().hline(
                        r.x_range(),
                        r.center().y,
                        Stroke::new(1.0, color::STROKE_SUBTLE),
                    );
                    let f = MenuRow::new(self.id("cat-all"), &all)
                        .icon(icons::SQUARES_FOUR)
                        .selected(self.selection == Some(Selection::All))
                        .show(ui);
                    if f.activated {
                        picked = Some((Selection::All, all.clone()));
                    }
                    let (r, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
                    ui.painter().text(
                        pos2(r.left() + 12.0, r.center().y + 4.0),
                        Align2::LEFT_CENTER,
                        "Loads everything: slower",
                        font::caption(),
                        color::TEXT_3,
                    );
                }
            });
        picked
    }

    fn category_chips(
        &mut self,
        ui: &mut Ui,
        cats: Option<&[Category]>,
        session: &Arc<Session>,
    ) -> Option<(Selection, String)> {
        let cats = cats?;
        let mut picked = None;
        egui::ScrollArea::horizontal()
            .id_salt(self.id("chips-scroll"))
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(8, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for c in cats {
                        let sel = Selection::Category(c.id.clone());
                        let label = if session.is_category_locked(K::CONTENT, &c.id, &c.name) {
                            format!("{} {}", icons::LOCK_SIMPLE, c.name)
                        } else {
                            c.name.clone()
                        };
                        if Chip::new(label)
                            .selected(self.selection.as_ref() == Some(&sel))
                            .id(Id::unique((K::KEY, "chip", c.id.as_str())))
                            .show(ui)
                            .activated
                        {
                            picked = Some((sel, c.name.clone()));
                        }
                    }
                    let all = format!("All {}", K::PLURAL);
                    if Chip::new(all.clone())
                        .selected(self.selection == Some(Selection::All))
                        .id(self.id("chip-all"))
                        .show(ui)
                        .activated
                    {
                        picked = Some((Selection::All, all));
                    }
                });
            });
        focus::forward_scroll(ui);
        picked
    }

    fn toolbar(&mut self, ui: &mut Ui, rect: Rect, cats: Option<&[Category]>) {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let placeholder = match &self.selection {
            Some(Selection::Category(_)) => {
                format!("Search in {}", self.selection_name(cats))
            }
            _ => format!("Search {}", K::PLURAL),
        };
        let search_w = (rect.width() * 0.45).clamp(240.0, 440.0);
        let sort_id = self.sort_id();
        let search_id = self.id("search");
        let labels: Vec<&str> = VodSort::ALL.iter().map(|s| s.label()).collect();
        let current = VodSort::ALL
            .iter()
            .position(|s| *s == self.sort)
            .unwrap_or(0);
        let mut picked_sort = None;
        let mut changed = false;
        let query = &mut self.query;
        focus::scope(&mut child, Id::unique((K::KEY, "toolbar")), |ui| {
            let r = SearchField::new(search_id, &placeholder)
                .size(search_w, 44.0)
                .show(ui, query);
            changed = r.changed;
            // Right at the end of the text moves on to the sort menu (the field keeps
            // Left/Right for editing otherwise).
            if r.focus.focused && ui.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                let len = query.chars().count();
                let at_end = egui::TextEdit::load_state(ui.ctx(), search_id.with("te"))
                    .and_then(|s| s.cursor.char_range())
                    .is_none_or(|c| c.primary.index.0 >= len);
                if at_end {
                    focus::request(ui.ctx(), Dropdown::button_id(sort_id));
                }
            }
            let w = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(w, 44.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    picked_sort = Dropdown::new(sort_id, "Sort: ", &labels).show(ui, current);
                },
            );
        });
        if changed {
            self.query_changed = Some(Instant::now());
        }
        if let Some(i) = picked_sort
            && let Some(s) = VodSort::ALL.get(i)
        {
            self.sort = *s;
            self.scroll_to = Some(0);
        }
    }

    fn body(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, body: Rect, session: &Arc<Session>) {
        let mut child = ui.new_child(UiBuilder::new().max_rect(body));
        let ui = &mut child;
        // Categories failed and nothing to show.
        if self.categories.value().is_none()
            && let Some(err) = self.categories.error().cloned()
        {
            if EmptyState::new(
                self.id("cats-error"),
                Illustration::ErrorOffline,
                err.title(),
                &err.user_message(),
            )
            .action("Retry", true)
            .show(ui)
            .is_some_and(|f| f.activated)
            {
                self.load_categories(cx);
            }
            return;
        }
        let listing = self.current_listing();
        let view = self
            .view
            .value()
            .filter(|v| listing.as_ref().is_some_and(|l| l.serial == v.key.serial))
            .map(|v| Arc::clone(&v.indices));
        match (&listing, view) {
            (Some(listing), Some(indices)) => {
                if listing.items.is_empty() {
                    let head = format!("No {} here", K::PLURAL);
                    let body = format!("Your provider lists no {} in this category.", K::PLURAL);
                    let _ =
                        EmptyState::new(self.id("empty"), Illustration::EmptySearch, &head, &body)
                            .illustration_size(160.0)
                            .show(ui);
                } else if indices.is_empty() {
                    if EmptyState::new(
                        self.id("no-match"),
                        Illustration::EmptySearch,
                        "No matches",
                        "Try a shorter word, or search in another category.",
                    )
                    .action("Clear search", false)
                    .illustration_size(160.0)
                    .show(ui)
                    .is_some_and(|f| f.activated)
                    {
                        self.query.clear();
                        self.applied_query.clear();
                        focus::request(&cx.egui, self.id("search").with("te"));
                    }
                } else {
                    let listing = Arc::clone(listing);
                    self.grid(cx, ui, &listing, &indices, session);
                }
            }
            _ => {
                if listing.is_none()
                    && !self.list.is_loading()
                    && let Some(err) = self.list.error().cloned()
                {
                    if EmptyState::new(
                        self.id("list-error"),
                        Illustration::ErrorOffline,
                        err.title(),
                        &err.user_message(),
                    )
                    .action("Retry", true)
                    .show(ui)
                    .is_some_and(|f| f.activated)
                        && let Some(sel) = self.selection.clone()
                    {
                        self.load_list(cx, sel);
                    }
                } else if self.selection == Some(Selection::All) && listing.is_none() {
                    self.all_progress(cx, ui);
                } else {
                    self.grid_skeleton(cx, ui);
                }
            }
        }
    }

    fn grid(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        listing: &Arc<Listing<K::Item>>,
        indices: &Arc<Vec<usize>>,
        session: &Arc<Session>,
    ) {
        // Posters widen a little so the columns fill the row (no ragged right edge).
        let base = cx.layout.poster_width;
        let inner = ui.available_width() - 32.0 + 16.0;
        let cols = ((inner / (base + 16.0)).floor()).max(1.0);
        let w = ((inner / cols) - 16.0)
            .floor()
            .clamp(base * 0.8, base * 1.2);
        let cell = vec2(w, PosterCard::height(w));
        let images = cx.services.images.clone();
        let tv = cx.layout.tv;
        let favs = session.favourite_ids(K::CONTENT);
        let progress = self.progress.value().cloned().unwrap_or_default();
        let items = Arc::clone(&listing.items);
        let key = K::KEY;
        let mut open = None;
        let id_of = |i: usize| {
            indices
                .get(i)
                .and_then(|&k| items.get(k))
                .map_or(Id::NULL, |it| card_id(key, it.key()))
        };
        VirtualGrid::new(self.id("grid"), cell, indices.len())
            .gap(vec2(16.0, 24.0))
            .scroll_to(self.scroll_to.take())
            .show(ui, id_of, |ui, i, rect| {
                let Some(item) = indices.get(i).and_then(|&k| items.get(k)) else {
                    return;
                };
                let id = item.key();
                let img = images.get(K::poster(item), ImageSize::Poster);
                let shown = vod::display_title(item.name());
                let meta = K::meta(item);
                let p = progress.get(&id).and_then(|p| {
                    if p.finished {
                        Some(1.0)
                    } else {
                        p.fraction().map(|f| f as f32).filter(|f| *f > 0.01)
                    }
                });
                let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
                let f = PosterCard::new(card_id(key, id), &img, shown.title, rect.width())
                    .meta(Some(&meta))
                    .meta_on_focus_only(tv)
                    .progress(p)
                    .autofocus(i == 0)
                    .show(&mut child);
                if favs.contains(&id.to_string()) {
                    heart_badge(ui, rect, f.anim);
                }
                if f.activated {
                    open = Some(id);
                }
            });
        if let Some(id) = open {
            cx.nav.push(K::detail(id));
        }
    }

    fn grid_skeleton(&self, cx: &AppCtx<'_>, ui: &mut Ui) {
        let loading = self.list.show_skeleton()
            || self.categories.show_skeleton()
            || (self.selection.is_none() && self.categories.is_loading());
        if !loading {
            return;
        }
        let rect = ui.max_rect().shrink(16.0);
        let w = cx.layout.poster_width;
        let h = (w * 1.5).round();
        let cols = (((rect.width() + 16.0) / (w + 16.0)).floor() as usize).max(1);
        let painter = ui.painter().clone();
        let mut y = rect.top();
        while y < rect.bottom() {
            for c in 0..cols {
                let x = rect.left() + c as f32 * (w + 16.0);
                skeleton(
                    &cx.egui,
                    &painter,
                    Rect::from_min_size(pos2(x, y), vec2(w, h)),
                    radius::MD,
                );
                skeleton(
                    &cx.egui,
                    &painter,
                    Rect::from_min_size(pos2(x, y + h + 14.0), vec2(w * 0.7, 12.0)),
                    radius::XS,
                );
            }
            y += PosterCard::height(w) + 24.0;
        }
    }

    /// "All" is loading: spinner, elapsed time, Cancel.
    fn all_progress(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        let c = pos2(rect.center().x, rect.top() + rect.height() * 0.3);
        let time = ui.input(|i| i.time);
        paint::spinner(&painter, c, 24.0, time, 3.0);
        painter.text(
            c + vec2(0.0, 64.0),
            Align2::CENTER_CENTER,
            format!("Loading all {}", K::PLURAL),
            font::display_sized(28.0),
            color::TEXT,
        );
        let elapsed = self.list_started.map_or(0, |t| t.elapsed().as_secs());
        let body = format!(
            "Big catalogues can take a minute ({elapsed} s so far). Single categories load \
             much faster."
        );
        let mut job = text::job(&body, font::body(), color::TEXT_2, 480.0, 2);
        job.halign = egui::Align::Center;
        let g = painter.layout_job(job);
        painter.galley(c + vec2(0.0, 98.0), g, color::TEXT_2);
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        let b = Button::secondary("Cancel")
            .icon(icons::X)
            .id(self.id("all-cancel"))
            .autofocus();
        let size = b.size(ui);
        let r = Rect::from_min_size(pos2(c.x - size.x / 2.0, c.y + 170.0), size);
        let mut child = ui.new_child(UiBuilder::new().max_rect(r));
        if b.show(&mut child).activated {
            self.list.cancel();
            let back = self.previous.take().filter(|p| *p != Selection::All);
            self.selection = None;
            match back {
                Some(sel) => self.apply_selection(cx, sel),
                None => self.decide_initial(cx),
            }
        }
    }

    fn pin_dialog(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(p) = &mut self.pin else { return };
        let title = format!("\u{201c}{}\u{201d} is locked", text::cap(&p.name, 40));
        match p.prompt.show_dialog(ui.ctx(), cx.services, &title, None) {
            PinPromptResult::Verified(_) => {
                if let (Some(session), Selection::Category(id)) = (cx.session(), &p.selection) {
                    session.unlock_category(K::CONTENT, id);
                }
                let sel = p.selection.clone();
                self.pin = None;
                self.apply_selection(cx, sel);
            }
            PinPromptResult::Cancelled | PinPromptResult::Forgot => {
                self.pin = None;
                if self.selection.is_none() {
                    self.decide_initial(cx);
                }
            }
            PinPromptResult::Pending => {}
        }
    }
}

/// A favourite heart on a poster's top-right corner (follows the focus lift).
fn heart_badge(ui: &Ui, cell: Rect, anim: f32) {
    let w = cell.width();
    let img = Rect::from_min_size(cell.min, vec2(w, (w * 1.5).round()));
    let img = if crate::ui::motion::reduced(ui.ctx()) {
        img
    } else {
        img.expand2(img.size() * 0.03 * anim)
    };
    let c = pos2(img.right() - 20.0, img.top() + 20.0);
    let p = ui.painter();
    p.circle_filled(c, 15.0, Color32::from_black_alpha(160));
    icons::paint(p, c, icons::HEART, 17.0, Style::Fill, color::ACCENT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_core::VodStream;

    fn movie(id: u64, name: &str, cat: &str, added: i64, rating: f64) -> VodStream {
        serde_json::from_value(serde_json::json!({
            "stream_id": id, "name": name, "category_id": cat,
            "added": added.to_string(), "rating": rating.to_string(),
        }))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn listing(items: Vec<VodStream>) -> Listing<VodStream> {
        let index = SearchIndex::new(&items);
        Listing {
            serial: 1,
            selection: Selection::All,
            items: Arc::new(items),
            index,
            orders: Mutex::new(HashMap::new()),
        }
    }

    fn cats(v: &VodStream) -> Vec<&CategoryId> {
        v.category_id.iter().chain(v.category_ids.iter()).collect()
    }

    #[test]
    fn views_filter_sort_and_hide_locked_categories() {
        let l = listing(vec![
            movie(1, "Zebra Crossing (2001)", "1", 100, 6.0),
            movie(2, "FR | Éclair de lune", "2", 300, 8.0),
            movie(3, "Adult Thing", "9", 200, 9.0),
            movie(4, "apple pie", "1", 400, 0.0),
        ]);
        let none = HashSet::new();
        let ids = |v: Vec<usize>| {
            v.into_iter()
                .map(|i| l.items[i].stream_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(compute_view(&l, "", VodSort::Server, &none, cats)),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            ids(compute_view(&l, "", VodSort::Name, &none, cats)),
            vec![3, 4, 2, 1]
        );
        assert_eq!(
            ids(compute_view(&l, "", VodSort::Recent, &none, cats)),
            vec![4, 2, 3, 1]
        );
        assert_eq!(
            ids(compute_view(&l, "", VodSort::Rating, &none, cats)),
            vec![3, 2, 1, 4]
        );
        // Accent-insensitive plain search, never a regex.
        assert_eq!(
            ids(compute_view(&l, "ECLAIR", VodSort::Server, &none, cats)),
            vec![2]
        );
        assert!(compute_view(&l, ".*", VodSort::Server, &none, cats).is_empty());
        assert_eq!(
            ids(compute_view(&l, "(2001", VodSort::Server, &none, cats)),
            vec![1]
        );
        // Locked categories stay out of "All".
        let locked: HashSet<CategoryId> = [CategoryId::from("9")].into_iter().collect();
        assert_eq!(
            ids(compute_view(&l, "", VodSort::Rating, &locked, cats)),
            vec![2, 1, 4]
        );
        // Sort orders are cached per list.
        let a = l.order(VodSort::Name);
        let b = l.order(VodSort::Name);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count_label(1, "movie", "movies"), "1 movie");
        assert_eq!(count_label(35_482, "movie", "movies"), "35482 movies");
    }
}

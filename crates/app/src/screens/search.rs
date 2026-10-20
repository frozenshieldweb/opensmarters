// SPDX-License-Identifier: GPL-3.0-or-later
//! Search: one field across live channels, films, series and (cached) programme titles
//! of the active profile.
//!
//! - **Off the UI thread:** the sources are gathered in a loader and the corpus
//!   (pre-normalised names) is built on the blocking pool **only when they changed**
//!   (a content fingerprint, [`Sources::id`]); coming back from a detail page reuses it.
//!   Each query (debounced 200 ms) runs on the blocking pool; a query that extends the
//!   previous one only re-checks the previous matches ([`engine::Corpus::search`]).
//!   Results carry the sources they index into ([`Results`]), so a rebuild can never
//!   make old hits point into new lists.
//! - **Plain text:** case- and diacritics-insensitive substring search, never a regex.
//! - **Large catalogues:** films/series are searched in what is already cached (the
//!   "All" list, or the categories opened before). The 35 000-film "All" list is only
//!   fetched when the user asks ("Search all films").
//! - **Keyboard first:** typing anywhere goes to the field; ↓ (or Enter) from the field
//!   jumps to the first result. Recent searches (per profile, `services::search_history`;
//!   Settings › Storage clears them) show as chips while the field is empty.
//! - **Parental locks:** items of locked categories are never listed. Every category of
//!   an item counts (not just the primary one), and while parental control is on the
//!   film/series category lists are loaded first (keyword locks match their names); if
//!   they can't be, films and series are left out.

mod engine;

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, CornerRadius, Event, Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2,
};
use opensmarters_core::chrono::{Duration as ChronoDuration, Local, Utc};
use opensmarters_core::{Category, CategoryId, Series, VodStream};
use opensmarters_store::ContentKind;
use serde::de::DeserializeOwned;

use super::common::{channel_list, locks};
use super::{AppCtx, Screen, shell};
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{Route, Section};
use crate::playback::ZapContext;
use crate::services::catalog::{LiveChannel, Programme, split_tag};
use crate::services::session::guide::xmltv_programme;
use crate::services::session::{series_key, vod_streams_key};
use crate::services::{ImageSize, Session, search_history, vod};
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::callout::Badge;
use crate::ui::widgets::{
    Button, ChannelLogo, ChannelTile, Chip, EmptyState, PosterCard, Rail, SearchField, VirtualGrid,
    VirtualList,
};
use crate::ui::{paint, text};
pub use engine::{Corpus, Hits, Kind, push_recent, rank, thousands};

/// Recent-search chips shown (the per-profile history keeps more).
const RECENT_CHIPS: usize = 8;
const DEBOUNCE: Duration = Duration::from_millis(200);
/// Cards per group in the "All" view.
const RAIL_MAX: usize = 30;
/// Programme rows in the "All" view.
const PROGRAMME_ROWS: usize = 6;
const ROW_H: f32 = 64.0;

fn field_id() -> Id {
    // The focus id of `SearchField` is its id `.with("te")`.
    Id::unique("search.field").with("te")
}

/// A programme result.
#[derive(Debug, Clone)]
pub struct ProgrammeHit {
    /// Index into the channel list.
    pub channel: usize,
    /// The programme.
    pub programme: Programme,
}

/// Everything searchable for the session, with the corpus.
#[derive(Debug, Default)]
pub struct Sources {
    /// Fingerprint of the content: equal ids mean equal lists (and corpus).
    id: u64,
    channels: Arc<Vec<LiveChannel>>,
    movies: Vec<VodStream>,
    series: Vec<Series>,
    programmes: Vec<ProgrammeHit>,
    movies_complete: bool,
    series_complete: bool,
    /// Parental control is on but the film/series categories couldn't be loaded: films
    /// and series are left out rather than listed unchecked.
    vod_withheld: bool,
    corpus: Corpus,
}

impl Sources {
    /// The content fingerprint ([`Sources::id`]).
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// Search results, with the sources their indices point into.
#[derive(Debug)]
pub struct Results {
    sources: Arc<Sources>,
    hits: Hits,
}

/// What a film or series list entry needs for gathering.
trait VodItem: Clone + DeserializeOwned + Send + Sync + 'static {
    const KIND: ContentKind;
    const CATEGORIES_KEY: &'static str;
    fn list_key(category: Option<&CategoryId>) -> String;
    fn item_id(&self) -> u64;
    fn categories(&self) -> Vec<&CategoryId>;
}

impl VodItem for VodStream {
    const KIND: ContentKind = ContentKind::Movie;
    const CATEGORIES_KEY: &'static str = "xt:vod_categories";
    fn list_key(category: Option<&CategoryId>) -> String {
        vod_streams_key(category)
    }
    fn item_id(&self) -> u64 {
        self.stream_id
    }
    fn categories(&self) -> Vec<&CategoryId> {
        locks::item_categories(self.category_id.as_ref(), &self.category_ids)
    }
}

impl VodItem for Series {
    const KIND: ContentKind = ContentKind::Series;
    const CATEGORIES_KEY: &'static str = "xt:series_categories";
    fn list_key(category: Option<&CategoryId>) -> String {
        series_key(category)
    }
    fn item_id(&self) -> u64 {
        self.series_id
    }
    fn categories(&self) -> Vec<&CategoryId> {
        locks::item_categories(self.category_id.as_ref(), &self.category_ids)
    }
}

/// Films or series gathered from the cache.
#[derive(Debug)]
struct Gathered<T> {
    items: Vec<T>,
    /// The "All" list was cached (everything was searched).
    complete: bool,
    /// Categories searched / categories known.
    cats: (usize, usize),
    /// Left out because the locks couldn't be checked.
    withheld: bool,
}

impl<T> Gathered<T> {
    /// Nothing (M3U profiles have no films/series lists).
    fn none() -> Self {
        Self {
            items: Vec::new(),
            complete: true,
            cats: (0, 0),
            withheld: false,
        }
    }
}

/// The category list of `T` for the lock check: cached, else (parental control on)
/// loaded from the provider, because keyword locks need the names.
async fn categories_for_locks<T: VodItem>(session: &Arc<Session>) -> Option<Arc<Vec<Category>>> {
    if let Some(c) = session
        .peek_cached::<Vec<Category>>(T::CATEGORIES_KEY)
        .await
    {
        return Some(c);
    }
    if !session.parental_enabled() {
        return None;
    }
    match T::KIND {
        ContentKind::Series => session.series_categories().await.ok(),
        _ => session.vod_categories().await.ok(),
    }
}

async fn gather<T: VodItem>(session: &Arc<Session>) -> Gathered<T> {
    let cats = categories_for_locks::<T>(session).await;
    if session.parental_enabled() && cats.is_none() {
        return Gathered {
            items: Vec::new(),
            complete: true,
            cats: (0, 0),
            withheld: true,
        };
    }
    let names = cats
        .as_deref()
        .map(|c| locks::CategoryNames::new(c.iter()))
        .unwrap_or_default();
    let locked = |item: &T| locks::is_item_locked(session, T::KIND, &item.categories(), &names);
    if let Some(all) = session.peek_cached::<Vec<T>>(&T::list_key(None)).await {
        return Gathered {
            items: all.iter().filter(|i| !locked(i)).cloned().collect(),
            complete: true,
            cats: (0, 0),
            withheld: false,
        };
    }
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut loaded = 0;
    let total = cats.as_ref().map_or(0, |c| c.len());
    if let Some(cats) = &cats {
        for c in cats.iter() {
            if session.is_category_locked(T::KIND, &c.id, &c.name) {
                continue;
            }
            if let Some(list) = session
                .peek_cached::<Vec<T>>(&T::list_key(Some(&c.id)))
                .await
            {
                loaded += 1;
                for i in list.iter() {
                    // Also in a locked category: left out.
                    if seen.insert(i.item_id()) && !locked(i) {
                        items.push(i.clone());
                    }
                }
            }
        }
    }
    Gathered {
        items,
        complete: false,
        cats: (loaded, total),
        withheld: false,
    }
}

/// The content fingerprint of the lists a corpus is built from.
fn fingerprint(
    channels: &[LiveChannel],
    movies: &Gathered<VodStream>,
    series: &Gathered<Series>,
    programmes: &[ProgrammeHit],
) -> u64 {
    let mut h = DefaultHasher::new();
    channels.len().hash(&mut h);
    for c in channels {
        c.key.hash(&mut h);
        c.name.hash(&mut h);
    }
    for g in [
        (movies.complete, movies.cats, movies.withheld),
        (series.complete, series.cats, series.withheld),
    ] {
        g.hash(&mut h);
    }
    movies.items.len().hash(&mut h);
    for m in &movies.items {
        m.stream_id.hash(&mut h);
        m.name.hash(&mut h);
    }
    series.items.len().hash(&mut h);
    for s in &series.items {
        s.series_id.hash(&mut h);
        s.name.hash(&mut h);
    }
    programmes.len().hash(&mut h);
    for p in programmes {
        p.channel.hash(&mut h);
        p.programme.start.timestamp().hash(&mut h);
        p.programme.stop.timestamp().hash(&mut h);
        p.programme.title.hash(&mut h);
        p.programme.has_archive.hash(&mut h);
    }
    // Never 0 (the id of empty default sources).
    h.finish().max(1)
}

/// Collects everything searchable (cache only, except the live catalogue and, with
/// parental control on, the category lists) and builds the corpus on the blocking pool,
/// unless `previous` already has exactly this content.
async fn build_sources(
    session: Arc<Session>,
    previous: Option<Arc<Sources>>,
) -> Result<Arc<Sources>, AppError> {
    let channels: Vec<LiveChannel> = match session.live_catalog().await {
        Ok(cat) => channel_list::unlocked_in_session(&session, &cat, &cat.channels),
        Err(_) => Vec::new(),
    };
    let (movies, series) = if session.is_xtream() {
        (
            gather::<VodStream>(&session).await,
            gather::<Series>(&session).await,
        )
    } else {
        (Gathered::none(), Gathered::none())
    };
    let rows = session.cached_guide_rows();
    let xmltv = session.loaded_xmltv();
    tokio::task::spawn_blocking(move || {
        let now = Utc::now();
        let from = now - ChronoDuration::hours(3);
        let until = now + ChronoDuration::hours(24);
        let index: HashMap<&str, usize> = channels
            .iter()
            .enumerate()
            .map(|(i, c)| (c.key.as_str(), i))
            .collect();
        let mut programmes = Vec::new();
        for (key, row) in &rows {
            let Some(&ci) = index.get(key.as_str()) else {
                continue;
            };
            for p in &row.programmes {
                if p.stop > from && p.start < until {
                    programmes.push(ProgrammeHit {
                        channel: ci,
                        programme: p.clone(),
                    });
                }
            }
        }
        if let Some(epg) = &xmltv {
            for (ci, ch) in channels.iter().enumerate() {
                let Some(id) = ch.epg_id.as_deref() else {
                    continue;
                };
                for p in epg.programmes_for(id) {
                    let stop = p.stop.unwrap_or(p.start + ChronoDuration::minutes(30));
                    if stop > from && p.start < until {
                        programmes.push(ProgrammeHit {
                            channel: ci,
                            programme: xmltv_programme(p),
                        });
                    }
                }
            }
        }
        programmes.sort_by_key(|h| (h.programme.start, h.channel));
        let id = fingerprint(&channels, &movies, &series, &programmes);
        if let Some(prev) = previous.filter(|p| p.id == id) {
            // Same content: keep the corpus (and the results that index into it).
            return Ok(prev);
        }
        let mut corpus = Corpus::default();
        corpus.add(Kind::Channels, channels.iter().map(|c| c.name.as_str()));
        corpus.add(Kind::Movies, movies.items.iter().map(|m| m.name.as_str()));
        corpus.add(Kind::Series, series.items.iter().map(|s| s.name.as_str()));
        corpus.add(
            Kind::Programmes,
            programmes.iter().map(|h| h.programme.title.as_str()),
        );
        Ok(Arc::new(Sources {
            id,
            channels: Arc::new(channels),
            movies: movies.items,
            series: series.items,
            programmes,
            movies_complete: movies.complete,
            series_complete: series.complete,
            vod_withheld: movies.withheld || series.withheld,
            corpus,
        }))
    })
    .await
    .map_err(|_| AppError::Cancelled)?
}

/// "a", "a and b", "a, b and c".
fn natural_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// The quiet line under the results: what was searched, in plain words.
fn scope_caption(src: &Sources, xtream: bool) -> String {
    let mut parts = vec![plural(src.channels.len(), "channel", "channels")];
    if xtream && !src.vod_withheld {
        parts.push(plural(src.movies.len(), "film", "films"));
        parts.push(plural(src.series.len(), "series", "series"));
    }
    if !src.programmes.is_empty() {
        parts.push(plural(
            src.programmes.len(),
            "programme in the guide",
            "programmes in the guide",
        ));
    }
    let mut s = format!("Searched {}.", natural_list(&parts));
    if xtream && !src.vod_withheld && (!src.movies_complete || !src.series_complete) {
        s.push_str(" Films and series come from the categories you've opened.");
    }
    if src.vod_withheld {
        s.push_str(" Films and series are hidden while their categories can't be checked for parental locks.");
    }
    s
}

/// What "Search all …" loads.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LoadAll {
    Movies,
    Series,
    Both,
}

impl LoadAll {
    fn of(kind: Kind) -> Self {
        if kind == Kind::Movies {
            LoadAll::Movies
        } else {
            LoadAll::Series
        }
    }

    fn covers(self, kind: Kind) -> bool {
        matches!(
            (self, kind),
            (LoadAll::Both, _) | (LoadAll::Movies, Kind::Movies) | (LoadAll::Series, Kind::Series)
        )
    }
}

/// The Search screen.
#[derive(Debug)]
pub struct SearchScreen {
    query: String,
    sources: Loader<Arc<Sources>>,
    hits: Loader<Arc<Results>>,
    searched: Option<(String, u64)>,
    last_edit: Instant,
    filter: Option<Kind>,
    recent: Loader<Vec<String>>,
    load_all: Loader<()>,
    load_all_kind: Option<LoadAll>,
    first_result: Option<Id>,
    focus_first: bool,
    details: Option<(Arc<Sources>, ProgrammeHit)>,
}

impl SearchScreen {
    /// Creates the screen with an initial query (from `search:q=…`).
    pub fn new(query: String) -> Self {
        Self {
            query: text::cap(&query, 200).into_owned(),
            sources: Loader::new(),
            hits: Loader::new(),
            searched: None,
            last_edit: Instant::now() - DEBOUNCE,
            filter: None,
            recent: Loader::new(),
            load_all: Loader::new(),
            load_all_kind: None,
            first_result: None,
            focus_first: false,
            details: None,
        }
    }

    /// Gathers the sources again; the corpus is rebuilt only if they changed.
    fn load_sources(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let previous = self.sources.value().cloned();
        self.sources
            .start(cx.rt(), &cx.egui, move |_| build_sources(session, previous));
    }

    fn remember(&mut self, cx: &AppCtx<'_>) {
        let q = self.query.trim().to_owned();
        if q.is_empty() {
            return;
        }
        let Some(session) = cx.session() else { return };
        // Show it at once; the shared per-profile history is the source of truth
        // (Settings › Storage clears it).
        let mut list = self.recent.value().cloned().unwrap_or_default();
        push_recent(&mut list, &q, RECENT_CHIPS);
        self.recent.set(list);
        let profile = session.profile_id();
        cx.services.blocking("save recent searches", move |s| {
            search_history::record(s, profile, &q)
        });
    }

    /// Runs the (debounced) search when the query or the sources changed.
    fn maybe_search(&mut self, cx: &AppCtx<'_>) {
        let Some(src) = self.sources.value().cloned() else {
            return;
        };
        let key = (self.query.trim().to_owned(), src.id);
        if self.searched.as_ref() == Some(&key) {
            return;
        }
        if self.last_edit.elapsed() < DEBOUNCE {
            cx.egui
                .request_repaint_after(DEBOUNCE.saturating_sub(self.last_edit.elapsed()));
            return;
        }
        // Narrow the previous results only when they index into these very sources.
        let previous = self
            .hits
            .value()
            .filter(|r| r.sources.id == src.id)
            .cloned();
        let q = key.0.clone();
        self.searched = Some(key);
        self.hits.start_blocking(cx.rt(), &cx.egui, move || {
            let hits = src.corpus.search(&q, previous.as_ref().map(|r| &r.hits));
            Ok(Arc::new(Results { sources: src, hits }))
        });
    }

    fn start_load_all(&mut self, cx: &AppCtx<'_>, what: LoadAll) {
        let Some(session) = cx.session() else { return };
        self.load_all_kind = Some(what);
        self.load_all.start(cx.rt(), &cx.egui, move |_| async move {
            if what != LoadAll::Series {
                session.vod_streams(None).await?;
            }
            if what != LoadAll::Movies {
                session.series_list(None).await?;
            }
            Ok(())
        });
    }

    fn loading_all(&self) -> Option<LoadAll> {
        self.load_all
            .is_loading()
            .then_some(self.load_all_kind)
            .flatten()
    }

    fn activate(&mut self, cx: &mut AppCtx<'_>, res: &Results, kind: Kind, idx: usize) {
        let Some(session) = cx.session() else { return };
        let src = &res.sources;
        self.remember(cx);
        match kind {
            Kind::Channels => {
                // Zap through the channel results, in result order.
                let list: Vec<LiveChannel> = res
                    .hits
                    .of(Kind::Channels)
                    .iter()
                    .filter_map(|&i| src.channels.get(i as usize).cloned())
                    .collect();
                let Some(pos) = res
                    .hits
                    .of(Kind::Channels)
                    .iter()
                    .position(|&i| i as usize == idx)
                else {
                    return;
                };
                let channels = Arc::new(list);
                if let Some(ch) = channels.get(pos) {
                    match session.live_request(
                        ch,
                        Some(ZapContext {
                            channels: Arc::clone(&channels),
                            index: pos,
                        }),
                    ) {
                        Ok(req) => cx.play(req, true),
                        Err(e) => cx.toasts.error(e.user_message()),
                    }
                }
            }
            Kind::Movies => {
                if let Some(m) = src.movies.get(idx) {
                    cx.nav.push(Route::MovieDetail {
                        stream_id: m.stream_id,
                    });
                }
            }
            Kind::Series => {
                if let Some(s) = src.series.get(idx) {
                    cx.nav.push(Route::SeriesDetail {
                        series_id: s.series_id,
                    });
                }
            }
            Kind::Programmes => {
                let Some(h) = src.programmes.get(idx).cloned() else {
                    return;
                };
                let Some(ch) = src.channels.get(h.channel).cloned() else {
                    return;
                };
                let p = &h.programme;
                let now = cx.now;
                if p.start <= now && now < p.stop {
                    let channels = Arc::clone(&src.channels);
                    match session.live_request(
                        &ch,
                        Some(ZapContext {
                            channels,
                            index: h.channel,
                        }),
                    ) {
                        Ok(req) => cx.play(req, true),
                        Err(e) => cx.toasts.error(e.user_message()),
                    }
                } else if p.stop <= now
                    && p.has_archive
                    && let Some(sid) = ch.stream_id()
                {
                    super::catchup::open(cx, sid, Some(p.start.timestamp()), true);
                } else {
                    self.details = Some((Arc::clone(src), h));
                }
            }
        }
    }

    fn clear_query(&mut self, cx: &AppCtx<'_>) {
        self.query.clear();
        self.filter = None;
        self.last_edit = Instant::now() - DEBOUNCE;
        focus::request(&cx.egui, field_id());
    }
}

impl Screen for SearchScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        // The user may have opened categories or loaded guides meanwhile: gather again
        // (the corpus is only rebuilt when something changed).
        self.load_sources(cx);
        // Re-read every time: Settings › Storage may have cleared it.
        if let Some(session) = cx.session() {
            let store = cx.services.store.clone();
            let profile = session.profile_id();
            self.recent.start_blocking(cx.rt(), &cx.egui, move || {
                let mut list = search_history::load(&store, profile).unwrap_or_default();
                list.truncate(RECENT_CHIPS);
                Ok(list)
            });
        }
        if self.query.is_empty() {
            focus::request(&cx.egui, field_id());
        }
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if self.details.is_some() {
            return;
        }
        let in_field = focus::is_focused(ctx, field_id());
        if in_field {
            // ↓ / Enter jump from the field to the first result.
            let (down, enter) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.key_pressed(Key::Enter),
                )
            });
            if enter {
                self.remember(cx);
            }
            if (down || enter) && !self.query.trim().is_empty() {
                match self.first_result {
                    Some(id) => focus::request(ctx, id),
                    None => self.focus_first = true,
                }
            } else if down {
                focus::give_back_nav(ctx, focus::Dir::Down);
            }
            return;
        }
        if focus::text_input_focused(ctx) {
            return;
        }
        // Typing anywhere goes to the field.
        let typed: String = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect()
        });
        if !typed.trim().is_empty() && !ctx.input(|i| i.modifiers.ctrl || i.modifiers.alt) {
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Text(_))));
            if self.query.chars().count() + typed.chars().count() <= 200 {
                self.query.push_str(&typed);
            }
            self.last_edit = Instant::now();
            focus::request(ctx, field_id());
        }
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        if self.details.take().is_some() {
            return true;
        }
        if self.filter.take().is_some() {
            return true;
        }
        false
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Search)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.sources.poll();
        self.hits.poll();
        self.recent.poll();
        if self.load_all.poll() {
            if let Some(e) = self.load_all.error() {
                cx.toasts.error(e.user_message());
            } else {
                self.load_sources(cx);
            }
            self.load_all_kind = None;
        }
        let Some(session) = cx.session() else { return };
        shell::status_cluster(cx, ui);
        let content = cx.layout.content;
        let m = cx.layout.margin;
        shell::page_title(cx, ui, "Search");

        // Field.
        let field_w = (content.width() - 2.0 * m).min(880.0);
        let field_rect = Rect::from_min_size(
            pos2(content.left() + m, content.top() + 96.0),
            vec2(field_w, 52.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field_rect));
        let before = self.query.clone();
        let resp = focus::scope(&mut child, Id::unique("search.field-scope"), |ui| {
            SearchField::new(
                Id::unique("search.field"),
                "Search channels, films, series and programmes",
            )
            .size(field_w, 52.0)
            .autofocus(self.query.is_empty())
            .show(ui, &mut self.query)
        });
        if resp.changed || before != self.query {
            self.last_edit = Instant::now();
        }
        self.maybe_search(cx);

        let query_empty = self.query.trim().is_empty();
        let painter = ui.painter().clone();
        let mut y = field_rect.bottom() + 20.0;
        let preparing = self.sources.value().is_none() && self.sources.show_skeleton();
        if preparing {
            paint::spinner(
                &painter,
                pos2(field_rect.left() + 16.0, y + 8.0),
                7.0,
                ui.input(|i| i.time),
                2.0,
            );
            painter.text(
                pos2(field_rect.left() + 32.0, y + 8.0),
                Align2::LEFT_CENTER,
                "Getting search ready…",
                font::caption(),
                color::TEXT_3,
            );
            ui.ctx().request_repaint();
            y += 28.0;
        }

        let area = Rect::from_min_max(
            pos2(content.left(), y),
            pos2(content.right(), content.bottom()),
        );
        if query_empty {
            self.first_result = None;
            // A pending "focus the first result" belongs to a query that is gone.
            self.focus_first = false;
            self.recents(
                cx,
                ui,
                Rect::from_min_max(
                    pos2(area.left() + m, area.top()),
                    pos2(area.right() - m, area.bottom()),
                ),
            );
            return;
        }
        let Some(res) = self.hits.value().cloned() else {
            return;
        };
        let hits = &res.hits;
        // A group filter whose group came up empty: back to All.
        if self.filter.is_some_and(|k| hits.of(k).is_empty()) {
            self.filter = None;
        }

        if hits.total() == 0 {
            // ↓/Enter from the field lands on "Clear search" (the first thing below).
            let clear = Id::unique("search.none").with("action");
            self.first_result = Some(clear);
            if std::mem::take(&mut self.focus_first) {
                focus::request(&cx.egui, clear);
            }
            self.no_matches(cx, ui, &session, &res.sources, area);
            self.overlays(cx, ui, &session);
            return;
        }

        // Type chips: only groups with results.
        let chips = Rect::from_min_size(
            pos2(area.left() + m - 8.0, area.top()),
            vec2(area.width() - 2.0 * m + 16.0, 52.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(chips));
        let mut pick: Option<Option<Kind>> = None;
        focus::scope(&mut child, Id::unique("search.types"), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_space(8.0);
                if Chip::new("All")
                    .count(hits.total())
                    .selected(self.filter.is_none())
                    .id(Id::unique("search.type.all"))
                    .show(ui)
                    .activated
                {
                    pick = Some(None);
                }
                for kind in Kind::ALL {
                    let n = hits.of(kind).len();
                    if n == 0 {
                        continue;
                    }
                    if Chip::new(kind.title())
                        .count(n)
                        .selected(self.filter == Some(kind))
                        .id(Id::unique(("search.type", kind.title())))
                        .show(ui)
                        .activated
                    {
                        pick = Some(Some(kind));
                    }
                }
            });
        });
        if let Some(p) = pick {
            self.filter = p;
        }
        let results = Rect::from_min_max(pos2(area.left(), chips.bottom() + 8.0), area.max);
        match self.filter {
            None => self.all_results(cx, ui, &session, &res, results),
            Some(kind) => self.group_results(cx, ui, &session, &res, kind, results),
        }
        if self.focus_first
            && let Some(id) = self.first_result
        {
            self.focus_first = false;
            focus::request(&cx.egui, id);
        }
        self.overlays(cx, ui, &session);
    }
}

impl SearchScreen {
    /// The idle state: recent searches as chips, and what can be searched.
    fn recents(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, rect: Rect) {
        let list = self.recent.value().cloned().unwrap_or_default();
        let painter = ui.painter().clone();
        let hint = "Find channels, films, series and programmes by name. Accents and capitals don't matter.";
        if list.is_empty() {
            text::block(
                &painter,
                rect.min + vec2(8.0, 12.0),
                hint,
                font::body(),
                color::TEXT_2,
                rect.width().min(640.0),
                2,
            );
            return;
        }
        text::overline(
            &painter,
            rect.min + vec2(8.0, 16.0),
            Align2::LEFT_CENTER,
            "Recent searches",
            color::TEXT_3,
        );
        let chips = Rect::from_min_size(
            rect.min + vec2(0.0, 32.0),
            vec2(rect.width().min(960.0), 112.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(chips));
        let mut picked: Option<String> = None;
        let mut clear = false;
        focus::scope(&mut child, Id::unique("search.recent"), |ui| {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 12.0);
                ui.add_space(8.0);
                for (i, q) in list.iter().enumerate() {
                    if Chip::new(q.clone())
                        .id(Id::unique(("search.recent", i)))
                        .show(ui)
                        .activated
                    {
                        picked = Some(q.clone());
                    }
                }
                clear = Button::ghost("Clear")
                    .small()
                    .icon(icons::X)
                    .id(Id::unique("search.recent.clear"))
                    .show(ui)
                    .activated;
            });
        });
        let used = child.min_rect().bottom();
        text::block(
            &painter,
            pos2(rect.left() + 8.0, used + 24.0),
            hint,
            font::caption(),
            color::TEXT_3,
            rect.width().min(640.0),
            2,
        );
        if let Some(q) = picked {
            self.query = q;
            self.last_edit = Instant::now() - DEBOUNCE;
            self.focus_first = true;
        }
        if clear {
            self.recent.set(Vec::new());
            if let Some(session) = cx.session() {
                let profile = session.profile_id();
                cx.services.blocking("clear recent searches", move |s| {
                    search_history::clear(s, profile).map(|_| ())
                });
            }
            focus::request(&cx.egui, field_id());
        }
    }

    /// Nothing matched: the empty-search state ("No matches", Clear search) and, when
    /// films/series were only partly searched, a way to search them all.
    fn no_matches(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        src: &Sources,
        area: Rect,
    ) {
        let partial = session.is_xtream()
            && !src.vod_withheld
            && (!src.movies_complete || !src.series_complete);
        let loading = self.loading_all();
        let what = match (src.movies_complete, src.series_complete) {
            (false, true) => LoadAll::Movies,
            (true, false) => LoadAll::Series,
            _ => LoadAll::Both,
        };
        let body = if loading.is_some() {
            "Loading every film and series from your provider. This can take a while on large catalogues."
        } else if partial {
            "Try a shorter word. Films and series were only searched in the categories you've opened."
        } else {
            "Try a shorter word, or search in another category."
        };
        let second = match what {
            LoadAll::Movies => "Search all films",
            LoadAll::Series => "Search all series",
            LoadAll::Both => "Search all films and series",
        };
        // Optically centred a little above the middle of the page.
        let rect = Rect::from_min_max(
            area.min,
            pos2(area.right(), area.bottom() - area.height() * 0.1),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let mut state = EmptyState::new(
            Id::unique("search.none"),
            Illustration::EmptySearch,
            "No matches",
            body,
        )
        .action("Clear search", false)
        .illustration_size(if cx.layout.tv { 140.0 } else { 160.0 });
        if partial && loading.is_none() {
            state = state.second_action(second, Some(icons::LIST_MAGNIFYING_GLASS));
        }
        let r = state.show_both(&mut child);
        if loading.is_some() {
            paint::spinner(
                ui.painter(),
                pos2(rect.center().x, rect.bottom() - 8.0),
                9.0,
                ui.input(|i| i.time),
                2.0,
            );
            ui.ctx().request_repaint();
        }
        if r.action.is_some_and(|f| f.activated) {
            self.clear_query(cx);
        }
        if r.second.is_some_and(|f| f.activated) {
            self.start_load_all(cx, what);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn all_results(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        res: &Arc<Results>,
        rect: Rect,
    ) {
        let m = cx.layout.margin;
        let src = &res.sources;
        let hits = &res.hits;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let images = cx.services.images.clone();
        let mut first: Option<Id> = None;
        let mut act: Option<(Kind, usize)> = None;
        let mut see_all: Option<Kind> = None;
        let mut load_all: Option<LoadAll> = None;
        let mut deferred: Vec<Kind> = Vec::new();
        let loading_all = self.loading_all();
        egui::ScrollArea::vertical()
            .id_salt("search.results")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // Channels.
                let ch = hits.of(Kind::Channels);
                if !ch.is_empty() {
                    let size = cx.layout.channel_tile_size();
                    let r = Rail::new(Id::unique("search.rail.channels"), "Channels")
                        .kicker(count_label(ch.len()))
                        .see_all(ch.len() > RAIL_MAX)
                        .margin(m)
                        .show(ui, |ui| {
                            let mut a = None;
                            for &i in ch.iter().take(RAIL_MAX) {
                                let Some(c) = src.channels.get(i as usize) else {
                                    continue;
                                };
                                let id = Id::unique(("search.ch", &c.key));
                                first.get_or_insert(id);
                                let logo = images.get(c.logo.as_deref(), ImageSize::Logo);
                                let (_, now_p, _) = session.now_next(c, cx.now);
                                let f = ChannelTile::new(id, &logo, &c.name, size)
                                    .now(now_p.as_ref().map(|p| p.title.as_str()))
                                    .progress(now_p.as_ref().map(|p| p.progress(cx.now)))
                                    .show(ui);
                                if f.activated {
                                    a = Some(i as usize);
                                }
                            }
                            a
                        });
                    if r.see_all {
                        see_all = Some(Kind::Channels);
                    }
                    if let Some(i) = r.inner {
                        act = Some((Kind::Channels, i));
                    }
                    ui.add_space(8.0);
                }
                // Films / series.
                for kind in [Kind::Movies, Kind::Series] {
                    if !session.is_xtream() || src.vod_withheld {
                        continue;
                    }
                    let list = hits.of(kind);
                    let complete = if kind == Kind::Movies {
                        src.movies_complete
                    } else {
                        src.series_complete
                    };
                    if list.is_empty() && complete {
                        continue;
                    }
                    if !list.is_empty() {
                        let pw = cx.layout.poster_width;
                        let r = Rail::new(Id::unique(("search.rail", kind.title())), kind.title())
                            .kicker(count_label(list.len()))
                            .see_all(list.len() > RAIL_MAX)
                            .margin(m)
                            .show(ui, |ui| {
                                let mut a = None;
                                for &i in list.iter().take(RAIL_MAX) {
                                    let id = item_id(src, kind, i as usize);
                                    let Some((title, meta, poster)) =
                                        poster_parts(src, kind, i as usize)
                                    else {
                                        continue;
                                    };
                                    first.get_or_insert(id);
                                    let img = images.get(poster, ImageSize::Poster);
                                    let f = PosterCard::new(id, &img, title, pw)
                                        .meta(Some(meta.as_str()).filter(|m| !m.is_empty()))
                                        .meta_on_focus_only(cx.layout.tv)
                                        .show(ui);
                                    if f.activated {
                                        a = Some(i as usize);
                                    }
                                }
                                a
                            });
                        if r.see_all {
                            see_all = Some(kind);
                        }
                        if let Some(i) = r.inner {
                            act = Some((kind, i));
                        }
                    }
                    if !complete {
                        if list.is_empty() {
                            deferred.push(kind);
                        } else if load_hint(ui, kind, true, loading_all, m) {
                            load_all = Some(LoadAll::of(kind));
                        }
                    }
                    if !list.is_empty() {
                        ui.add_space(8.0);
                    }
                }
                // Programmes.
                let progs = hits.of(Kind::Programmes);
                if !progs.is_empty() {
                    let (head, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
                    let painter = ui.painter().clone();
                    let tr = painter.text(
                        pos2(head.left() + m, head.center().y),
                        Align2::LEFT_CENTER,
                        "Programmes",
                        font::title(),
                        color::TEXT,
                    );
                    text::overline(
                        &painter,
                        pos2(tr.right() + 12.0, head.center().y + 2.0),
                        Align2::LEFT_CENTER,
                        &count_label(progs.len()),
                        color::TEXT_3,
                    );
                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        pos2(head.left() + m - 8.0, head.bottom()),
                        vec2(
                            head.width() - 2.0 * m + 16.0,
                            (ROW_H + 8.0) * PROGRAMME_ROWS.min(progs.len()) as f32 + 16.0,
                        ),
                    )));
                    focus::scope(&mut c, Id::unique("search.progs"), |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.add_space(8.0);
                        for &i in progs.iter().take(PROGRAMME_ROWS) {
                            let Some(h) = src.programmes.get(i as usize) else {
                                continue;
                            };
                            let id = Id::unique(("search.prog", i));
                            first.get_or_insert(id);
                            let (r, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), ROW_H),
                                Sense::hover(),
                            );
                            if programme_row(cx, ui, &src.channels, h, id, r) {
                                act = Some((Kind::Programmes, i as usize));
                            }
                        }
                    });
                    ui.add_space((ROW_H + 8.0) * PROGRAMME_ROWS.min(progs.len()) as f32 + 16.0);
                    if progs.len() > PROGRAMME_ROWS {
                        let mut c =
                            ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                                pos2(head.left() + m, ui.cursor().top()),
                                vec2(240.0, 44.0),
                            )));
                        if Button::ghost(format!("All {} programmes", progs.len()))
                            .trailing(icons::CARET_RIGHT)
                            .id(Id::unique("search.progs.all"))
                            .show(&mut c)
                            .activated
                        {
                            see_all = Some(Kind::Programmes);
                        }
                        ui.add_space(52.0);
                    }
                }
                // Groups with no match in a partly searched catalogue: offer the rest.
                if !deferred.is_empty() {
                    ui.add_space(8.0);
                    for kind in deferred {
                        if load_hint(ui, kind, false, loading_all, m) {
                            load_all = Some(LoadAll::of(kind));
                        }
                    }
                }
                // What was searched (quiet, at the end).
                ui.add_space(16.0);
                let caption = scope_caption(src, session.is_xtream());
                let w = (ui.available_width() - 2.0 * m).max(200.0);
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
                text::block(
                    ui.painter(),
                    pos2(r.left() + m, r.top()),
                    &caption,
                    font::caption(),
                    color::TEXT_3,
                    w.min(880.0),
                    2,
                );
                ui.add_space(48.0);
            });
        focus::forward_scroll(&child);
        self.first_result = first;
        if let Some(k) = see_all {
            self.filter = Some(k);
        }
        if let Some(k) = load_all {
            self.start_load_all(cx, k);
        }
        if let Some((k, i)) = act {
            self.activate(cx, res, k, i);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn group_results(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        res: &Arc<Results>,
        kind: Kind,
        rect: Rect,
    ) {
        let m = cx.layout.margin;
        let src = &res.sources;
        let list: Vec<u32> = res.hits.of(kind).to_vec();
        let inner = Rect::from_min_max(
            pos2(rect.left() + m - 8.0, rect.top()),
            pos2(rect.right() - m + 8.0, rect.bottom() - 8.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        let images = cx.services.images.clone();
        let mut act: Option<usize> = None;
        self.first_result = list.first().map(|&i| item_id(src, kind, i as usize));
        match kind {
            Kind::Channels | Kind::Programmes => {
                VirtualList::new(Id::unique(("search.list", kind.title())), ROW_H, list.len())
                    .gap(8.0)
                    .show(
                        &mut child,
                        |i| {
                            list.get(i)
                                .map_or(Id::NULL, |&ix| item_id(src, kind, ix as usize))
                        },
                        |ui, i, r| {
                            let Some(&ix) = list.get(i) else { return };
                            let id = item_id(src, kind, ix as usize);
                            let hit = if kind == Kind::Channels {
                                src.channels
                                    .get(ix as usize)
                                    .map(|c| channel_row(cx, ui, session, &images, c, id, r))
                            } else {
                                src.programmes
                                    .get(ix as usize)
                                    .map(|h| programme_row(cx, ui, &src.channels, h, id, r))
                            };
                            if hit == Some(true) {
                                act = Some(ix as usize);
                            }
                        },
                    );
            }
            Kind::Movies | Kind::Series => {
                let pw = cx.layout.poster_width;
                VirtualGrid::new(
                    Id::unique(("search.grid", kind.title())),
                    vec2(pw, PosterCard::height(pw)),
                    list.len(),
                )
                .show(
                    &mut child,
                    |i| {
                        list.get(i)
                            .map_or(Id::NULL, |&ix| item_id(src, kind, ix as usize))
                    },
                    |ui, i, r| {
                        let Some(&ix) = list.get(i) else { return };
                        let id = item_id(src, kind, ix as usize);
                        let Some((title, meta, poster)) = poster_parts(src, kind, ix as usize)
                        else {
                            return;
                        };
                        let img = images.get(poster, ImageSize::Poster);
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r));
                        if PosterCard::new(id, &img, title, r.width())
                            .meta(Some(meta.as_str()).filter(|m| !m.is_empty()))
                            .meta_on_focus_only(cx.layout.tv)
                            .show(&mut c)
                            .activated
                        {
                            act = Some(ix as usize);
                        }
                    },
                );
            }
        }
        if let Some(i) = act {
            self.activate(cx, res, kind, i);
        }
    }

    fn overlays(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>) {
        let Some((src, h)) = self.details.clone() else {
            return;
        };
        let Some(ch) = src.channels.get(h.channel).cloned() else {
            self.details = None;
            return;
        };
        let (close, watch) = super::guide::details_dialog(
            cx,
            ui.ctx(),
            Id::unique("search.details"),
            &ch,
            &h.programme,
        );
        if close || watch {
            self.details = None;
            if let Some(id) = self.first_result {
                focus::request(&cx.egui, id);
            }
        }
        if watch {
            match session.live_request(
                &ch,
                Some(ZapContext {
                    channels: Arc::clone(&src.channels),
                    index: h.channel,
                }),
            ) {
                Ok(req) => cx.play(req, true),
                Err(e) => cx.toasts.error(e.user_message()),
            }
        }
    }
}

fn count_label(n: usize) -> String {
    if n == 1 {
        "1 result".to_owned()
    } else {
        format!("{n} results")
    }
}

/// The "only categories you've opened were searched" row with its button (or the
/// progress while loading); `found` = the group has results above it. Returns true when
/// the button is activated.
fn load_hint(ui: &mut Ui, kind: Kind, found: bool, loading: Option<LoadAll>, margin: f32) -> bool {
    let what = if kind == Kind::Movies {
        "films"
    } else {
        "series"
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
    let painter = ui.painter().clone();
    let x = rect.left() + margin;
    let cy = rect.center().y;
    if loading.is_some_and(|l| l.covers(kind)) {
        paint::spinner(&painter, pos2(x + 10.0, cy), 8.0, ui.input(|i| i.time), 2.0);
        text::line(
            &painter,
            pos2(x + 28.0, cy),
            Align2::LEFT_CENTER,
            &format!(
                "Loading every {what} from your provider… This can take a while on large catalogues."
            ),
            font::body(),
            color::TEXT_2,
            rect.width() - 2.0 * margin - 28.0,
        );
        ui.ctx().request_repaint();
        return false;
    }
    icons::paint(
        &painter,
        pos2(x + 10.0, cy),
        icons::INFO,
        18.0,
        Style::Regular,
        color::INFO,
    );
    let tr = text::line(
        &painter,
        pos2(x + 30.0, cy),
        Align2::LEFT_CENTER,
        &if found {
            format!("Showing {what} from the categories you've opened.")
        } else {
            format!("No {what} found in the categories you've opened.")
        },
        font::body(),
        color::TEXT_2,
        (rect.width() - 2.0 * margin - 300.0).max(200.0),
    );
    let b = Button::secondary(format!("Search all {what}"))
        .small()
        .icon(icons::LIST_MAGNIFYING_GLASS)
        .id(Id::unique(("search.load-all", what)));
    let sz = b.size(ui);
    let br = Rect::from_min_size(pos2(tr.right() + 16.0, cy - sz.y / 2.0), sz);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(br));
    b.show(&mut c).activated
}

/// Poster title (tag and year split off), meta line ("FR · 2004 · ★ 7.4") and poster URL
/// of a film/series result.
fn poster_parts(src: &Sources, kind: Kind, i: usize) -> Option<(&str, String, Option<&str>)> {
    match kind {
        Kind::Movies => src.movies.get(i).map(|v| {
            (
                vod::display_title(&v.name).title,
                vod::movie_card_meta(v),
                v.stream_icon.as_deref(),
            )
        }),
        Kind::Series => src.series.get(i).map(|s| {
            (
                vod::display_title(&s.name).title,
                vod::series_card_meta(s),
                s.poster(),
            )
        }),
        Kind::Channels | Kind::Programmes => None,
    }
}

fn item_id(src: &Sources, kind: Kind, i: usize) -> Id {
    match kind {
        Kind::Channels => src
            .channels
            .get(i)
            .map_or(Id::NULL, |c| Id::unique(("search.ch", &c.key))),
        Kind::Movies => src
            .movies
            .get(i)
            .map_or(Id::NULL, |v| Id::unique(("search.movie", v.stream_id))),
        Kind::Series => src
            .series
            .get(i)
            .map_or(Id::NULL, |s| Id::unique(("search.series", s.series_id))),
        Kind::Programmes => Id::unique(("search.prog", i as u32)),
    }
}

/// A channel result row. Returns true when activated.
fn channel_row(
    cx: &AppCtx<'_>,
    ui: &mut Ui,
    session: &Arc<Session>,
    images: &crate::services::ImageService,
    c: &LiveChannel,
    id: Id,
    r: Rect,
) -> bool {
    let (_, f) = focus::focusable(ui, id, r, FocusOpts::default());
    let painter = ui.painter().clone();
    if f.focused {
        painter.rect_filled(r, CornerRadius::same(radius::MD), color::SURFACE_3);
    }
    let num = c
        .number
        .map_or_else(|| "—".to_owned(), |n| format!("{n:03}"));
    painter.text(
        pos2(r.left() + 56.0, r.center().y),
        Align2::RIGHT_CENTER,
        num,
        font::caption_strong(),
        color::TEXT_3,
    );
    let logo = Rect::from_min_size(pos2(r.left() + 68.0, r.center().y - 20.0), vec2(64.0, 40.0));
    let slot = images.get(c.logo.as_deref(), ImageSize::Logo);
    ChannelLogo::paint(ui, &painter, logo, &c.name, &slot);
    let tx = logo.right() + 12.0;
    text::line(
        &painter,
        pos2(tx, r.top() + 22.0),
        Align2::LEFT_CENTER,
        &c.name,
        font::body_strong(),
        color::TEXT,
        r.right() - tx - 24.0,
    );
    let (_, now_p, _) = session.now_next(c, cx.now);
    if let Some(p) = now_p {
        text::line(
            &painter,
            pos2(tx, r.top() + 44.0),
            Align2::LEFT_CENTER,
            &p.title,
            font::caption(),
            color::TEXT_2,
            r.right() - tx - 24.0,
        );
    }
    if f.focused {
        paint::focus_ring(&painter, r, radius::MD, 1.0);
    }
    f.activated
}

/// A programme result row. Returns true when activated.
fn programme_row(
    cx: &AppCtx<'_>,
    ui: &mut Ui,
    channels: &[LiveChannel],
    h: &ProgrammeHit,
    id: Id,
    r: Rect,
) -> bool {
    let (_, f) = focus::focusable(ui, id, r, FocusOpts::default());
    let painter = ui.painter().clone();
    painter.rect_filled(
        r,
        CornerRadius::same(radius::MD),
        if f.focused {
            color::SURFACE_3
        } else {
            color::SURFACE_1
        },
    );
    let p = &h.programme;
    let now = cx.now;
    let ch = channels.get(h.channel);
    let logo = Rect::from_min_size(pos2(r.left() + 12.0, r.center().y - 15.0), vec2(48.0, 30.0));
    if let Some(ch) = ch {
        let slot = cx.services.images.get(ch.logo.as_deref(), ImageSize::Logo);
        ChannelLogo::paint(ui, &painter, logo, &ch.name, &slot);
    }
    let tx = logo.right() + 16.0;
    let right_w = 180.0;
    text::line(
        &painter,
        pos2(tx, r.top() + 22.0),
        Align2::LEFT_CENTER,
        &p.title,
        font::body_strong(),
        color::TEXT,
        r.right() - right_w - tx,
    );
    let when = format!(
        "{} · {}–{} · {}",
        super::guide::relative_day(p.start, now),
        cx.prefs.format_time(&p.start.with_timezone(&Local)),
        cx.prefs.format_time(&p.stop.with_timezone(&Local)),
        ch.map_or("", |c| split_tag(&c.name).1),
    );
    text::line(
        &painter,
        pos2(tx, r.top() + 44.0),
        Align2::LEFT_CENTER,
        &when,
        font::caption(),
        color::TEXT_3,
        r.right() - right_w - tx,
    );
    let rx = r.right() - 16.0;
    if p.start <= now && now < p.stop {
        Badge::live(&painter, pos2(rx, r.center().y), Align2::RIGHT_CENTER);
    } else if p.stop <= now && p.has_archive {
        let t = painter.text(
            pos2(rx, r.center().y),
            Align2::RIGHT_CENTER,
            "Catch-up",
            font::caption(),
            color::SAGE,
        );
        icons::paint(
            &painter,
            pos2(t.left() - 12.0, r.center().y),
            icons::CLOCK_COUNTER_CLOCKWISE,
            16.0,
            Style::Regular,
            color::SAGE,
        );
    }
    if f.focused {
        paint::focus_ring(&painter, r, radius::MD, 1.0);
    }
    f.activated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lists() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(natural_list(&s(&[])), "");
        assert_eq!(natural_list(&s(&["a"])), "a");
        assert_eq!(natural_list(&s(&["a", "b"])), "a and b");
        assert_eq!(natural_list(&s(&["a", "b", "c"])), "a, b and c");
    }

    #[test]
    fn scope_caption_is_plain_language() {
        let src = Sources {
            channels: Arc::new(Vec::new()),
            movies_complete: false,
            series_complete: true,
            ..Sources::default()
        };
        assert_eq!(
            scope_caption(&src, true),
            "Searched 0 channels, 0 films and 0 series. Films and series come from the categories you've opened."
        );
        assert_eq!(scope_caption(&src, false), "Searched 0 channels.");
        let withheld = Sources {
            vod_withheld: true,
            movies_complete: true,
            series_complete: true,
            ..Sources::default()
        };
        assert!(scope_caption(&withheld, true).contains("hidden"));
    }

    #[test]
    fn fingerprint_follows_content() {
        let g = |ids: &[u64]| Gathered {
            items: ids
                .iter()
                .map(|&id| {
                    serde_json::from_value::<VodStream>(serde_json::json!({
                        "stream_id": id, "name": format!("Film {id}")
                    }))
                    .unwrap_or_else(|e| panic!("{e}"))
                })
                .collect::<Vec<_>>(),
            complete: true,
            cats: (0, 0),
            withheld: false,
        };
        let s = || Gathered::<Series> {
            items: Vec::new(),
            complete: true,
            cats: (0, 0),
            withheld: false,
        };
        let a = fingerprint(&[], &g(&[1, 2]), &s(), &[]);
        assert_eq!(a, fingerprint(&[], &g(&[1, 2]), &s(), &[]), "stable");
        assert_ne!(a, fingerprint(&[], &g(&[1, 3]), &s(), &[]));
        assert_ne!(a, 0);
    }
}

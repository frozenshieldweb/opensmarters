// SPDX-License-Identifier: GPL-3.0-or-later
//! Search: one field across live channels, films, series and (cached) programme titles
//! of the active profile.
//!
//! - **Off the UI thread:** the corpus (pre-normalised names) is built in a loader, and
//!   each query (debounced 200 ms) runs on the blocking pool; a query that extends the
//!   previous one only re-checks the previous matches ([`engine::Corpus::search`]).
//! - **Plain text:** case- and diacritics-insensitive substring search, never a regex.
//! - **Large catalogues:** films/series are searched in what is already cached (the
//!   "All" list, or the categories opened before). The 35 000-film "All" list is only
//!   fetched when the user asks ("Load all films to search").
//! - **Keyboard first:** typing anywhere goes to the field; ↓ (or Enter) from the field
//!   jumps to the first result. Recent searches (a non-secret setting) show as chips.
//! - Parental locks apply: items of locked categories are never listed.

mod engine;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, CornerRadius, Event, Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2,
};
use opensmarters_core::chrono::{Duration as ChronoDuration, Local, Utc};
use opensmarters_core::{Category, CategoryId, Series, VodStream};
use opensmarters_store::ContentKind;
use opensmarters_store::settings::Setting;

use super::{AppCtx, Screen, shell};
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{Route, Section};
use crate::playback::ZapContext;
use crate::services::catalog::{LiveChannel, Programme, split_tag};
use crate::services::session::guide::xmltv_programme;
use crate::services::session::{series_key, vod_streams_key};
use crate::services::{ImageSize, Session};
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

/// Recent searches (global, non-secret).
pub const RECENT: Setting<Vec<String>> = Setting::new("search.recent", Vec::new);
const RECENT_CAP: usize = 8;
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
    channels: Arc<Vec<LiveChannel>>,
    movies: Vec<VodStream>,
    series: Vec<Series>,
    programmes: Vec<ProgrammeHit>,
    movies_complete: bool,
    series_complete: bool,
    movie_cats: (usize, usize),
    series_cats: (usize, usize),
    corpus: Corpus,
}

async fn gather_vod(session: &Arc<Session>) -> (Vec<VodStream>, bool, (usize, usize)) {
    let cats = session
        .peek_cached::<Vec<Category>>("xt:vod_categories")
        .await;
    let locked = |id: Option<&CategoryId>| {
        id.is_some_and(|id| {
            let name = cats
                .as_ref()
                .and_then(|c| c.iter().find(|c| &c.id == id))
                .map_or("", |c| c.name.as_str());
            session.is_category_locked(ContentKind::Movie, id, name)
        })
    };
    if let Some(all) = session
        .peek_cached::<Vec<VodStream>>(&vod_streams_key(None))
        .await
    {
        let list = all
            .iter()
            .filter(|v| !locked(v.category_id.as_ref()))
            .cloned()
            .collect();
        return (list, true, (0, 0));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut loaded = 0;
    let total = cats.as_ref().map_or(0, |c| c.len());
    if let Some(cats) = &cats {
        for c in cats.iter() {
            if session.is_category_locked(ContentKind::Movie, &c.id, &c.name) {
                continue;
            }
            if let Some(list) = session
                .peek_cached::<Vec<VodStream>>(&vod_streams_key(Some(&c.id)))
                .await
            {
                loaded += 1;
                for v in list.iter() {
                    if seen.insert(v.stream_id) {
                        out.push(v.clone());
                    }
                }
            }
        }
    }
    (out, false, (loaded, total))
}

async fn gather_series(session: &Arc<Session>) -> (Vec<Series>, bool, (usize, usize)) {
    let cats = session
        .peek_cached::<Vec<Category>>("xt:series_categories")
        .await;
    let locked = |id: Option<&CategoryId>| {
        id.is_some_and(|id| {
            let name = cats
                .as_ref()
                .and_then(|c| c.iter().find(|c| &c.id == id))
                .map_or("", |c| c.name.as_str());
            session.is_category_locked(ContentKind::Series, id, name)
        })
    };
    if let Some(all) = session.peek_cached::<Vec<Series>>(&series_key(None)).await {
        let list = all
            .iter()
            .filter(|s| !locked(s.category_id.as_ref()))
            .cloned()
            .collect();
        return (list, true, (0, 0));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut loaded = 0;
    let total = cats.as_ref().map_or(0, |c| c.len());
    if let Some(cats) = &cats {
        for c in cats.iter() {
            if session.is_category_locked(ContentKind::Series, &c.id, &c.name) {
                continue;
            }
            if let Some(list) = session
                .peek_cached::<Vec<Series>>(&series_key(Some(&c.id)))
                .await
            {
                loaded += 1;
                for s in list.iter() {
                    if seen.insert(s.series_id) {
                        out.push(s.clone());
                    }
                }
            }
        }
    }
    (out, false, (loaded, total))
}

/// Collects everything searchable (cache only, except the live catalogue) and builds the
/// corpus on the blocking pool.
async fn build_sources(session: Arc<Session>) -> Result<Arc<Sources>, AppError> {
    let cat = session.live_catalog().await.ok();
    let parental = session.parental_enabled();
    let channels: Vec<LiveChannel> = cat
        .as_ref()
        .map(|cat| {
            cat.channels
                .iter()
                .filter(|c| {
                    !parental
                        || !c.categories.iter().any(|id| {
                            session.is_category_locked(
                                ContentKind::Live,
                                id,
                                cat.category_name(id).unwrap_or(""),
                            )
                        })
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let (movies, movies_complete, movie_cats) = if session.is_xtream() {
        gather_vod(&session).await
    } else {
        (Vec::new(), true, (0, 0))
    };
    let (series, series_complete, series_cats) = if session.is_xtream() {
        gather_series(&session).await
    } else {
        (Vec::new(), true, (0, 0))
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
        let mut corpus = Corpus::default();
        corpus.add(Kind::Channels, channels.iter().map(|c| c.name.as_str()));
        corpus.add(Kind::Movies, movies.iter().map(|m| m.name.as_str()));
        corpus.add(Kind::Series, series.iter().map(|s| s.name.as_str()));
        corpus.add(
            Kind::Programmes,
            programmes.iter().map(|h| h.programme.title.as_str()),
        );
        Ok(Arc::new(Sources {
            channels: Arc::new(channels),
            movies,
            series,
            programmes,
            movies_complete,
            series_complete,
            movie_cats,
            series_cats,
            corpus,
        }))
    })
    .await
    .map_err(|_| AppError::Cancelled)?
}

/// The Search screen.
#[derive(Debug)]
pub struct SearchScreen {
    query: String,
    sources: Loader<Arc<Sources>>,
    hits: Loader<Arc<Hits>>,
    searched: Option<(String, u64)>,
    last_edit: Instant,
    filter: Option<Kind>,
    recent: Loader<Vec<String>>,
    load_all: Loader<()>,
    load_all_kind: Option<Kind>,
    first_result: Option<Id>,
    focus_first: bool,
    details: Option<ProgrammeHit>,
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

    fn load_sources(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        self.sources
            .start(cx.rt(), &cx.egui, move |_| build_sources(session));
        self.searched = None;
    }

    fn remember(&mut self, cx: &AppCtx<'_>) {
        let q = self.query.trim().to_owned();
        if q.is_empty() {
            return;
        }
        let mut list = self.recent.value().cloned().unwrap_or_default();
        push_recent(&mut list, &q, RECENT_CAP);
        self.recent.set(list.clone());
        cx.services.blocking("save recent searches", move |s| {
            s.settings().set(&RECENT, &list)
        });
    }

    /// Runs the (debounced) search when the query or the corpus changed.
    fn maybe_search(&mut self, cx: &AppCtx<'_>) {
        let Some(src) = self.sources.value().cloned() else {
            return;
        };
        let key = (self.query.trim().to_owned(), self.sources.generation());
        if self.searched.as_ref() == Some(&key) {
            return;
        }
        if self.last_edit.elapsed() < DEBOUNCE {
            cx.egui
                .request_repaint_after(DEBOUNCE.saturating_sub(self.last_edit.elapsed()));
            return;
        }
        let previous = self
            .hits
            .value()
            .cloned()
            .filter(|_| self.searched.as_ref().is_some_and(|(_, g)| *g == key.1));
        let q = key.0.clone();
        self.searched = Some(key);
        self.hits.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(Arc::new(src.corpus.search(&q, previous.as_deref())))
        });
    }

    fn start_load_all(&mut self, cx: &AppCtx<'_>, kind: Kind) {
        let Some(session) = cx.session() else { return };
        self.load_all_kind = Some(kind);
        self.load_all.start(cx.rt(), &cx.egui, move |_| async move {
            match kind {
                Kind::Movies => session.vod_streams(None).await.map(|_| ()),
                _ => session.series_list(None).await.map(|_| ()),
            }
        });
    }

    fn activate(&mut self, cx: &mut AppCtx<'_>, kind: Kind, idx: usize) {
        let Some(session) = cx.session() else { return };
        let Some(src) = self.sources.value().cloned() else {
            return;
        };
        self.remember(cx);
        match kind {
            Kind::Channels => {
                let Some(hits) = self.hits.value().cloned() else {
                    return;
                };
                // Zap through the channel results, in result order.
                let list: Vec<LiveChannel> = hits
                    .of(Kind::Channels)
                    .iter()
                    .filter_map(|&i| src.channels.get(i as usize).cloned())
                    .collect();
                let Some(pos) = hits
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
                    self.details = Some(h);
                }
            }
        }
    }
}

impl Screen for SearchScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        // Rebuild every time: the user may have opened categories or loaded guides.
        self.load_sources(cx);
        if self.recent.is_idle() {
            let store = cx.services.store.clone();
            self.recent.start_blocking(cx.rt(), &cx.egui, move || {
                Ok(store.settings().get(&RECENT).unwrap_or_default())
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

        let mut y = field_rect.bottom() + 16.0;
        let query_empty = self.query.trim().is_empty();
        let painter = ui.painter().clone();

        // What is searched (and what isn't).
        if let Some(src) = self.sources.value().cloned() {
            let mut parts = vec![format!("{} channels", thousands(src.channels.len()))];
            if session.is_xtream() {
                parts.push(if src.movies_complete {
                    format!("{} films", thousands(src.movies.len()))
                } else {
                    format!(
                        "{} films from {} of {} categories",
                        thousands(src.movies.len()),
                        src.movie_cats.0,
                        src.movie_cats.1
                    )
                });
                parts.push(if src.series_complete {
                    format!("{} series", thousands(src.series.len()))
                } else {
                    format!(
                        "{} series from {} of {} categories",
                        thousands(src.series.len()),
                        src.series_cats.0,
                        src.series_cats.1
                    )
                });
            }
            parts.push(format!(
                "{} programmes from the guide",
                thousands(src.programmes.len())
            ));
            text::line(
                &painter,
                pos2(field_rect.left() + 8.0, y + 8.0),
                Align2::LEFT_CENTER,
                &format!("Searching {}", parts.join(" · ")),
                font::caption(),
                color::TEXT_3,
                content.width() - 2.0 * m,
            );
        } else if self.sources.show_skeleton() {
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
                "Preparing search…",
                font::caption(),
                color::TEXT_3,
            );
            ui.ctx().request_repaint();
        }
        y += 28.0;

        let area = Rect::from_min_max(
            pos2(content.left(), y),
            pos2(content.right(), content.bottom()),
        );
        if query_empty {
            self.first_result = None;
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
        let (Some(src), Some(hits)) = (self.sources.value().cloned(), self.hits.value().cloned())
        else {
            return;
        };

        // Type chips.
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
                    if n == 0 && !matches!(kind, Kind::Movies | Kind::Series) {
                        continue;
                    }
                    if n == 0 && !session.is_xtream() {
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
        if hits.total() == 0 && src.movies_complete && src.series_complete {
            self.first_result = None;
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(results));
            let f = EmptyState::new(
                Id::unique("search.none"),
                Illustration::EmptySearch,
                "No matches",
                "Try a shorter word, or check the spelling.",
            )
            .action("Clear search", false)
            .illustration_size(160.0)
            .show(&mut child);
            if f.is_some_and(|f| f.activated) {
                self.query.clear();
                focus::request(&cx.egui, field_id());
            }
            return;
        }
        match self.filter {
            None => self.all_results(cx, ui, &session, &src, &hits, results),
            Some(kind) => self.group_results(cx, ui, &session, &src, &hits, kind, results),
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
    fn recents(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, rect: Rect) {
        let list = self.recent.value().cloned().unwrap_or_default();
        let painter = ui.painter().clone();
        if list.is_empty() {
            text::block(
                &painter,
                rect.min + vec2(8.0, 16.0),
                "Find channels, films, series and programmes by name. Just start typing — accents and capitals don't matter.",
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
        let chips = Rect::from_min_size(rect.min + vec2(0.0, 32.0), vec2(rect.width(), 52.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(chips));
        let mut picked: Option<String> = None;
        let mut clear = false;
        focus::scope(&mut child, Id::unique("search.recent"), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
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
        if let Some(q) = picked {
            self.query = q;
            self.last_edit = Instant::now() - DEBOUNCE;
            self.focus_first = true;
        }
        if clear {
            self.recent.set(Vec::new());
            cx.services.blocking("clear recent searches", |s| {
                s.settings().set(&RECENT, &Vec::<String>::new())
            });
            focus::request(&cx.egui, field_id());
        }
    }

    #[allow(clippy::too_many_lines)]
    fn all_results(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        src: &Arc<Sources>,
        hits: &Arc<Hits>,
        rect: Rect,
    ) {
        let m = cx.layout.margin;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let images = cx.services.images.clone();
        let mut first: Option<Id> = None;
        let mut act: Option<(Kind, usize)> = None;
        let mut see_all: Option<Kind> = None;
        let mut load_all: Option<Kind> = None;
        let mut deferred: Vec<Kind> = Vec::new();
        let loading_all = self
            .load_all
            .is_loading()
            .then_some(self.load_all_kind)
            .flatten();
        egui::ScrollArea::vertical()
            .id_salt("search.results")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if hits.total() == 0 {
                    // Nothing in what is loaded; the load hints follow.
                    let (r, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 72.0), Sense::hover());
                    let painter = ui.painter().clone();
                    painter.text(
                        pos2(r.left() + m, r.top() + 24.0),
                        Align2::LEFT_CENTER,
                        "No matches yet",
                        font::title(),
                        color::TEXT,
                    );
                    painter.text(
                        pos2(r.left() + m, r.top() + 54.0),
                        Align2::LEFT_CENTER,
                        "Try a shorter word, or search the whole catalogue below.",
                        font::body(),
                        color::TEXT_2,
                    );
                }
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
                    if !session.is_xtream() {
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
                                    let (id, name, poster) = match kind {
                                        Kind::Movies => {
                                            let Some(v) = src.movies.get(i as usize) else {
                                                continue;
                                            };
                                            (
                                                Id::unique(("search.movie", v.stream_id)),
                                                v.name.as_str(),
                                                v.stream_icon.as_deref(),
                                            )
                                        }
                                        _ => {
                                            let Some(s) = src.series.get(i as usize) else {
                                                continue;
                                            };
                                            (
                                                Id::unique(("search.series", s.series_id)),
                                                s.name.as_str(),
                                                s.poster(),
                                            )
                                        }
                                    };
                                    first.get_or_insert(id);
                                    let img = images.get(poster, ImageSize::Poster);
                                    let (tag, title) = split_tag(name);
                                    let f = PosterCard::new(id, &img, title, pw)
                                        .meta(tag)
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
                        } else if load_hint(ui, kind, loading_all, m) {
                            load_all = Some(kind);
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
                // Groups with no match in an incomplete catalogue: offer the full list.
                if !deferred.is_empty() {
                    ui.add_space(8.0);
                    for kind in deferred {
                        if load_hint(ui, kind, loading_all, m) {
                            load_all = Some(kind);
                        }
                    }
                }
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
            self.activate(cx, k, i);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn group_results(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        src: &Arc<Sources>,
        hits: &Arc<Hits>,
        kind: Kind,
        rect: Rect,
    ) {
        let m = cx.layout.margin;
        let list: Vec<u32> = hits.of(kind).to_vec();
        let inner = Rect::from_min_max(
            pos2(rect.left() + m - 8.0, rect.top()),
            pos2(rect.right() - m + 8.0, rect.bottom() - 8.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        let images = cx.services.images.clone();
        let mut act: Option<usize> = None;
        self.first_result = list.first().map(|&i| item_id(src, kind, i as usize));
        if list.is_empty() {
            text::block(
                ui.painter(),
                inner.min + vec2(8.0, 16.0),
                "No matches here. Try All, or load the full catalogue from the All view.",
                font::body(),
                color::TEXT_2,
                inner.width(),
                2,
            );
            return;
        }
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
                        let (name, poster) = if kind == Kind::Movies {
                            let Some(v) = src.movies.get(ix as usize) else {
                                return;
                            };
                            (v.name.as_str(), v.stream_icon.as_deref())
                        } else {
                            let Some(s) = src.series.get(ix as usize) else {
                                return;
                            };
                            (s.name.as_str(), s.poster())
                        };
                        let img = images.get(poster, ImageSize::Poster);
                        let (tag, title) = split_tag(name);
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r));
                        if PosterCard::new(id, &img, title, r.width())
                            .meta(tag)
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
            self.activate(cx, kind, i);
        }
    }

    fn overlays(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>) {
        let Some(h) = self.details.clone() else {
            return;
        };
        let Some(src) = self.sources.value().cloned() else {
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

/// The "only categories you've opened were searched" row with its Load-all button (or
/// the progress while loading). Returns true when the button is activated.
fn load_hint(ui: &mut Ui, kind: Kind, loading: Option<Kind>, margin: f32) -> bool {
    let what = if kind == Kind::Movies {
        "films"
    } else {
        "series"
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
    let painter = ui.painter().clone();
    let x = rect.left() + margin;
    let cy = rect.center().y;
    if loading == Some(kind) {
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
        &format!("Only {what} from categories you've opened were searched."),
        font::body(),
        color::TEXT_2,
        (rect.width() - 2.0 * margin - 300.0).max(200.0),
    );
    let b = Button::secondary(format!("Load all {what}"))
        .small()
        .icon(icons::ARROW_DOWN)
        .id(Id::unique(("search.load-all", what)));
    let sz = b.size(ui);
    let br = Rect::from_min_size(pos2(tr.right() + 16.0, cy - sz.y / 2.0), sz);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(br));
    b.show(&mut c).activated
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

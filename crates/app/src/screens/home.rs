// SPDX-License-Identifier: GPL-3.0-or-later
//! Home (§8.2): a hero band and content rails.
//!
//! - **Hero:** the featured item (the most recent "continue watching" item, else the
//!   newest movie, else a welcome band) and then **whatever rail card the focus rests on**
//!   (debounced 250 ms). The provider backdrop crossfades (400 ms `cubic_in_out`, both
//!   textures kept alive until the blend ends) and the text fades in (160 ms); reduced
//!   motion shortens both to ≤ 100 ms fades. The buttons act on the item shown.
//! - **Rails** in a fixed order: Continue watching, Live now (favourite channels, else
//!   recently watched), Recently added, Series, Favourites (films and series). Each rail
//!   owns its slot from the first frame: a skeleton after 150 ms while it loads (same
//!   geometry as the real rail), then the content fades in, so nothing jumps or reorders
//!   as data arrives. Empty rails collapse (animated).
//! - **Initial focus** goes to the hero's primary button. "See all" buttons appear only
//!   once the hero is there, so the fallback focus can never park on one.
//! - Items in locked categories (parental control) stay off Home.
//! - Without a session yet (the startup profile is still opening) Home shows its
//!   skeleton; the app replaces it as soon as the profile is open.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{
    self, Align2, Color32, Id, Painter, Rect, ScrollArea, Sense, TextureHandle, Ui, pos2, vec2,
};
use opensmarters_core::catalog::{SortOrder, sorted_indices};
use opensmarters_core::{Category, CategoryId, Series, SeriesInfo, VodInfo, VodStream};
use opensmarters_store::{ContentKind, Favourite, PlaybackKind, PlaybackProgress, RecentChannel};

use super::{AppCtx, Screen};
use crate::error::AppError;
use crate::focus;
use crate::nav::{Route, Section};
use crate::playback::ZapContext;
use crate::services::catalog::{LiveCatalog, LiveChannel};
use crate::services::session::{series_key, vod_streams_key};
use crate::services::{ImageService, ImageSize, ImageSlot, Session, vod};
use crate::task::Loader;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::icons;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::{SkeletonCard, rail_height, skeleton, skeleton_rail};
use crate::ui::widgets::{BackdropCard, Button, ChannelTile, EmptyState, PosterCard, Rail};
use crate::ui::{motion, paint, text};

/// Categories sampled for Home's rails when the "All" lists are not cached.
const HOME_CATEGORIES: usize = 3;
/// Cards per rail.
const RAIL_ITEMS: usize = 20;
/// The hero follows a rail card once the focus has rested on it this long (§7).
const HERO_DEBOUNCE: f64 = 0.25;
/// How long the hero waits for an item's details before showing what the list knows.
const HERO_DETAILS_WAIT: f64 = 0.6;
/// The rails overlap the hero's faded bottom by this much (§8.2).
const RAIL_OVERLAP: f32 = 48.0;
/// Vertical gap between rails (the rail's own paint allowance makes it ≈ 32 pt).
const RAIL_GAP: f32 = 16.0;
/// Skeletons appear only after this delay (§6.16).
const SKELETON_DELAY: f64 = 0.15;

/// A movie as Home knows it (from a list or a favourite's snapshot).
#[derive(Debug, Clone, PartialEq)]
struct MovieRef {
    id: u64,
    name: String,
    poster: Option<String>,
    extension: Option<String>,
    /// Card meta ("2026 · ★ 7.4"), empty when unknown.
    meta: String,
}

impl MovieRef {
    fn from_stream(m: &VodStream) -> Self {
        Self {
            id: m.stream_id,
            name: m.name.clone(),
            poster: m.stream_icon.clone(),
            extension: m.container_extension.clone(),
            meta: vod::movie_card_meta(m),
        }
    }
}

/// A series as Home knows it.
#[derive(Debug, Clone, PartialEq)]
struct SeriesRef {
    id: u64,
    name: String,
    poster: Option<String>,
    meta: String,
}

impl SeriesRef {
    fn from_series(s: &Series) -> Self {
        Self {
            id: s.series_id,
            name: s.name.clone(),
            poster: s.poster().map(str::to_owned),
            meta: vod::series_card_meta(s),
        }
    }
}

/// What the hero is about.
#[derive(Debug, Clone)]
enum HeroItem {
    Resume(PlaybackProgress),
    Movie(MovieRef),
    Series(SeriesRef),
    Welcome,
}

/// Identity of a hero item (cache key, "is this the same hero?").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HeroKey {
    Progress(String),
    Movie(u64),
    Series(u64),
    Welcome,
}

impl HeroItem {
    fn key(&self) -> HeroKey {
        match self {
            HeroItem::Resume(p) => HeroKey::Progress(format!("{:?}:{}", p.kind, p.item_id)),
            HeroItem::Movie(m) => HeroKey::Movie(m.id),
            HeroItem::Series(s) => HeroKey::Series(s.id),
            HeroItem::Welcome => HeroKey::Welcome,
        }
    }
}

/// The hero's text and art, built from an item (and its details when loaded).
#[derive(Debug, Clone)]
struct Hero {
    key: HeroKey,
    kicker: String,
    title: String,
    meta: String,
    synopsis: String,
    backdrop: Option<String>,
    item: HeroItem,
    /// Container extension from the details (movies).
    extension: Option<String>,
}

/// Hero state: featured item, debounced focus target, details cache, the text fade and
/// the backdrop crossfade.
#[derive(Default)]
struct HeroState {
    /// The item shown when nothing else is: first continue-watching, newest movie, welcome.
    featured: Option<HeroItem>,
    /// The item the hero is showing or about to show.
    target: Option<HeroItem>,
    /// A rail card the focus rests on (key, item, its rail's kicker, since).
    pending: Option<(HeroKey, HeroItem, &'static str, f64)>,
    /// The kicker over the title: the rail the item came from ("Recently added").
    target_kicker: &'static str,
    /// The user has pressed a key or moved the pointer on Home: only then does the hero
    /// follow the focus (a fallback focus parked on a card while loading must not
    /// replace the featured item).
    interacted: bool,
    /// Since when the target waits for its details.
    target_since: f64,
    /// Heroes with their details loaded.
    cache: HashMap<HeroKey, Hero>,
    loader: Loader<Hero>,
    loading: Option<HeroKey>,
    /// What's on screen.
    shown: Option<Hero>,
    /// When the shown text appeared (fade-in).
    shown_at: f64,
    /// The backdrop on screen (url, texture) and the one fading out.
    back_cur: Option<(String, TextureHandle)>,
    back_prev: Option<TextureHandle>,
    back_blend_at: f64,
}

impl std::fmt::Debug for HeroState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeroState")
            .field("featured", &self.featured.as_ref().map(HeroItem::key))
            .field("target", &self.target.as_ref().map(HeroItem::key))
            .field("shown", &self.shown.as_ref().map(|h| &h.key))
            .finish_non_exhaustive()
    }
}

/// A rail's loading state for its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// Still loading: reserve the slot (skeleton after 150 ms).
    Loading,
    /// Has cards.
    Ready,
    /// Loaded and empty, or not applicable: no slot.
    Empty,
    /// Not decided yet and not worth a slot (store rails in their first 150 ms): left
    /// out without touching the slot's animation, so it appears without sliding.
    Pending,
}

/// The Home screen.
#[derive(Debug)]
pub struct HomeScreen {
    progress: Loader<Vec<PlaybackProgress>>,
    recents: Loader<Vec<RecentChannel>>,
    live: Loader<Arc<LiveCatalog>>,
    movies: Loader<Arc<Vec<VodStream>>>,
    series: Loader<Arc<Vec<Series>>>,
    favourites: Loader<Vec<Favourite>>,
    recent_movies: Vec<VodStream>,
    recent_series: Vec<Series>,
    progress_epoch: u64,
    hero: HeroState,
    /// Rail cards → the hero item they stand for (rebuilt every frame).
    focus_map: HashMap<Id, (HeroItem, &'static str)>,
    /// When the screen started waiting for content (skeleton delay without a session).
    created: Instant,
    /// The session the data was loaded for.
    loaded_for: Option<opensmarters_store::ProfileId>,
    /// When the data loads started (store rails get 150 ms to answer before their slot
    /// is reserved).
    loaded_at: Option<Instant>,
}

impl Default for HomeScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// Keeps the items whose category isn't locked (`names`: category id → name, for the
/// keyword locks).
fn unlocked<T>(
    session: &Session,
    kind: ContentKind,
    items: impl IntoIterator<Item = T>,
    category: impl Fn(&T) -> Option<&CategoryId>,
    names: &HashMap<CategoryId, String>,
) -> Vec<T> {
    if !session.parental_enabled() {
        return items.into_iter().collect();
    }
    items
        .into_iter()
        .filter(|it| {
            category(it).is_none_or(|c| {
                let name = names.get(c).map_or("", String::as_str);
                !session.is_category_locked(kind, c, name)
            })
        })
        .collect()
}

fn names_of(cats: &[Category]) -> HashMap<CategoryId, String> {
    cats.iter()
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect()
}

impl HomeScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {
            progress: Loader::new(),
            recents: Loader::new(),
            live: Loader::new(),
            movies: Loader::new(),
            series: Loader::new(),
            favourites: Loader::new(),
            recent_movies: Vec::new(),
            recent_series: Vec::new(),
            progress_epoch: 0,
            hero: HeroState::default(),
            focus_map: HashMap::new(),
            created: Instant::now(),
            loaded_for: None,
            loaded_at: None,
        }
    }

    fn load_progress(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let profile = session.profile_id();
        let store = cx.services.store.clone();
        self.progress_epoch = cx.playback.progress_epoch();
        self.progress.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(store.playback().continue_watching(profile, RAIL_ITEMS)?)
        });
    }

    fn load_favourites(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let s = Arc::clone(&session);
        self.favourites
            .start(cx.rt(), &cx.egui, move |_| async move {
                let profile = s.profile_id();
                let store = Arc::clone(s.store());
                let mut favs = tokio::task::spawn_blocking(move || {
                    let f = store.favourites();
                    let mut all = f.list(profile, ContentKind::Movie)?;
                    all.extend(f.list(profile, ContentKind::Series)?);
                    Ok::<_, opensmarters_store::StoreError>(all)
                })
                .await
                .map_err(|_| AppError::Cancelled)??;
                // Most recently saved first.
                favs.sort_by_key(|f| std::cmp::Reverse(f.added_at));
                // Category names from the cache only (never wait for the network here).
                let mut names = HashMap::new();
                if s.is_xtream() {
                    for key in ["xt:vod_categories", "xt:series_categories"] {
                        if let Some(cats) = s.peek_cached::<Vec<Category>>(key).await {
                            names.extend(names_of(&cats));
                        }
                    }
                }
                // Items saved from a category that is locked now stay off Home.
                // TODO(batch B): `Session::is_item_locked` once it lands.
                if s.parental_enabled() {
                    favs.retain(|f| {
                        f.snapshot.category_id.as_deref().is_none_or(|c| {
                            let id = CategoryId::new(c);
                            let name = names.get(&id).map_or("", String::as_str);
                            !s.is_category_locked(f.kind, &id, name)
                        })
                    });
                }
                favs.truncate(RAIL_ITEMS);
                Ok(favs)
            });
    }

    fn load(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        self.loaded_for = Some(session.profile_id());
        self.loaded_at = Some(Instant::now());
        let profile = session.profile_id();
        self.load_progress(cx);
        self.load_favourites(cx);
        let store = cx.services.store.clone();
        self.recents.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(store.recent_live().list(profile, RAIL_ITEMS)?)
        });
        let s = Arc::clone(&session);
        self.live.start(
            cx.rt(),
            &cx.egui,
            move |_| async move { s.live_catalog().await },
        );
        if session.is_xtream() {
            // Never fetch the VOD/series "All" lists here (tens of MB, ~20 s on big
            // panels): use them only if cached, else the first few unlocked categories.
            let s = Arc::clone(&session);
            self.movies.start(cx.rt(), &cx.egui, move |_| async move {
                let cats = s.vod_categories().await?;
                let names = names_of(&cats);
                if let Some(all) = s
                    .peek_cached::<Vec<VodStream>>(&vod_streams_key(None))
                    .await
                {
                    let kept = unlocked(
                        &s,
                        ContentKind::Movie,
                        all.iter().cloned(),
                        |m| m.category_id.as_ref(),
                        &names,
                    );
                    return Ok(Arc::new(kept));
                }
                let open = unlocked(
                    &s,
                    ContentKind::Movie,
                    cats.iter().cloned(),
                    |c| Some(&c.id),
                    &names,
                );
                let mut out: Vec<VodStream> = Vec::new();
                for c in open.iter().take(HOME_CATEGORIES) {
                    if let Ok(v) = s.vod_streams(Some(c.id.clone())).await {
                        out.extend(v.iter().cloned());
                    }
                }
                Ok(Arc::new(out))
            });
            let s = Arc::clone(&session);
            self.series.start(cx.rt(), &cx.egui, move |_| async move {
                let cats = s.series_categories().await?;
                let names = names_of(&cats);
                if let Some(all) = s.peek_cached::<Vec<Series>>(&series_key(None)).await {
                    let kept = unlocked(
                        &s,
                        ContentKind::Series,
                        all.iter().cloned(),
                        |m| m.category_id.as_ref(),
                        &names,
                    );
                    return Ok(Arc::new(kept));
                }
                let open = unlocked(
                    &s,
                    ContentKind::Series,
                    cats.iter().cloned(),
                    |c| Some(&c.id),
                    &names,
                );
                let mut out: Vec<Series> = Vec::new();
                for c in open.iter().take(HOME_CATEGORIES) {
                    if let Ok(v) = s.series_list(Some(c.id.clone())).await {
                        out.extend(v.iter().cloned());
                    }
                }
                Ok(Arc::new(out))
            });
        }
    }

    fn play_progress(cx: &mut AppCtx<'_>, p: &PlaybackProgress) {
        let Some(session) = cx.session() else { return };
        // Catch-up resumes from the Catch-up screen (it knows the archive and the
        // timeshift-format fallback).
        if p.kind == PlaybackKind::Catchup
            && let Some((stream_id, start)) =
                crate::services::session::catchup::parse_progress_id(&p.item_id)
        {
            super::catchup::open(cx, stream_id, Some(start), true);
            return;
        }
        match session.resume_request(p) {
            Ok(req) => cx.play(req, true),
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    fn play_movie(cx: &mut AppCtx<'_>, m: &MovieRef, extension: Option<&str>) {
        let Some(session) = cx.session() else { return };
        // The player shows the catalogue title without the provider tag and "(1981)".
        let shown = vod::display_title(&m.name);
        let year = vod::release_year(None, &m.name).map(|y| y.to_string());
        match session.movie_request(
            m.id,
            extension.or(m.extension.as_deref()),
            shown.title,
            year,
            m.poster.as_deref(),
            None,
        ) {
            Ok(req) => cx.play(req, true),
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    fn live_now(&self, cx: &AppCtx<'_>) -> (Vec<LiveChannel>, &'static str) {
        let (Some(session), Some(cat)) = (cx.session(), self.live.value()) else {
            return (Vec::new(), "");
        };
        let favs = session.favourite_ids(ContentKind::Live);
        let fav: Vec<LiveChannel> = cat
            .channels
            .iter()
            .filter(|c| favs.contains(&c.key))
            .take(RAIL_ITEMS)
            .cloned()
            .collect();
        if !fav.is_empty() {
            return (fav, "Favourites");
        }
        let recent: Vec<LiveChannel> = self
            .recents
            .value()
            .map(|r| {
                r.iter()
                    .filter_map(|rc| cat.get(&rc.stream_id).cloned())
                    .collect()
            })
            .unwrap_or_default();
        (recent, "Recently watched")
    }

    /// Live now's slot: known empty without waiting for the catalogue when there are no
    /// favourite channels and nothing was watched recently.
    fn live_slot(&self, cx: &AppCtx<'_>) -> Slot {
        let Some(session) = cx.session() else {
            return Slot::Loading;
        };
        let no_favs = session.favourite_ids(ContentKind::Live).is_empty();
        if no_favs && self.recents.value().is_some_and(Vec::is_empty) {
            return Slot::Empty;
        }
        if self.live.value().is_none() && self.live.error().is_none() {
            return Slot::Loading;
        }
        if self.live_now(cx).0.is_empty() {
            Slot::Empty
        } else {
            Slot::Ready
        }
    }

    // -----------------------------------------------------------------------------------
    // Hero
    // -----------------------------------------------------------------------------------

    /// Decides the featured item once its inputs are in.
    fn update_featured(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        if !self.progress.has_finished() {
            return;
        }
        let first = self
            .progress
            .value()
            .and_then(|v| v.first().cloned())
            .map(HeroItem::Resume);
        let featured = match first {
            Some(p) => Some((p, "Continue watching")),
            None if session.is_xtream() && !self.movies.has_finished() => None,
            None => Some(
                self.recent_movies
                    .first()
                    .map_or((HeroItem::Welcome, ""), |m| {
                        (HeroItem::Movie(MovieRef::from_stream(m)), "Recently added")
                    }),
            ),
        };
        let Some((featured, kicker)) = featured else {
            return;
        };
        let changed = self
            .hero
            .featured
            .as_ref()
            .is_none_or(|f| f.key() != featured.key());
        if changed {
            // A new featured item (e.g. progress saved) replaces the hero unless the user
            // is browsing the rails.
            let following = self
                .hero
                .target
                .as_ref()
                .zip(self.hero.featured.as_ref())
                .is_some_and(|(t, f)| t.key() != f.key());
            if !following {
                self.hero.target = Some(featured.clone());
                self.hero.target_kicker = kicker;
                self.hero.target_since = cx.egui.input(|i| i.time);
            }
            self.hero.featured = Some(featured);
        }
    }

    /// Follows the focus: a rail card that keeps the focus for 250 ms becomes the hero.
    fn follow_focus(&mut self, ctx: &egui::Context) {
        if !self.hero.interacted {
            self.hero.interacted = ctx.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { pressed: true, .. } | egui::Event::PointerMoved(_)
                    )
                })
            });
        }
        if self.hero.featured.is_none() || !self.hero.interacted {
            return;
        }
        let now = ctx.input(|i| i.time);
        let Some((item, kicker)) =
            focus::current(ctx).and_then(|id| self.focus_map.get(&id).cloned())
        else {
            // On the hero's buttons, "See all" or outside Home: keep what's shown.
            self.hero.pending = None;
            return;
        };
        let key = item.key();
        if self.hero.target.as_ref().is_some_and(|t| t.key() == key) {
            self.hero.pending = None;
            return;
        }
        match &self.hero.pending {
            Some((k, _, _, since)) if *k == key => {
                if now - since >= HERO_DEBOUNCE {
                    self.hero.target = Some(item);
                    self.hero.target_kicker = kicker;
                    self.hero.target_since = now;
                    self.hero.pending = None;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        HERO_DEBOUNCE - (now - since) + 0.01,
                    ));
                }
            }
            _ => {
                self.hero.pending = Some((key, item, kicker, now));
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(HERO_DEBOUNCE));
            }
        }
    }

    /// Loads the target's details and swaps the shown hero when they're in (or after
    /// [`HERO_DETAILS_WAIT`] with what the lists know).
    fn resolve_hero(&mut self, cx: &AppCtx<'_>) {
        if self.hero.loader.poll()
            && let Some(h) = self.hero.loader.take()
        {
            self.hero.loading = None;
            self.hero.cache.insert(h.key.clone(), h);
        }
        if self.hero.loader.error().is_some() {
            // Keep the basic hero; don't retry in a loop.
            if let Some(k) = self.hero.loading.take()
                && let Some(t) = self.hero.target.clone()
                && t.key() == k
            {
                self.hero.cache.insert(k, basic_hero(&t, true));
            }
            self.hero.loader = Loader::new();
        }
        let Some(target) = self.hero.target.clone() else {
            return;
        };
        let key = target.key();
        let now = cx.egui.input(|i| i.time);
        let ready = self.hero.cache.get(&key).cloned();
        if ready.is_none() && self.hero.loading.as_ref() != Some(&key) {
            self.start_details(cx, &target);
        }
        let next = match ready {
            Some(h) => Some(h),
            None if now - self.hero.target_since >= HERO_DETAILS_WAIT => {
                // Don't hold an empty hero for slow details: show what we know now.
                Some(basic_hero(&target, false))
            }
            None => {
                cx.egui
                    .request_repaint_after(std::time::Duration::from_millis(60));
                None
            }
        };
        if let Some(mut h) = next {
            if !self.hero.target_kicker.is_empty() {
                h.kicker = self.hero.target_kicker.to_owned();
            }
            match &self.hero.shown {
                Some(s) if s.key == h.key => {
                    // Details arrived for the hero on screen: update in place.
                    if s.synopsis != h.synopsis
                        || s.meta != h.meta
                        || s.backdrop != h.backdrop
                        || s.kicker != h.kicker
                    {
                        self.hero.shown = Some(h);
                    }
                }
                _ => {
                    self.hero.shown = Some(h);
                    self.hero.shown_at = now;
                }
            }
        }
    }

    fn start_details(&mut self, cx: &AppCtx<'_>, item: &HeroItem) {
        let Some(session) = cx.session() else { return };
        let item = item.clone();
        let profile_name = session.profile().name.clone();
        self.hero.loading = Some(item.key());
        self.hero
            .loader
            .start(cx.rt(), &cx.egui, move |_| async move {
                Ok(match &item {
                    HeroItem::Welcome => Hero {
                        key: HeroKey::Welcome,
                        kicker: "Welcome back".into(),
                        title: profile_name,
                        meta: String::new(),
                        synopsis: "Pick up a channel, or explore what's new from your provider."
                            .into(),
                        backdrop: None,
                        item: item.clone(),
                        extension: None,
                    },
                    HeroItem::Movie(m) => {
                        let info = session.vod_info(m.id).await.ok();
                        let mut h = movie_hero(&m.name, info.as_deref(), item.clone());
                        h.kicker = String::new();
                        if h.backdrop.is_none() {
                            h.backdrop = m.poster.clone();
                        }
                        h
                    }
                    HeroItem::Series(s) => {
                        let info = session.series_info(s.id).await.ok();
                        let mut h = series_hero(s, info.as_deref(), item.clone());
                        if h.backdrop.is_none() {
                            h.backdrop = s.poster.clone();
                        }
                        h
                    }
                    HeroItem::Resume(p) => resume_hero(&session, p, item.clone()).await,
                })
            });
    }

    /// Keeps the backdrop textures for the crossfade.
    fn update_backdrop(&mut self, ctx: &egui::Context, images: &ImageService) {
        let now = ctx.input(|i| i.time);
        let want = self.hero.shown.as_ref().and_then(|h| h.backdrop.clone());
        let cur_url = self.hero.back_cur.as_ref().map(|(u, _)| u.clone());
        if want == cur_url {
            return;
        }
        match images.get(want.as_deref(), ImageSize::Backdrop) {
            ImageSlot::Ready(tex) => {
                self.hero.back_prev = self.hero.back_cur.take().map(|(_, t)| t);
                self.hero.back_cur = want.map(|u| (u, tex));
                self.hero.back_blend_at = now;
            }
            // Keep the current art until the new one is decoded.
            ImageSlot::Loading => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
            ImageSlot::None | ImageSlot::Failed => {
                if self.hero.back_cur.is_some() {
                    self.hero.back_prev = self.hero.back_cur.take().map(|(_, t)| t);
                    self.hero.back_blend_at = now;
                }
            }
        }
    }

    /// The hero band: backdrop (crossfading), scrims, text block and buttons.
    fn hero_band(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, band: Rect) {
        let ctx = cx.egui.clone();
        let painter = ui.painter().clone();
        let now = ctx.input(|i| i.time);
        // Backdrop crossfade: the outgoing art at full strength, the incoming art over it.
        let blend_secs = motion::fade(&ctx, crate::ui::theme::motion::SLOWER) as f64;
        let t = if blend_secs <= 0.0 {
            1.0
        } else {
            ((now - self.hero.back_blend_at) / blend_secs).clamp(0.0, 1.0) as f32
        };
        let t = egui::emath::easing::cubic_in_out(t);
        if t < 1.0 {
            ctx.request_repaint();
        } else {
            self.hero.back_prev = None;
        }
        let tint = Color32::from_gray(150);
        let paint_tex = |tex: &TextureHandle, alpha: f32| {
            let uv = paint::cover_uv(tex.size_vec2(), band.size(), vec2(0.7, 0.3));
            painter.image(tex.id(), band, uv, tint.gamma_multiply(alpha));
        };
        if let Some(prev) = &self.hero.back_prev {
            let a = if self.hero.back_cur.is_some() {
                1.0
            } else {
                1.0 - t
            };
            paint_tex(prev, a);
        }
        if let Some((_, cur)) = &self.hero.back_cur {
            paint_tex(
                cur,
                if self.hero.back_prev.is_some() || t < 1.0 {
                    t
                } else {
                    1.0
                },
            );
        }
        paint::scrim_left(&painter, band);
        paint::scrim_bottom(&painter, band, 0.4);

        let (rect, _) = ui.allocate_exact_size(band.size(), Sense::hover());
        let m = cx.layout.margin;
        let tv = cx.layout.tv;
        let layout = HeroLayout::new(rect, m, tv);
        let hero_scope = Id::unique("home.hero");
        let Some(h) = self.hero.shown.clone() else {
            // Not decided yet: the hero's skeleton (after 150 ms). Its primary pill
            // already holds the initial focus (same id as the real button), so the
            // fallback focus never lands on a rail card (or a "See all") and scrolls the
            // page while the hero loads.
            let pill = Rect::from_min_size(pos2(layout.x, layout.buttons_top), vec2(168.0, 52.0));
            focus::scope(ui, hero_scope, |ui| {
                focus::focusable(
                    ui,
                    Id::unique("home.hero.play"),
                    pill,
                    focus::FocusOpts::default().autofocus(),
                );
            });
            if self.skeleton_due(cx) {
                layout.skeleton(&ctx, &painter);
            }
            return;
        };
        let text_secs = motion::fade(&ctx, crate::ui::theme::motion::BASE) as f64;
        let alpha = if text_secs <= 0.0 {
            1.0
        } else {
            egui::emath::easing::cubic_out(
                ((now - self.hero.shown_at) / text_secs).clamp(0.0, 1.0) as f32
            )
        };
        if alpha < 1.0 {
            ctx.request_repaint();
        }
        let mut faded = painter.clone();
        faded.multiply_opacity(alpha);
        layout.paint_text(&faded, &h);

        let row = Rect::from_min_size(
            pos2(layout.x, layout.buttons_top),
            vec2(rect.width() - 2.0 * m, 52.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.multiply_opacity(alpha.max(0.35));
        // Focus entering the buttons (Up from the first rail) brings the whole band into
        // view, not just the button.
        let reveal = vec2(16.0, (layout.buttons_top - rect.top() + 8.0).max(16.0));
        focus::scope(&mut child, hero_scope, |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            Self::hero_buttons(cx, ui, &h, reveal);
        });
    }

    /// The hero's buttons (fixed ids so the focus stays put when the hero changes).
    fn hero_buttons(cx: &mut AppCtx<'_>, ui: &mut Ui, h: &Hero, reveal: egui::Vec2) {
        let primary_id = Id::unique("home.hero.play");
        let secondary_id = Id::unique("home.hero.info");
        match &h.item {
            HeroItem::Resume(p) => {
                let label = format!("Resume {}", text::timecode(p.position_secs));
                if Button::primary(label)
                    .icon(icons::PLAY)
                    .large()
                    .autofocus()
                    .scroll_margin(reveal)
                    .id(primary_id)
                    .show(ui)
                    .activated
                {
                    Self::play_progress(cx, p);
                }
                let detail = match p.kind {
                    PlaybackKind::Movie => p
                        .item_id
                        .parse::<u64>()
                        .ok()
                        .map(|id| Route::MovieDetail { stream_id: id }),
                    PlaybackKind::Episode => p
                        .snapshot
                        .series_id
                        .as_deref()
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|id| Route::SeriesDetail { series_id: id }),
                    PlaybackKind::Catchup => None,
                };
                if let Some(route) = detail
                    && Button::secondary("More info")
                        .icon(icons::INFO)
                        .large()
                        .scroll_margin(reveal)
                        .id(secondary_id)
                        .show(ui)
                        .activated
                {
                    cx.nav.push(route);
                }
            }
            HeroItem::Movie(mv) => {
                if Button::primary("Play")
                    .icon(icons::PLAY)
                    .large()
                    .autofocus()
                    .scroll_margin(reveal)
                    .id(primary_id)
                    .show(ui)
                    .activated
                {
                    Self::play_movie(cx, mv, h.extension.as_deref());
                }
                if Button::secondary("More info")
                    .icon(icons::INFO)
                    .large()
                    .scroll_margin(reveal)
                    .id(secondary_id)
                    .show(ui)
                    .activated
                {
                    cx.nav.push(Route::MovieDetail { stream_id: mv.id });
                }
            }
            HeroItem::Series(s) => {
                if Button::primary("Episodes")
                    .icon(icons::LIST_BULLETS)
                    .large()
                    .autofocus()
                    .scroll_margin(reveal)
                    .id(primary_id)
                    .show(ui)
                    .activated
                {
                    cx.nav.push(Route::SeriesDetail { series_id: s.id });
                }
            }
            HeroItem::Welcome => {
                if Button::primary("Watch Live TV")
                    .icon(icons::TELEVISION_SIMPLE)
                    .large()
                    .autofocus()
                    .scroll_margin(reveal)
                    .id(primary_id)
                    .show(ui)
                    .activated
                {
                    cx.nav.section(Route::live());
                }
                if Button::secondary("Guide")
                    .icon(icons::ROWS)
                    .large()
                    .scroll_margin(reveal)
                    .id(secondary_id)
                    .show(ui)
                    .activated
                {
                    cx.nav.section(Route::Guide);
                }
            }
        }
    }

    /// True while a store-backed rail may still answer before its slot is reserved: the
    /// store answers within milliseconds, so a skeleton (or a reserved gap that then
    /// collapses) would only flash.
    fn store_grace(&self, loader_done: bool) -> bool {
        !loader_done
            && self
                .loaded_at
                .is_none_or(|t| t.elapsed().as_secs_f64() < SKELETON_DELAY)
    }

    /// True once skeletons may show (§6.16: never before 150 ms).
    fn skeleton_due(&self, cx: &AppCtx<'_>) -> bool {
        let due = self.created.elapsed().as_secs_f64() >= SKELETON_DELAY;
        if !due {
            cx.egui
                .request_repaint_after(std::time::Duration::from_secs_f64(SKELETON_DELAY));
        }
        due
    }

    // -----------------------------------------------------------------------------------
    // Rails
    // -----------------------------------------------------------------------------------

    /// One rail slot: the real rail when ready (fading in), its skeleton while loading,
    /// collapsing when it turns out empty.
    fn slot<R>(
        &self,
        cx: &AppCtx<'_>,
        ui: &mut Ui,
        spec: SlotSpec,
        content: impl FnOnce(&mut Ui) -> R,
    ) -> Option<R> {
        let SlotSpec {
            id,
            state,
            card,
            heading_w,
        } = spec;
        if state == Slot::Pending {
            return None;
        }
        let ctx = cx.egui.clone();
        let full_h = rail_height(card) + RAIL_GAP;
        let h = ctx.animate_value_with_time(
            id.with("slot-h"),
            if state == Slot::Empty { 0.0 } else { full_h },
            motion::duration(&ctx, crate::ui::theme::motion::BASE),
        );
        let fade = ctx.animate_bool_with_time_and_easing(
            id.with("slot-in"),
            state == Slot::Ready,
            motion::fade(&ctx, crate::ui::theme::motion::BASE),
            egui::emath::easing::cubic_out,
        );
        match state {
            Slot::Ready => {
                let mut out = None;
                ui.scope(|ui| {
                    ui.multiply_opacity(fade);
                    out = Some(content(ui));
                });
                ui.add_space(RAIL_GAP);
                out
            }
            Slot::Loading | Slot::Empty | Slot::Pending => {
                if h < 0.5 {
                    return None;
                }
                let (slot, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                if self.skeleton_due(cx) {
                    let mut p = ui.painter().with_clip_rect(ui.clip_rect().intersect(slot));
                    if state == Slot::Empty {
                        p.multiply_opacity(h / full_h);
                    }
                    let rail = Rect::from_min_size(slot.min, vec2(slot.width(), rail_height(card)));
                    skeleton_rail(&ctx, &p, rail, cx.layout.margin, card, heading_w);
                }
                None
            }
        }
    }
}

/// One rail slot: its focus/animation id, loading state, card shape and the width of its
/// heading's skeleton bar.
#[derive(Debug, Clone, Copy)]
struct SlotSpec {
    id: Id,
    state: Slot,
    card: SkeletonCard,
    heading_w: f32,
}

/// Positions of the hero's text block, bottom-aligned above the rails (§8.2).
#[derive(Debug, Clone, Copy)]
struct HeroLayout {
    x: f32,
    text_w: f32,
    title_w: f32,
    title_rows: usize,
    synopsis_rows: usize,
    /// Top of the button row.
    buttons_top: f32,
    /// Top limit for the text (below the band's top edge).
    top: f32,
}

impl HeroLayout {
    fn new(band: Rect, margin: f32, tv: bool) -> Self {
        let x = band.left() + margin;
        let avail_w = band.width() - 2.0 * margin;
        // The buttons sit 24 pt above where the rails start (the band minus the overlap).
        let buttons_top = band.bottom() - RAIL_OVERLAP - 24.0 - 52.0;
        let top = band.top() + if tv { 20.0 } else { 40.0 };
        let room = buttons_top - top;
        // kicker 16 + 6 + title + 8 + meta 18 + 12 + synopsis + 24
        let fixed = 16.0 + 6.0 + 8.0 + 18.0 + 12.0 + 24.0;
        let (title_rows, synopsis_rows) = [(2, 2), (1, 2), (1, 1)]
            .into_iter()
            .find(|(t, s)| fixed + *t as f32 * 46.0 + *s as f32 * 24.0 <= room)
            .unwrap_or((1, 1));
        Self {
            x,
            text_w: 560.0_f32.min(avail_w),
            title_w: 900.0_f32.min(avail_w),
            title_rows,
            synopsis_rows,
            buttons_top,
            top,
        }
    }

    /// Paints kicker, title, meta and synopsis bottom-up from the buttons.
    fn paint_text(&self, painter: &Painter, h: &Hero) {
        let syn = (!h.synopsis.is_empty()).then(|| {
            text::galley(
                painter,
                &h.synopsis,
                font::body(),
                color::TEXT_2,
                self.text_w,
                self.synopsis_rows,
            )
        });
        let title = text::galley(
            painter,
            &h.title,
            font::display(),
            color::TEXT,
            self.title_w,
            self.title_rows,
        );
        let mut y = self.buttons_top - 24.0;
        if let Some(g) = &syn {
            y -= g.size().y;
            painter.galley(pos2(self.x, y), Arc::clone(g), color::TEXT_2);
            y -= 12.0;
        }
        if !h.meta.is_empty() {
            y -= 18.0;
            text::line(
                painter,
                pos2(self.x, y),
                Align2::LEFT_TOP,
                &h.meta,
                font::caption(),
                color::TEXT_2,
                self.text_w,
            );
            y -= 8.0;
        }
        y -= title.size().y;
        painter.galley(pos2(self.x, y), Arc::clone(&title), color::TEXT);
        if !h.kicker.is_empty() {
            text::overline(
                painter,
                pos2(self.x, (y - 6.0).max(self.top)),
                Align2::LEFT_BOTTOM,
                &h.kicker,
                color::TEXT_2,
            );
        }
    }

    /// The hero's loading skeleton (same geometry as the text and buttons).
    fn skeleton(&self, ctx: &egui::Context, painter: &Painter) {
        let x = self.x;
        let b = self.buttons_top;
        skeleton(
            ctx,
            painter,
            Rect::from_min_size(pos2(x, b), vec2(168.0, 52.0)),
            radius::SM,
        );
        skeleton(
            ctx,
            painter,
            Rect::from_min_size(pos2(x + 180.0, b), vec2(150.0, 52.0)),
            radius::SM,
        );
        let mut y = b - 24.0;
        for w in [0.62_f32, 1.0].into_iter().take(self.synopsis_rows) {
            y -= 24.0;
            skeleton(
                ctx,
                painter,
                Rect::from_min_size(pos2(x, y + 6.0), vec2(self.text_w * w, 12.0)),
                radius::XS,
            );
        }
        y -= 12.0 + 18.0;
        skeleton(
            ctx,
            painter,
            Rect::from_min_size(pos2(x, y + 3.0), vec2(220.0, 12.0)),
            radius::XS,
        );
        y -= 8.0 + 46.0;
        skeleton(
            ctx,
            painter,
            Rect::from_min_size(
                pos2(x, y + 6.0),
                vec2((self.title_w * 0.6).min(520.0), 36.0),
            ),
            radius::SM,
        );
        y -= 6.0 + 16.0;
        skeleton(
            ctx,
            painter,
            Rect::from_min_size(pos2(x, y + 4.0), vec2(120.0, 10.0)),
            radius::XS,
        );
    }
}

/// What the hero shows before (or without) an item's details. The list's poster stands
/// in for the backdrop only when the details failed (`final_art`): while they load, the
/// current backdrop stays, so the art changes once, straight to the real backdrop.
fn basic_hero(item: &HeroItem, final_art: bool) -> Hero {
    let key = item.key();
    match item {
        HeroItem::Movie(m) => {
            let shown = vod::display_title(&m.name);
            Hero {
                key,
                kicker: String::new(),
                title: shown.title.to_owned(),
                meta: m.meta.clone(),
                synopsis: String::new(),
                backdrop: m.poster.clone().filter(|_| final_art),
                item: item.clone(),
                extension: m.extension.clone(),
            }
        }
        HeroItem::Series(s) => Hero {
            key,
            kicker: "Series".into(),
            title: vod::display_title(&s.name).title.to_owned(),
            meta: s.meta.clone(),
            synopsis: String::new(),
            backdrop: s.poster.clone().filter(|_| final_art),
            item: item.clone(),
            extension: None,
        },
        HeroItem::Resume(p) => Hero {
            key,
            kicker: "Continue watching".into(),
            title: p.snapshot.title.clone(),
            meta: p
                .duration_secs
                .map(|d| text::remaining((d - p.position_secs).max(0.0)))
                .unwrap_or_default(),
            synopsis: String::new(),
            backdrop: p.snapshot.poster_url.clone().filter(|_| final_art),
            item: item.clone(),
            extension: None,
        },
        HeroItem::Welcome => Hero {
            key,
            kicker: "Welcome back".into(),
            title: String::new(),
            meta: String::new(),
            synopsis: "Pick up a channel, or explore what's new from your provider.".into(),
            backdrop: None,
            item: item.clone(),
            extension: None,
        },
    }
}

fn movie_hero(fallback_title: &str, info: Option<&VodInfo>, item: HeroItem) -> Hero {
    let mut meta = Vec::new();
    let mut synopsis = String::new();
    let mut backdrop = None;
    let mut title = fallback_title.to_owned();
    let mut release_date = None;
    let mut extension = None;
    if let Some(i) = info {
        let d = &i.info;
        if let Some(n) = &d.name {
            title = n.clone();
        }
        release_date = d.release_date.clone();
        if let Some(secs) = d.duration_secs {
            meta.push(text::duration_short(secs as f64));
        }
        if let Some(r) = vod::rating_label(d.rating) {
            meta.push(r);
        }
        if let Some(g) = &d.genre {
            meta.push(text::cap(g, 40).into_owned());
        }
        synopsis = d.plot.clone().unwrap_or_default();
        backdrop = d
            .backdrop_path
            .first()
            .cloned()
            .or_else(|| d.movie_image.clone());
        extension = i
            .movie_data
            .as_ref()
            .and_then(|m| m.container_extension.clone());
    }
    // Year from the release date, else a trailing "(YYYY)" in the name.
    if let Some(y) = vod::release_year(release_date.as_deref(), &title)
        .or_else(|| vod::display_title(fallback_title).year)
    {
        meta.insert(0, y.to_string());
    }
    let shown = vod::display_title(&title);
    if let Some(tag) = shown.tag {
        meta.insert(0, tag.to_owned());
    }
    Hero {
        key: item.key(),
        kicker: String::new(),
        title: shown.title.to_owned(),
        meta: meta.join(" · "),
        synopsis: text::cap_paragraph(synopsis.trim(), 600),
        backdrop,
        item,
        extension,
    }
}

fn series_hero(s: &SeriesRef, info: Option<&SeriesInfo>, item: HeroItem) -> Hero {
    let mut h = basic_hero(&item, false);
    if let Some(i) = info {
        let d = &i.info;
        let raw = vod::series_name(i);
        let shown = vod::display_title(&raw);
        if !shown.title.trim().is_empty() {
            h.title = shown.title.to_owned();
        }
        let seasons = i
            .episodes
            .iter()
            .filter(|(n, e)| **n > 0 && !e.is_empty())
            .count();
        let rating = d.rating.or(d.rating_5based.map(|r| r * 2.0));
        h.meta = vod::meta_line(
            [
                vod::release_year(d.release_date.as_deref(), &raw).map(|y| y.to_string()),
                (seasons > 0)
                    .then(|| format!("{seasons} season{}", if seasons == 1 { "" } else { "s" })),
                vod::rating_label(rating),
                d.genre.as_deref().map(|g| text::cap(g, 40).into_owned()),
            ]
            .into_iter()
            .flatten(),
        );
        h.synopsis = d
            .plot
            .as_deref()
            .map(|p| text::cap_paragraph(p.trim(), 600))
            .unwrap_or_default();
        h.backdrop = d
            .backdrop_path
            .first()
            .cloned()
            .or_else(|| d.cover.clone())
            .or_else(|| s.poster.clone());
    }
    h
}

async fn resume_hero(session: &Arc<Session>, p: &PlaybackProgress, item: HeroItem) -> Hero {
    let left = p.duration_secs.map(|d| (d - p.position_secs).max(0.0));
    let mut hero = match p.kind {
        PlaybackKind::Movie => {
            let info = match p.item_id.parse::<u64>() {
                Ok(id) => session.vod_info(id).await.ok(),
                Err(_) => None,
            };
            movie_hero(&p.snapshot.title, info.as_deref(), item)
        }
        PlaybackKind::Episode => {
            let info = match p
                .snapshot
                .series_id
                .as_deref()
                .and_then(|s| s.parse::<u64>().ok())
            {
                Some(id) => session.series_info(id).await.ok(),
                None => None,
            };
            let (title, synopsis, backdrop) = match &info {
                Some(i) => (
                    i.info.name.as_deref().map_or_else(
                        || p.snapshot.title.clone(),
                        |n| vod::display_title(n).title.to_owned(),
                    ),
                    i.info.plot.clone().unwrap_or_default(),
                    i.info
                        .backdrop_path
                        .first()
                        .cloned()
                        .or(i.info.cover.clone()),
                ),
                None => (p.snapshot.title.clone(), String::new(), None),
            };
            let se = match (p.snapshot.season, p.snapshot.episode) {
                (Some(s), Some(e)) => {
                    format!("S{s} · E{e} · {}", text::cap(&p.snapshot.title, 60))
                }
                _ => String::new(),
            };
            Hero {
                key: item.key(),
                kicker: String::new(),
                title,
                meta: se,
                synopsis: text::cap_paragraph(synopsis.trim(), 600),
                backdrop,
                item,
                extension: None,
            }
        }
        PlaybackKind::Catchup => Hero {
            key: item.key(),
            kicker: String::new(),
            title: p.snapshot.title.clone(),
            meta: "Catch-up".into(),
            synopsis: String::new(),
            backdrop: None,
            item,
            extension: None,
        },
    };
    hero.kicker = "Continue watching".into();
    if let Some(l) = left {
        let r = text::remaining(l);
        hero.meta = if hero.meta.is_empty() {
            r
        } else {
            format!("{} · {r}", hero.meta)
        };
    }
    if hero.backdrop.is_none() {
        hero.backdrop = p.snapshot.poster_url.clone();
    }
    hero
}

impl Screen for HomeScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.load(cx);
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Home)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let session = cx.session();
        if session.is_some() && self.loaded_for != session.as_ref().map(|s| s.profile_id()) {
            // The startup profile opened while Home was waiting.
            self.load(cx);
        }
        if cx.playback.progress_epoch() != self.progress_epoch {
            // A progress save finished after we loaded (e.g. leaving the player).
            self.load_progress(cx);
            self.hero
                .cache
                .retain(|k, _| !matches!(k, HeroKey::Progress(_)));
        }
        self.progress.poll();
        self.recents.poll();
        self.live.poll();
        self.favourites.poll();
        if self.movies.poll()
            && let Some(all) = self.movies.value()
        {
            let idx = sorted_indices(all, SortOrder::RecentlyAdded);
            self.recent_movies = idx
                .into_iter()
                .take(RAIL_ITEMS)
                .filter_map(|i| all.get(i).cloned())
                .collect();
        }
        if self.series.poll()
            && let Some(all) = self.series.value()
        {
            let idx = sorted_indices(all, SortOrder::RecentlyAdded);
            self.recent_series = idx
                .into_iter()
                .take(RAIL_ITEMS)
                .filter_map(|i| all.get(i).cloned())
                .collect();
        }
        self.update_featured(cx);
        let ctx = cx.egui.clone();
        self.follow_focus(&ctx);
        self.resolve_hero(cx);
        let images = cx.services.images.clone();
        self.update_backdrop(&ctx, &images);

        let content = cx.layout.content;
        let m = cx.layout.margin;
        let tv = cx.layout.tv;
        // Hero band: 52% of the window (§8.2); TV keeps it to ≈ 44% above the rails so the
        // first rail's titles stay on screen at 10 feet.
        let hero_h = if tv {
            (content.height() * 0.44).round() + RAIL_OVERLAP
        } else {
            (content.height() * 0.52).max(400.0)
        };
        let painter = ui.painter().clone();
        // Ambient background behind everything.
        if !assets::paint_cover(
            ui,
            &painter,
            Asset::HomeAmbient,
            content,
            vec2(1.0, 0.0),
            0,
            Color32::WHITE,
        ) {
            painter.rect_filled(content, 0.0, color::BG);
        }

        let progress = self.progress.value().cloned().unwrap_or_default();
        let (live_now, live_kicker) = self.live_now(cx);
        let xtream = session.as_ref().is_none_or(|s| s.is_xtream());
        let hero_ready = self.hero.shown.is_some();
        let state = |l_has: bool, finished: bool, failed: bool| {
            if l_has {
                Slot::Ready
            } else if finished || failed {
                Slot::Empty
            } else {
                Slot::Loading
            }
        };
        // Before the profile is open only the provider rails reserve slots: the store
        // rails (continue watching, favourites, live now's emptiness) resolve within
        // milliseconds once it is, so their skeletons would only flash.
        let open = session.is_some();
        let cw_done = self.progress.has_finished() || self.progress.error().is_some();
        let fav_done = self.favourites.has_finished() || self.favourites.error().is_some();
        let recents_done = self.recents.has_finished() || self.recents.error().is_some();
        let cw_slot = if open && !self.store_grace(cw_done) {
            state(
                !progress.is_empty(),
                self.progress.has_finished(),
                self.progress.error().is_some(),
            )
        } else {
            Slot::Pending
        };
        let live_slot = if open && !self.store_grace(recents_done) {
            self.live_slot(cx)
        } else {
            Slot::Pending
        };
        let movies_slot = if xtream {
            state(
                !self.recent_movies.is_empty(),
                self.movies.has_finished(),
                self.movies.error().is_some(),
            )
        } else {
            Slot::Empty
        };
        let series_slot = if xtream {
            state(
                !self.recent_series.is_empty(),
                self.series.has_finished(),
                self.series.error().is_some(),
            )
        } else {
            Slot::Empty
        };
        let favs = self.favourites.value().cloned().unwrap_or_default();
        let fav_slot = if open && !self.store_grace(fav_done) {
            state(
                !favs.is_empty(),
                self.favourites.has_finished(),
                self.favourites.error().is_some(),
            )
        } else {
            Slot::Pending
        };
        let all_empty = [cw_slot, live_slot, movies_slot, series_slot, fav_slot]
            .iter()
            .all(|s| *s == Slot::Empty);

        let mut focus_map: HashMap<Id, (HeroItem, &'static str)> = HashMap::new();
        ScrollArea::vertical()
            .id_salt("home.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let top = ui.cursor().top();
                let band =
                    Rect::from_min_size(pos2(content.left(), top), vec2(content.width(), hero_h));
                self.hero_band(cx, ui, band);
                ui.add_space(-RAIL_OVERLAP);

                // Continue watching.
                let size = cx.layout.backdrop_card_size();
                let r = self.slot(
                    cx,
                    ui,
                    SlotSpec {
                        id: Id::unique("home.rail.continue"),
                        state: cw_slot,
                        card: SkeletonCard::Tile(size),
                        heading_w: 220.0,
                    },
                    |ui| {
                        Rail::new(Id::unique("home.rail.continue"), "Continue watching")
                            .kicker(format!(
                                "{} title{}",
                                progress.len(),
                                if progress.len() == 1 { "" } else { "s" }
                            ))
                            .margin(m)
                            .show(ui, |ui| {
                                let mut play = None;
                                for p in &progress {
                                    let img = images
                                        .get(p.snapshot.poster_url.as_deref(), ImageSize::Card);
                                    let meta = continue_meta(p);
                                    let id = Id::unique((
                                        "home.cw",
                                        format!("{:?}", p.kind),
                                        &p.item_id,
                                    ));
                                    let f = BackdropCard::new(id, &img, &p.snapshot.title, size)
                                        .meta(Some(&meta))
                                        .progress(p.fraction().map(|f| f as f32))
                                        .show(ui);
                                    focus_map.insert(
                                        id,
                                        (HeroItem::Resume(p.clone()), "Continue watching"),
                                    );
                                    if f.activated {
                                        play = Some(p.clone());
                                    }
                                }
                                play
                            })
                            .inner
                    },
                );
                if let Some(Some(p)) = r {
                    Self::play_progress(cx, &p);
                }

                // Live now.
                let size = cx.layout.channel_tile_size();
                let session2 = session.clone();
                let now = cx.now;
                let r = self.slot(
                    cx,
                    ui,
                    SlotSpec {
                        id: Id::unique("home.rail.live"),
                        state: live_slot,
                        card: SkeletonCard::Tile(size),
                        heading_w: 150.0,
                    },
                    |ui| {
                        let channels = Arc::new(live_now.clone());
                        let r = Rail::new(Id::unique("home.rail.live"), "Live now")
                            .kicker(live_kicker)
                            .see_all(hero_ready)
                            .margin(m)
                            .show(ui, |ui| {
                                let mut play = None;
                                for (i, ch) in live_now.iter().enumerate() {
                                    let logo = images.get(ch.logo.as_deref(), ImageSize::Logo);
                                    let now_p =
                                        session2.as_ref().and_then(|s| s.now_next(ch, now).1);
                                    let f = ChannelTile::new(
                                        Id::unique(("home.live", &ch.key)),
                                        &logo,
                                        &ch.name,
                                        size,
                                    )
                                    .now(now_p.as_ref().map(|p| p.title.as_str()))
                                    .progress(now_p.as_ref().map(|p| p.progress(now)))
                                    .show(ui);
                                    if f.activated {
                                        play = Some(i);
                                    }
                                }
                                play
                            });
                        (r.see_all, r.inner, channels)
                    },
                );
                if let Some((see_all, play, channels)) = r {
                    if see_all {
                        cx.nav.section(Route::Live {
                            category: Some("favourites".into()),
                        });
                    }
                    if let Some(i) = play
                        && let Some(ch) = channels.get(i)
                        && let Some(session) = cx.session()
                    {
                        match session.live_request(
                            ch,
                            Some(ZapContext {
                                channels: Arc::clone(&channels),
                                index: i,
                            }),
                        ) {
                            Ok(req) => cx.play(req, true),
                            Err(e) => cx.toasts.error(e.user_message()),
                        }
                    }
                }

                // Recently added movies.
                let poster_w = cx.layout.poster_width;
                let movies = self.recent_movies.clone();
                let r = self.slot(
                    cx,
                    ui,
                    SlotSpec {
                        id: Id::unique("home.rail.movies"),
                        state: movies_slot,
                        card: SkeletonCard::Poster(poster_w),
                        heading_w: 200.0,
                    },
                    |ui| {
                        let r = Rail::new(Id::unique("home.rail.movies"), "Recently added")
                            .kicker(format!("{} titles", movies.len()))
                            .see_all(hero_ready)
                            .margin(m)
                            .show(ui, |ui| {
                                let mut open = None;
                                for mv in &movies {
                                    let img =
                                        images.get(mv.stream_icon.as_deref(), ImageSize::Poster);
                                    let meta = vod::movie_card_meta(mv);
                                    let id = Id::unique(("home.movie", mv.stream_id));
                                    let f = PosterCard::new(
                                        id,
                                        &img,
                                        vod::display_title(&mv.name).title,
                                        poster_w,
                                    )
                                    .meta(Some(&meta))
                                    .meta_on_focus_only(tv)
                                    .show(ui);
                                    focus_map.insert(
                                        id,
                                        (
                                            HeroItem::Movie(MovieRef::from_stream(mv)),
                                            "Recently added",
                                        ),
                                    );
                                    if f.activated {
                                        open = Some(mv.stream_id);
                                    }
                                }
                                open
                            });
                        (r.see_all, r.inner)
                    },
                );
                if let Some((see_all, open)) = r {
                    if see_all {
                        cx.nav.section(Route::Movies { category: None });
                    }
                    if let Some(id) = open {
                        cx.nav.push(Route::MovieDetail { stream_id: id });
                    }
                }

                // Series.
                let series = self.recent_series.clone();
                let r = self.slot(
                    cx,
                    ui,
                    SlotSpec {
                        id: Id::unique("home.rail.series"),
                        state: series_slot,
                        card: SkeletonCard::Poster(poster_w),
                        heading_w: 120.0,
                    },
                    |ui| {
                        let r = Rail::new(Id::unique("home.rail.series"), "Series")
                            .kicker("Recently updated")
                            .see_all(hero_ready)
                            .margin(m)
                            .show(ui, |ui| {
                                let mut open = None;
                                for s in &series {
                                    let img = images.get(s.poster(), ImageSize::Poster);
                                    let meta = vod::series_card_meta(s);
                                    let id = Id::unique(("home.series", s.series_id));
                                    let f = PosterCard::new(
                                        id,
                                        &img,
                                        vod::display_title(&s.name).title,
                                        poster_w,
                                    )
                                    .meta(Some(&meta))
                                    .meta_on_focus_only(tv)
                                    .show(ui);
                                    focus_map.insert(
                                        id,
                                        (HeroItem::Series(SeriesRef::from_series(s)), "Series"),
                                    );
                                    if f.activated {
                                        open = Some(s.series_id);
                                    }
                                }
                                open
                            });
                        (r.see_all, r.inner)
                    },
                );
                if let Some((see_all, open)) = r {
                    if see_all {
                        cx.nav.section(Route::Series { category: None });
                    }
                    if let Some(id) = open {
                        cx.nav.push(Route::SeriesDetail { series_id: id });
                    }
                }

                // Favourites (films and series).
                let r = self.slot(
                    cx,
                    ui,
                    SlotSpec {
                        id: Id::unique("home.rail.favourites"),
                        state: fav_slot,
                        card: SkeletonCard::Poster(poster_w),
                        heading_w: 160.0,
                    },
                    |ui| {
                        let r = Rail::new(Id::unique("home.rail.favourites"), "Favourites")
                            .kicker(format!("{} saved", favs.len()))
                            .see_all(hero_ready)
                            .margin(m)
                            .show(ui, |ui| {
                                let mut open = None;
                                for f in &favs {
                                    let Ok(item_id) = f.item_id.parse::<u64>() else {
                                        continue;
                                    };
                                    let img = images
                                        .get(f.snapshot.icon_url.as_deref(), ImageSize::Poster);
                                    let (route, item, kind_label) = match f.kind {
                                        ContentKind::Series => (
                                            Route::SeriesDetail { series_id: item_id },
                                            HeroItem::Series(SeriesRef {
                                                id: item_id,
                                                name: f.snapshot.name.clone(),
                                                poster: f.snapshot.icon_url.clone(),
                                                meta: "Series".into(),
                                            }),
                                            "Series",
                                        ),
                                        _ => (
                                            Route::MovieDetail { stream_id: item_id },
                                            HeroItem::Movie(MovieRef {
                                                id: item_id,
                                                name: f.snapshot.name.clone(),
                                                poster: f.snapshot.icon_url.clone(),
                                                extension: None,
                                                meta: String::new(),
                                            }),
                                            "Film",
                                        ),
                                    };
                                    let shown = vod::display_title(&f.snapshot.name);
                                    let meta = vod::meta_line(
                                        [
                                            Some(kind_label.to_owned()),
                                            shown.year.map(|y| y.to_string()),
                                        ]
                                        .into_iter()
                                        .flatten(),
                                    );
                                    let id = Id::unique(("home.fav", f.kind, item_id));
                                    let card = PosterCard::new(id, &img, shown.title, poster_w)
                                        .meta(Some(&meta))
                                        .meta_on_focus_only(tv)
                                        .show(ui);
                                    focus_map.insert(id, (item, "Favourites"));
                                    if card.activated {
                                        open = Some(route);
                                    }
                                }
                                open
                            });
                        (r.see_all, r.inner)
                    },
                );
                if let Some((see_all, open)) = r {
                    if see_all {
                        cx.nav.section(Route::Favourites);
                    }
                    if let Some(route) = open {
                        cx.nav.push(route);
                    }
                }

                if all_empty && let Some(err) = self.live.error().cloned() {
                    let (r, _) =
                        ui.allocate_exact_size(vec2(content.width(), 420.0), Sense::hover());
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
                    let f = EmptyState::new(
                        Id::unique("home.error"),
                        Illustration::ErrorOffline,
                        err.title(),
                        &err.user_message(),
                    )
                    .action("Retry", !hero_ready)
                    .illustration_size(160.0)
                    .show(&mut child);
                    if f.is_some_and(|f| f.activated) {
                        self.load(cx);
                    }
                }
                ui.add_space(48.0);
            });
        self.focus_map = focus_map;
    }
}

/// "S1 · E3 · 18 min remaining" / "1 h 06 remaining".
fn continue_meta(p: &PlaybackProgress) -> String {
    let left = p
        .duration_secs
        .map(|d| text::remaining(d - p.position_secs))
        .unwrap_or_default();
    match (p.kind, p.snapshot.season, p.snapshot.episode) {
        (PlaybackKind::Episode, Some(s), Some(e)) => format!("S{s} · E{e} · {left}"),
        _ => left,
    }
}

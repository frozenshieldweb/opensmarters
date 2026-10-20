// SPDX-License-Identifier: GPL-3.0-or-later
//! Home (§8.2): a hero band (the most recent "continue watching" item, else the newest
//! movie, else a welcome band on the ambient background) and rails: Continue watching,
//! Live now (favourite channels, else recently watched), Recently added movies, Series.
//! Empty rails are hidden.

use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, Id, Rect, ScrollArea, Sense, Ui, pos2, vec2};
use opensmarters_core::catalog::{SortOrder, sorted_indices};
use opensmarters_core::{Series, VodStream};
use opensmarters_store::{ContentKind, PlaybackKind, PlaybackProgress, RecentChannel};

use super::{AppCtx, Screen};
use crate::error::AppError;
use crate::nav::{Route, Section};
use crate::playback::ZapContext;
use crate::services::catalog::{LiveCatalog, LiveChannel, split_tag};
use crate::services::session::{series_key, vod_streams_key};
use crate::services::{ImageSize, ImageSlot};
use crate::task::Loader;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::icons;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{BackdropCard, Button, ChannelTile, EmptyState, PosterCard, Rail};
use crate::ui::{paint, text};

/// Categories sampled for Home's rails when the "All" lists are not cached.
const HOME_CATEGORIES: usize = 3;

#[derive(Debug, Clone)]
enum HeroAction {
    Resume(PlaybackProgress),
    Movie(VodStream),
    Welcome,
}

#[derive(Debug, Clone)]
struct Hero {
    kicker: String,
    title: String,
    meta: String,
    synopsis: String,
    backdrop: Option<String>,
    action: HeroAction,
}

/// The Home screen.
#[derive(Debug)]
pub struct HomeScreen {
    progress: Loader<Vec<PlaybackProgress>>,
    recents: Loader<Vec<RecentChannel>>,
    live: Loader<Arc<LiveCatalog>>,
    movies: Loader<Arc<Vec<VodStream>>>,
    series: Loader<Arc<Vec<Series>>>,
    hero: Loader<Hero>,
    hero_source: Option<String>,
    recent_movies: Vec<VodStream>,
    recent_series: Vec<Series>,
    progress_epoch: u64,
}

impl Default for HomeScreen {
    fn default() -> Self {
        Self::new()
    }
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
            hero: Loader::new(),
            hero_source: None,
            recent_movies: Vec::new(),
            recent_series: Vec::new(),
            progress_epoch: 0,
        }
    }

    fn load_progress(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let profile = session.profile_id();
        let store = cx.services.store.clone();
        self.progress_epoch = cx.playback.progress_epoch();
        self.progress.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(store.playback().continue_watching(profile, 20)?)
        });
    }

    fn load(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let profile = session.profile_id();
        self.load_progress(cx);
        let store = cx.services.store.clone();
        self.recents.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(store.recent_live().list(profile, 20)?)
        });
        let s = Arc::clone(&session);
        self.live.start(
            cx.rt(),
            &cx.egui,
            move |_| async move { s.live_catalog().await },
        );
        if session.is_xtream() {
            // Never fetch the VOD/series "All" lists here (tens of MB, ~20 s on big
            // panels): use them only if cached, else the first few categories.
            let s = Arc::clone(&session);
            self.movies.start(cx.rt(), &cx.egui, move |_| async move {
                if let Some(all) = s.peek_cached(&vod_streams_key(None)).await {
                    return Ok(all);
                }
                let cats = s.vod_categories().await?;
                let mut out: Vec<VodStream> = Vec::new();
                for c in cats.iter().take(HOME_CATEGORIES) {
                    if let Ok(v) = s.vod_streams(Some(c.id.clone())).await {
                        out.extend(v.iter().cloned());
                    }
                }
                Ok(Arc::new(out))
            });
            let s = Arc::clone(&session);
            self.series.start(cx.rt(), &cx.egui, move |_| async move {
                if let Some(all) = s.peek_cached(&series_key(None)).await {
                    return Ok(all);
                }
                let cats = s.series_categories().await?;
                let mut out: Vec<Series> = Vec::new();
                for c in cats.iter().take(HOME_CATEGORIES) {
                    if let Ok(v) = s.series_list(Some(c.id.clone())).await {
                        out.extend(v.iter().cloned());
                    }
                }
                Ok(Arc::new(out))
            });
        }
    }

    /// Picks the hero item and loads its details (backdrop, synopsis).
    fn update_hero(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        // Wait for the inputs that decide the hero.
        if !self.progress.has_finished() || (session.is_xtream() && !self.movies.has_finished()) {
            return;
        }
        let progress = self.progress.value().and_then(|v| v.first().cloned());
        let (source, hero_action) = match (&progress, self.recent_movies.first()) {
            (Some(p), _) => (
                format!("p:{:?}:{}", p.kind, p.item_id),
                HeroAction::Resume(p.clone()),
            ),
            (None, Some(m)) => (format!("m:{}", m.stream_id), HeroAction::Movie(m.clone())),
            (None, None) => ("welcome".to_owned(), HeroAction::Welcome),
        };
        if self.hero_source.as_deref() == Some(source.as_str()) {
            return;
        }
        self.hero_source = Some(source);
        let profile_name = session.profile().name.clone();
        self.hero.start(cx.rt(), &cx.egui, move |_| async move {
            Ok(match hero_action {
                HeroAction::Welcome => Hero {
                    kicker: "Welcome back".into(),
                    title: profile_name,
                    meta: String::new(),
                    synopsis: "Pick up a channel, or explore what's new from your provider.".into(),
                    backdrop: None,
                    action: HeroAction::Welcome,
                },
                HeroAction::Movie(m) => {
                    let info = session.vod_info(m.stream_id).await.ok();
                    let mut hero =
                        movie_hero(&m.name, info.as_deref(), HeroAction::Movie(m.clone()));
                    hero.kicker = "Recently added".into();
                    if hero.backdrop.is_none() {
                        hero.backdrop = m.stream_icon.clone();
                    }
                    hero
                }
                HeroAction::Resume(p) => {
                    let left = p.duration_secs.map(|d| (d - p.position_secs).max(0.0));
                    let mut hero = match p.kind {
                        PlaybackKind::Movie => {
                            let info = match p.item_id.parse::<u64>() {
                                Ok(id) => session.vod_info(id).await.ok(),
                                Err(_) => None,
                            };
                            movie_hero(
                                &p.snapshot.title,
                                info.as_deref(),
                                HeroAction::Resume(p.clone()),
                            )
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
                                    i.info
                                        .name
                                        .clone()
                                        .unwrap_or_else(|| p.snapshot.title.clone()),
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
                                kicker: String::new(),
                                title,
                                meta: se,
                                synopsis,
                                backdrop,
                                action: HeroAction::Resume(p.clone()),
                            }
                        }
                        PlaybackKind::Catchup => Hero {
                            kicker: String::new(),
                            title: p.snapshot.title.clone(),
                            meta: "Catch-up".into(),
                            synopsis: String::new(),
                            backdrop: None,
                            action: HeroAction::Resume(p.clone()),
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
            })
        });
    }

    fn play_progress(cx: &mut AppCtx<'_>, p: &PlaybackProgress) {
        let Some(session) = cx.session() else { return };
        // Catch-up resumes from the Catch-up screen (it knows the archive and the
        // timeshift-format fallback).
        if p.kind == opensmarters_store::PlaybackKind::Catchup
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

    fn play_movie(cx: &mut AppCtx<'_>, m: &VodStream) {
        let Some(session) = cx.session() else { return };
        match session.movie_request(
            m.stream_id,
            m.container_extension.as_deref(),
            &m.name,
            None,
            m.stream_icon.as_deref(),
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
            .take(20)
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
}

fn movie_hero(
    fallback_title: &str,
    info: Option<&opensmarters_core::VodInfo>,
    action: HeroAction,
) -> Hero {
    let mut meta = Vec::new();
    let mut synopsis = String::new();
    let mut backdrop = None;
    let mut title = fallback_title.to_owned();
    if let Some(i) = info {
        let d = &i.info;
        if let Some(n) = &d.name {
            title = n.clone();
        }
        if let Some(y) = d.release_date.as_deref().and_then(|r| r.get(..4)) {
            meta.push(y.to_owned());
        }
        if let Some(secs) = d.duration_secs {
            meta.push(text::duration_short(secs as f64));
        }
        if let Some(r) = d.rating.filter(|r| *r > 0.0) {
            meta.push(format!("{} {r:.1}", icons::STAR));
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
    }
    let (tag, clean) = split_tag(&title);
    if let Some(tag) = tag {
        meta.insert(0, tag.to_owned());
    }
    let title = clean.to_owned();
    Hero {
        kicker: String::new(),
        title,
        meta: meta.join(" · "),
        synopsis,
        backdrop,
        action,
    }
}

impl Screen for HomeScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.load(cx);
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Home)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        if cx.playback.progress_epoch() != self.progress_epoch {
            // A progress save finished after we loaded (e.g. leaving the player).
            self.load_progress(cx);
            self.hero_source = None;
        }
        self.progress.poll();
        self.recents.poll();
        self.live.poll();
        if self.movies.poll()
            && let Some(all) = self.movies.value()
        {
            let idx = sorted_indices(all, SortOrder::RecentlyAdded);
            self.recent_movies = idx
                .into_iter()
                .take(20)
                .filter_map(|i| all.get(i).cloned())
                .collect();
        }
        if self.series.poll()
            && let Some(all) = self.series.value()
        {
            let idx = sorted_indices(all, SortOrder::RecentlyAdded);
            self.recent_series = idx
                .into_iter()
                .take(20)
                .filter_map(|i| all.get(i).cloned())
                .collect();
        }
        self.update_hero(cx);
        self.hero.poll();

        let content = cx.layout.content;
        let m = cx.layout.margin;
        let hero_h = (content.height() * 0.52).max(420.0);
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
        let any_content = !progress.is_empty()
            || !live_now.is_empty()
            || !self.recent_movies.is_empty()
            || !self.recent_series.is_empty();
        let loading =
            self.progress.is_loading() || self.live.is_loading() || self.movies.is_loading();

        let images = cx.services.images.clone();
        ScrollArea::vertical()
            .id_salt("home.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let top = ui.cursor().top();
                let band =
                    Rect::from_min_size(pos2(content.left(), top), vec2(content.width(), hero_h));
                self.hero_band(cx, ui, band, &images);
                ui.add_space(-48.0);

                if !progress.is_empty() {
                    let size = cx.layout.backdrop_card_size();
                    let r = Rail::new(Id::unique("home.rail.continue"), "Continue watching")
                        .kicker(format!(
                            "{} title{}",
                            progress.len(),
                            if progress.len() == 1 { "" } else { "s" }
                        ))
                        .margin(m)
                        .show(ui, |ui| {
                            let mut play = None;
                            for (i, p) in progress.iter().enumerate() {
                                let img =
                                    images.get(p.snapshot.poster_url.as_deref(), ImageSize::Card);
                                let meta = match (p.kind, p.snapshot.season, p.snapshot.episode) {
                                    (PlaybackKind::Episode, Some(s), Some(e)) => {
                                        let left = p
                                            .duration_secs
                                            .map(|d| text::remaining(d - p.position_secs))
                                            .unwrap_or_default();
                                        format!("S{s} · E{e} · {left}")
                                    }
                                    _ => p
                                        .duration_secs
                                        .map(|d| text::remaining(d - p.position_secs))
                                        .unwrap_or_default(),
                                };
                                let f = BackdropCard::new(
                                    Id::unique(("home.cw", i)),
                                    &img,
                                    &p.snapshot.title,
                                    size,
                                )
                                .meta(Some(&meta))
                                .progress(p.fraction().map(|f| f as f32))
                                .show(ui);
                                if f.activated {
                                    play = Some(p.clone());
                                }
                            }
                            play
                        });
                    if let Some(p) = r.inner {
                        Self::play_progress(cx, &p);
                    }
                    ui.add_space(16.0);
                }

                if !live_now.is_empty()
                    && let Some(session) = cx.session()
                {
                    let size = cx.layout.channel_tile_size();
                    let channels = Arc::new(live_now.clone());
                    let r = Rail::new(Id::unique("home.rail.live"), "Live now")
                        .kicker(live_kicker)
                        .see_all(true)
                        .margin(m)
                        .show(ui, |ui| {
                            let mut play = None;
                            for (i, ch) in live_now.iter().enumerate() {
                                let logo = images.get(ch.logo.as_deref(), ImageSize::Logo);
                                let (_, now_p, _) = session.now_next(ch, cx.now);
                                let f = ChannelTile::new(
                                    Id::unique(("home.live", &ch.key)),
                                    &logo,
                                    &ch.name,
                                    size,
                                )
                                .now(now_p.as_ref().map(|p| p.title.as_str()))
                                .progress(now_p.as_ref().map(|p| p.progress(cx.now)))
                                .show(ui);
                                if f.activated {
                                    play = Some(i);
                                }
                            }
                            play
                        });
                    if r.see_all {
                        cx.nav.section(Route::Live {
                            category: Some("favourites".into()),
                        });
                    }
                    if let Some(i) = r.inner
                        && let Some(ch) = channels.get(i)
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
                    ui.add_space(16.0);
                }

                let poster_w = cx.layout.poster_width;
                let tv = cx.layout.tv;
                if !self.recent_movies.is_empty() {
                    let movies = self.recent_movies.clone();
                    let r = Rail::new(Id::unique("home.rail.movies"), "Recently added")
                        .kicker(format!("{} titles", movies.len()))
                        .see_all(true)
                        .margin(m)
                        .show(ui, |ui| {
                            let mut open = None;
                            for mv in &movies {
                                let img = images.get(mv.stream_icon.as_deref(), ImageSize::Poster);
                                let meta = movie_meta(mv);
                                let f = PosterCard::new(
                                    Id::unique(("home.movie", mv.stream_id)),
                                    &img,
                                    split_tag(&mv.name).1,
                                    poster_w,
                                )
                                .meta(Some(&meta))
                                .meta_on_focus_only(tv)
                                .show(ui);
                                if f.activated {
                                    open = Some(mv.stream_id);
                                }
                            }
                            open
                        });
                    if r.see_all {
                        cx.nav.section(Route::Movies { category: None });
                    }
                    if let Some(id) = r.inner {
                        cx.nav.push(Route::MovieDetail { stream_id: id });
                    }
                    ui.add_space(16.0);
                }

                if !self.recent_series.is_empty() {
                    let series = self.recent_series.clone();
                    let r = Rail::new(Id::unique("home.rail.series"), "Series")
                        .kicker("Recently updated")
                        .see_all(true)
                        .margin(m)
                        .show(ui, |ui| {
                            let mut open = None;
                            for s in &series {
                                let img = images.get(s.poster(), ImageSize::Poster);
                                let meta = series_meta(s);
                                let f = PosterCard::new(
                                    Id::unique(("home.series", s.series_id)),
                                    &img,
                                    split_tag(&s.name).1,
                                    poster_w,
                                )
                                .meta(Some(&meta))
                                .meta_on_focus_only(tv)
                                .show(ui);
                                if f.activated {
                                    open = Some(s.series_id);
                                }
                            }
                            open
                        });
                    if r.see_all {
                        cx.nav.section(Route::Series { category: None });
                    }
                    if let Some(id) = r.inner {
                        cx.nav.push(Route::SeriesDetail { series_id: id });
                    }
                }

                if !any_content && !loading {
                    let (r, _) =
                        ui.allocate_exact_size(vec2(content.width(), 420.0), Sense::hover());
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
                    let err = self
                        .live
                        .error()
                        .cloned()
                        .unwrap_or_else(|| AppError::msg(""));
                    if self.live.error().is_some() {
                        let f = EmptyState::new(
                            Id::unique("home.error"),
                            Illustration::ErrorOffline,
                            err.title(),
                            &err.user_message(),
                        )
                        .action("Retry", true)
                        .illustration_size(160.0)
                        .show(&mut child);
                        if f.is_some_and(|f| f.activated) {
                            self.load(cx);
                        }
                    }
                }
                ui.add_space(48.0);
            });
    }
}

impl HomeScreen {
    fn hero_band(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        band: Rect,
        images: &crate::services::ImageService,
    ) {
        let painter = ui.painter().clone();
        let m = cx.layout.margin;
        let hero = self.hero.value().cloned();
        // Backdrop (provider image tinted, else the ambient art already painted).
        if let Some(h) = &hero
            && let ImageSlot::Ready(tex) = images.get(h.backdrop.as_deref(), ImageSize::Backdrop)
        {
            let a = crate::ui::motion::animate_bool(
                &cx.egui,
                Id::unique(("home.hero.fade", h.title.as_str())),
                true,
                crate::ui::theme::motion::SLOWER,
            );
            let uv = paint::cover_uv(tex.size_vec2(), band.size(), vec2(0.7, 0.3));
            painter.image(
                tex.id(),
                band,
                uv,
                Color32::from_gray(150).gamma_multiply(a),
            );
        }
        paint::scrim_left(&painter, band);
        paint::scrim_bottom(&painter, band, 0.4);

        let (rect, _) = ui.allocate_exact_size(band.size(), Sense::hover());
        let Some(h) = hero else {
            return;
        };
        // Text block, bottom-aligned 48 pt above the band edge (+ the rail overlap).
        let x = rect.left() + m;
        let text_w = 560.0_f32.min(rect.width() - 2.0 * m);
        let buttons_h = 52.0;
        let syn_g = text::galley(
            &painter,
            &h.synopsis,
            font::body(),
            color::TEXT_2,
            text_w,
            2,
        );
        let title_g = text::galley(
            &painter,
            &h.title,
            font::display(),
            color::TEXT,
            (rect.width() - 2.0 * m).min(900.0),
            2,
        );
        let mut y = rect.bottom() - 48.0 - 48.0 - buttons_h;
        let syn_h = if h.synopsis.is_empty() {
            0.0
        } else {
            syn_g.size().y + 20.0
        };
        let meta_h = if h.meta.is_empty() { 0.0 } else { 26.0 };
        y -= syn_h + meta_h + title_g.size().y + 30.0;
        if !h.kicker.is_empty() {
            text::overline(
                &painter,
                pos2(x, y),
                Align2::LEFT_CENTER,
                &h.kicker,
                color::TEXT_2,
            );
        }
        y += 14.0;
        painter.galley(pos2(x, y), title_g.clone(), color::TEXT);
        y += title_g.size().y + 6.0;
        if !h.meta.is_empty() {
            text::line(
                &painter,
                pos2(x, y),
                Align2::LEFT_TOP,
                &h.meta,
                font::caption(),
                color::TEXT_2,
                text_w,
            );
            y += meta_h;
        }
        if !h.synopsis.is_empty() {
            y += 6.0;
            painter.galley(pos2(x, y), syn_g.clone(), color::TEXT_2);
            y += syn_g.size().y + 14.0;
        }
        y += 10.0;
        let row = Rect::from_min_size(pos2(x, y), vec2(rect.width() - 2.0 * m, buttons_h));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        crate::focus::scope(&mut child, Id::unique("home.hero"), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            match &h.action {
                HeroAction::Resume(p) => {
                    let label = format!("Resume {}", text::timecode(p.position_secs));
                    if Button::primary(label)
                        .icon(icons::PLAY)
                        .large()
                        .autofocus()
                        .id(Id::unique("home.hero.play"))
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
                            .id(Id::unique("home.hero.info"))
                            .show(ui)
                            .activated
                    {
                        cx.nav.push(route);
                    }
                }
                HeroAction::Movie(mv) => {
                    if Button::primary("Play")
                        .icon(icons::PLAY)
                        .large()
                        .autofocus()
                        .id(Id::unique("home.hero.play"))
                        .show(ui)
                        .activated
                    {
                        Self::play_movie(cx, mv);
                    }
                    if Button::secondary("More info")
                        .icon(icons::INFO)
                        .large()
                        .id(Id::unique("home.hero.info"))
                        .show(ui)
                        .activated
                    {
                        cx.nav.push(Route::MovieDetail {
                            stream_id: mv.stream_id,
                        });
                    }
                }
                HeroAction::Welcome => {
                    if Button::primary("Watch Live TV")
                        .icon(icons::TELEVISION_SIMPLE)
                        .large()
                        .autofocus()
                        .id(Id::unique("home.hero.live"))
                        .show(ui)
                        .activated
                    {
                        cx.nav.section(Route::live());
                    }
                    if Button::secondary("Guide")
                        .icon(icons::ROWS)
                        .large()
                        .id(Id::unique("home.hero.guide"))
                        .show(ui)
                        .activated
                    {
                        cx.nav.section(Route::Guide);
                    }
                }
            }
        });
    }
}

fn movie_meta(m: &VodStream) -> String {
    let mut parts = Vec::new();
    if let (Some(tag), _) = split_tag(&m.name) {
        parts.push(tag.to_owned());
    }
    if let Some(y) = m.added.map(|a| a.format("%Y").to_string()) {
        parts.push(y);
    }
    if let Some(r) = m.rating_out_of_5().filter(|r| *r > 0.0) {
        parts.push(format!("{} {:.1}", icons::STAR, r * 2.0));
    }
    parts.join(" · ")
}

fn series_meta(s: &Series) -> String {
    let mut parts = Vec::new();
    if let (Some(tag), _) = split_tag(&s.name) {
        parts.push(tag.to_owned());
    }
    if let Some(y) = s.release_date.as_deref().and_then(|r| r.get(..4)) {
        parts.push(y.to_owned());
    }
    if let Some(r) = s.rating_out_of_5().filter(|r| *r > 0.0) {
        parts.push(format!("{} {:.1}", icons::STAR, r * 2.0));
    }
    parts.join(" · ")
}

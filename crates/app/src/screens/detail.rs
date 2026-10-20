// SPDX-License-Identifier: GPL-3.0-or-later
//! Movie and series detail (§8.4, `assets/concepts/detail.jpg`).
//!
//! - **Header:** the provider backdrop full-bleed (tinted, `scrim-left` + `scrim-bottom`;
//!   the poster or the ambient art when there is none), the title in the display serif,
//!   a meta line (year · runtime · ★ rating · genres) with tag pills (provider tag,
//!   4K/HDR), the plot (4 lines, "More"), cast and director.
//! - **Actions:** Play / **Resume 42:10** + Play from start, ♥ favourite, Trailer (only
//!   with a YouTube id; opens the browser after a confirmation, see [`trailer`]).
//!   Series: **Continue S1·E3** (resume, next episode after a finished one, or the
//!   first episode) + Play from start.
//! - **Series:** season tabs (specials last, gaps skipped) and a virtualised episode
//!   list with stills, progress and watched toggles ([`episodes`]).
//! - **Movies:** "More like this" from the movie's category.
//! - Progress comes from the store and reloads when the player saves
//!   (`progress_epoch`), so coming back from the player shows the new position.

mod episodes;
mod trailer;

use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, Id, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use opensmarters_core::{CategoryId, SeriesInfo, VodInfo, VodStream};
use opensmarters_store::{ContentKind, ItemSnapshot, PlaybackProgress};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::playback::next::EpisodeQueue;
use crate::services::session::SeriesWatch;
use crate::services::vod::{self, ContinueKind, ContinueTarget};
use crate::services::{ImageSize, ImageSlot};
use crate::task::Loader;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::chip::tag;
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{Button, EmptyState, IconButton, PosterCard, Rail};
use crate::ui::{icons, paint, text};
use episodes::{EpisodeAction, Episodes};

/// What the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    /// A movie (`stream_id`).
    Movie(u64),
    /// A series (`series_id`).
    Series(u64),
}

/// Header text and art, built once per loaded info.
#[derive(Debug, Clone, Default)]
struct Header {
    /// Provider name (favourites snapshot, search).
    raw_name: String,
    /// Display title (no tag, no year).
    title: String,
    /// Tag pills: provider tag, 4K/HDR.
    tags: Vec<String>,
    /// "2004 · 1 h 52 · ★ 7.4 · Drama · Mystery".
    meta: String,
    plot: String,
    /// "Cast: … · Directed by …".
    credits: String,
    backdrop: Option<String>,
    poster: Option<String>,
    /// Validated YouTube id.
    trailer: Option<String>,
    category: Option<CategoryId>,
    /// Movie container extension (`movie_data`).
    extension: Option<String>,
    /// OSD subtitle for a movie ("2004 · Drama").
    subtitle: String,
}

fn genres(g: Option<&str>) -> Option<String> {
    let g = text::cap(g?.trim(), 80);
    let parts: Vec<&str> = g
        .split([',', '/', '|'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(3)
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn credits(cast: Option<&str>, director: Option<&str>) -> String {
    let cast = cast
        .map(|c| text::cap(c.trim(), 160).into_owned())
        .filter(|c| !c.is_empty())
        .map(|c| format!("Cast: {c}"));
    let director = director
        .map(|d| text::cap(d.trim(), 80).into_owned())
        .filter(|d| !d.is_empty())
        .map(|d| format!("Directed by {d}"));
    vod::meta_line([cast, director].into_iter().flatten())
}

fn http(url: Option<&String>) -> Option<String> {
    url.filter(|u| {
        let l = u.trim_start().to_ascii_lowercase();
        l.starts_with("http://") || l.starts_with("https://")
    })
    .cloned()
}

fn movie_header(info: &VodInfo) -> Header {
    let d = &info.info;
    let md = info.movie_data.as_ref();
    let raw = d
        .name
        .clone()
        .or_else(|| md.and_then(|m| m.name.clone()))
        .unwrap_or_default();
    let shown = vod::display_title(&raw);
    let year = vod::release_year(d.release_date.as_deref(), &raw).map(|y| y.to_string());
    let duration = d
        .duration_secs
        .or_else(|| d.duration.as_deref().and_then(vod::parse_duration))
        .map(|s| text::duration_short(s as f64));
    let genre = genres(d.genre.as_deref());
    let mut tags: Vec<String> = shown.tag.map(str::to_owned).into_iter().collect();
    tags.extend(vod::quality_tags(&raw).into_iter().map(str::to_owned));
    Header {
        raw_name: raw.clone(),
        title: text::cap(shown.title, 160).into_owned(),
        tags,
        meta: vod::meta_line(
            [
                year.clone(),
                duration,
                vod::rating_label(d.rating),
                genre.clone(),
            ]
            .into_iter()
            .flatten(),
        ),
        plot: d
            .plot
            .as_deref()
            .map(|p| text::cap_paragraph(p.trim(), 1500))
            .unwrap_or_default(),
        credits: credits(d.cast.as_deref(), d.director.as_deref()),
        backdrop: http(d.backdrop_path.first()),
        poster: http(d.movie_image.as_ref()),
        trailer: d.youtube_trailer.as_deref().and_then(vod::youtube_id),
        category: md.and_then(|m| m.category_id.clone()),
        extension: md.and_then(|m| m.container_extension.clone()),
        subtitle: vod::meta_line([year, genre].into_iter().flatten()),
    }
}

use vod::series_name;

fn series_header(info: &SeriesInfo) -> Header {
    let d = &info.info;
    let raw = series_name(info);
    let shown = vod::display_title(&raw);
    let year = vod::release_year(d.release_date.as_deref(), &raw).map(|y| y.to_string());
    let seasons = episodes::season_order(info)
        .into_iter()
        .filter(|s| *s > 0)
        .count();
    let seasons =
        (seasons > 0).then(|| format!("{seasons} season{}", if seasons == 1 { "" } else { "s" }));
    let rating = d.rating.or(d.rating_5based.map(|r| r * 2.0));
    let mut tags: Vec<String> = shown.tag.map(str::to_owned).into_iter().collect();
    tags.extend(vod::quality_tags(&raw).into_iter().map(str::to_owned));
    Header {
        raw_name: raw.clone(),
        title: text::cap(shown.title, 160).into_owned(),
        tags,
        meta: vod::meta_line(
            [
                year,
                seasons,
                vod::rating_label(rating),
                genres(d.genre.as_deref()),
            ]
            .into_iter()
            .flatten(),
        ),
        plot: d
            .plot
            .as_deref()
            .map(|p| text::cap_paragraph(p.trim(), 1500))
            .unwrap_or_default(),
        credits: credits(d.cast.as_deref(), d.director.as_deref()),
        backdrop: http(d.backdrop_path.first()),
        poster: http(d.cover.as_ref()),
        trailer: d.youtube_trailer.as_deref().and_then(vod::youtube_id),
        category: d.category_id.clone(),
        extension: None,
        subtitle: String::new(),
    }
}

/// The detail screen.
#[derive(Debug)]
pub struct DetailScreen {
    /// What to show.
    pub kind: DetailKind,
    movie: Loader<Arc<VodInfo>>,
    series: Loader<Arc<SeriesInfo>>,
    movie_progress: Loader<Option<PlaybackProgress>>,
    watch: Loader<Arc<SeriesWatch>>,
    similar: Loader<Arc<Vec<VodStream>>>,
    progress_epoch: u64,
    header: Option<Header>,
    header_gen: u64,
    season: Option<u32>,
    season_follows_continue: bool,
    plot_expanded: bool,
    trailer: trailer::TrailerPrompt,
    scroll_y: f32,
}

impl DetailScreen {
    fn new(kind: DetailKind) -> Self {
        Self {
            kind,
            movie: Loader::new(),
            series: Loader::new(),
            movie_progress: Loader::new(),
            watch: Loader::new(),
            similar: Loader::new(),
            progress_epoch: 0,
            header: None,
            header_gen: 0,
            season: None,
            season_follows_continue: true,
            plot_expanded: false,
            trailer: trailer::TrailerPrompt::default(),
            scroll_y: 0.0,
        }
    }

    /// A movie page.
    pub fn movie(stream_id: u64) -> Self {
        Self::new(DetailKind::Movie(stream_id))
    }

    /// A series page.
    pub fn series(series_id: u64) -> Self {
        Self::new(DetailKind::Series(series_id))
    }

    fn load_info(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        match self.kind {
            DetailKind::Movie(id) => {
                self.movie.start(cx.rt(), &cx.egui, move |_| async move {
                    session.vod_info(id).await
                });
            }
            DetailKind::Series(id) => {
                self.series.start(cx.rt(), &cx.egui, move |_| async move {
                    session.series_info(id).await
                });
            }
        }
    }

    fn load_progress(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        self.progress_epoch = cx.playback.progress_epoch();
        match self.kind {
            DetailKind::Movie(id) => {
                self.movie_progress
                    .start(cx.rt(), &cx.egui, move |_| async move {
                        session.movie_progress(id).await
                    });
            }
            DetailKind::Series(id) => {
                self.watch.start(cx.rt(), &cx.egui, move |_| async move {
                    session.series_watch(id).await.map(Arc::new)
                });
            }
        }
    }

    fn load_similar(&mut self, cx: &AppCtx<'_>, category: CategoryId) {
        let Some(session) = cx.session() else { return };
        self.similar.start(cx.rt(), &cx.egui, move |_| async move {
            session.vod_streams(Some(category)).await
        });
    }

    fn info_error(&self) -> Option<crate::error::AppError> {
        match self.kind {
            DetailKind::Movie(_) if self.movie.value().is_none() => self.movie.error().cloned(),
            DetailKind::Series(_) if self.series.value().is_none() => self.series.error().cloned(),
            _ => None,
        }
    }

    fn refresh_header(&mut self, cx: &AppCtx<'_>) {
        let (generation, header) = match self.kind {
            DetailKind::Movie(_) => (
                self.movie.generation(),
                self.movie.value().map(|i| movie_header(i)),
            ),
            DetailKind::Series(_) => (
                self.series.generation(),
                self.series.value().map(|i| series_header(i)),
            ),
        };
        if let Some(h) = header
            && (self.header.is_none() || self.header_gen != generation)
        {
            self.header_gen = generation;
            if let (DetailKind::Movie(_), Some(cat)) = (self.kind, h.category.clone())
                && self.similar.is_idle()
            {
                self.load_similar(cx, cat);
            }
            self.header = Some(h);
        }
    }

    /// The movie's container extension: `movie_data`, else its list entry.
    fn movie_extension(&self, id: u64) -> Option<String> {
        self.header
            .as_ref()
            .and_then(|h| h.extension.clone())
            .or_else(|| {
                self.similar
                    .value()
                    .and_then(|l| l.iter().find(|m| m.stream_id == id))
                    .and_then(|m| m.container_extension.clone())
            })
    }

    fn play_movie(&mut self, cx: &mut AppCtx<'_>, id: u64, resume: Option<f64>) {
        let (Some(session), Some(h)) = (cx.session(), self.header.clone()) else {
            return;
        };
        let ext = self.movie_extension(id);
        let subtitle = Some(h.subtitle.clone()).filter(|s| !s.is_empty());
        match session.movie_request(
            id,
            ext.as_deref(),
            &h.title,
            subtitle,
            h.poster.as_deref(),
            resume,
        ) {
            Ok(req) => cx.play(req, true),
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    fn play_episode(&mut self, cx: &mut AppCtx<'_>, episode_id: u64, resume: Option<f64>) {
        let (DetailKind::Series(series_id), Some(session), Some(info)) =
            (self.kind, cx.session(), self.series.value().cloned())
        else {
            return;
        };
        let Some((season, number, ep)) = vod::locate_episode(&info, episode_id) else {
            return;
        };
        let name = series_name(&info);
        let artwork = self.header.as_ref().and_then(|h| h.poster.clone());
        // The episode still suits Continue watching's 16:9 cards better than the poster.
        let still = ep.info.movie_image.clone().or_else(|| artwork.clone());
        match session.series_episode_request(
            series_id,
            &name,
            ep,
            season,
            number,
            still.as_deref(),
            resume,
        ) {
            Ok(req) => {
                cx.playback.set_episode_queue(EpisodeQueue {
                    series_id,
                    series_name: name,
                    artwork: session.safe_url(artwork.as_deref()),
                    info: Arc::clone(&info),
                });
                cx.play(req, true);
            }
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    fn toggle_favourite(&mut self, cx: &mut AppCtx<'_>) {
        let (Some(session), Some(h)) = (cx.session(), self.header.as_ref()) else {
            return;
        };
        let (kind, id) = match self.kind {
            DetailKind::Movie(id) => (ContentKind::Movie, id),
            DetailKind::Series(id) => (ContentKind::Series, id),
        };
        let snapshot = ItemSnapshot {
            name: h.raw_name.clone(),
            icon_url: h.poster.clone(),
            category_id: h.category.as_ref().map(|c| c.as_str().to_owned()),
        };
        let name = text::cap(&h.title, 40).into_owned();
        if session.toggle_favourite(kind, &id.to_string(), snapshot) {
            cx.toasts.success(format!("{name} added to Favourites."));
        } else {
            cx.toasts.info(format!("{name} removed from Favourites."));
        }
    }

    fn continue_target(&self, cx: &AppCtx<'_>) -> Option<ContinueTarget> {
        let info = self.series.value()?;
        let session = cx.session()?;
        let last = self.watch.value().and_then(|w| w.last.as_ref());
        vod::continue_target(info, last, &session.resume_thresholds())
    }

    fn set_watched(&mut self, cx: &mut AppCtx<'_>, episode_id: u64, watched: bool) {
        let (DetailKind::Series(series_id), Some(session), Some(info)) =
            (self.kind, cx.session(), self.series.value().cloned())
        else {
            return;
        };
        let artwork = self.header.as_ref().and_then(|h| h.poster.clone());
        self.watch.start(cx.rt(), &cx.egui, move |_| async move {
            if let Some((season, number, ep)) = vod::locate_episode(&info, episode_id) {
                let name = series_name(&info);
                session
                    .set_episode_watched(
                        series_id,
                        &name,
                        ep,
                        season,
                        number,
                        artwork.as_deref(),
                        watched,
                    )
                    .await?;
            }
            session.series_watch(series_id).await.map(Arc::new)
        });
    }
}

impl Screen for DetailScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        let idle = match self.kind {
            DetailKind::Movie(_) => self.movie.is_idle() || self.movie.error().is_some(),
            DetailKind::Series(_) => self.series.is_idle() || self.series.error().is_some(),
        };
        if idle {
            self.load_info(cx);
        }
        self.load_progress(cx);
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        self.trailer.close()
    }

    fn section(&self) -> Option<Section> {
        Some(match self.kind {
            DetailKind::Movie(_) => Section::Movies,
            DetailKind::Series(_) => Section::Series,
        })
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.movie.poll();
        self.series.poll();
        self.movie_progress.poll();
        self.watch.poll();
        self.similar.poll();
        if cx.playback.progress_epoch() != self.progress_epoch {
            self.load_progress(cx);
        }
        self.refresh_header(cx);

        let content = cx.layout.content;
        let m = cx.layout.margin;
        self.paint_backdrop(cx, ui, content);

        if let Some(err) = self.info_error() {
            let mut child = ui.new_child(UiBuilder::new().max_rect(content.shrink2(vec2(m, 64.0))));
            let r = EmptyState::new(
                Id::unique("detail.error"),
                Illustration::ErrorOffline,
                err.title(),
                &err.user_message(),
            )
            .action("Retry", true)
            .show(&mut child);
            if r.is_some_and(|f| f.activated) {
                self.load_info(cx);
            }
            shell::status_cluster(cx, ui);
            self.trailer.show(cx, &cx.egui.clone());
            return;
        }
        let missing = self
            .header
            .as_ref()
            .is_some_and(|h| h.title.trim().is_empty())
            && self.series.value().is_none_or(|s| s.episodes.is_empty());
        if missing {
            let mut child = ui.new_child(UiBuilder::new().max_rect(content.shrink2(vec2(m, 64.0))));
            let what = match self.kind {
                DetailKind::Movie(_) => "This movie isn't available",
                DetailKind::Series(_) => "This series isn't available",
            };
            if EmptyState::new(
                Id::unique("detail.missing"),
                Illustration::EmptySearch,
                what,
                "Your provider no longer lists it. It may come back later.",
            )
            .action("Back", false)
            .show(&mut child)
            .is_some_and(|f| f.activated)
            {
                cx.nav.back();
            }
            shell::status_cluster(cx, ui);
            return;
        }

        let out = ScrollArea::vertical()
            .id_salt(("detail.scroll", self.kind_id()))
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| self.page(cx, ui));
        self.scroll_y = out.state.clamped_offset().y;
        // Keep the clock legible once the page scrolls under it.
        if self.scroll_y > 1.0 {
            let a = (self.scroll_y / 120.0).clamp(0.0, 1.0);
            paint::v_gradient(
                ui.painter(),
                Rect::from_min_size(content.min, vec2(content.width(), 104.0)),
                &[
                    (0.0, paint::bg_alpha(0.92 * a)),
                    (0.6, paint::bg_alpha(0.6 * a)),
                    (1.0, paint::bg_alpha(0.0)),
                ],
            );
        }
        shell::status_cluster(cx, ui);
        let ctx = cx.egui.clone();
        self.trailer.show(cx, &ctx);
    }
}

impl DetailScreen {
    fn kind_id(&self) -> (u8, u64) {
        match self.kind {
            DetailKind::Movie(id) => (0, id),
            DetailKind::Series(id) => (1, id),
        }
    }

    /// Backdrop (fixed) with tint and scrims; darkens as the page scrolls.
    fn paint_backdrop(&self, cx: &AppCtx<'_>, ui: &Ui, content: Rect) {
        let painter = ui.painter().clone();
        painter.rect_filled(content, 0.0, color::BG);
        let images = &cx.services.images;
        let h = self.header.as_ref();
        let backdrop = h.and_then(|h| h.backdrop.as_deref());
        let mut drawn = false;
        for (url, tint) in [
            (backdrop, 150u8),
            (h.and_then(|h| h.poster.as_deref()), 96u8),
        ] {
            if url.is_none() {
                continue;
            }
            match images.get(url, ImageSize::Backdrop) {
                ImageSlot::Ready(tex) => {
                    let a = crate::ui::motion::animate_bool(
                        &cx.egui,
                        Id::unique(("detail.backdrop", url)),
                        true,
                        crate::ui::theme::motion::SLOWER,
                    );
                    let uv = paint::cover_uv(tex.size_vec2(), content.size(), vec2(0.7, 0.3));
                    painter.image(
                        tex.id(),
                        content,
                        uv,
                        Color32::from_gray(tint).gamma_multiply(a),
                    );
                    drawn = true;
                    break;
                }
                ImageSlot::Loading => {
                    drawn = true; // keep the plain background until it arrives
                    break;
                }
                _ => {}
            }
        }
        if !drawn {
            assets::paint_cover(
                ui,
                &painter,
                Asset::HomeAmbient,
                content,
                vec2(1.0, 0.0),
                0,
                Color32::WHITE,
            );
        }
        paint::scrim_left(&painter, content);
        paint::scrim_bottom(&painter, content, 0.45);
        let dim = (self.scroll_y / 360.0).clamp(0.0, 0.85);
        if dim > 0.0 {
            painter.rect_filled(content, 0.0, paint::bg_alpha(dim));
        }
    }

    /// The scrolling page: back, header, actions, then episodes or "More like this".
    fn page(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let content = cx.layout.content;
        let m = cx.layout.margin;
        let full = ui.available_width();
        let x0 = ui.max_rect().left() + m;
        let text_w = (full - 2.0 * m).min(640.0);
        let painter = ui.painter().clone();

        // Back.
        ui.add_space(34.0);
        let (row, _) = ui.allocate_exact_size(vec2(full, 44.0), Sense::hover());
        let back = Button::ghost("Back")
            .icon(icons::CARET_LEFT)
            .small()
            .id(Id::unique("detail.back"));
        let bs = back.size(ui);
        let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(
            pos2(x0 - 16.0, row.center().y - bs.y / 2.0),
            bs,
        )));
        if back.show(&mut child).activated {
            cx.nav.back();
        }
        let header = self.header.clone();
        let tv = cx.layout.tv;
        let top = (content.height() * if tv { 0.18 } else { 0.26 } - 78.0).max(16.0);
        ui.add_space(top);

        let Some(h) = header else {
            // Loading: skeleton lines where the header will be.
            if self.movie.show_skeleton() || self.series.show_skeleton() {
                let (r, _) = ui.allocate_exact_size(vec2(full, 260.0), Sense::hover());
                let ctx = cx.egui.clone();
                skeleton(
                    &ctx,
                    &painter,
                    Rect::from_min_size(pos2(x0, r.top()), vec2(520.0, 44.0)),
                    radius::SM,
                );
                skeleton(
                    &ctx,
                    &painter,
                    Rect::from_min_size(pos2(x0, r.top() + 64.0), vec2(260.0, 14.0)),
                    radius::XS,
                );
                for i in 0..3 {
                    let w = if i == 2 { 360.0 } else { text_w };
                    skeleton(
                        &ctx,
                        &painter,
                        Rect::from_min_size(
                            pos2(x0, r.top() + 100.0 + i as f32 * 26.0),
                            vec2(w, 14.0),
                        ),
                        radius::XS,
                    );
                }
                skeleton(
                    &ctx,
                    &painter,
                    Rect::from_min_size(pos2(x0, r.top() + 196.0), vec2(180.0, 52.0)),
                    radius::SM,
                );
            }
            return;
        };

        // Title (2 lines max).
        let title_w = (full - 2.0 * m).min(960.0);
        let tg = text::galley(&painter, &h.title, font::display(), color::TEXT, title_w, 2);
        let (r, _) = ui.allocate_exact_size(vec2(full, tg.size().y + 10.0), Sense::hover());
        painter.galley(pos2(x0, r.top()), tg, color::TEXT);

        // Meta line + tags.
        let (r, _) = ui.allocate_exact_size(vec2(full, 26.0), Sense::hover());
        let mut x = x0;
        if !h.meta.is_empty() {
            let mr = text::line(
                &painter,
                pos2(x, r.center().y),
                Align2::LEFT_CENTER,
                &h.meta,
                font::caption(),
                color::TEXT_2,
                text_w,
            );
            x = mr.right() + 12.0;
        }
        for t in &h.tags {
            let tr = tag(&painter, pos2(x, r.center().y - 10.0), t);
            x = tr.right() + 6.0;
        }
        ui.add_space(14.0);

        // Plot.
        if !h.plot.is_empty() {
            let rows = if self.plot_expanded { 14 } else { 4 };
            let pg = text::galley(&painter, &h.plot, font::body(), color::TEXT_2, text_w, rows);
            let elided = pg.elided;
            let (r, _) = ui.allocate_exact_size(vec2(full, pg.size().y), Sense::hover());
            painter.galley(pos2(x0, r.top()), pg, color::TEXT_2);
            if elided || self.plot_expanded {
                let more = Button::ghost(if self.plot_expanded { "Less" } else { "More" })
                    .trailing(if self.plot_expanded {
                        icons::CARET_UP
                    } else {
                        icons::CARET_DOWN
                    })
                    .small()
                    .id(Id::unique("detail.more"));
                let ms = more.size(ui);
                let (r, _) = ui.allocate_exact_size(vec2(full, ms.y + 4.0), Sense::hover());
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(Rect::from_min_size(pos2(x0 - 16.0, r.top() + 4.0), ms)),
                );
                if more.show(&mut child).activated {
                    self.plot_expanded = !self.plot_expanded;
                }
            }
            ui.add_space(12.0);
        }

        // Cast / director.
        if !h.credits.is_empty() {
            let cg = text::galley(
                &painter,
                &h.credits,
                font::caption(),
                color::TEXT_3,
                text_w,
                2,
            );
            let (r, _) = ui.allocate_exact_size(vec2(full, cg.size().y), Sense::hover());
            painter.galley(pos2(x0, r.top()), cg, color::TEXT_3);
        }
        ui.add_space(28.0);

        // Actions.
        let (row, _) = ui.allocate_exact_size(vec2(full, 52.0), Sense::hover());
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(x0, row.top()),
                    pos2(row.right() - m, row.bottom()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        crate::focus::scope(&mut child, Id::unique("detail.actions"), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            self.actions(cx, ui, &h);
        });

        match self.kind {
            DetailKind::Movie(id) => {
                ui.add_space(40.0);
                self.similar_rail(cx, ui, id, h.category.as_ref());
            }
            DetailKind::Series(_) => {
                ui.add_space(44.0);
                self.episodes(cx, ui);
            }
        }
        ui.add_space(48.0);
    }

    fn actions(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, h: &Header) {
        let Some(session) = cx.session() else { return };
        let thresholds = session.resume_thresholds();
        match self.kind {
            DetailKind::Movie(id) => {
                let progress = self.movie_progress.value().cloned().flatten();
                let resume = progress
                    .as_ref()
                    .filter(|p| p.is_resumable(&thresholds))
                    .map(|p| p.position_secs);
                let label = match resume {
                    Some(pos) => format!("Resume {}", text::timecode(pos)),
                    None => "Play".to_owned(),
                };
                if Button::primary(label)
                    .icon(icons::PLAY)
                    .large()
                    .autofocus()
                    .id(Id::unique("detail.play"))
                    .show(ui)
                    .activated
                {
                    self.play_movie(cx, id, resume);
                }
                if resume.is_some()
                    && Button::secondary("Play from start")
                        .icon(icons::ARROW_COUNTER_CLOCKWISE)
                        .large()
                        .id(Id::unique("detail.restart"))
                        .show(ui)
                        .activated
                {
                    self.play_movie(cx, id, None);
                }
            }
            DetailKind::Series(_) => {
                let target = self.continue_target(cx);
                let info = self.series.value().cloned();
                let code = target.and_then(|t| {
                    info.as_ref()
                        .and_then(|i| vod::locate_episode(i, t.episode_id))
                        .map(|(s, n, _)| vod::episode_code(s, n))
                });
                let (label, resume) = match (target, &code) {
                    (Some(t), Some(code)) => match t.kind {
                        ContinueKind::Resume(pos) => (
                            format!("Continue {code} · {}", text::timecode(pos)),
                            Some(pos),
                        ),
                        ContinueKind::Next => (format!("Continue {code}"), None),
                        ContinueKind::Start
                            if self.watch.value().is_some_and(|w| w.last.is_some()) =>
                        {
                            (format!("Continue {code}"), None)
                        }
                        ContinueKind::Start => (format!("Play {code}"), None),
                        ContinueKind::Again => ("Watch again".to_owned(), None),
                    },
                    _ => ("Play".to_owned(), None),
                };
                let enabled = target.is_some();
                if Button::primary(label)
                    .icon(icons::PLAY)
                    .large()
                    .enabled(enabled)
                    .autofocus()
                    .id(Id::unique("detail.play"))
                    .show(ui)
                    .activated
                    && let Some(t) = target
                {
                    self.play_episode(cx, t.episode_id, resume);
                }
                if let (Some(t), Some(_)) = (target, resume)
                    && Button::secondary("Play from start")
                        .icon(icons::ARROW_COUNTER_CLOCKWISE)
                        .large()
                        .id(Id::unique("detail.restart"))
                        .show(ui)
                        .activated
                {
                    self.play_episode(cx, t.episode_id, None);
                }
            }
        }
        if let Some(video) = &h.trailer
            && Button::secondary("Trailer")
                .icon(icons::FILM_SLATE)
                .large()
                .id(Id::unique("detail.trailer-button"))
                .show(ui)
                .activated
        {
            self.trailer.ask(video.clone());
        }
        let (kind, id) = match self.kind {
            DetailKind::Movie(id) => (ContentKind::Movie, id),
            DetailKind::Series(id) => (ContentKind::Series, id),
        };
        let fav = session.is_favourite(kind, &id.to_string());
        ui.add_space(4.0);
        if IconButton::new(icons::HEART)
            .size(52.0, 24.0)
            .fill(fav)
            .selected(fav)
            .id(Id::unique("detail.fav"))
            .show(ui)
            .activated
        {
            self.toggle_favourite(cx);
        }
    }

    fn similar_rail(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        id: u64,
        category: Option<&CategoryId>,
    ) {
        let Some(list) = self.similar.value().cloned() else {
            return;
        };
        let items: Vec<&VodStream> = list.iter().filter(|m| m.stream_id != id).take(20).collect();
        if items.is_empty() {
            return;
        }
        let images = cx.services.images.clone();
        let w = cx.layout.poster_width;
        let tv = cx.layout.tv;
        let r = Rail::new(Id::unique("detail.similar"), "More like this")
            .see_all(category.is_some())
            .margin(cx.layout.margin)
            .show(ui, |ui| {
                let mut open = None;
                for mv in &items {
                    let img = images.get(mv.stream_icon.as_deref(), ImageSize::Poster);
                    let meta = vod::movie_card_meta(mv);
                    let f = PosterCard::new(
                        Id::unique(("detail.similar", mv.stream_id)),
                        &img,
                        vod::display_title(&mv.name).title,
                        w,
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
        if r.see_all
            && let Some(c) = category
        {
            cx.nav.section(Route::Movies {
                category: Some(c.as_str().to_owned()),
            });
        }
        if let Some(id) = r.inner {
            cx.nav.push(Route::MovieDetail { stream_id: id });
        }
    }

    fn episodes(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let (Some(info), Some(session)) = (self.series.value().cloned(), cx.session()) else {
            return;
        };
        let target = self.continue_target(cx);
        // Open on the season of the "Continue" episode (until the user picks one).
        if self.season_follows_continue
            && self.watch.has_finished()
            && let Some(t) = target
            && let Some((s, _, _)) = vod::locate_episode(&info, t.episode_id)
        {
            self.season = Some(s);
            self.season_follows_continue = false;
        }
        let before = self.season;
        let images = cx.services.images.clone();
        let name = series_name(&info);
        let artwork = self.header.as_ref().and_then(|h| h.poster.clone());
        let watch = self.watch.value().cloned();
        let e = Episodes {
            info: &info,
            watch: watch.as_deref(),
            series_name: &name,
            artwork: artwork.as_deref(),
            continue_id: target.map(|t| t.episode_id),
            images: &images,
            margin: cx.layout.margin,
            thresholds: session.resume_thresholds(),
        };
        let action = episodes::show(ui, &e, &mut self.season);
        if before.is_some() && self.season != before {
            self.season_follows_continue = false;
        }
        match action {
            Some(EpisodeAction::Play { id, resume }) => self.play_episode(cx, id, resume),
            Some(EpisodeAction::SetWatched { id, watched }) => self.set_watched(cx, id, watched),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_core::de::parse_object;

    #[test]
    fn movie_header_from_info() {
        let info: VodInfo = parse_object(
            br#"{"info":{"name":"FR | The Saltward Road (2026) 4K","releasedate":"2025-03-01",
                 "duration_secs":"6720","rating":"7.8","genre":"Drama, Mystery",
                 "plot":"A plot.","cast":"Nera Voss, Talven Rill","director":"Selka Dorn",
                 "backdrop_path":["https://img.example/b.jpg"],"youtube_trailer":"dQw4w9WgXcQ",
                 "movie_image":"https://img.example/p.jpg"},
               "movie_data":{"stream_id":5,"category_id":"7","container_extension":"mkv"}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let h = movie_header(&info);
        assert_eq!(h.title, "The Saltward Road (2026) 4K");
        assert_eq!(h.tags, vec!["FR".to_owned(), "4K".to_owned()]);
        assert_eq!(
            h.meta,
            format!("2025 · 1 h 52 · {} 7.8 · Drama · Mystery", icons::STAR)
        );
        assert_eq!(
            h.credits,
            "Cast: Nera Voss, Talven Rill · Directed by Selka Dorn"
        );
        assert_eq!(h.trailer.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(h.extension.as_deref(), Some("mkv"));
        assert_eq!(h.category.as_ref().map(CategoryId::as_str), Some("7"));
        assert_eq!(h.backdrop.as_deref(), Some("https://img.example/b.jpg"));
        assert_eq!(h.subtitle, "2025 · Drama · Mystery");
    }

    #[test]
    fn non_http_art_and_bad_trailers_are_dropped() {
        let info: VodInfo = parse_object(
            br#"{"info":{"name":"X (1999)","backdrop_path":"file:///etc/x","youtube_trailer":"<script>","movie_image":"javascript:1"}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let h = movie_header(&info);
        assert_eq!(h.title, "X");
        assert_eq!(h.meta, "1999");
        assert!(h.backdrop.is_none() && h.poster.is_none() && h.trailer.is_none());
    }
}

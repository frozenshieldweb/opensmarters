// SPDX-License-Identifier: GPL-3.0-or-later
//! The series part of the detail page: season tabs (regular seasons in order, then
//! "Specials"; gaps are simply absent) and the episode list of the selected season.
//!
//! The list lives in the page's vertical scroll area and is **virtualised by
//! visibility**: every row reserves its space, but only rows within a look-ahead of the
//! viewport are built (and request their stills). The look-ahead keeps the next row
//! registered with the focus engine, so Up/Down move row by row and scroll the page.

use eframe::egui::{self, Align2, Id, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use opensmarters_core::SeriesInfo;
use opensmarters_store::ResumeThresholds;

use crate::services::session::SeriesWatch;
use crate::services::vod;
use crate::services::{ImageService, ImageSize};
use crate::ui::text;
use crate::ui::theme::{color, font};
use crate::ui::widgets::episode_row::ROW_HEIGHT;
use crate::ui::widgets::{EpisodeRow, Tabs};

/// Rows built beyond the visible area (points).
const LOOK_AHEAD: f32 = 2.5 * ROW_HEIGHT;
/// Gap between rows.
const ROW_GAP: f32 = 8.0;

/// What the user did in the list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum EpisodeAction {
    /// Play an episode (from `resume` seconds, or the start).
    Play {
        /// Episode id.
        id: u64,
        /// Resume position.
        resume: Option<f64>,
    },
    /// Mark watched (`true`) or unwatched.
    SetWatched {
        /// Episode id.
        id: u64,
        /// New state.
        watched: bool,
    },
    /// p7b: a download control (episode, season, open, retry…).
    Download(crate::dvr::ui::DownloadAction),
}

/// The season keys in tab order: regular seasons ascending, then specials (0).
pub(super) fn season_order(info: &SeriesInfo) -> Vec<u32> {
    let mut v: Vec<u32> = info
        .episodes
        .iter()
        .filter(|(_, eps)| !eps.is_empty())
        .map(|(s, _)| *s)
        .filter(|s| *s > 0)
        .collect();
    if info.episodes.get(&0).is_some_and(|e| !e.is_empty()) {
        v.push(0);
    }
    v
}

/// Inputs of [`show`].
pub(super) struct Episodes<'a> {
    pub info: &'a SeriesInfo,
    pub watch: Option<&'a SeriesWatch>,
    pub series_name: &'a str,
    pub artwork: Option<&'a str>,
    pub continue_id: Option<u64>,
    pub images: &'a ImageService,
    pub margin: f32,
    pub thresholds: ResumeThresholds,
    /// p7b: download state of the episodes (`dvr::ui::episode_downloads`).
    pub downloads: &'a crate::dvr::ui::EpisodeDownloads,
}

/// Season tabs + the episode rows of `season`.
pub(super) fn show(
    ui: &mut Ui,
    e: &Episodes<'_>,
    season: &mut Option<u32>,
) -> Option<EpisodeAction> {
    let order = season_order(e.info);
    if order.is_empty() {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::hover());
        let p = ui.painter();
        p.text(
            pos2(r.left() + e.margin, r.top() + 24.0),
            Align2::LEFT_CENTER,
            "No episodes yet",
            font::title(),
            color::TEXT,
        );
        p.text(
            pos2(r.left() + e.margin, r.top() + 56.0),
            Align2::LEFT_CENTER,
            "Your provider hasn't published any episodes of this series.",
            font::body(),
            color::TEXT_2,
        );
        return None;
    }
    let current = season.filter(|s| order.contains(s)).unwrap_or(order[0]);
    *season = Some(current);
    let full = ui.available_width();
    let x0 = ui.max_rect().left() + e.margin;

    // Tabs (or a heading for a single season).
    let (head, _) = ui.allocate_exact_size(vec2(full, 52.0), Sense::hover());
    if order.len() > 1 {
        let labels: Vec<String> = order.iter().map(|s| vod::season_label(*s)).collect();
        let mut idx = order.iter().position(|s| *s == current).unwrap_or(0);
        let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(
            pos2(x0 - 4.0, head.top() + 4.0),
            pos2(head.right() - e.margin, head.bottom()),
        )));
        let changed = crate::focus::scope(&mut child, Id::unique("detail.seasons"), |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("detail.seasons.scroll")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .content_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    Tabs::new(Id::unique("detail.season-tabs"), &labels).show(ui, &mut idx)
                })
                .inner
        });
        crate::focus::forward_scroll(ui);
        if changed && let Some(s) = order.get(idx) {
            *season = Some(*s);
        }
    } else {
        ui.painter().text(
            pos2(x0, head.center().y),
            Align2::LEFT_CENTER,
            vod::season_label(current),
            font::title(),
            color::TEXT,
        );
    }
    let season_now = season.unwrap_or(current);
    let eps = e
        .info
        .episodes
        .get(&season_now)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let row_w = (full - 2.0 * e.margin + 24.0).min(1240.0);
    let count = format!(
        "{} episode{}",
        eps.len(),
        if eps.len() == 1 { "" } else { "s" }
    );
    ui.painter().text(
        pos2(x0 - 12.0 + row_w - 12.0, head.center().y + 2.0),
        Align2::RIGHT_CENTER,
        count,
        font::caption(),
        color::TEXT_3,
    );
    ui.add_space(12.0);

    let mut action = None;
    // p7b: "Download season", left of the episode count.
    if let Some(a) = crate::dvr::ui::season_button(
        ui,
        e.downloads,
        head,
        x0 - 24.0 + row_w,
        season_now,
        eps.len(),
    ) {
        action = Some(EpisodeAction::Download(a));
    }
    for (i, ep) in eps.iter().enumerate() {
        let (slot, _) = ui.allocate_exact_size(vec2(full, ROW_HEIGHT + ROW_GAP), Sense::hover());
        let rect = Rect::from_min_size(pos2(x0 - 12.0, slot.top()), vec2(row_w, ROW_HEIGHT));
        let (rect, download_rect) = crate::dvr::ui::split_episode_row(e.downloads, rect); // p7b
        if !ui.is_rect_visible(rect.expand2(vec2(0.0, LOOK_AHEAD))) {
            continue;
        }
        let number = vod::episode_number(ep, i);
        let title = format!(
            "{number} · {}",
            vod::episode_title(&ep.title, e.series_name, number)
        );
        let progress = e.watch.and_then(|w| w.get(ep.id));
        let watched = e.watch.is_some_and(|w| w.watched(ep.id));
        let resumable = progress.filter(|p| p.is_resumable(&e.thresholds));
        let duration = ep
            .info
            .duration_secs
            .or_else(|| ep.info.duration.as_deref().and_then(vod::parse_duration));
        let left = resumable.and_then(|p| {
            p.duration_secs
                .or(duration.map(|d| d as f64))
                .map(|d| text::remaining(d - p.position_secs))
        });
        let meta = vod::meta_line(
            [
                duration.map(|d| text::duration_short(d as f64)),
                ep.info
                    .release_date
                    .as_deref()
                    .map(|d| text::cap(d.trim(), 16).into_owned()),
                vod::rating_label(ep.info.rating),
                left,
            ]
            .into_iter()
            .flatten(),
        );
        let synopsis = ep
            .info
            .plot
            .as_deref()
            .map(|p| text::cap_paragraph(p, 600))
            .unwrap_or_default();
        let still = ep.info.movie_image.as_deref().or(e.artwork);
        let img = e.images.get(still, ImageSize::Card);
        let frac = if watched && resumable.is_none() {
            Some(1.0)
        } else {
            progress.and_then(|p| p.fraction()).map(|f| f as f32)
        };
        let r = EpisodeRow::new(Id::unique(("detail.ep", ep.id)), &img, &title)
            .meta(&meta)
            .synopsis(&synopsis)
            .progress(frac)
            .watched(watched)
            .highlight(e.continue_id == Some(ep.id))
            .show_at(ui, rect);
        if r.play {
            action = Some(EpisodeAction::Play {
                id: ep.id,
                resume: resumable.map(|p| p.position_secs),
            });
        }
        if r.toggle_watched {
            action = Some(EpisodeAction::SetWatched {
                id: ep.id,
                watched: !watched,
            });
        }
        // p7b: the episode's download control.
        if let Some(a) = crate::dvr::ui::episode_button(ui, e.downloads, ep.id, download_rect) {
            action = Some(EpisodeAction::Download(a));
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_core::Episode;

    fn ep(id: u64) -> Episode {
        Episode {
            id,
            episode_num: None,
            title: String::new(),
            container_extension: None,
            season: None,
            added: None,
            direct_source: None,
            info: Default::default(),
        }
    }

    #[test]
    fn specials_come_last_and_gaps_are_skipped() {
        let mut info = SeriesInfo::default();
        info.episodes.insert(0, vec![ep(1)]);
        info.episodes.insert(1, vec![ep(2)]);
        info.episodes.insert(3, vec![ep(3)]);
        info.episodes.insert(4, Vec::new());
        assert_eq!(season_order(&info), vec![1, 3, 0]);
        assert!(season_order(&SeriesInfo::default()).is_empty());
    }
}

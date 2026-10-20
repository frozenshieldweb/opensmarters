// SPDX-License-Identifier: GPL-3.0-or-later
//! Movie / series detail (stub). Phase 4 replaces this file with the full detail page
//! (§8.4). Until then it shows the title and lets a movie be played, so Home's cards lead
//! somewhere useful.

use std::sync::Arc;

use eframe::egui::{self, Align2, Id, pos2};
use opensmarters_core::{SeriesInfo, VodInfo};

use super::{AppCtx, Screen, shell};
use crate::nav::Section;
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{Button, EmptyState};
use crate::ui::{icons, text};

/// What the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    /// A movie (`stream_id`).
    Movie(u64),
    /// A series (`series_id`).
    Series(u64),
}

/// Detail screen (placeholder).
#[derive(Debug)]
pub struct DetailScreen {
    /// What to show.
    pub kind: DetailKind,
    movie: Loader<Arc<VodInfo>>,
    series: Loader<Arc<SeriesInfo>>,
}

impl DetailScreen {
    /// A movie page.
    pub fn movie(stream_id: u64) -> Self {
        Self {
            kind: DetailKind::Movie(stream_id),
            movie: Loader::new(),
            series: Loader::new(),
        }
    }

    /// A series page.
    pub fn series(series_id: u64) -> Self {
        Self {
            kind: DetailKind::Series(series_id),
            movie: Loader::new(),
            series: Loader::new(),
        }
    }
}

impl Screen for DetailScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        match self.kind {
            DetailKind::Movie(id) if self.movie.is_idle() => {
                self.movie.start(cx.rt(), &cx.egui, move |_| async move {
                    session.vod_info(id).await
                });
            }
            DetailKind::Series(id) if self.series.is_idle() => {
                self.series.start(cx.rt(), &cx.egui, move |_| async move {
                    session.series_info(id).await
                });
            }
            _ => {}
        }
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        self.movie.poll();
        self.series.poll();
        shell::status_cluster(cx, ui);
        let title = match self.kind {
            DetailKind::Movie(_) => self.movie.value().and_then(|v| v.info.name.clone()),
            DetailKind::Series(_) => self.series.value().and_then(|v| v.info.name.clone()),
        }
        .unwrap_or_default();
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter().clone();
        let m = cx.layout.margin;
        painter.text(
            pos2(rect.left() + m, rect.top() + 56.0),
            Align2::LEFT_CENTER,
            "Details",
            font::heading(),
            color::TEXT,
        );
        if !title.is_empty() {
            text::line(
                &painter,
                pos2(rect.left() + m, rect.top() + 120.0),
                Align2::LEFT_TOP,
                &title,
                font::display(),
                color::TEXT,
                rect.width() - 2.0 * m,
            );
        }
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
            pos2(rect.left() + m, rect.top() + 190.0),
            pos2(rect.right() - m, rect.top() + 260.0),
        )));
        child.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if let (DetailKind::Movie(id), Some(info)) = (self.kind, self.movie.value())
                && Button::primary("Play")
                    .icon(icons::PLAY)
                    .large()
                    .autofocus()
                    .show(ui)
                    .activated
                && let Some(session) = cx.session()
            {
                {
                    let ext = info
                        .movie_data
                        .as_ref()
                        .and_then(|m| m.container_extension.clone());
                    let name = info.info.name.clone().unwrap_or_default();
                    match session.movie_request(
                        id,
                        ext.as_deref(),
                        &name,
                        None,
                        info.info.movie_image.as_deref(),
                        None,
                    ) {
                        Ok(req) => cx.play(req, true),
                        Err(e) => cx.toasts.error(e.user_message()),
                    }
                }
            }
            if Button::secondary("Back")
                .icon(icons::CARET_LEFT)
                .large()
                .show(ui)
                .activated
            {
                cx.nav.back();
            }
        });
        let mut rest = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
            pos2(rect.left(), rect.top() + 280.0),
            rect.max,
        )));
        let _ = EmptyState::new(
            Id::unique("detail.soon"),
            Illustration::EmptyGuide,
            "The full detail page is on its way",
            "Synopsis, cast, seasons and episodes arrive in the next phase.",
        )
        .illustration_size(140.0)
        .show(&mut rest);
    }

    fn section(&self) -> Option<Section> {
        Some(match self.kind {
            DetailKind::Movie(_) => Section::Movies,
            DetailKind::Series(_) => Section::Series,
        })
    }
}

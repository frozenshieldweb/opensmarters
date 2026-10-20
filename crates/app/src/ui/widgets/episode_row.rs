// SPDX-License-Identifier: GPL-3.0-or-later
//! An episode row (§8.4 series detail): a 200×112 still with the watch progress, the
//! number and title (`body-strong`), a caption meta line, a two-line synopsis, and a
//! "Mark watched" / "Watched" toggle on the right.
//!
//! Two focus targets: the row itself (Enter plays) and the toggle (Right from the row).
//! Rows don't scale on focus; they brighten to `surface-3` with the ring (§5).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Rect, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::focus::{self, FocusOpts};
use crate::services::ImageSlot;
use crate::ui::assets::{self, Asset};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::Button;
use crate::ui::{paint, text};

/// Height of a row (still + padding).
pub const ROW_HEIGHT: f32 = 136.0;
/// Still size.
pub const THUMB: egui::Vec2 = egui::Vec2::new(200.0, 112.0);

/// What a row reports.
#[derive(Debug, Clone, Copy, Default)]
pub struct EpisodeRowResponse {
    /// Enter/click on the row: play.
    pub play: bool,
    /// The watched toggle was activated.
    pub toggle_watched: bool,
    /// The row has the focus.
    pub focused: bool,
}

/// An episode row. See the [module docs](self).
#[derive(Debug)]
pub struct EpisodeRow<'a> {
    id: Id,
    image: &'a ImageSlot,
    title: &'a str,
    meta: &'a str,
    synopsis: &'a str,
    progress: Option<f32>,
    watched: bool,
    autofocus: bool,
    highlight: bool,
}

impl<'a> EpisodeRow<'a> {
    /// A row with a stable id (derive it from the episode id).
    pub fn new(id: Id, image: &'a ImageSlot, title: &'a str) -> Self {
        Self {
            id,
            image,
            title,
            meta: "",
            synopsis: "",
            progress: None,
            watched: false,
            autofocus: false,
            highlight: false,
        }
    }

    /// Caption line ("45 min · 2021-03-04").
    #[must_use]
    pub fn meta(mut self, m: &'a str) -> Self {
        self.meta = m;
        self
    }

    /// Synopsis (two lines).
    #[must_use]
    pub fn synopsis(mut self, s: &'a str) -> Self {
        self.synopsis = s;
        self
    }

    /// Watch progress 0..1.
    #[must_use]
    pub fn progress(mut self, p: Option<f32>) -> Self {
        self.progress = p;
        self
    }

    /// Watched to the end.
    #[must_use]
    pub fn watched(mut self, on: bool) -> Self {
        self.watched = on;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Marks the "continue" episode (accent bar on the left).
    #[must_use]
    pub fn highlight(mut self, on: bool) -> Self {
        self.highlight = on;
        self
    }

    /// The row's focus id.
    pub fn row_id(id: Id) -> Id {
        id.with("row")
    }

    /// Paints the row at `rect` (full width, [`ROW_HEIGHT`] tall).
    pub fn show_at(self, ui: &mut Ui, rect: Rect) -> EpisodeRowResponse {
        let toggle_label = if self.watched {
            "Watched"
        } else {
            "Mark watched"
        };
        let toggle = Button::ghost(toggle_label)
            .icon(if self.watched {
                icons::CHECK_CIRCLE
            } else {
                icons::CHECK
            })
            .small()
            .id(self.id.with("watched"));
        let tsize = toggle.size(ui);
        let toggle_rect = Rect::from_min_size(
            pos2(
                rect.right() - 12.0 - tsize.x,
                rect.center().y - tsize.y / 2.0,
            ),
            tsize,
        );
        let row_rect = Rect::from_min_max(rect.min, pos2(toggle_rect.left() - 16.0, rect.max.y));
        let (_, f) = focus::focusable(
            ui,
            Self::row_id(self.id),
            row_rect,
            FocusOpts::default()
                .autofocus_if(self.autofocus)
                .scroll_margin2(vec2(16.0, 48.0)),
        );
        let painter = ui.painter().clone();
        if f.focused {
            painter.rect_filled(row_rect, CornerRadius::same(radius::MD), color::SURFACE_3);
        }
        if self.highlight {
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(row_rect.left(), row_rect.top() + 16.0),
                    vec2(3.0, row_rect.height() - 32.0),
                ),
                CornerRadius::same(2),
                color::ACCENT,
            );
        }
        // Still.
        let thumb = Rect::from_min_size(
            pos2(row_rect.left() + 12.0, rect.center().y - THUMB.y / 2.0),
            THUMB,
        );
        match self.image {
            ImageSlot::Ready(tex) => {
                let uv = paint::cover_uv(tex.size_vec2(), thumb.size(), vec2(0.5, 0.4));
                paint::image_rounded(&painter, tex.id(), thumb, uv, radius::SM, Color32::WHITE);
            }
            ImageSlot::Loading => {
                crate::ui::widgets::feedback::skeleton(ui.ctx(), &painter, thumb, radius::SM);
            }
            _ => {
                if !assets::paint_cover(
                    ui,
                    &painter,
                    Asset::BackdropPlaceholder,
                    thumb,
                    vec2(0.5, 0.5),
                    radius::SM,
                    Color32::WHITE,
                ) {
                    painter.rect_filled(thumb, CornerRadius::same(radius::SM), color::SURFACE_2);
                }
            }
        }
        painter.rect_stroke(
            thumb,
            CornerRadius::same(radius::SM),
            Stroke::new(1.0, color::STROKE_SUBTLE),
            StrokeKind::Inside,
        );
        if let Some(p) = self.progress.filter(|p| *p > 0.0) {
            let bar = Rect::from_min_size(
                pos2(thumb.left() + 8.0, thumb.bottom() - 10.0),
                vec2(thumb.width() - 16.0, 4.0),
            );
            paint::v_gradient(
                &painter,
                Rect::from_min_max(pos2(thumb.left(), thumb.bottom() - 28.0), thumb.max)
                    .shrink2(vec2(0.5, 0.0)),
                &[
                    (0.0, Color32::from_black_alpha(0)),
                    (1.0, Color32::from_black_alpha(160)),
                ],
            );
            paint::progress_bar(&painter, bar, p, color::ACCENT, color::PROGRESS_TRACK);
        }
        if self.watched {
            let c = pos2(thumb.right() - 16.0, thumb.top() + 16.0);
            painter.circle_filled(c, 12.0, Color32::from_black_alpha(170));
            icons::paint(
                &painter,
                c,
                icons::CHECK_CIRCLE,
                20.0,
                Style::Fill,
                color::SAGE,
            );
        }
        // Text column.
        let tx = thumb.right() + 20.0;
        let tw = (row_rect.right() - 16.0 - tx).max(60.0);
        let fg = if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        let mut y = thumb.top() + 4.0;
        let tr = text::line(
            &painter,
            pos2(tx, y),
            Align2::LEFT_TOP,
            self.title,
            font::body_strong(),
            color::TEXT,
            tw,
        );
        y = tr.bottom() + 4.0;
        if !self.meta.is_empty() {
            let mr = text::line(
                &painter,
                pos2(tx, y),
                Align2::LEFT_TOP,
                self.meta,
                font::caption(),
                color::TEXT_3,
                tw,
            );
            y = mr.bottom() + 8.0;
        }
        if !self.synopsis.is_empty() {
            text::block(
                &painter,
                pos2(tx, y),
                self.synopsis,
                font::body(),
                fg,
                tw,
                2,
            );
        }
        if f.focused {
            paint::focus_ring(&painter, row_rect, radius::MD, 1.0);
        }
        // Watched toggle.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(toggle_rect));
        let tf = toggle.show(&mut child);
        EpisodeRowResponse {
            play: f.activated,
            toggle_watched: tf.activated,
            focused: f.focused,
        }
    }
}

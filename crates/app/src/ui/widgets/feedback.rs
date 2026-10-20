// SPDX-License-Identifier: GPL-3.0-or-later
//! Loading and empty states: skeletons with a shared shimmer (§6.16), empty/error states
//! with illustrations (§6.13), the "coming soon" placeholder used by stub screens.

use eframe::egui::{
    self, Align2, Color32, Context, CornerRadius, Id, Painter, Rect, Sense, Ui, pos2, vec2,
};

use crate::focus::Focus;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::theme::{color, font};
use crate::ui::widgets::Button;
use crate::ui::{motion, paint, text};

/// Paints a skeleton block with the shared shimmer (static with reduced motion).
pub fn skeleton(ctx: &Context, painter: &Painter, rect: Rect, radius: u8) {
    painter.rect_filled(rect, CornerRadius::same(radius), color::SURFACE_2);
    if motion::reduced(ctx) {
        return;
    }
    let screen = ctx.content_rect();
    let t = (ctx.input(|i| i.time) % 1.4) / 1.4;
    let band_w = 120.0;
    let x = screen.left() - band_w + (screen.width() + band_w * 2.0) * t as f32;
    let band = Rect::from_min_max(pos2(x, rect.top()), pos2(x + band_w, rect.bottom()));
    let clip = band.intersect(rect);
    if clip.is_positive() {
        let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        let c = Color32::from_white_alpha(13);
        paint::h_gradient(
            &p,
            band,
            &[
                (0.0, Color32::TRANSPARENT),
                (0.5, c),
                (1.0, Color32::TRANSPARENT),
            ],
        );
    }
    ctx.request_repaint();
}

/// A skeleton text line (`width` × 12 pt, rounded).
pub fn skeleton_line(ctx: &Context, painter: &Painter, pos: egui::Pos2, width: f32) {
    skeleton(ctx, painter, Rect::from_min_size(pos, vec2(width, 12.0)), 4);
}

/// The card shape a [`skeleton_rail`] repeats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SkeletonCard {
    /// A 2:3 poster `width` wide, with the title and meta lines below it.
    Poster(f32),
    /// A 16:9 backdrop card or channel tile of this size (text inside).
    Tile(egui::Vec2),
}

impl SkeletonCard {
    /// Height of the card including any text below it (matches `PosterCard::height`).
    pub fn height(self) -> f32 {
        match self {
            SkeletonCard::Poster(w) => (w * 1.5).round() + 10.0 + 22.0 + 20.0,
            SkeletonCard::Tile(s) => s.y,
        }
    }

    fn width(self) -> f32 {
        match self {
            SkeletonCard::Poster(w) => w,
            SkeletonCard::Tile(s) => s.x,
        }
    }
}

/// Height of a content rail (§6.2, `widgets::Rail`) of `card`: the 40 pt heading row,
/// 12 pt to the cards, 16 pt of paint allowance below them.
pub fn rail_height(card: SkeletonCard) -> f32 {
    40.0 + 12.0 + card.height() + 16.0
}

/// A loading content rail with exactly the real rail's geometry (§6.16): a heading bar
/// and as many card skeletons as fit `slot` (first card at `margin`, 16 pt gaps; the last
/// one is cut like the real rail's peek). `slot` is the rail's full rect
/// ([`rail_height`] tall).
pub fn skeleton_rail(
    ctx: &Context,
    painter: &Painter,
    slot: Rect,
    margin: f32,
    card: SkeletonCard,
    heading_width: f32,
) {
    let head_y = slot.top() + 20.0;
    skeleton(
        ctx,
        painter,
        Rect::from_min_size(
            pos2(slot.left() + margin, head_y - 9.0),
            vec2(heading_width, 18.0),
        ),
        4,
    );
    let p = painter.with_clip_rect(painter.clip_rect().intersect(slot));
    let top = slot.top() + 52.0;
    let w = card.width();
    let mut x = slot.left() + margin;
    while x < slot.right() {
        match card {
            SkeletonCard::Poster(_) => {
                let img = Rect::from_min_size(pos2(x, top), vec2(w, (w * 1.5).round()));
                skeleton(ctx, &p, img, 12);
                skeleton(
                    ctx,
                    &p,
                    Rect::from_min_size(pos2(x, img.bottom() + 14.0), vec2(w * 0.78, 12.0)),
                    4,
                );
                skeleton(
                    ctx,
                    &p,
                    Rect::from_min_size(pos2(x, img.bottom() + 36.0), vec2(w * 0.42, 10.0)),
                    4,
                );
            }
            SkeletonCard::Tile(s) => {
                skeleton(ctx, &p, Rect::from_min_size(pos2(x, top), s), 12);
            }
        }
        x += w + 16.0;
    }
}

/// An empty/error state: illustration, Fraunces headline, body, one action.
#[derive(Debug)]
pub struct EmptyState<'a> {
    illustration: Illustration,
    headline: &'a str,
    body: &'a str,
    action: Option<(&'a str, bool)>,
    id: Id,
    size: f32,
}

impl<'a> EmptyState<'a> {
    /// An empty state.
    pub fn new(id: Id, illustration: Illustration, headline: &'a str, body: &'a str) -> Self {
        Self {
            illustration,
            headline,
            body,
            action: None,
            id,
            size: 200.0,
        }
    }

    /// One action button (`primary` = accent fill).
    #[must_use]
    pub fn action(mut self, label: &'a str, primary: bool) -> Self {
        self.action = Some((label, primary));
        self
    }

    /// Illustration size (200 desktop).
    #[must_use]
    pub fn illustration_size(mut self, s: f32) -> Self {
        self.size = s;
        self
    }

    /// Adds the state centred in the available rect. Returns the action's focus.
    pub fn show(self, ui: &mut Ui) -> Option<Focus> {
        let avail = ui.available_rect_before_wrap();
        let body_g = {
            let mut job = text::job(self.body, font::body(), color::TEXT_2, 480.0, 2);
            job.halign = egui::Align::Center;
            ui.painter().layout_job(job)
        };
        let total = self.size
            + 24.0
            + 34.0
            + 8.0
            + body_g.size().y
            + if self.action.is_some() {
                24.0 + 44.0
            } else {
                0.0
            };
        let top = avail.top() + ((avail.height() - total) / 2.0).max(16.0);
        let cx = avail.center().x;
        let painter = ui.painter().clone();
        let ill =
            Rect::from_center_size(pos2(cx, top + self.size / 2.0), vec2(self.size, self.size));
        assets::paint_contain(
            ui,
            &painter,
            Asset::Illustration(self.illustration),
            ill,
            Color32::WHITE,
        );
        let hy = ill.bottom() + 24.0;
        text::line(
            &painter,
            pos2(cx, hy),
            Align2::CENTER_TOP,
            self.headline,
            crate::ui::theme::font::display_sized(28.0),
            color::TEXT,
            560.0,
        );
        let by = hy + 34.0 + 8.0;
        painter.galley(pos2(cx, by), body_g.clone(), color::TEXT_2);
        let mut out = None;
        if let Some((label, primary)) = self.action {
            let b = if primary {
                Button::primary(label)
            } else {
                Button::secondary(label)
            };
            let b = b.id(self.id.with("action")).autofocus();
            let size = b.size(ui);
            let rect =
                Rect::from_min_size(pos2(cx - size.x / 2.0, by + body_g.size().y + 24.0), size);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            out = Some(b.show(&mut child));
        }
        ui.allocate_rect(avail, Sense::hover());
        out
    }
}

/// The placeholder for screens built in a later phase: heading, explanation, and a
/// "Back to Home" button. Returns true when that button is activated.
pub fn coming_soon(
    ui: &mut Ui,
    id: Id,
    title: &str,
    headline: &str,
    body: &str,
    illustration: Option<Illustration>,
    margin: f32,
) -> bool {
    let avail = ui.available_rect_before_wrap();
    let painter = ui.painter().clone();
    painter.text(
        pos2(avail.left() + margin, avail.top() + 56.0),
        Align2::LEFT_CENTER,
        title,
        font::heading(),
        color::TEXT,
    );
    let mut inner = avail;
    inner.min.y += 96.0;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    let state = match illustration {
        Some(ill) => EmptyState::new(id, ill, headline, body),
        None => EmptyState::new(id, Illustration::EmptyGuide, headline, body),
    };
    let f = state.action("Back to Home", false).show(&mut child);
    if illustration.is_none() {
        // Brand symbol instead of an illustration (painted over the slot).
    }
    ui.allocate_rect(avail, Sense::hover());
    f.is_some_and(|f| f.activated)
}

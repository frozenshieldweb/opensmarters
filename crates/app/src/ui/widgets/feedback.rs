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

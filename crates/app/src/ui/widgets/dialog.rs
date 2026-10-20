// SPDX-License-Identifier: GPL-3.0-or-later
//! Modal dialogs (§6.11): scrim backdrop, `surface-1` panel, focus trapped inside.
//!
//! The owning screen keeps the "open" state and closes it in `Screen::on_back` (Esc =
//! cancel). Actions are right-aligned and laid out **right to left**: add the primary
//! action first (it ends up rightmost). Put the initial focus on the *safe* action with
//! `.autofocus()`.
//!
//! ```ignore
//! if self.confirm_delete {
//!     Dialog::new(Id::unique("profiles.delete"), "Delete profile?")
//!         .body("Its favourites and history are deleted too.")
//!         .show(ui.ctx(), |ui| {
//!             if Button::danger("Delete").show(ui).activated { … }
//!             if Button::secondary("Cancel").autofocus().show(ui).activated { self.confirm_delete = false; }
//!         });
//! }
//! ```

use eframe::egui::{
    self, Align, Area, Context, CornerRadius, Frame, Id, Layout, Margin, Order, Sense, Ui, vec2,
};

use crate::focus;
use crate::ui::text;
use crate::ui::theme::{color, elevation, font, radius};

/// A modal dialog.
#[derive(Debug)]
pub struct Dialog<'a> {
    id: Id,
    title: &'a str,
    body: Option<&'a str>,
    width: f32,
}

impl<'a> Dialog<'a> {
    /// A dialog with an id (its focus scope) and a title.
    pub fn new(id: Id, title: &'a str) -> Self {
        Self {
            id,
            title,
            body: None,
            width: 520.0,
        }
    }

    /// Body text (`text-2`).
    #[must_use]
    pub fn body(mut self, b: &'a str) -> Self {
        self.body = Some(b);
        self
    }

    /// Width (480–560 recommended; wider for content like the PIN pad).
    #[must_use]
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }

    /// Shows the dialog. `actions` adds the buttons right to left (primary first, so it
    /// is rightmost). For other layouts use [`Dialog::show_custom`].
    pub fn show<R>(self, ctx: &Context, actions: impl FnOnce(&mut Ui) -> R) -> R {
        let body = self.body.map(str::to_owned);
        let title = self.title.to_owned();
        self.show_custom(ctx, move |ui| {
            ui.label(
                egui::RichText::new(text::cap(&title, 120))
                    .font(font::title())
                    .color(color::TEXT),
            );
            if let Some(b) = &body {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(text::cap(b, 800))
                        .font(font::body())
                        .color(color::TEXT_2),
                );
            }
            ui.add_space(20.0);
            let w = ui.available_width();
            ui.allocate_ui_with_layout(vec2(w, 44.0), Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                actions(ui)
            })
            .inner
        })
    }

    /// Shows the dialog with fully custom content.
    pub fn show_custom<R>(self, ctx: &Context, content: impl FnOnce(&mut Ui) -> R) -> R {
        focus::trap(ctx, self.id);
        let screen = ctx.content_rect();
        Area::new(self.id.with("area"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.set_min_size(screen.size());
                // Scrim that also swallows clicks to the page below.
                let _ = ui.allocate_rect(screen, Sense::click());
                ui.painter().rect_filled(screen, 0.0, color::SCRIM);
                let panel_w = self.width.min(screen.width() - 48.0);
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_center_size(
                            screen.center(),
                            vec2(panel_w, screen.height() - 48.0),
                        ))
                        .layout(Layout::top_down(Align::Min)),
                );
                focus::scope(&mut child, self.id, |ui| {
                    // Centre vertically with a sizing estimate kept between frames.
                    let h_id = self.id.with("h");
                    let last_h: f32 = ui.ctx().data(|d| d.get_temp(h_id)).unwrap_or(200.0);
                    ui.add_space(((ui.available_height() - last_h) / 2.0).max(0.0));
                    let r = Frame::new()
                        .fill(color::SURFACE_1)
                        .corner_radius(CornerRadius::same(radius::LG))
                        .shadow(elevation::E3)
                        .stroke(egui::Stroke::new(1.0, color::STROKE_SUBTLE))
                        .inner_margin(Margin::same(32))
                        .show(ui, |ui| {
                            ui.set_width(panel_w - 64.0);
                            content(ui)
                        });
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(h_id, r.response.rect.height()));
                    r.inner
                })
            })
            .inner
    }
}

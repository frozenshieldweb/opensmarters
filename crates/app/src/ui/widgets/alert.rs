// SPDX-License-Identifier: GPL-3.0-or-later
//! The app-wide prompt card ([`crate::services::alerts`]): a small dialog-styled panel at
//! the bottom right (`surface-1`, r-lg, `e3`, hairline edge) with an icon, a title, a body,
//! an optional countdown bar and 1–3 buttons (primary rightmost). It fades and slides in
//! (instant with reduced motion). The focus handling (trap, keys) belongs to the alerts
//! service; this widget only draws and reports the chosen action.

use eframe::egui::{
    self, Align, Align2, Area, Context, CornerRadius, Frame, Id, Layout, Margin, Order, Pos2,
    Sense, vec2,
};

use crate::focus;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::Button;
use crate::ui::{motion, paint, text};

/// One button of the card.
#[derive(Debug, Clone, Copy)]
pub struct CardAction<'a> {
    /// Focus id.
    pub id: Id,
    /// Label.
    pub label: &'a str,
    /// Primary (champagne) button.
    pub primary: bool,
}

/// The card. See the [module docs](self).
#[derive(Debug)]
pub struct AlertCard<'a> {
    /// Area id and focus scope of the buttons.
    pub id: Id,
    /// Phosphor glyph.
    pub icon: &'static str,
    /// Title.
    pub title: &'a str,
    /// Body (may be empty).
    pub body: &'a str,
    /// Buttons, left to right.
    pub actions: &'a [CardAction<'a>],
    /// Countdown: fraction left (1 → 0), its label ("Stopping in") and the time left
    /// ("0:23").
    pub countdown: Option<(f32, &'a str, String)>,
    /// Prompts waiting behind this one ("+2 more").
    pub more: usize,
    /// Enter/Space arrived for the card (activates the focused button).
    pub activate: bool,
}

const PAD: f32 = 20.0;
const ICON_D: f32 = 40.0;

impl AlertCard<'_> {
    /// Draws the card with its bottom-right corner at `anchor`, `width` wide. Returns the
    /// index of the action chosen this frame.
    pub fn show(self, ctx: &Context, anchor: Pos2, width: f32) -> Option<usize> {
        // Fade + 16 pt slide in, keyed by the card id (restarts if it wasn't drawn lately).
        let now = ctx.input(|i| i.time);
        let born_id = self.id.with("born");
        let (born, last) = ctx
            .data(|d| d.get_temp::<(f64, f64)>(born_id))
            .filter(|(_, last)| now - last < 0.5)
            .unwrap_or((now, now));
        let _ = last;
        ctx.data_mut(|d| d.insert_temp(born_id, (born, now)));
        let t = if motion::reduced(ctx) {
            1.0
        } else {
            let x = ((now - born) as f32 / crate::ui::theme::motion::BASE).clamp(0.0, 1.0);
            egui::emath::easing::cubic_out(x)
        };
        if t < 1.0 {
            ctx.request_repaint();
        }

        let mut chosen = None;
        let area = Area::new(self.id.with("area"))
            .order(Order::Foreground)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(anchor + vec2(0.0, (1.0 - t) * 16.0))
            .show(ctx, |ui| {
                ui.multiply_opacity(t);
                Frame::new()
                    .fill(color::SURFACE_1)
                    .corner_radius(CornerRadius::same(radius::LG))
                    .shadow(elevation::E3)
                    .stroke(egui::Stroke::new(1.0, color::STROKE_SUBTLE))
                    .inner_margin(Margin::same(PAD as i8))
                    .show(ui, |ui| {
                        let w = width - 2.0 * PAD;
                        ui.set_width(w);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        let text_x = ICON_D + 14.0;
                        let text_w = w - text_x;

                        // Icon + title (+ "2 more").
                        let painter = ui.painter().clone();
                        let more = (self.more > 0).then(|| format!("+{} more", self.more));
                        let more_w = more
                            .as_deref()
                            .map_or(0.0, |m| text::width(&painter, m, font::caption()) + 12.0);
                        let title = text::galley(
                            &painter,
                            &text::cap(self.title, 160),
                            font::title_sm(),
                            color::TEXT,
                            text_w - more_w,
                            2,
                        );
                        let head_h = title.size().y.max(ICON_D);
                        let (head, _) = ui.allocate_exact_size(vec2(w, head_h), Sense::hover());
                        let c = egui::pos2(head.left() + ICON_D / 2.0, head.top() + ICON_D / 2.0);
                        painter.circle_filled(c, ICON_D / 2.0, color::SURFACE_3);
                        icons::paint(&painter, c, self.icon, 22.0, Style::Regular, color::TEXT);
                        let ty = head.top() + (ICON_D - title.size().y).max(0.0) / 2.0;
                        painter.galley(egui::pos2(head.left() + text_x, ty), title, color::TEXT);
                        if let Some(m) = more {
                            painter.text(
                                egui::pos2(head.right(), head.top() + 10.0),
                                Align2::RIGHT_CENTER,
                                m,
                                font::caption(),
                                color::TEXT_3,
                            );
                        }

                        // Body.
                        if !self.body.trim().is_empty() {
                            let g = text::galley(
                                &painter,
                                &text::cap(self.body, 400),
                                font::body(),
                                color::TEXT_2,
                                text_w,
                                3,
                            );
                            ui.add_space(4.0);
                            let (r, _) =
                                ui.allocate_exact_size(vec2(w, g.size().y), Sense::hover());
                            painter.galley(
                                egui::pos2(r.left() + text_x, r.top()),
                                g,
                                color::TEXT_2,
                            );
                        }

                        // Countdown.
                        if let Some((frac, label, left)) = &self.countdown {
                            ui.add_space(12.0);
                            let (r, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
                            // Bar only when the title already says the time.
                            let text_room = if label.is_empty() { 0.0 } else { 132.0 };
                            let bar = egui::Rect::from_min_max(
                                egui::pos2(r.left() + text_x, r.center().y - 2.0),
                                egui::pos2(r.right() - text_room, r.center().y + 2.0),
                            );
                            paint::progress_bar(
                                &painter,
                                bar,
                                *frac,
                                color::ACCENT,
                                color::PROGRESS_TRACK,
                            );
                            if !label.is_empty() {
                                painter.text(
                                    egui::pos2(r.right(), r.center().y),
                                    Align2::RIGHT_CENTER,
                                    format!("{label} {left}"),
                                    font::caption_strong(),
                                    color::TEXT,
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Buttons, right-aligned, primary rightmost.
                        if !self.actions.is_empty() {
                            ui.add_space(18.0);
                            ui.allocate_ui_with_layout(
                                vec2(w, 44.0),
                                Layout::right_to_left(Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 12.0;
                                    focus::scope(ui, self.id, |ui| {
                                        for (i, a) in self.actions.iter().enumerate().rev() {
                                            let b = if a.primary {
                                                Button::primary(a.label)
                                            } else {
                                                Button::secondary(a.label)
                                            };
                                            let f = b.id(a.id).show(ui);
                                            if f.activated || (self.activate && f.focused) {
                                                chosen = Some(i);
                                            }
                                        }
                                    });
                                },
                            );
                        }
                    });
            });
        // Above a dialog that opened after it (the card may hold the focus).
        ctx.move_to_top(area.response.layer_id);
        chosen
    }
}

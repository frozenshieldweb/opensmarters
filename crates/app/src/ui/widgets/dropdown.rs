// SPDX-License-Identifier: GPL-3.0-or-later
//! A dropdown (§8.4 sort menu): a secondary button with a `CARET_DOWN` that opens a
//! menu of options below it.
//!
//! - While open, the menu is a focus trap (arrows stay inside, the selected option gets
//!   the focus); Enter picks an option and closes it.
//! - **Esc** belongs to the owning screen: call [`Dropdown::close`] from
//!   `Screen::on_back` (it returns true when a menu was open).
//! - A click outside closes it.
//!
//! ```ignore
//! if let Some(i) = Dropdown::new(Id::unique("movies.sort"), "Sort: ", &labels).show(ui, current) {
//!     self.sort = VodSort::ALL[i];
//! }
//! ```

use eframe::egui::{
    self, Align2, Area, CornerRadius, Frame, Id, Margin, Order, Sense, Ui, pos2, vec2,
};

use crate::focus::{self, FocusOpts};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::Button;
use crate::ui::{paint, text};

const ROW_H: f32 = 44.0;

/// A dropdown button + menu. See the [module docs](self).
#[derive(Debug)]
pub struct Dropdown<'a> {
    id: Id,
    prefix: &'a str,
    options: &'a [&'a str],
    icon: &'static str,
}

impl<'a> Dropdown<'a> {
    /// A dropdown with a stable id; the button reads `prefix` + the selected option.
    pub fn new(id: Id, prefix: &'a str, options: &'a [&'a str]) -> Self {
        Self {
            id,
            prefix,
            options,
            icon: icons::SORT_ASCENDING,
        }
    }

    /// Leading icon of the button.
    #[must_use]
    pub fn icon(mut self, glyph: &'static str) -> Self {
        self.icon = glyph;
        self
    }

    fn open_id(id: Id) -> Id {
        id.with("open")
    }

    /// The button's focus id.
    pub fn button_id(id: Id) -> Id {
        id.with("button")
    }

    /// True while the menu of `id` is open.
    pub fn is_open(ctx: &egui::Context, id: Id) -> bool {
        ctx.data(|d| d.get_temp::<bool>(Self::open_id(id)))
            .unwrap_or(false)
    }

    /// Closes the menu of `id` (Esc). Returns true if it was open.
    pub fn close(ctx: &egui::Context, id: Id) -> bool {
        let was = Self::is_open(ctx, id);
        if was {
            ctx.data_mut(|d| d.insert_temp(Self::open_id(id), false));
            focus::request(ctx, Self::button_id(id));
        }
        was
    }

    /// Adds the button (and the menu when open). Returns the chosen option.
    pub fn show(self, ui: &mut Ui, selected: usize) -> Option<usize> {
        let ctx = ui.ctx().clone();
        let label = format!(
            "{}{}",
            self.prefix,
            self.options.get(selected).copied().unwrap_or_default()
        );
        let button = Button::secondary(label)
            .icon(self.icon)
            .trailing(icons::CARET_DOWN)
            .id(Self::button_id(self.id));
        let size = button.size(ui);
        let (anchor, _) = ui.allocate_exact_size(size, Sense::hover());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(anchor));
        let f = button.show(&mut child);
        // Give the focus back to the button the frame after the menu closed (while it
        // was open this frame its focus trap would send the focus back into the menu).
        let refocus = self.id.with("refocus");
        if ctx
            .data_mut(|d| d.remove_temp::<bool>(refocus))
            .unwrap_or(false)
        {
            focus::request(&ctx, Self::button_id(self.id));
        }
        let mut open = Self::is_open(&ctx, self.id);
        if f.activated {
            open = !open;
            if open {
                focus::request(&ctx, self.id.with(("opt", selected)));
            }
        }
        let mut chosen = None;
        if open {
            let width = anchor.width().max(240.0);
            let menu_h = self.options.len() as f32 * ROW_H + 16.0;
            let screen = ctx.content_rect();
            let mut pos = pos2(anchor.right() - width, anchor.bottom() + 8.0);
            if pos.y + menu_h > screen.bottom() - 8.0 {
                pos.y = anchor.top() - 8.0 - menu_h;
            }
            pos.x = pos.x.max(screen.left() + 8.0);
            let scope = self.id.with("menu");
            focus::trap(&ctx, scope);
            let menu = Area::new(self.id.with("area"))
                .order(Order::Foreground)
                .fixed_pos(pos)
                .show(&ctx, |ui| {
                    Frame::new()
                        .fill(color::SURFACE_1)
                        .corner_radius(CornerRadius::same(radius::MD))
                        .shadow(elevation::E2)
                        .stroke(egui::Stroke::new(1.0, color::STROKE_SUBTLE))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(width - 16.0);
                            ui.spacing_mut().item_spacing.y = 0.0;
                            focus::scope(ui, scope, |ui| {
                                for (i, opt) in self.options.iter().enumerate() {
                                    let (rect, _) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), ROW_H),
                                        Sense::hover(),
                                    );
                                    let (_, rf) = focus::focusable(
                                        ui,
                                        self.id.with(("opt", i)),
                                        rect,
                                        FocusOpts::default().autofocus_if(i == selected),
                                    );
                                    let p = ui.painter().clone();
                                    if rf.focused {
                                        p.rect_filled(
                                            rect,
                                            CornerRadius::same(radius::SM),
                                            color::SURFACE_3,
                                        );
                                    }
                                    text::line(
                                        &p,
                                        pos2(rect.left() + 14.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        opt,
                                        font::body_strong(),
                                        if rf.focused || i == selected {
                                            color::TEXT
                                        } else {
                                            color::TEXT_2
                                        },
                                        rect.width() - 48.0,
                                    );
                                    if i == selected {
                                        icons::paint(
                                            &p,
                                            pos2(rect.right() - 20.0, rect.center().y),
                                            icons::CHECK,
                                            18.0,
                                            Style::Regular,
                                            color::ACCENT,
                                        );
                                    }
                                    if rf.focused {
                                        paint::focus_ring(&p, rect, radius::SM, 1.0);
                                    }
                                    if rf.activated {
                                        chosen = Some(i);
                                    }
                                }
                            });
                        });
                });
            let clicked_outside = ctx.input(|i| {
                i.pointer.any_click()
                    && i.pointer
                        .interact_pos()
                        .is_some_and(|p| !menu.response.rect.contains(p) && !anchor.contains(p))
            });
            if chosen.is_some() || clicked_outside {
                open = false;
                ctx.data_mut(|d| d.insert_temp(refocus, true));
                ctx.request_repaint();
            }
        }
        ctx.data_mut(|d| d.insert_temp(Self::open_id(self.id), open));
        chosen
    }
}

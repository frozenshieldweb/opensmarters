// SPDX-License-Identifier: GPL-3.0-or-later
//! Content rails (§6.2): a heading row and a horizontal carousel of cards.
//!
//! - The carousel is its own focus scope, so returning to a rail restores its last
//!   focused card.
//! - **Paint allowance:** the scroll area has 16 pt of inner padding above/below and the
//!   page margin before the first card, so the focused card's lift, ring and glow are not
//!   clipped.
//! - No visible scrollbar; the focused card scrolls into view with part of the next card
//!   peeking.

use eframe::egui::{self, Align2, Id, Margin, ScrollArea, Sense, Ui, pos2, vec2};

use crate::focus;
use crate::ui::icons;
use crate::ui::text;
use crate::ui::theme::{color, font};
use crate::ui::widgets::Button;

/// What a rail reports.
#[derive(Debug)]
pub struct RailResponse<R> {
    /// "See all" was activated.
    pub see_all: bool,
    /// The closure's result.
    pub inner: R,
}

/// A rail: heading + carousel.
#[derive(Debug)]
pub struct Rail<'a> {
    id: Id,
    title: &'a str,
    kicker: Option<String>,
    see_all: bool,
    margin: f32,
    gap: f32,
}

impl<'a> Rail<'a> {
    /// A rail with a stable id (the focus scope) and a heading.
    pub fn new(id: Id, title: &'a str) -> Self {
        Self {
            id,
            title,
            kicker: None,
            see_all: false,
            margin: 48.0,
            gap: 16.0,
        }
    }

    /// Overline after the title ("6 TITLES", "FAVOURITES").
    #[must_use]
    pub fn kicker(mut self, k: impl Into<String>) -> Self {
        self.kicker = Some(k.into());
        self
    }

    /// Shows a "See all" ghost button on the right.
    #[must_use]
    pub fn see_all(mut self, on: bool) -> Self {
        self.see_all = on;
        self
    }

    /// Page margin (first card alignment).
    #[must_use]
    pub fn margin(mut self, m: f32) -> Self {
        self.margin = m;
        self
    }

    /// Adds the rail. `add_cards` lays cards out left to right (the ui is horizontal with
    /// a 16 pt gap).
    pub fn show<R>(self, ui: &mut Ui, add_cards: impl FnOnce(&mut Ui) -> R) -> RailResponse<R> {
        let width = ui.available_width();
        let mut see_all = false;
        // Heading row (44 pt high to fit the ghost button).
        let (head, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
        let painter = ui.painter().clone();
        let title_rect = painter.text(
            pos2(head.left() + self.margin, head.center().y),
            Align2::LEFT_CENTER,
            text::cap(self.title, 80),
            font::title(),
            color::TEXT,
        );
        if let Some(k) = &self.kicker {
            text::overline(
                &painter,
                pos2(title_rect.right() + 12.0, head.center().y + 1.0),
                Align2::LEFT_CENTER,
                k,
                color::TEXT_3,
            );
        }
        if self.see_all {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_max(
                        pos2(head.right() - self.margin - 140.0, head.top()),
                        pos2(head.right() - self.margin + 12.0, head.bottom()),
                    ))
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            focus::scope(&mut child, self.id.with("head"), |ui| {
                let f = Button::ghost("See all")
                    .trailing(icons::CARET_RIGHT)
                    .small()
                    .id(self.id.with("see-all"))
                    .show(ui);
                see_all = f.activated;
            });
        }
        ui.add_space(12.0 - 16.0 - ui.spacing().item_spacing.y);
        let inner = focus::scope(ui, self.id, |ui| {
            ScrollArea::horizontal()
                .id_salt(self.id.with("scroll"))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .content_margin(Margin {
                    left: self.margin as i8,
                    right: self.margin as i8,
                    top: 16,
                    bottom: 16,
                })
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = self.gap;
                        add_cards(ui)
                    })
                    .inner
                })
                .inner
        });
        focus::forward_scroll(ui);
        RailResponse { see_all, inner }
    }
}

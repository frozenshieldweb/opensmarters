// SPDX-License-Identifier: GPL-3.0-or-later
//! [`UndoToast`]: a toast-styled card with an **Undo** button, shown for a few seconds
//! after an action that can be taken back (hiding a category). Unlike [`super::Toasts`]
//! it is drawn by the screen where the action happened, so its button can be reached with
//! the arrows or the mouse; screens also take Ctrl+Z while it shows.

use eframe::egui::{CornerRadius, Id, Pos2, Rect, Ui, UiBuilder, pos2, vec2};

use crate::focus;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::Button;
use crate::ui::{paint, text};

/// Height of the card (a one-line toast, §6.12).
const HEIGHT: f32 = 48.0;

/// An undo toast (§6.12 look: `surface-3`, r-md, `e2`, leading icon + `body`).
#[derive(Debug)]
pub struct UndoToast<'a> {
    id: Id,
    text: &'a str,
    icon: &'static str,
}

impl<'a> UndoToast<'a> {
    /// A toast saying `text` (plain language, e.g. "Hidden “FR | Sports”").
    pub fn new(id: Id, text: &'a str) -> Self {
        Self {
            id,
            text,
            icon: icons::INFO,
        }
    }

    /// The leading icon.
    #[must_use]
    pub fn icon(mut self, glyph: &'static str) -> Self {
        self.icon = glyph;
        self
    }

    /// The id of the Undo button (to give it the focus).
    pub fn button_id(id: Id) -> Id {
        id.with("undo")
    }

    /// Paints the card with its bottom edge centred on `bottom_center`, at most
    /// `max_width` wide. Returns true when Undo was activated.
    pub fn show(self, ui: &mut Ui, bottom_center: Pos2, max_width: f32) -> bool {
        let painter = ui.painter().clone();
        let button = Button::ghost("Undo")
            .icon(icons::ARROW_COUNTER_CLOCKWISE)
            .small()
            .id(Self::button_id(self.id));
        let bsize = button.size(ui);
        // [16] icon 20 [12] text [16] button [6]
        let chrome = 16.0 + 20.0 + 12.0 + 16.0 + bsize.x + 6.0;
        let galley = text::galley(
            &painter,
            self.text,
            font::body(),
            color::TEXT,
            (max_width - chrome).max(80.0),
            1,
        );
        let w = (chrome + galley.size().x).min(max_width.max(chrome + 80.0));
        let rect = Rect::from_min_size(
            pos2(bottom_center.x - w / 2.0, bottom_center.y - HEIGHT),
            vec2(w, HEIGHT),
        );
        paint::shadow(&painter, rect, radius::MD, elevation::E2);
        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_3);
        icons::paint(
            &painter,
            pos2(rect.left() + 26.0, rect.center().y),
            self.icon,
            20.0,
            Style::Regular,
            color::TEXT_2,
        );
        painter.galley(
            pos2(rect.left() + 48.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color::TEXT,
        );
        let br = Rect::from_min_size(
            pos2(
                rect.right() - 6.0 - bsize.x,
                rect.center().y - bsize.y / 2.0,
            ),
            bsize,
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(br));
        focus::scope(&mut child, self.id, |ui| button.show(ui)).activated
    }
}

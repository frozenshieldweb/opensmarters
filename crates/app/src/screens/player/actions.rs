// SPDX-License-Identifier: GPL-3.0-or-later
//! Extra OSD action buttons (the right cluster after the built-in ones). Features add
//! theirs with [`osd_icon`] in the marked "Feature actions" block of `PlayerScreen::osd`.

use eframe::egui::{Id, Rect, Ui, pos2, vec2};

use crate::ui::widgets::IconButton;

/// One round OSD icon button laid out right to left: placed at `*rx`, which then moves
/// left for the next one. `active` shows it selected (e.g. its panel is open). Returns
/// true when activated. Never reuse a glyph already on the OSD (DESIGN §6.10).
pub(crate) fn osd_icon(
    ui: &mut Ui,
    rx: &mut f32,
    cy: f32,
    glyph: &'static str,
    id: &'static str,
    active: bool,
) -> bool {
    let r = Rect::from_center_size(pos2(*rx, cy), vec2(48.0, 48.0));
    let mut b = IconButton::new(glyph).size(48.0, 24.0).id(Id::unique(id));
    if active {
        b = b.surface();
    }
    *rx -= 48.0 + 16.0;
    b.show_at(ui, r).activated
}

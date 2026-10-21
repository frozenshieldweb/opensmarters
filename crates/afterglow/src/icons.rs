// SPDX-License-Identifier: GPL-3.0-or-later
//! Phosphor icons (MIT, <https://phosphoricons.com>), loaded from the official TTFs in
//! `assets/fonts/` because `egui-phosphor` does not support egui 0.37 yet.
//!
//! - Every glyph is a `&str` constant in [`glyphs`] (re-exported here), named like the
//!   Phosphor icon: `house` → [`HOUSE`], `television-simple` → [`TELEVISION_SIMPLE`].
//! - Regular (outline) for icons at rest, Fill for selected/on states; both share code
//!   points, so pick the family with [`regular`] / [`fill`] or [`Style`].
//! - The UI text families fall back to Phosphor Regular, so a glyph can also be mixed
//!   into ordinary text: `format!("{} Resume", icons::PLAY)`.
//!
//! ```ignore
//! use afterglow::icons::{self, Style};
//! icons::paint(&painter, center, icons::HEART, 20.0, Style::Fill, theme::color::ACCENT);
//! ```

use egui::{self, Align2, Color32, FontFamily, FontId, Pos2, Rect};

pub mod glyphs;
pub use glyphs::*;

/// Font family name of Phosphor Regular.
pub const FAMILY_REGULAR: &str = "ph";
/// Font family name of Phosphor Fill.
pub const FAMILY_FILL: &str = "ph-fill";

pub(crate) const REGULAR_TTF: &[u8] = include_bytes!("../../../assets/fonts/Phosphor.ttf");
pub(crate) const FILL_TTF: &[u8] = include_bytes!("../../../assets/fonts/Phosphor-Fill.ttf");

/// Icon weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Outline, for icons at rest.
    #[default]
    Regular,
    /// Filled, for selected/on states (active nav item, favourited heart).
    Fill,
}

/// [`FontId`] for Phosphor Regular at `size` points.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_REGULAR.into()))
}

/// [`FontId`] for Phosphor Fill at `size` points.
pub fn fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_FILL.into()))
}

/// [`FontId`] for `style` at `size`.
pub fn font(size: f32, style: Style) -> FontId {
    match style {
        Style::Regular => regular(size),
        Style::Fill => fill(size),
    }
}

/// Paints `glyph` centred on `center`. Returns the glyph's rect.
pub fn paint(
    painter: &egui::Painter,
    center: Pos2,
    glyph: &str,
    size: f32,
    style: Style,
    color: Color32,
) -> Rect {
    painter.text(
        center,
        Align2::CENTER_CENTER,
        glyph,
        font(size, style),
        color,
    )
}

/// A [`egui::RichText`] for an icon, for use with egui's own widgets.
pub fn rich(glyph: &str, size: f32, style: Style) -> egui::RichText {
    egui::RichText::new(glyph).font(font(size, style))
}

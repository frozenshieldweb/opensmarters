// SPDX-License-Identifier: GPL-3.0-or-later
//! Afterglow design tokens (`docs/DESIGN.md` §4) and the egui style/font setup (§9).
//!
//! Use the tokens instead of literal colours and sizes:
//!
//! ```ignore
//! use afterglow::theme::{color, font, radius, space};
//! painter.rect_filled(rect, radius::MD, color::SURFACE_2);
//! painter.text(pos, Align2::LEFT_TOP, "Live TV", font::title(), color::TEXT);
//! ```

use std::sync::Arc;

use egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Shadow,
    Stroke, TextStyle, Theme, epaint::text::VariationCoords,
};

use crate::icons;

/// Colour tokens (§4.1–4.3). Alpha tokens use unmultiplied sRGB, as the doc requires.
pub mod color {
    use egui::Color32;

    /// App background, central panel.
    pub const BG: Color32 = Color32::from_rgb(0x14, 0x10, 0x13);
    /// Wells behind lists, EPG background, below the hero.
    pub const BG_DEEP: Color32 = Color32::from_rgb(0x0D, 0x0A, 0x0C);
    /// Behind video only.
    pub const VIDEO: Color32 = Color32::BLACK;
    /// Side rail, settings panes, dialogs, menus.
    pub const SURFACE_1: Color32 = Color32::from_rgb(0x20, 0x19, 0x1E);
    /// Cards, inputs, list rows (rest), chips.
    pub const SURFACE_2: Color32 = Color32::from_rgb(0x2C, 0x22, 0x28);
    /// Hovered/focused rows, secondary buttons, selected cell.
    pub const SURFACE_3: Color32 = Color32::from_rgb(0x3A, 0x2D, 0x34);
    /// Pressed, toggle track (off).
    pub const SURFACE_4: Color32 = Color32::from_rgb(0x4A, 0x3A, 0x43);
    /// Secondary button hover.
    pub const SECONDARY_HOVER: Color32 = Color32::from_rgb(0x46, 0x38, 0x40);
    /// Hairlines, dividers, card edges (white @8%).
    pub const STROKE_SUBTLE: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 20);
    /// Input borders, outlines (white @14%).
    pub const STROKE: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 36);
    /// Modal backdrop, text over imagery (`bg-deep` @72%).
    pub const SCRIM: Color32 = Color32::from_rgba_unmultiplied_const(0x0D, 0x0A, 0x0C, 184);

    /// Primary text.
    pub const TEXT: Color32 = Color32::from_rgb(0xF4, 0xEC, 0xE4);
    /// Secondary text, synopsis, inactive nav labels.
    pub const TEXT_2: Color32 = Color32::from_rgb(0xBF, 0xAF, 0xB6);
    /// Metadata, timestamps, placeholders.
    pub const TEXT_3: Color32 = Color32::from_rgb(0xA8, 0x98, 0x9E);
    /// Disabled labels only.
    pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x6E, 0x61, 0x68);
    /// Text and icons on accent, live and cream fills.
    pub const TEXT_ON_ACCENT: Color32 = Color32::from_rgb(0x1A, 0x13, 0x16);

    /// Champagne: primary button, progress, selected icon, links.
    pub const ACCENT: Color32 = Color32::from_rgb(0xE6, 0xBE, 0x91);
    /// Primary hover.
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xEF, 0xCD, 0xA6);
    /// Primary pressed.
    pub const ACCENT_PRESSED: Color32 = Color32::from_rgb(0xD4, 0xA8, 0x77);
    /// Selection background, EPG elapsed tint (accent @16%).
    pub const ACCENT_SOFT: Color32 = Color32::from_rgba_unmultiplied_const(0xE6, 0xBE, 0x91, 41);
    /// Focus ring.
    pub const FOCUS: Color32 = Color32::from_rgb(0xF2, 0xD3, 0xAE);
    /// Focus outer glow (accent @28%).
    pub const FOCUS_GLOW: Color32 = Color32::from_rgba_unmultiplied_const(230, 190, 145, 72);
    /// Secondary accent: catch-up available, "new" tags, info chips.
    pub const SAGE: Color32 = Color32::from_rgb(0xA4, 0xB6, 0xA4);
    /// Selected chip/segment fill.
    pub const CREAM: Color32 = Color32::from_rgb(0xF4, 0xEC, 0xE4);
    /// Live dot, LIVE badge, EPG now-line. Live indicators only.
    pub const LIVE: Color32 = Color32::from_rgb(0xF0, 0x66, 0x5E);
    /// Error text/icons, destructive button text.
    pub const DANGER: Color32 = Color32::from_rgb(0xF2, 0x8B, 0x82);
    /// Error callout fill (danger @14%).
    pub const DANGER_BG: Color32 = Color32::from_rgba_unmultiplied_const(0xF2, 0x8B, 0x82, 36);
    /// Warnings (e.g. unencrypted HTTP).
    pub const WARNING: Color32 = Color32::from_rgb(0xF0, 0xA8, 0x48);
    /// Warning callout fill (warning @14%).
    pub const WARNING_BG: Color32 = Color32::from_rgba_unmultiplied_const(0xF0, 0xA8, 0x48, 36);
    /// Connected, saved, valid.
    pub const SUCCESS: Color32 = Color32::from_rgb(0x8F, 0xC7, 0x9A);
    /// Neutral notices.
    pub const INFO: Color32 = Color32::from_rgb(0x9F, 0xB4, 0xD6);
    /// Info callout fill (info @12%).
    pub const INFO_BG: Color32 = Color32::from_rgba_unmultiplied_const(0x9F, 0xB4, 0xD6, 31);
    /// Progress track (white @14%).
    pub const PROGRESS_TRACK: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 36);
    /// Ghost button hover (white @6%).
    pub const GHOST_HOVER: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 15);
    /// Ghost button pressed (white @10%).
    pub const GHOST_PRESSED: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 26);

    /// Monogram tile hues (§6.4), picked by a stable hash of the channel name.
    pub const MONOGRAM: [Color32; 6] = [
        Color32::from_rgb(0x4A, 0x3A, 0x43),
        Color32::from_rgb(0x3E, 0x4A, 0x44),
        Color32::from_rgb(0x3C, 0x44, 0x56),
        Color32::from_rgb(0x4B, 0x3F, 0x35),
        Color32::from_rgb(0x45, 0x39, 0x4F),
        Color32::from_rgb(0x4A, 0x38, 0x38),
    ];
}

/// Spacing scale (§4.4), in points.
pub mod space {
    /// 2 pt.
    pub const XXS: f32 = 2.0;
    /// 4 pt.
    pub const XS: f32 = 4.0;
    /// 8 pt: default gap inside components.
    pub const SM: f32 = 8.0;
    /// 12 pt.
    pub const MD: f32 = 12.0;
    /// 16 pt: gap between cards.
    pub const LG: f32 = 16.0;
    /// 24 pt.
    pub const XL: f32 = 24.0;
    /// 32 pt: between rails.
    pub const XXL: f32 = 32.0;
    /// 48 pt.
    pub const X3L: f32 = 48.0;
    /// 64 pt.
    pub const X4L: f32 = 64.0;
    /// 96 pt.
    pub const X5L: f32 = 96.0;
}

/// Corner radii (§4.4).
pub mod radius {
    /// 4 pt: tags, progress bars, small EPG cells.
    pub const XS: u8 = 4;
    /// 8 pt: buttons, inputs, menu items.
    pub const SM: u8 = 8;
    /// 12 pt: cards, list rows, toasts.
    pub const MD: u8 = 12;
    /// 16 pt: dialogs, OSD panels, track menu.
    pub const LG: u8 = 16;
    /// 24 pt: sheets, large hero containers.
    pub const XL: u8 = 24;
    /// Pills, chips, avatars, icon buttons.
    pub const FULL: u8 = 255;
}

/// Stroke widths (§4.4).
pub mod stroke {
    /// Hairline.
    pub const HAIRLINE: f32 = 1.0;
    /// Inputs and outline buttons.
    pub const CONTROL: f32 = 1.5;
    /// Focus ring width (plus an equal gap).
    pub const FOCUS_RING: f32 = 3.0;
    /// Progress bars.
    pub const PROGRESS: f32 = 4.0;
}

/// Elevation (§4.5).
pub mod elevation {
    use egui::{Color32, Shadow};

    /// Focused/lifted cards.
    pub const E1: Shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(115),
    };
    /// Menus, popups.
    pub const E2: Shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    /// Dialogs, track menu.
    pub const E3: Shadow = Shadow {
        offset: [0, 18],
        blur: 48,
        spread: 4,
        color: Color32::from_black_alpha(166),
    };
}

/// Motion durations in seconds (§7).
pub mod motion {
    /// Hover/pressed colour, pressed dip.
    pub const FAST: f32 = 0.09;
    /// Focus scale, chip/segment slide.
    pub const BASE: f32 = 0.16;
    /// Side-rail expand, sheet slide-in, page fade.
    pub const SLOW: f32 = 0.24;
    /// Backdrop crossfade.
    pub const SLOWER: f32 = 0.4;
    /// OSD fade.
    pub const OSD: f32 = 0.2;
    /// OSD auto-hide delay.
    pub const OSD_HIDE_AFTER: f32 = 3.0;
}

/// Font family names registered by [`install`].
pub mod family {
    /// Source Sans 3, weight 600.
    pub const UI_600: &str = "ui-600";
    /// Source Sans 3, weight 700.
    pub const UI_700: &str = "ui-700";
    /// Fraunces display (opsz 72, wght 560, SOFT 100, WONK 0).
    pub const DISPLAY: &str = "display";
}

/// Type ramp (§4.7) as [`FontId`]s for painter-based widgets.
pub mod font {
    use egui::{FontFamily, FontId};

    use super::family;

    fn named(size: f32, name: &str) -> FontId {
        FontId::new(size, FontFamily::Name(name.into()))
    }

    /// 13 pt, 400: metadata, timestamps, helper text.
    pub fn caption() -> FontId {
        FontId::proportional(13.0)
    }
    /// 13 pt, 600 (channel numbers, emphasised captions).
    pub fn caption_strong() -> FontId {
        named(13.0, family::UI_600)
    }
    /// 12 pt, 600 + tracking, UPPERCASE: badges, section kickers. Use
    /// the app's `ui::text::overline` to get the letter spacing.
    pub fn overline() -> FontId {
        named(12.0, family::UI_600)
    }
    /// 16 pt, 400: default text, synopsis.
    pub fn body() -> FontId {
        FontId::proportional(16.0)
    }
    /// 16 pt, 600: list titles, channel names.
    pub fn body_strong() -> FontId {
        named(16.0, family::UI_600)
    }
    /// 16 pt, 600: buttons, chips, tabs.
    pub fn button() -> FontId {
        named(16.0, family::UI_600)
    }
    /// 18 pt, 600: card titles on focus, compact dialog titles.
    pub fn title_sm() -> FontId {
        named(18.0, family::UI_600)
    }
    /// 22 pt, 600: rail headings, dialog titles.
    pub fn title() -> FontId {
        named(22.0, family::UI_600)
    }
    /// 28 pt, 700: screen titles.
    pub fn heading() -> FontId {
        named(28.0, family::UI_700)
    }
    /// 40 pt Fraunces: detail title, Home hero title, "Who's watching?".
    pub fn display() -> FontId {
        named(40.0, family::DISPLAY)
    }
    /// Fraunces at an arbitrary size (empty-state headlines use 28).
    pub fn display_sized(size: f32) -> FontId {
        named(size, family::DISPLAY)
    }
    /// 56 pt Fraunces: onboarding headline only.
    pub fn hero() -> FontId {
        named(56.0, family::DISPLAY)
    }
    /// 64 pt, 700: channel-number entry overlay.
    pub fn numeric_xl() -> FontId {
        named(64.0, family::UI_700)
    }
    /// Phosphor Regular at `size`.
    pub fn icon(size: f32) -> FontId {
        named(size, crate::icons::FAMILY_REGULAR)
    }
    /// Phosphor Fill at `size`.
    pub fn icon_fill(size: f32) -> FontId {
        named(size, crate::icons::FAMILY_FILL)
    }
}

const UI_VF: &[u8] = include_bytes!("../../../assets/fonts/SourceSans3-Variable.ttf");
const DISPLAY_VF: &[u8] = include_bytes!("../../../assets/fonts/Fraunces-Variable.ttf");

/// The Fraunces variable font (unmodified, OFL), for faces at other coordinates than
/// `display` (the installer sets the wordmark live at `opsz` 144, as LAUNCH_PROMPT §0 asks).
pub fn display_font_bytes() -> &'static [u8] {
    DISPLAY_VF
}

/// A face of a variable font at the given axis coordinates (`[("wght", 600.0)]`).
pub fn variable_face(bytes: &'static [u8], coords: &[(&str, f32)]) -> Arc<FontData> {
    face(bytes, coords)
}

fn face(bytes: &'static [u8], coords: &[(&str, f32)]) -> Arc<FontData> {
    let tags: Vec<([u8; 4], f32)> = coords
        .iter()
        .filter_map(|(tag, v)| {
            let b = tag.as_bytes();
            (b.len() == 4).then(|| ([b[0], b[1], b[2], b[3]], *v))
        })
        .collect();
    Arc::new(FontData::from_static(bytes).tweak(FontTweak {
        coords: VariationCoords::new(tags.iter().map(|(t, v)| (t, *v))),
        ..Default::default()
    }))
}

/// The font definitions: Source Sans 3 (400/600/700) for UI, Fraunces for display,
/// Phosphor Regular/Fill for icons, egui's defaults as fallbacks.
pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for (name, data) in [
        ("ui-400", face(UI_VF, &[("wght", 400.0)])),
        ("ui-600", face(UI_VF, &[("wght", 600.0)])),
        ("ui-700", face(UI_VF, &[("wght", 700.0)])),
        (
            "display",
            face(
                DISPLAY_VF,
                &[
                    ("opsz", 72.0),
                    ("wght", 560.0),
                    ("SOFT", 100.0),
                    ("WONK", 0.0),
                ],
            ),
        ),
        (
            icons::FAMILY_REGULAR,
            Arc::new(FontData::from_static(icons::REGULAR_TTF).tweak(FontTweak {
                // Phosphor glyphs sit slightly high next to Source Sans.
                y_offset_factor: 0.1,
                ..Default::default()
            })),
        ),
        (
            icons::FAMILY_FILL,
            Arc::new(FontData::from_static(icons::FILL_TTF).tweak(FontTweak {
                y_offset_factor: 0.1,
                ..Default::default()
            })),
        ),
    ] {
        fonts.font_data.insert(name.to_owned(), data);
    }
    // Text families fall back to Phosphor Regular (so `format!("{} Play", icons::PLAY)`
    // works in one label) and then to egui's defaults (emoji, symbols).
    let chain = |primary: &str| {
        let mut v = vec![primary.to_owned(), icons::FAMILY_REGULAR.to_owned()];
        v.extend(fallbacks.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, chain("ui-400"));
    for fam in ["ui-600", "ui-700", "display"] {
        fonts
            .families
            .insert(FontFamily::Name(fam.into()), chain(fam));
    }
    fonts.families.insert(
        FontFamily::Name(icons::FAMILY_REGULAR.into()),
        vec![icons::FAMILY_REGULAR.to_owned()],
    );
    fonts.families.insert(
        FontFamily::Name(icons::FAMILY_FILL.into()),
        vec![icons::FAMILY_FILL.to_owned()],
    );
    fonts
}

/// Installs fonts and the Afterglow style. Call once at startup.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, apply_style);
}

fn apply_style(s: &mut egui::Style) {
    use egui::Color32 as C;
    let text_styles = [
        (TextStyle::Small, FontId::proportional(13.0)),
        (TextStyle::Body, FontId::proportional(16.0)),
        (
            TextStyle::Button,
            FontId::new(16.0, FontFamily::Name(family::UI_600.into())),
        ),
        (
            TextStyle::Heading,
            FontId::new(28.0, FontFamily::Name(family::UI_700.into())),
        ),
        (TextStyle::Monospace, FontId::monospace(14.0)),
        (TextStyle::Name("overline".into()), font::overline()),
        (TextStyle::Name("body-strong".into()), font::body_strong()),
        (TextStyle::Name("title-sm".into()), font::title_sm()),
        (TextStyle::Name("title".into()), font::title()),
        (TextStyle::Name("display".into()), font::display()),
        (TextStyle::Name("hero".into()), font::hero()),
        (TextStyle::Name("numeric-xl".into()), font::numeric_xl()),
    ];
    s.text_styles = text_styles.into_iter().collect();

    let v = &mut s.visuals;
    v.dark_mode = true;
    v.panel_fill = color::BG;
    v.window_fill = color::SURFACE_1;
    v.extreme_bg_color = color::BG_DEEP;
    v.faint_bg_color = color::SURFACE_1;
    v.text_edit_bg_color = Some(color::SURFACE_2);
    v.override_text_color = None;
    v.hyperlink_color = color::ACCENT;
    v.warn_fg_color = color::WARNING;
    v.error_fg_color = color::DANGER;
    v.selection.bg_fill = C::from_rgba_unmultiplied(230, 190, 145, 56);
    v.selection.stroke = Stroke::new(1.0, color::TEXT);
    v.window_corner_radius = CornerRadius::same(radius::LG);
    v.menu_corner_radius = CornerRadius::same(radius::MD);
    v.window_stroke = Stroke::new(1.0, color::STROKE_SUBTLE);
    v.window_shadow = elevation::E3;
    v.popup_shadow = elevation::E2;
    v.text_cursor.stroke = Stroke::new(2.0, color::ACCENT);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = color::SURFACE_1;
    w.noninteractive.bg_stroke = Stroke::new(1.0, color::STROKE_SUBTLE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, color::TEXT_2);
    for (st, fill, fg) in [
        (&mut w.inactive, color::SURFACE_2, color::TEXT_2),
        (&mut w.hovered, color::SURFACE_3, color::TEXT),
        (&mut w.active, color::SURFACE_4, color::TEXT),
        (&mut w.open, color::SURFACE_3, color::TEXT),
    ] {
        st.bg_fill = fill;
        st.weak_bg_fill = fill;
        st.fg_stroke = Stroke::new(1.5, fg);
        st.corner_radius = CornerRadius::same(radius::SM);
        st.expansion = 0.0;
    }
    w.inactive.bg_stroke = Stroke::new(1.0, color::STROKE);
    w.hovered.bg_stroke = Stroke::new(1.5, C::from_white_alpha(56));
    w.active.bg_stroke = Stroke::new(1.5, color::ACCENT);
    s.spacing.item_spacing = egui::vec2(8.0, 8.0);
    s.spacing.button_padding = egui::vec2(20.0, 12.0);
    s.spacing.interact_size = egui::vec2(44.0, 44.0);
    s.spacing.scroll = egui::style::ScrollStyle::thin();
    s.animation_time = motion::FAST;
    s.scroll_animation = egui::style::ScrollAnimation {
        points_per_second: 1400.0,
        duration: egui::Rangef::new(0.12, 0.24),
    };
    // Our own focus engine draws focus; egui's subtle outline stays off.
    v.widgets.hovered.expansion = 0.0;
}

/// Glow shadow behind a focused element, scaled by `t` (0..1).
pub fn focus_glow_shadow(t: f32) -> Shadow {
    let alpha = (72.0 * t.clamp(0.0, 1.0)).round() as u8;
    Shadow {
        offset: [0, 0],
        blur: 24,
        spread: 2,
        color: Color32::from_rgba_unmultiplied(230, 190, 145, alpha),
    }
}

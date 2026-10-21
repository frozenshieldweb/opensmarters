// SPDX-License-Identifier: GPL-3.0-or-later
//! The installer's controls, painted with Afterglow tokens (`docs/DESIGN.md` §5, §6.5):
//! buttons, a quiet link, checkbox rows, the header's close button, and text helpers.
//!
//! Every control is an egui-focusable widget (`Sense::click()`), so Tab / Shift+Tab /
//! arrows move the focus, Enter and Space activate, and AccessKit gets a role and a label
//! (`widget_info`). Pointer hover moves the focus (one highlight for mouse and keyboard).
//! The focus treatment is the app's: glow (animated, 160 ms) → element → 3 pt ring.

use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Id, Pos2, Rect, Response, Role, Sense, Stroke,
    Ui, Vec2, WidgetInfo, pos2, vec2,
};
use std::sync::Arc;

use afterglow::icons::{self, Style};
use afterglow::theme::{color, font, motion, radius};
use afterglow::{motion as anim, paint};

/// Button variants (DESIGN §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Champagne fill: the one primary action of a screen.
    Primary,
    /// `surface-3` fill.
    Secondary,
    /// No fill (Options, tertiary actions).
    Ghost,
}

/// Lays out one line of text.
pub fn galley(ui: &Ui, text: &str, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_owned(), font, color)
}

/// Paints one line of text anchored by `align` at `pos`; returns its rect. Screen readers
/// get it as a label.
pub fn text(ui: &Ui, pos: Pos2, align: Align2, text: &str, font: FontId, color: Color32) -> Rect {
    let rect = ui.painter().text(pos, align, text, font, color);
    label(ui, rect, text);
    rect
}

/// Exposes painted text to AccessKit (screen readers) as a static label.
pub fn label(ui: &Ui, rect: Rect, text: &str) {
    let resp = ui.interact(rect, ui.scope_id().with(("label", text)), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
}

/// Paints text wrapped to `width` with its top-left at `pos`; returns its rect. A text that
/// wraps to two lines is balanced (no orphaned last word).
pub fn wrapped(ui: &Ui, pos: Pos2, width: f32, text: &str, font: FontId, color: Color32) -> Rect {
    let shown = balanced(ui, text, &font, width);
    let g = ui.painter().layout(shown, font, color, width);
    let rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color);
    label(ui, rect, text);
    rect
}

/// `text` with a line break where a two-line wrap is most even, so the second line never
/// holds a lone word. Other texts are returned as they are.
pub fn balanced(ui: &Ui, text: &str, font: &FontId, max: f32) -> String {
    if text.contains('\n') {
        return text.to_owned();
    }
    let galley = ui
        .painter()
        .layout(text.to_owned(), font.clone(), Color32::WHITE, max);
    let rows = galley.rows.len();
    if rows > 2 {
        // Longer paragraphs: never leave a single word on the last line; break before
        // the last two words instead (the lines above re-wrap naturally).
        let last_is_one_word = galley.rows.last().is_some_and(|r| {
            let line: String = r.glyphs.iter().map(|g| g.chr).collect();
            !line.trim().contains(' ')
        });
        if last_is_one_word
            && let Some(i) = text.trim_end().rfind(' ')
            && let Some(j) = text[..i].rfind(' ')
        {
            let candidate = format!("{}\n{}", &text[..j], &text[j + 1..]);
            let after = ui
                .painter()
                .layout(candidate.clone(), font.clone(), Color32::WHITE, max)
                .rows
                .len();
            if after <= rows {
                return candidate;
            }
        }
        return text.to_owned();
    }
    if rows != 2 {
        return text.to_owned();
    }
    let mut best: Option<(f32, usize)> = None;
    for (i, ch) in text.char_indices() {
        if ch != ' ' {
            continue;
        }
        let a = width(ui, &text[..i], font.clone());
        let b = width(ui, &text[i + 1..], font.clone());
        if a <= max && b <= max && best.is_none_or(|(m, _)| a.max(b) < m) {
            best = Some((a.max(b), i));
        }
    }
    best.map_or_else(
        || text.to_owned(),
        |(_, i)| format!("{}\n{}", &text[..i], &text[i + 1..]),
    )
}

/// Width of `text` in `font`.
pub fn width(ui: &Ui, text: &str, font: FontId) -> f32 {
    galley(ui, text, font, Color32::WHITE).size().x
}

/// Shortens a Windows path to fit `max` points, keeping the drive and as many of the last
/// folders as fit (`C:\…\Local\Programs\opensmarters`): the end is the part that matters.
pub fn ellipsize_path(ui: &Ui, path: &str, font: &FontId, max: f32) -> String {
    if width(ui, path, font.clone()) <= max {
        return path.to_owned();
    }
    let parts: Vec<&str> = path.split('\\').collect();
    let head = parts.first().copied().unwrap_or("");
    for skip in 1..parts.len() {
        let s = format!("{head}\\…\\{}", parts[skip..].join("\\"));
        if width(ui, &s, font.clone()) <= max {
            return s;
        }
    }
    // Even the last folder alone is too long: cut it from the left.
    let last: Vec<char> = parts.last().copied().unwrap_or("").chars().collect();
    for start in 1..last.len() {
        let s = format!("…{}", last[start..].iter().collect::<String>());
        if width(ui, &s, font.clone()) <= max {
            return s;
        }
    }
    "…".to_owned()
}

/// Hover moves the focus (DESIGN §5: no separate hover style).
fn hover_focus(ui: &Ui, resp: &Response) {
    if resp.hovered() && !resp.has_focus() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
        resp.request_focus();
    }
}

/// A focusable area with the shared behaviour; returns the response and the animated
/// focus amount (0..1, 160 ms).
fn focusable(ui: &mut Ui, id: Id, rect: Rect, enabled: bool) -> (Response, f32) {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, id, sense);
    if enabled {
        hover_focus(ui, &resp);
    }
    let t = anim::animate_bool(ui.ctx(), id.with("focus"), resp.has_focus(), motion::BASE);
    (resp, t)
}

/// Size of a button with `label` (large = 52 pt tall, 28 pt padding; else 44 / 20).
pub fn button_size(ui: &Ui, label: &str, large: bool) -> Vec2 {
    let (h, pad) = if large { (52.0, 28.0) } else { (44.0, 20.0) };
    vec2((width(ui, label, font::button()) + pad * 2.0).round(), h)
}

/// A text button at `rect`. Clicked (mouse, Enter or Space) → `resp.clicked()`.
pub fn button(ui: &mut Ui, id: Id, rect: Rect, label: &str, kind: Kind, enabled: bool) -> Response {
    let (resp, t) = focusable(ui, id, rect, enabled);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    let focused = resp.has_focus();
    let pressed = resp.is_pointer_button_down_on();
    let (fill, fg) = match kind {
        Kind::Primary => (
            if pressed {
                color::ACCENT_PRESSED
            } else if focused {
                color::ACCENT_HOVER
            } else {
                color::ACCENT
            },
            color::TEXT_ON_ACCENT,
        ),
        Kind::Secondary => (
            if pressed {
                color::SURFACE_4
            } else if focused {
                color::SECONDARY_HOVER
            } else {
                color::SURFACE_3
            },
            color::TEXT,
        ),
        Kind::Ghost => (
            if pressed {
                color::GHOST_PRESSED
            } else if focused {
                color::GHOST_HOVER
            } else {
                Color32::TRANSPARENT
            },
            if focused { color::TEXT } else { color::TEXT_2 },
        ),
    };
    let painter = ui.painter().clone();
    let alpha = if enabled { 1.0 } else { 0.4 };
    if enabled {
        paint::focus_glow(&painter, rect, radius::SM, t);
    }
    painter.rect_filled(
        rect,
        CornerRadius::same(radius::SM),
        fill.gamma_multiply(alpha),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font::button(),
        fg.gamma_multiply(alpha),
    );
    if focused {
        paint::focus_ring(&painter, rect, radius::SM, 1.0);
    }
    resp
}

/// A quiet inline link (`text-2`, underlined; `text` with the ring when focused). `pos`
/// is the left end of the text's baseline row (left-centre). Returns the response.
pub fn link(ui: &mut Ui, id: Id, pos: Pos2, label: &str, font: FontId) -> Response {
    let g = galley(ui, label, font.clone(), color::TEXT_2);
    let rect = Rect::from_min_size(pos2(pos.x, pos.y - g.size().y / 2.0), g.size());
    let hit = rect.expand2(vec2(4.0, 6.0));
    let (resp, t) = focusable(ui, id, hit, true);
    resp.widget_info(|| WidgetInfo::labeled(Role::Link, true, label));
    let focused = resp.has_focus();
    let painter = ui.painter().clone();
    paint::focus_glow(&painter, hit, radius::XS, t * 0.6);
    let c = if focused { color::TEXT } else { color::TEXT_2 };
    painter.text(rect.left_center(), Align2::LEFT_CENTER, label, font, c);
    let y = rect.bottom() - 1.0;
    painter.line_segment(
        [pos2(rect.left(), y), pos2(rect.right(), y)],
        Stroke::new(1.0, c.gamma_multiply(0.7)),
    );
    if focused {
        paint::focus_ring(&painter, hit, radius::XS, 1.0);
    }
    resp
}

/// A full-width checkbox row (48 pt): a Phosphor square (Regular) or checked square
/// (Fill, cream) and the label. Click / Enter / Space toggles `checked`.
pub fn checkbox_row(ui: &mut Ui, id: Id, rect: Rect, label: &str, checked: &mut bool) -> Response {
    let (resp, t) = focusable(ui, id, rect, true);
    let on = *checked;
    resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, on, label));
    if resp.clicked() {
        *checked = !*checked;
    }
    let focused = resp.has_focus();
    let painter = ui.painter().clone();
    paint::focus_glow(&painter, rect, radius::SM, t);
    if focused {
        painter.rect_filled(rect, CornerRadius::same(radius::SM), color::SURFACE_3);
    }
    let icon_c = pos2(rect.left() + 14.0 + 11.0, rect.center().y);
    let (glyph, style, c) = if *checked {
        (icons::CHECK_SQUARE, Style::Fill, color::CREAM)
    } else {
        (
            icons::SQUARE,
            Style::Regular,
            if focused { color::TEXT } else { color::TEXT_2 },
        )
    };
    icons::paint(&painter, icon_c, glyph, 22.0, style, c);
    let label_c = if focused || *checked {
        color::TEXT
    } else {
        color::TEXT_2
    };
    let text_left = rect.left() + 14.0 + 22.0 + 12.0;
    let label_w = rect.right() - 12.0 - text_left;
    let g = painter.layout(
        balanced(ui, label, &font::body(), label_w),
        font::body(),
        label_c,
        label_w,
    );
    let pos = pos2(text_left, rect.center().y - g.size().y / 2.0);
    painter.galley(pos, g, label_c);
    if focused {
        paint::focus_ring(&painter, rect, radius::SM, 1.0);
    }
    resp
}

/// The header's close button: a 32 pt circle with an X, on a dark disc so it reads over
/// the art.
pub fn close_button(ui: &mut Ui, id: Id, center: Pos2) -> Response {
    let rect = Rect::from_center_size(center, Vec2::splat(32.0));
    let (resp, t) = focusable(ui, id, rect, true);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Close"));
    let focused = resp.has_focus();
    let painter = ui.painter().clone();
    paint::focus_glow(&painter, rect, radius::FULL, t);
    let fill = if resp.is_pointer_button_down_on() {
        color::SURFACE_4
    } else if focused {
        color::SURFACE_3
    } else {
        color::SCRIM
    };
    painter.circle_filled(center, 16.0, fill);
    icons::paint(
        &painter,
        center,
        icons::X,
        16.0,
        Style::Regular,
        if focused { color::TEXT } else { color::TEXT_2 },
    );
    if focused {
        paint::focus_ring_circle(&painter, center, 16.0, 1.0);
    }
    resp
}

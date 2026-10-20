// SPDX-License-Identifier: GPL-3.0-or-later
//! Inline callouts (§6.17) and small badges (LIVE, NEW).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Painter, Pos2, Rect, Sense, Ui, pos2, text::LayoutJob,
    text::TextFormat, vec2,
};

use crate::ui::icons::{self, Style};
use crate::ui::text;
use crate::ui::theme::{color, font, radius};

/// Callout status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutKind {
    /// Warning (e.g. unencrypted HTTP).
    Warning,
    /// Error.
    Danger,
    /// Neutral notice.
    Info,
    /// Success.
    Success,
}

impl CalloutKind {
    fn colors(self) -> (Color32, Color32, &'static str) {
        match self {
            CalloutKind::Warning => (color::WARNING_BG, color::WARNING, icons::WARNING),
            CalloutKind::Danger => (color::DANGER_BG, color::DANGER, icons::WARNING_CIRCLE),
            CalloutKind::Info => (color::INFO_BG, color::INFO, icons::INFO),
            CalloutKind::Success => (
                Color32::from_rgba_unmultiplied(0x8F, 0xC7, 0x9A, 31),
                color::SUCCESS,
                icons::CHECK_CIRCLE,
            ),
        }
    }
}

/// A callout: status icon + bold lead + body, wrapped, never dismissable by itself.
#[derive(Debug, Clone)]
pub struct Callout {
    kind: CalloutKind,
    lead: String,
    body: String,
    width: f32,
}

impl Callout {
    /// A callout. `lead` is shown in bold before `body` (either may be empty).
    pub fn new(kind: CalloutKind, lead: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind,
            lead: lead.into(),
            body: body.into(),
            width: 440.0,
        }
    }

    /// The standard unencrypted-connection warning.
    pub fn unencrypted() -> Self {
        Self::new(
            CalloutKind::Warning,
            "Unencrypted connection:",
            "your password is visible on the network.",
        )
    }

    /// Width.
    #[must_use]
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }

    /// Adds the callout.
    pub fn show(self, ui: &mut Ui) -> Rect {
        let (bg, fg, glyph) = self.kind.colors();
        let text_w = self.width - 16.0 - 28.0 - 16.0;
        let mut job = LayoutJob::default();
        if !self.lead.is_empty() {
            job.append(
                &self.lead,
                0.0,
                TextFormat::simple(font::body_strong(), color::TEXT),
            );
            job.append(" ", 0.0, TextFormat::simple(font::body(), color::TEXT));
        }
        job.append(
            &text::cap(&self.body, 600),
            0.0,
            TextFormat::simple(font::body(), color::TEXT),
        );
        job.wrap.max_width = text_w;
        let painter = ui.painter().clone();
        let galley = painter.layout_job(job);
        let h = (galley.size().y + 24.0).max(48.0);
        let (rect, _) = ui.allocate_exact_size(vec2(self.width, h), Sense::hover());
        painter.rect_filled(rect, CornerRadius::same(radius::SM), bg);
        icons::paint(
            &painter,
            pos2(rect.left() + 28.0, rect.top() + 24.0),
            glyph,
            20.0,
            Style::Regular,
            fg,
        );
        painter.galley(
            pos2(rect.left() + 48.0, rect.top() + 12.0),
            galley,
            color::TEXT,
        );
        rect
    }
}

/// A small pill badge.
#[derive(Debug, Clone, Copy)]
pub struct Badge;

impl Badge {
    /// The LIVE badge (`live` pill, overline text). Returns its rect.
    pub fn live(painter: &Painter, pos: Pos2, align: Align2) -> Rect {
        Self::pill(
            painter,
            pos,
            align,
            "LIVE",
            color::LIVE,
            color::TEXT_ON_ACCENT,
        )
    }

    /// A pill with overline text.
    pub fn pill(
        painter: &Painter,
        pos: Pos2,
        align: Align2,
        label: &str,
        fill: Color32,
        fg: Color32,
    ) -> Rect {
        let g = painter.layout_job(text::overline_job(label, fg));
        let size = vec2(g.size().x + 16.0, 22.0);
        let rect = align.anchor_size(pos, size);
        painter.rect_filled(rect, CornerRadius::same(radius::FULL), fill);
        painter.galley(
            pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0),
            g,
            fg,
        );
        rect
    }

    /// "● LIVE" dot + overline (cards).
    pub fn live_dot(painter: &Painter, pos: Pos2) -> Rect {
        painter.circle_filled(pos2(pos.x + 3.0, pos.y), 3.0, color::LIVE);
        let r = text::overline(
            painter,
            pos2(pos.x + 10.0, pos.y),
            Align2::LEFT_CENTER,
            "LIVE",
            color::TEXT_2,
        );
        r.union(Rect::from_center_size(pos, vec2(6.0, 6.0)))
    }
}

/// Paints a status line "● Connected" style (icon + word, never colour alone).
pub fn status_line(painter: &Painter, pos: Pos2, glyph: &str, label: &str, c: Color32) -> Rect {
    icons::paint(
        painter,
        pos2(pos.x + 8.0, pos.y),
        glyph,
        16.0,
        Style::Regular,
        c,
    );
    let r = painter.text(
        pos2(pos.x + 22.0, pos.y),
        Align2::LEFT_CENTER,
        label,
        font::caption(),
        c,
    );
    let _ = egui::Id::NULL;
    r
}

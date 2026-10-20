// SPDX-License-Identifier: GPL-3.0-or-later
//! Toasts (§6.12): bottom-centre, 4 s (errors 6 s), at most 2, never take focus.
//! Screens add them with `cx.toasts.info("…")` etc.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{Align2, Area, Color32, Context, CornerRadius, Id, Order, Rect, pos2, vec2};

use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::{paint, text};

/// Toast status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral.
    Info,
    /// Saved, connected.
    Success,
    /// Warning.
    Warning,
    /// Error (shown 6 s).
    Error,
}

#[derive(Debug, Clone)]
struct Toast {
    kind: ToastKind,
    text: String,
    key: Option<String>,
    created: Instant,
}

/// The toast queue.
#[derive(Debug, Default)]
pub struct Toasts {
    items: VecDeque<Toast>,
}

impl Toasts {
    /// Adds a toast. A toast with the same `key` replaces the previous one (e.g. the
    /// "Reconnecting… (n)" counter).
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>, key: Option<&str>) {
        if let Some(k) = key {
            self.items.retain(|t| t.key.as_deref() != Some(k));
        }
        self.items.push_back(Toast {
            kind,
            text: text.into(),
            key: key.map(str::to_owned),
            created: Instant::now(),
        });
        while self.items.len() > 2 {
            self.items.pop_front();
        }
    }

    /// Info toast.
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Info, text, None);
    }

    /// Success toast.
    pub fn success(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Success, text, None);
    }

    /// Warning toast.
    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Warning, text, None);
    }

    /// Error toast.
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(ToastKind::Error, text, None);
    }

    /// Removes a keyed toast.
    pub fn dismiss(&mut self, key: &str) {
        self.items.retain(|t| t.key.as_deref() != Some(key));
    }

    /// Paints the toasts above `bottom` (the bottom safe edge).
    pub fn show(&mut self, ctx: &Context, bottom: f32) {
        self.items.retain(|t| {
            let ttl = if t.kind == ToastKind::Error { 6.0 } else { 4.0 };
            t.created.elapsed() < Duration::from_secs_f32(ttl)
        });
        if self.items.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let screen = ctx.content_rect();
        Area::new(Id::unique("toasts"))
            .order(Order::Tooltip)
            .interactable(false)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let painter = ui.painter().clone();
                let mut y = bottom - 48.0;
                for t in self.items.iter().rev() {
                    let (glyph, c) = match t.kind {
                        ToastKind::Info => (icons::INFO, color::INFO),
                        ToastKind::Success => (icons::CHECK_CIRCLE, color::SUCCESS),
                        ToastKind::Warning => (icons::WARNING, color::WARNING),
                        ToastKind::Error => (icons::WARNING_CIRCLE, color::DANGER),
                    };
                    let g = text::galley(
                        &painter,
                        &t.text,
                        font::body(),
                        color::TEXT,
                        520.0 - 64.0,
                        3,
                    );
                    let w = g.size().x + 16.0 + 20.0 + 12.0 + 16.0;
                    let h = (g.size().y + 24.0).max(48.0);
                    let rect =
                        Rect::from_min_size(pos2(screen.center().x - w / 2.0, y - h), vec2(w, h));
                    let age = t.created.elapsed().as_secs_f32();
                    let alpha = (age / 0.2).clamp(0.0, 1.0);
                    paint::shadow(&painter, rect, radius::MD, elevation::E2);
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(radius::MD),
                        color::SURFACE_3.gamma_multiply(alpha.max(0.6)),
                    );
                    icons::paint(
                        &painter,
                        pos2(rect.left() + 26.0, rect.center().y),
                        glyph,
                        20.0,
                        Style::Regular,
                        c,
                    );
                    painter.galley(
                        pos2(rect.left() + 48.0, rect.center().y - g.size().y / 2.0),
                        g,
                        color::TEXT,
                    );
                    y = rect.top() - 8.0;
                }
                let _ = (Align2::CENTER_CENTER, Color32::TRANSPARENT);
            });
    }
}

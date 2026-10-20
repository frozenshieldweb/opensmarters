// SPDX-License-Identifier: GPL-3.0-or-later
//! Buttons (§6.5).

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Sense, Ui, Vec2, vec2};

use crate::focus::{self, Focus, FocusOpts};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::{paint, text};

/// Button variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Champagne fill: one per view (Play, Resume, Sign in).
    Primary,
    /// `surface-3` fill (More info, Cancel).
    Secondary,
    /// No fill (See all, tertiary).
    Ghost,
    /// `surface-3` fill with danger text (Delete).
    Danger,
}

/// A text button with an optional leading/trailing icon.
#[derive(Debug, Clone)]
pub struct Button {
    label: String,
    kind: ButtonKind,
    icon: Option<&'static str>,
    trailing: Option<&'static str>,
    large: bool,
    width: Option<f32>,
    enabled: bool,
    opts: FocusOpts,
    id: Option<Id>,
    small: bool,
}

impl Button {
    fn new(label: impl Into<String>, kind: ButtonKind) -> Self {
        Self {
            label: label.into(),
            kind,
            icon: None,
            trailing: None,
            large: false,
            width: None,
            enabled: true,
            opts: FocusOpts::default(),
            id: None,
            small: false,
        }
    }

    /// Primary button.
    pub fn primary(label: impl Into<String>) -> Self {
        Self::new(label, ButtonKind::Primary)
    }

    /// Secondary button.
    pub fn secondary(label: impl Into<String>) -> Self {
        Self::new(label, ButtonKind::Secondary)
    }

    /// Ghost button.
    pub fn ghost(label: impl Into<String>) -> Self {
        Self::new(label, ButtonKind::Ghost)
    }

    /// Danger button.
    pub fn danger(label: impl Into<String>) -> Self {
        Self::new(label, ButtonKind::Danger)
    }

    /// Leading icon (Phosphor glyph).
    #[must_use]
    pub fn icon(mut self, glyph: &'static str) -> Self {
        self.icon = Some(glyph);
        self
    }

    /// Trailing icon (e.g. `CARET_RIGHT`).
    #[must_use]
    pub fn trailing(mut self, glyph: &'static str) -> Self {
        self.trailing = Some(glyph);
        self
    }

    /// Large variant (52 pt, hero/detail).
    #[must_use]
    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }

    /// Compact variant (36 pt; dense toolbars).
    #[must_use]
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// Fixed width.
    #[must_use]
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    /// Enabled (disabled = 40% opacity, not focusable).
    #[must_use]
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self) -> Self {
        self.opts = self.opts.autofocus();
        self
    }

    /// Conditional autofocus.
    #[must_use]
    pub fn autofocus_if(mut self, on: bool) -> Self {
        self.opts = self.opts.autofocus_if(on);
        self
    }

    /// Explicit id (needed when two buttons share a label in one parent).
    #[must_use]
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// The size this button will take.
    pub fn size(&self, ui: &Ui) -> Vec2 {
        let (h, pad) = self.metrics();
        let painter = ui.painter();
        let mut w = pad * 2.0 + text::width(painter, &self.label, font::button());
        if self.icon.is_some() {
            w += 20.0 + if self.label.is_empty() { 0.0 } else { 8.0 };
        }
        if self.trailing.is_some() {
            w += 16.0 + 8.0;
        }
        vec2(self.width.unwrap_or(w).round(), h)
    }

    fn metrics(&self) -> (f32, f32) {
        if self.large {
            (52.0, 28.0)
        } else if self.small {
            (36.0, 16.0)
        } else {
            (44.0, 20.0)
        }
    }

    /// Adds the button. `activated` is true on Enter/Space/click.
    pub fn show(self, ui: &mut Ui) -> Focus {
        let size = self.size(ui);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let id = self
            .id
            .unwrap_or_else(|| ui.make_persistent_id(("button", &self.label)));
        let painter = ui.painter().clone();
        let r = CornerRadius::same(radius::SM);
        if !self.enabled {
            let (fill, fg) = self.colors(false, false);
            painter.rect_filled(rect, r, fill.gamma_multiply(0.4));
            self.paint_content(&painter, rect, fg.gamma_multiply(0.4));
            return Focus {
                id,
                focused: false,
                activated: false,
                anim: 0.0,
            };
        }
        let (resp, f) = focus::focusable(ui, id, rect, self.opts);
        let pressed = resp.is_pointer_button_down_on();
        let (fill, fg) = self.colors(f.focused, pressed);
        paint::focus_glow(&painter, rect, radius::SM, f.anim);
        painter.rect_filled(rect, r, fill);
        self.paint_content(&painter, rect, fg);
        if f.focused {
            paint::focus_ring(&painter, rect, radius::SM, 1.0);
        }
        f
    }

    fn colors(&self, focused: bool, pressed: bool) -> (Color32, Color32) {
        match self.kind {
            ButtonKind::Primary => (
                if pressed {
                    color::ACCENT_PRESSED
                } else if focused {
                    color::ACCENT_HOVER
                } else {
                    color::ACCENT
                },
                color::TEXT_ON_ACCENT,
            ),
            ButtonKind::Secondary => (
                if pressed {
                    color::SURFACE_4
                } else if focused {
                    color::SECONDARY_HOVER
                } else {
                    color::SURFACE_3
                },
                color::TEXT,
            ),
            ButtonKind::Ghost => (
                if pressed {
                    color::GHOST_PRESSED
                } else if focused {
                    color::GHOST_HOVER
                } else {
                    Color32::TRANSPARENT
                },
                if focused { color::TEXT } else { color::TEXT_2 },
            ),
            ButtonKind::Danger => (
                if pressed {
                    color::SURFACE_4
                } else if focused {
                    color::SECONDARY_HOVER
                } else {
                    color::SURFACE_3
                },
                color::DANGER,
            ),
        }
    }

    fn paint_content(&self, painter: &egui::Painter, rect: egui::Rect, fg: Color32) {
        let (_, pad) = self.metrics();
        let label_w = text::width(painter, &self.label, font::button());
        let icon_w = if self.icon.is_some() {
            20.0 + if self.label.is_empty() { 0.0 } else { 8.0 }
        } else {
            0.0
        };
        let trail_w = if self.trailing.is_some() { 24.0 } else { 0.0 };
        let content_w = (icon_w + label_w + trail_w).min(rect.width() - pad * 2.0);
        let mut x = rect.center().x - content_w / 2.0;
        let cy = rect.center().y;
        if let Some(g) = self.icon {
            icons::paint(
                painter,
                egui::pos2(x + 10.0, cy),
                g,
                20.0,
                Style::Regular,
                fg,
            );
            x += icon_w;
        }
        if !self.label.is_empty() {
            let max = (rect.right() - pad - trail_w - x).max(10.0);
            text::line(
                painter,
                egui::pos2(x, cy),
                Align2::LEFT_CENTER,
                &self.label,
                font::button(),
                fg,
                max,
            );
            x += label_w.min(max);
        }
        if let Some(g) = self.trailing {
            icons::paint(
                painter,
                egui::pos2(x + 8.0 + 8.0, cy),
                g,
                16.0,
                Style::Regular,
                fg,
            );
        }
    }
}

/// A round icon button (44 pt by default; OSD/toolbars).
#[derive(Debug, Clone)]
pub struct IconButton {
    glyph: &'static str,
    size: f32,
    icon_size: f32,
    selected: bool,
    filled: bool,
    id_salt: Option<Id>,
    opts: FocusOpts,
    caption: Option<String>,
    tint: Option<Color32>,
    primary: bool,
    surface: bool,
}

impl IconButton {
    /// A 44 pt ghost icon button.
    pub fn new(glyph: &'static str) -> Self {
        Self {
            glyph,
            size: 44.0,
            icon_size: 20.0,
            selected: false,
            filled: false,
            id_salt: None,
            opts: FocusOpts::default(),
            caption: None,
            tint: None,
            primary: false,
            surface: false,
        }
    }

    /// Diameter and icon size.
    #[must_use]
    pub fn size(mut self, diameter: f32, icon: f32) -> Self {
        self.size = diameter;
        self.icon_size = icon;
        self
    }

    /// Selected state (accent icon).
    #[must_use]
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// Use the Fill glyph.
    #[must_use]
    pub fn fill(mut self, on: bool) -> Self {
        self.filled = on;
        self
    }

    /// Icon colour override.
    #[must_use]
    pub fn tint(mut self, c: Color32) -> Self {
        self.tint = Some(c);
        self
    }

    /// `accent`-filled circle (the OSD play/pause); focus adds the ring only.
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// `surface-3` filled circle at rest.
    #[must_use]
    pub fn surface(mut self) -> Self {
        self.surface = true;
        self
    }

    /// Small caption under the icon (e.g. "10" for seek buttons).
    #[must_use]
    pub fn caption(mut self, c: impl Into<String>) -> Self {
        self.caption = Some(c.into());
        self
    }

    /// Explicit id.
    #[must_use]
    pub fn id(mut self, id: Id) -> Self {
        self.id_salt = Some(id);
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self) -> Self {
        self.opts = self.opts.autofocus();
        self
    }

    /// Adds the button.
    pub fn show(self, ui: &mut Ui) -> Focus {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
        self.show_at(ui, rect)
    }

    /// Paints the button at `rect` (no layout allocation).
    pub fn show_at(self, ui: &mut Ui, rect: egui::Rect) -> Focus {
        let id = self
            .id_salt
            .unwrap_or_else(|| ui.make_persistent_id(("icon-button", self.glyph)));
        let (resp, f) = focus::focusable(ui, id, rect, self.opts);
        let painter = ui.painter().clone();
        let c = rect.center();
        let r = self.size / 2.0;
        let pressed = resp.is_pointer_button_down_on();
        if f.anim > 0.0 {
            paint::focus_glow(&painter, rect, radius::FULL, f.anim);
        }
        let fill = if self.primary {
            // Always accent-filled; focus adds only the ring.
            if pressed {
                color::ACCENT_PRESSED
            } else {
                color::ACCENT
            }
        } else if pressed {
            color::GHOST_PRESSED
        } else if f.focused {
            if self.surface {
                color::SECONDARY_HOVER
            } else {
                color::GHOST_HOVER
            }
        } else if self.surface || self.selected {
            color::SURFACE_3
        } else {
            Color32::TRANSPARENT
        };
        painter.circle_filled(c, r, fill);
        let fg = if self.primary {
            color::TEXT_ON_ACCENT
        } else if let Some(t) = self.tint {
            t
        } else if self.selected {
            color::ACCENT
        } else if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        let style = if self.filled {
            Style::Fill
        } else {
            Style::Regular
        };
        let icon_center = if self.caption.is_some() {
            c - vec2(0.0, 6.0)
        } else {
            c
        };
        icons::paint(&painter, icon_center, self.glyph, self.icon_size, style, fg);
        if let Some(cap) = &self.caption {
            painter.text(
                c + vec2(0.0, self.icon_size * 0.55 + 2.0),
                Align2::CENTER_CENTER,
                cap,
                font::caption(),
                fg,
            );
        }
        if f.focused {
            paint::focus_ring_circle(&painter, c, r, 1.0);
        }
        f
    }
}

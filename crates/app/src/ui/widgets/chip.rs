// SPDX-License-Identifier: GPL-3.0-or-later
//! Chips, segmented controls, toggles and tabs (§6.6, §8.6).

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};

use crate::focus::{self, Focus, FocusOpts};
use crate::ui::motion;
use crate::ui::theme::{color, font, radius};
use crate::ui::{paint, text};

/// A filter chip: 36 pt pill, `surface-2` at rest, `cream` when selected.
#[derive(Debug, Clone)]
pub struct Chip {
    label: String,
    count: Option<usize>,
    selected: bool,
    id: Option<Id>,
    opts: FocusOpts,
}

impl Chip {
    /// A chip.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            count: None,
            selected: false,
            id: None,
            opts: FocusOpts::default(),
        }
    }

    /// Item count shown after the label.
    #[must_use]
    pub fn count(mut self, n: usize) -> Self {
        self.count = Some(n);
        self
    }

    /// Selected state.
    #[must_use]
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// Explicit id.
    #[must_use]
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus_if(mut self, on: bool) -> Self {
        self.opts = self.opts.autofocus_if(on);
        self
    }

    /// Adds the chip.
    pub fn show(self, ui: &mut Ui) -> Focus {
        let label = text::cap(&self.label, 48).into_owned();
        let painter = ui.painter().clone();
        let lw = text::width(&painter, &label, font::button());
        let count = self.count.map(|n| n.to_string());
        let cw = count
            .as_ref()
            .map_or(0.0, |c| 8.0 + text::width(&painter, c, font::caption()));
        let size = vec2((32.0 + lw + cw).round(), 36.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let id = self
            .id
            .unwrap_or_else(|| ui.make_persistent_id(("chip", &label)));
        let (_, f) = focus::focusable(ui, id, rect, self.opts);
        paint::focus_glow(&painter, rect, radius::FULL, f.anim);
        let (fill, fg, fg2) = if self.selected {
            (color::CREAM, color::TEXT_ON_ACCENT, color::TEXT_ON_ACCENT)
        } else if f.focused {
            (color::SURFACE_3, color::TEXT, color::TEXT_3)
        } else {
            (color::SURFACE_2, color::TEXT_2, color::TEXT_3)
        };
        painter.rect_filled(rect, CornerRadius::same(radius::FULL), fill);
        let x = rect.left() + 16.0;
        painter.text(
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            &label,
            font::button(),
            fg,
        );
        if let Some(c) = count {
            painter.text(
                pos2(x + lw + 8.0, rect.center().y),
                Align2::LEFT_CENTER,
                c,
                font::caption(),
                fg2,
            );
        }
        if f.focused {
            paint::focus_ring(&painter, rect, radius::FULL, 1.0);
        }
        f
    }
}

/// A segmented control (Xtream | M3U playlist): returns the new selection when changed.
#[derive(Debug)]
pub struct Segmented<'a> {
    id: Id,
    options: &'a [&'a str],
    width: f32,
    autofocus: bool,
}

impl<'a> Segmented<'a> {
    /// A control with `options`, `width` points wide.
    pub fn new(id: Id, options: &'a [&'a str], width: f32) -> Self {
        Self {
            id,
            options,
            width,
            autofocus: false,
        }
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Adds the control; `selected` is updated on activation. Returns true if changed.
    pub fn show(self, ui: &mut Ui, selected: &mut usize) -> bool {
        let n = self.options.len().max(1);
        let (rect, _) = ui.allocate_exact_size(vec2(self.width, 44.0), Sense::hover());
        let painter = ui.painter().clone();
        painter.rect_filled(rect, CornerRadius::same(radius::FULL), color::SURFACE_2);
        let inner = rect.shrink(4.0);
        let seg_w = inner.width() / n as f32;
        let target = *selected as f32;
        let pos = motion::animate_value(
            ui.ctx(),
            self.id.with("slide"),
            target,
            crate::ui::theme::motion::BASE,
        );
        let pill = Rect::from_min_size(
            pos2(inner.left() + seg_w * pos, inner.top()),
            vec2(seg_w, inner.height()),
        );
        painter.rect_filled(pill, CornerRadius::same(radius::FULL), color::CREAM);
        let mut changed = false;
        for (i, label) in self.options.iter().enumerate() {
            let seg = Rect::from_min_size(
                pos2(inner.left() + seg_w * i as f32, inner.top()),
                vec2(seg_w, inner.height()),
            );
            let opts = FocusOpts::default().autofocus_if(self.autofocus && i == *selected);
            let (_, f) = focus::focusable(ui, self.id.with(i), seg, opts);
            if f.activated && *selected != i {
                *selected = i;
                changed = true;
            }
            let fg = if i == *selected {
                color::TEXT_ON_ACCENT
            } else if f.focused {
                color::TEXT
            } else {
                color::TEXT_2
            };
            painter.text(
                seg.center(),
                Align2::CENTER_CENTER,
                *label,
                font::button(),
                fg,
            );
            if f.focused {
                paint::focus_ring(&painter, seg, radius::FULL, 1.0);
            }
        }
        changed
    }
}

/// A 52×30 toggle. Returns the focus state; `on` flips on activation.
#[derive(Debug)]
pub struct Toggle {
    id: Id,
}

impl Toggle {
    /// A toggle with an explicit id.
    pub fn new(id: Id) -> Self {
        Self { id }
    }

    /// Adds the toggle at the cursor.
    pub fn show(self, ui: &mut Ui, on: &mut bool) -> Focus {
        let (rect, _) = ui.allocate_exact_size(vec2(52.0, 30.0), Sense::hover());
        self.show_at(ui, rect, on)
    }

    /// Paints the toggle at `rect`.
    pub fn show_at(self, ui: &mut Ui, rect: Rect, on: &mut bool) -> Focus {
        let (_, f) = focus::focusable(ui, self.id, rect, FocusOpts::default());
        if f.activated {
            *on = !*on;
        }
        let painter = ui.painter().clone();
        let t = motion::animate_bool(
            ui.ctx(),
            self.id.with("knob"),
            *on,
            crate::ui::theme::motion::BASE,
        );
        paint::focus_glow(&painter, rect, radius::FULL, f.anim);
        let track = paint::lerp_color(color::SURFACE_4, color::ACCENT, t);
        painter.rect_filled(rect, CornerRadius::same(radius::FULL), track);
        let knob_r = 11.0;
        let x = egui::lerp(rect.left() + 4.0 + knob_r..=rect.right() - 4.0 - knob_r, t);
        let knob = paint::lerp_color(color::TEXT_2, color::TEXT_ON_ACCENT, t);
        painter.circle_filled(pos2(x, rect.center().y), knob_r, knob);
        if f.focused {
            paint::focus_ring(&painter, rect, radius::FULL, 1.0);
        }
        f
    }
}

/// Text tabs (seasons): selected = `text` + 3 pt accent underline.
#[derive(Debug)]
pub struct Tabs<'a> {
    id: Id,
    labels: &'a [String],
}

impl<'a> Tabs<'a> {
    /// Tabs with labels.
    pub fn new(id: Id, labels: &'a [String]) -> Self {
        Self { id, labels }
    }

    /// Adds the tabs; returns true if the selection changed.
    pub fn show(self, ui: &mut Ui, selected: &mut usize) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 24.0;
            for (i, label) in self.labels.iter().enumerate() {
                let painter = ui.painter().clone();
                let w = text::width(&painter, label, font::button());
                let (rect, _) = ui.allocate_exact_size(vec2(w + 8.0, 40.0), Sense::hover());
                let (_, f) = focus::focusable(ui, self.id.with(i), rect, FocusOpts::default());
                if f.activated && *selected != i {
                    *selected = i;
                    changed = true;
                }
                let sel = *selected == i;
                let fg = if sel || f.focused {
                    color::TEXT
                } else {
                    color::TEXT_2
                };
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    font::button(),
                    fg,
                );
                if sel {
                    let bar = Rect::from_center_size(
                        pos2(rect.center().x, rect.center().y + 16.0),
                        vec2(w, 3.0),
                    );
                    painter.rect_filled(bar, CornerRadius::same(2), color::ACCENT);
                }
                if f.focused {
                    paint::focus_ring(&painter, rect, radius::SM, 1.0);
                }
            }
        });
        changed
    }
}

/// Paints a small pill tag (`HD`, `4K`) with a `stroke` outline. Returns its rect.
pub fn tag(painter: &egui::Painter, pos: egui::Pos2, label: &str) -> Rect {
    let job = text::overline_job(label, color::TEXT_2);
    let g = painter.layout_job(job);
    let rect = Rect::from_min_size(pos, vec2(g.size().x + 14.0, 20.0));
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::FULL),
        egui::Stroke::new(1.0, color::STROKE),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        pos2(rect.left() + 7.0, rect.center().y - g.size().y / 2.0),
        g,
        Color32::WHITE,
    );
    rect
}

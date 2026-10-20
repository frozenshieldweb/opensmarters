// SPDX-License-Identifier: GPL-3.0-or-later
//! Lists: [`VirtualList`] (only visible rows are built; Up/Down handled by index so the
//! focus can move to rows that are not rendered yet), [`MenuRow`] (category / settings
//! section rows) and [`SettingRow`] (label + description + control).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Rect, ScrollArea, Sense, Ui, Vec2, pos2, vec2,
};

use crate::focus::{self, Axis, Dir, Focus, FocusOpts};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::{paint, text};

#[derive(Debug, Clone, Copy, Default)]
struct ListState {
    focused: Option<usize>,
    offset: f32,
    viewport: f32,
}

/// A virtualised vertical list of fixed-height rows with keyboard navigation.
///
/// ```ignore
/// VirtualList::new(Id::unique("live.channels"), 64.0, channels.len())
///     .gap(8.0)
///     .show(ui, |i| row_id(i), |ui, i, rect| { /* paint row i at rect, register focus */ });
/// ```
/// The rows must register with [`crate::focus::register`] using the id given by
/// `item_id(i)` (the list is the focus scope).
#[derive(Debug)]
pub struct VirtualList {
    id: Id,
    row_height: f32,
    gap: f32,
    len: usize,
    scroll_to: Option<usize>,
    padding: Vec2,
}

impl VirtualList {
    /// A list (also its focus scope id).
    pub fn new(id: Id, row_height: f32, len: usize) -> Self {
        Self {
            id,
            row_height,
            gap: 0.0,
            len,
            scroll_to: None,
            padding: vec2(8.0, 8.0),
        }
    }

    /// Gap between rows.
    #[must_use]
    pub fn gap(mut self, g: f32) -> Self {
        self.gap = g;
        self
    }

    /// Inner padding (paint allowance for the focus ring).
    #[must_use]
    pub fn padding(mut self, p: Vec2) -> Self {
        self.padding = p;
        self
    }

    /// Scrolls so row `i` is visible this frame (e.g. the playing channel on entry).
    #[must_use]
    pub fn scroll_to(mut self, i: Option<usize>) -> Self {
        self.scroll_to = i;
        self
    }

    /// The index of the focused row of list `id`, as last seen.
    pub fn focused_index(ctx: &egui::Context, id: Id) -> Option<usize> {
        ctx.data(|d| d.get_temp::<ListState>(id.with("state")))
            .and_then(|s| s.focused)
    }

    /// Adds the list.
    pub fn show(
        self,
        ui: &mut Ui,
        item_id: impl Fn(usize) -> Id,
        mut add_row: impl FnMut(&mut Ui, usize, Rect),
    ) {
        let ctx = ui.ctx().clone();
        let state_id = self.id.with("state");
        let mut state: ListState = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
        let stride = self.row_height + self.gap;
        let mut target = self.scroll_to;

        // Keyboard: Up/Down by index (rows may not be rendered yet).
        if self.len > 0
            && let Some(dir) = focus::take_nav(&ctx, self.id, Axis::Vertical)
        {
            let cur = state.focused.unwrap_or(0);
            let next = match dir {
                Dir::Up => cur.checked_sub(1),
                Dir::Down => (cur + 1 < self.len).then_some(cur + 1),
                Dir::Left | Dir::Right => None,
            };
            match next {
                Some(n) => {
                    focus::request(&ctx, item_id(n));
                    state.focused = Some(n);
                    target = Some(n);
                }
                None => focus::give_back_nav(&ctx, dir),
            }
        }
        // PageUp/PageDown move by a page while the focus is in the list.
        if focus::is_in_scope(&ctx, self.id) && !focus::text_input_focused(&ctx) && self.len > 0 {
            let page = ((state.viewport / stride).floor() as usize).max(1);
            let (up, down) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
                )
            });
            let cur = state.focused.unwrap_or(0);
            let next = if up {
                Some(cur.saturating_sub(page))
            } else if down {
                Some((cur + page).min(self.len - 1))
            } else {
                None
            };
            if let Some(n) = next {
                focus::request(&ctx, item_id(n));
                state.focused = Some(n);
                target = Some(n);
            }
        }

        let mut area = ScrollArea::vertical()
            .id_salt(self.id.with("scroll"))
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded);
        if let Some(t) = target {
            let top = t as f32 * stride;
            let bottom = top + self.row_height + self.padding.y * 2.0;
            let view = state.viewport.max(stride);
            let mut off = state.offset;
            if top < off + stride * 0.5 {
                off = (top - stride * 0.5).max(0.0);
            } else if bottom > off + view - stride * 0.5 {
                off = bottom - view + stride * 0.5;
            }
            if (off - state.offset).abs() > 0.5 {
                area = area.vertical_scroll_offset(off.max(0.0));
            }
        }
        let pad = self.padding;
        area = area.content_margin(egui::Margin {
            left: pad.x as i8,
            right: pad.x as i8,
            top: pad.y as i8,
            bottom: pad.y as i8,
        });
        let output = focus::scope(ui, self.id, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            area.show_rows(ui, stride, self.len, |ui, range| {
                let w = ui.available_width();
                for i in range {
                    let (rect, _) = ui.allocate_exact_size(vec2(w, stride), Sense::hover());
                    let row = Rect::from_min_size(rect.min, vec2(w, self.row_height));
                    add_row(ui, i, row);
                    if focus::is_focused(ui.ctx(), item_id(i)) {
                        state.focused = Some(i);
                    }
                }
            })
        });
        focus::forward_scroll(ui);
        state.offset = output.state.clamped_offset().y;
        state.viewport = output.inner_rect.height();
        ctx.data_mut(|d| d.insert_temp(state_id, state));
    }
}

/// A 48 pt menu row (categories, settings sections): label + optional count; selected =
/// 3 pt cream bar + `text`; focus = `surface-3` + ring.
#[derive(Debug)]
pub struct MenuRow<'a> {
    id: Id,
    label: &'a str,
    count: Option<String>,
    selected: bool,
    icon: Option<&'static str>,
    locked: bool,
    height: f32,
    autofocus: bool,
}

impl<'a> MenuRow<'a> {
    /// A row.
    pub fn new(id: Id, label: &'a str) -> Self {
        Self {
            id,
            label,
            count: None,
            selected: false,
            icon: None,
            locked: false,
            height: 48.0,
            autofocus: false,
        }
    }

    /// Count on the right.
    #[must_use]
    pub fn count(mut self, n: Option<usize>) -> Self {
        self.count = n.map(|n| n.to_string());
        self
    }

    /// Selected.
    #[must_use]
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    /// Leading icon.
    #[must_use]
    pub fn icon(mut self, g: &'static str) -> Self {
        self.icon = Some(g);
        self
    }

    /// Shows a lock icon (parental control).
    #[must_use]
    pub fn locked(mut self, on: bool) -> Self {
        self.locked = on;
        self
    }

    /// Row height.
    #[must_use]
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Adds the row (full available width).
    pub fn show(self, ui: &mut Ui) -> Focus {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        self.show_at(ui, rect)
    }

    /// Paints the row at `rect`.
    pub fn show_at(self, ui: &mut Ui, rect: Rect) -> Focus {
        let (_, f) = focus::focusable(
            ui,
            self.id,
            rect,
            FocusOpts::default().autofocus_if(self.autofocus),
        );
        let painter = ui.painter().clone();
        if f.focused {
            painter.rect_filled(rect, CornerRadius::same(radius::SM), color::SURFACE_3);
        }
        if self.selected {
            let bar = Rect::from_min_size(
                pos2(rect.left() - 12.0, rect.center().y - 12.0),
                vec2(3.0, 24.0),
            );
            painter.rect_filled(bar, CornerRadius::same(2), color::CREAM);
        }
        let fg = if self.selected || f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        let mut x = rect.left() + 12.0;
        if let Some(g) = self.icon {
            icons::paint(
                &painter,
                pos2(x + 10.0, rect.center().y),
                g,
                20.0,
                Style::Regular,
                fg,
            );
            x += 32.0;
        }
        let right_w =
            if self.count.is_some() { 48.0 } else { 0.0 } + if self.locked { 24.0 } else { 0.0 };
        let font = font::body_strong();
        text::line(
            &painter,
            pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            self.label,
            font,
            fg,
            rect.right() - 12.0 - right_w - x,
        );
        let mut rx = rect.right() - 12.0;
        if let Some(c) = &self.count {
            let r = painter.text(
                pos2(rx, rect.center().y),
                Align2::RIGHT_CENTER,
                c,
                font::caption(),
                color::TEXT_3,
            );
            rx = r.left() - 8.0;
        }
        if self.locked {
            icons::paint(
                &painter,
                pos2(rx - 8.0, rect.center().y),
                icons::LOCK_SIMPLE,
                16.0,
                Style::Regular,
                color::TEXT_3,
            );
        }
        if f.focused {
            paint::focus_ring(&painter, rect, radius::SM, 1.0);
        }
        f
    }
}

/// A settings row: label + description on the left, a control on the right (§8.6).
///
/// The text column takes what the control leaves (the control's width is remembered from
/// the previous frame); the description wraps to up to three lines and the row grows
/// with it (56 pt minimum, 68 with a description), so nothing is cut off in TV mode.
#[derive(Debug)]
pub struct SettingRow<'a> {
    label: &'a str,
    description: Option<&'a str>,
}

/// Vertical padding above and below the text of a [`SettingRow`].
const SETTING_ROW_PAD: f32 = 12.0;

impl<'a> SettingRow<'a> {
    /// A row.
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            description: None,
        }
    }

    /// Description line.
    #[must_use]
    pub fn description(mut self, d: &'a str) -> Self {
        self.description = Some(d);
        self
    }

    /// The width of the text column for a row `w` wide whose control is `control_w`
    /// wide: what the control leaves (24 pt apart), but never under 40 % of the row.
    pub fn text_width(w: f32, control_w: f32) -> f32 {
        (w - control_w - 24.0).clamp((w * 0.4).min(w), w.max(0.0))
    }

    /// Adds the row; `control` is laid out right-aligned in the row.
    pub fn show<R>(self, ui: &mut Ui, control: impl FnOnce(&mut Ui) -> R) -> R {
        let w = ui.available_width();
        let mem = ui.scope_id().with(("setting-row.control-w", self.label));
        let control_w: f32 = ui
            .ctx()
            .data(|d| d.get_temp(mem))
            .unwrap_or(w * 0.45 - 24.0);
        let text_w = Self::text_width(w, control_w);
        let painter = ui.painter().clone();
        let label = text::galley(
            &painter,
            self.label,
            font::body_strong(),
            color::TEXT,
            text_w,
            2,
        );
        let desc = self
            .description
            .map(|d| text::galley(&painter, d, font::caption(), color::TEXT_3, text_w, 3));
        let text_h = label.size().y + desc.as_ref().map_or(0.0, |g| 2.0 + g.size().y);
        let min_h = if desc.is_some() { 68.0 } else { 56.0 };
        let h = (text_h + 2.0 * SETTING_ROW_PAD).max(min_h).ceil();
        let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let top = rect.center().y - text_h / 2.0;
        let label_h = label.size().y;
        painter.galley(pos2(rect.left(), top), label, color::TEXT);
        if let Some(g) = desc {
            painter.galley(pos2(rect.left(), top + label_h + 2.0), g, color::TEXT_3);
        }
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        let ctrl_rect = Rect::from_min_max(
            pos2(rect.left() + text_w + 16.0, rect.top()),
            pos2(rect.right() - 6.0, rect.bottom()),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(ctrl_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let out = control(&mut child);
        // Remember how wide the control is for the next frame's text column.
        let used = child.min_rect().width().ceil();
        if (used - control_w).abs() > 0.5 {
            ui.ctx().data_mut(|d| d.insert_temp(mem, used));
            ui.ctx().request_repaint();
        }
        let _ = Color32::TRANSPARENT;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_row_text_takes_what_the_control_leaves() {
        // Wide row, small toggle: the text gets almost everything.
        assert_eq!(SettingRow::text_width(700.0, 52.0), 624.0);
        // A wide segmented control in TV mode: the text keeps 40 %.
        assert_eq!(SettingRow::text_width(600.0, 500.0), 240.0);
        // Degenerate widths never panic.
        assert_eq!(SettingRow::text_width(0.0, 100.0), 0.0);
    }
}

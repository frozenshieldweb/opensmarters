// SPDX-License-Identifier: GPL-3.0-or-later
//! Text fields (§6.7): an egui `TextEdit` inside an Afterglow frame, integrated with the
//! focus engine (keyboard focus follows the one focus; Left/Right edit the text, Right at
//! the end of the text moves on to the reveal button).

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Id, Key, Margin, Rect, Sense, Stroke,
    StrokeKind, TextEdit, Ui, pos2, vec2,
};

use crate::focus::{self, Dir, Focus, FocusOpts};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::IconButton;
use crate::ui::{paint, text};

/// What a text field reports.
#[derive(Debug, Clone, Copy)]
pub struct TextFieldResponse {
    /// Focus state of the text input.
    pub focus: Focus,
    /// The text changed this frame.
    pub changed: bool,
    /// Enter was pressed in the field.
    pub submitted: bool,
}

/// A labelled single-line text field (48 pt).
#[derive(Debug)]
pub struct TextField<'a> {
    id: Id,
    label: &'a str,
    placeholder: &'a str,
    password: bool,
    error: Option<&'a str>,
    width: f32,
    autofocus: bool,
    char_limit: usize,
}

impl<'a> TextField<'a> {
    /// A field with an explicit id and a label (shown above it).
    pub fn new(id: Id, label: &'a str) -> Self {
        Self {
            id,
            label,
            placeholder: "",
            password: false,
            error: None,
            width: 440.0,
            autofocus: false,
            char_limit: 2048,
        }
    }

    /// Placeholder (`text-3`).
    #[must_use]
    pub fn placeholder(mut self, p: &'a str) -> Self {
        self.placeholder = p;
        self
    }

    /// Masks the text, with a reveal toggle.
    #[must_use]
    pub fn password(mut self, on: bool) -> Self {
        self.password = on;
        self
    }

    /// Error message (danger border + caption).
    #[must_use]
    pub fn error(mut self, e: Option<&'a str>) -> Self {
        self.error = e;
        self
    }

    /// Width in points.
    #[must_use]
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Maximum characters.
    #[must_use]
    pub fn char_limit(mut self, n: usize) -> Self {
        self.char_limit = n;
        self
    }

    /// Adds the field.
    pub fn show(self, ui: &mut Ui, value: &mut String) -> TextFieldResponse {
        let painter = ui.painter().clone();
        if !self.label.is_empty() {
            let (lr, _) = ui.allocate_exact_size(vec2(self.width, 18.0), Sense::hover());
            painter.text(
                lr.left_bottom(),
                Align2::LEFT_BOTTOM,
                self.label,
                font::caption(),
                color::TEXT_2,
            );
            ui.add_space(6.0 - ui.spacing().item_spacing.y);
        }
        let (rect, _) = ui.allocate_exact_size(vec2(self.width, 48.0), Sense::hover());
        let te_id = self.id.with("te");
        let focused_now = focus::is_focused(ui.ctx(), te_id);
        let border = if self.error.is_some() {
            Stroke::new(2.0, color::DANGER)
        } else if focused_now {
            Stroke::new(2.0, color::ACCENT)
        } else {
            Stroke::new(1.5, color::STROKE)
        };
        if focused_now {
            paint::focus_glow(&painter, rect, radius::SM, 1.0);
        }
        painter.rect(
            rect,
            CornerRadius::same(radius::SM),
            color::SURFACE_2,
            border,
            StrokeKind::Inside,
        );

        let reveal_id = self.id.with("reveal");
        let mut revealed = ui
            .ctx()
            .data(|d| d.get_temp::<bool>(reveal_id))
            .unwrap_or(false);
        let reveal_w = if self.password { 44.0 } else { 0.0 };
        let inner = Rect::from_min_max(
            rect.min + vec2(16.0, 0.0),
            pos2(rect.right() - 12.0 - reveal_w, rect.bottom()),
        );

        // Right at the end of the text moves the focus on (to the reveal button).
        let prev_cursor = TextEdit::load_state(ui.ctx(), te_id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index.0);
        let at_end = prev_cursor.is_none_or(|c| c >= value.chars().count());
        let at_start = prev_cursor.is_none_or(|c| c == 0);

        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        let output = TextEdit::singleline(value)
            .id(te_id)
            .frame(Frame::NONE)
            .margin(Margin::ZERO)
            .font(font::body())
            .text_color(color::TEXT)
            .hint_text(
                egui::RichText::new(self.placeholder)
                    .color(color::TEXT_3)
                    .font(font::body()),
            )
            .password(self.password && !revealed)
            .desired_width(inner.width())
            .align(Align2::LEFT_CENTER)
            .char_limit(self.char_limit)
            .show(&mut child);
        let resp = output.response.response;
        let f = focus::register(
            ui,
            &resp,
            FocusOpts::default()
                .text()
                .autofocus_if(self.autofocus)
                .scroll_margin(48.0),
        );
        // The one focus drives egui's keyboard focus.
        if f.focused && !resp.has_focus() {
            resp.request_focus();
        } else if !f.focused && resp.has_focus() {
            resp.surrender_focus();
        }
        let submitted = f.focused && ui.input(|i| i.key_pressed(Key::Enter));
        if f.focused {
            if at_end && ui.input(|i| i.key_pressed(Key::ArrowRight)) {
                focus::give_back_nav(ui.ctx(), Dir::Right);
            } else if at_start && ui.input(|i| i.key_pressed(Key::ArrowLeft)) {
                focus::give_back_nav(ui.ctx(), Dir::Left);
            }
        }

        if self.password {
            let br = Rect::from_center_size(
                pos2(rect.right() - 28.0, rect.center().y),
                vec2(36.0, 36.0),
            );
            let glyph = if revealed {
                icons::EYE_SLASH
            } else {
                icons::EYE
            };
            let rf = IconButton::new(glyph)
                .size(36.0, 20.0)
                .id(reveal_id.with("btn"))
                .show_at(ui, br);
            if rf.activated {
                revealed = !revealed;
                ui.ctx().data_mut(|d| d.insert_temp(reveal_id, revealed));
            }
        }

        if let Some(err) = self.error {
            let (er, _) = ui.allocate_exact_size(vec2(self.width, 20.0), Sense::hover());
            icons::paint(
                &painter,
                pos2(er.left() + 8.0, er.center().y),
                icons::WARNING,
                14.0,
                Style::Regular,
                color::DANGER,
            );
            text::line(
                &painter,
                pos2(er.left() + 20.0, er.center().y),
                Align2::LEFT_CENTER,
                err,
                font::caption(),
                color::DANGER,
                self.width - 20.0,
            );
        }
        TextFieldResponse {
            focus: f,
            changed: resp.changed(),
            submitted,
        }
    }
}

/// A 52 pt pill search field with a leading magnifier and a clear button.
#[derive(Debug)]
pub struct SearchField<'a> {
    id: Id,
    placeholder: &'a str,
    width: f32,
    height: f32,
    autofocus: bool,
}

impl<'a> SearchField<'a> {
    /// A search field.
    pub fn new(id: Id, placeholder: &'a str) -> Self {
        Self {
            id,
            placeholder,
            width: 320.0,
            height: 52.0,
            autofocus: false,
        }
    }

    /// Size.
    #[must_use]
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Preferred initial focus.
    #[must_use]
    pub fn autofocus(mut self, on: bool) -> Self {
        self.autofocus = on;
        self
    }

    /// Adds the field.
    pub fn show(self, ui: &mut Ui, value: &mut String) -> TextFieldResponse {
        let (rect, _) = ui.allocate_exact_size(vec2(self.width, self.height), Sense::hover());
        let painter = ui.painter().clone();
        let te_id = self.id.with("te");
        let focused_now = focus::is_focused(ui.ctx(), te_id);
        if focused_now {
            paint::focus_glow(&painter, rect, radius::FULL, 1.0);
        }
        let border = if focused_now {
            Stroke::new(2.0, color::ACCENT)
        } else {
            Stroke::new(1.5, color::STROKE)
        };
        painter.rect(
            rect,
            CornerRadius::same(radius::FULL),
            color::SURFACE_1,
            border,
            StrokeKind::Inside,
        );
        icons::paint(
            &painter,
            pos2(rect.left() + 24.0, rect.center().y),
            icons::MAGNIFYING_GLASS,
            18.0,
            Style::Regular,
            color::TEXT_3,
        );
        let clear_w = if value.is_empty() { 0.0 } else { 40.0 };
        let inner = Rect::from_min_max(
            pos2(rect.left() + 44.0, rect.top()),
            pos2(rect.right() - 16.0 - clear_w, rect.bottom()),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        let output = TextEdit::singleline(value)
            .id(te_id)
            .frame(Frame::NONE)
            .margin(Margin::ZERO)
            .font(font::body())
            .text_color(color::TEXT)
            .hint_text(
                egui::RichText::new(self.placeholder)
                    .color(color::TEXT_3)
                    .font(font::body()),
            )
            .desired_width(inner.width())
            .align(Align2::LEFT_CENTER)
            .char_limit(200)
            .show(&mut child);
        let resp = output.response.response;
        let f = focus::register(
            ui,
            &resp,
            FocusOpts::default().text().autofocus_if(self.autofocus),
        );
        if f.focused && !resp.has_focus() {
            resp.request_focus();
        } else if !f.focused && resp.has_focus() {
            resp.surrender_focus();
        }
        let mut changed = resp.changed();
        if !value.is_empty() {
            let br = Rect::from_center_size(
                pos2(rect.right() - 30.0, rect.center().y),
                vec2(32.0, 32.0),
            );
            let cf = IconButton::new(icons::X)
                .size(32.0, 16.0)
                .id(self.id.with("clear"))
                .show_at(ui, br);
            if cf.activated {
                value.clear();
                changed = true;
                focus::request(ui.ctx(), te_id);
            }
        }
        let submitted = f.focused && ui.input(|i| i.key_pressed(Key::Enter));
        let _ = Color32::TRANSPARENT;
        TextFieldResponse {
            focus: f,
            changed,
            submitted,
        }
    }
}

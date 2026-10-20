// SPDX-License-Identifier: GPL-3.0-or-later
//! The parental PIN pad (§6.14): 4 dots, a 3×4 keypad of 72 pt keys, digits from the
//! keyboard/remote, wrong-PIN shake, lockout countdown.
//!
//! The pad only collects digits; the caller verifies them off the UI thread with
//! `store.parental().verify_pin` and reports the outcome back with
//! [`PinPad::set_wrong`] / [`PinPad::set_locked_until`].
//!
//! PINs are 4–8 digits. To check a PIN, create the pad with the stored length
//! (`session.pin_length()`), so it submits on the last digit. To *choose* a PIN, use
//! [`PinPad::variable`]: it shows one more dot than typed (at least 4), submits at 8 digits,
//! and otherwise waits for the caller's "Continue" button ([`PinPad::submit`], registered
//! with [`PinPad::submit_id`] so typing on the keyboard moves the focus to it).

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2};
use opensmarters_core::secrecy::SecretString;

use crate::focus::{self, FocusOpts};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::{motion, paint};

/// What the pad reports.
#[derive(Debug)]
pub enum PinPadEvent {
    /// Nothing yet.
    None,
    /// The PIN was entered (4 digits by default). Verify it.
    Submitted(SecretString),
}

/// PIN pad state.
#[derive(Debug)]
pub struct PinPad {
    id: Id,
    digits: String,
    length: usize,
    min: usize,
    shake_at: Option<Instant>,
    message: Option<String>,
    locked_until: Option<Instant>,
    busy: bool,
}

impl PinPad {
    /// A pad for a `length`-digit PIN (4–8).
    pub fn new(id: Id, length: usize) -> Self {
        Self {
            id,
            digits: String::new(),
            length: length.clamp(4, 8),
            min: length.clamp(4, 8),
            shake_at: None,
            message: None,
            locked_until: None,
            busy: false,
        }
    }

    /// A pad for choosing a PIN of `min`–`max` digits (4–8): submits at `max` digits or via
    /// [`PinPad::submit`].
    pub fn variable(id: Id, min: usize, max: usize) -> Self {
        let min = min.clamp(4, 8);
        let mut pad = Self::new(id, max.clamp(min, 8));
        pad.min = min;
        pad
    }

    /// Digits typed so far.
    pub fn entered(&self) -> usize {
        self.digits.len()
    }

    /// True if [`PinPad::submit`] would submit (enough digits, not busy or locked).
    pub fn can_submit(&self) -> bool {
        !self.busy && self.locked_remaining().is_none() && self.digits.len() >= self.min
    }

    /// Submits the digits typed so far (variable pads), if [`PinPad::can_submit`].
    pub fn submit(&mut self) -> Option<SecretString> {
        if !self.can_submit() {
            return None;
        }
        self.busy = true;
        Some(SecretString::from(std::mem::take(&mut self.digits)))
    }

    /// The id to give the caller's "Continue" button (variable pads move the focus to it
    /// once enough digits were typed on the keyboard).
    pub fn submit_id(&self) -> Id {
        self.id.with("submit")
    }

    /// True while a lockout is running.
    pub fn is_locked(&self) -> bool {
        self.locked_remaining().is_some()
    }

    /// Reports a wrong PIN ("Wrong PIN. 2 attempts left").
    pub fn set_wrong(&mut self, message: impl Into<String>) {
        self.digits.clear();
        self.shake_at = Some(Instant::now());
        self.message = Some(message.into());
        self.busy = false;
    }

    /// Reports a lockout ending in `remaining`.
    pub fn set_locked_until(&mut self, remaining: Duration) {
        self.digits.clear();
        self.locked_until = Some(Instant::now() + remaining);
        self.busy = false;
    }

    /// Marks verification as running (keys disabled).
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// Clears the entry and messages.
    pub fn reset(&mut self) {
        self.digits.clear();
        self.message = None;
        self.shake_at = None;
        self.busy = false;
    }

    fn locked_remaining(&self) -> Option<Duration> {
        self.locked_until
            .and_then(|t| t.checked_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }

    fn push(&mut self, d: char) -> PinPadEvent {
        if self.busy || self.locked_remaining().is_some() || self.digits.len() >= self.length {
            return PinPadEvent::None;
        }
        self.digits.push(d);
        self.message = None;
        if self.digits.len() == self.length {
            self.busy = true;
            return PinPadEvent::Submitted(SecretString::from(std::mem::take(&mut self.digits)));
        }
        PinPadEvent::None
    }

    /// Adds the pad (360 pt wide). `title` is shown above the dots.
    pub fn show(&mut self, ui: &mut Ui, title: &str) -> PinPadEvent {
        let ctx = ui.ctx().clone();
        let mut event = PinPadEvent::None;
        // Digits and Backspace from the keyboard / remote.
        let typed: Vec<char> = ctx.input_mut(|i| {
            let mut v = Vec::new();
            for (k, c) in [
                (Key::Num0, '0'),
                (Key::Num1, '1'),
                (Key::Num2, '2'),
                (Key::Num3, '3'),
                (Key::Num4, '4'),
                (Key::Num5, '5'),
                (Key::Num6, '6'),
                (Key::Num7, '7'),
                (Key::Num8, '8'),
                (Key::Num9, '9'),
            ] {
                if i.consume_key(Modifiers::NONE, k) {
                    v.push(c);
                }
            }
            v
        });
        let typed_any = !typed.is_empty();
        for c in typed {
            if let PinPadEvent::Submitted(s) = self.push(c) {
                event = PinPadEvent::Submitted(s);
            }
        }
        if typed_any && self.min < self.length && self.digits.len() >= self.min {
            focus::request(&ctx, self.submit_id());
        }

        let width = 360.0;
        let painter = ui.painter().clone();
        let (head, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
        painter.text(
            head.center(),
            Align2::CENTER_CENTER,
            title,
            font::title(),
            color::TEXT,
        );

        // Dots with the wrong-PIN shake.
        let (dots_rect, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
        let mut dx = 0.0;
        if let Some(t) = self.shake_at {
            let e = t.elapsed().as_secs_f32();
            if e < 0.3 && !motion::reduced(&ctx) {
                dx = (e / 0.3 * std::f32::consts::TAU * 3.0).sin() * 8.0 * (1.0 - e / 0.3);
                ctx.request_repaint();
            }
        }
        let wrong = self.message.is_some();
        let dots = if self.min < self.length {
            (self.digits.len() + 1).clamp(self.min, self.length)
        } else {
            self.length
        };
        let n = dots as f32;
        let total = n * 14.0 + (n - 1.0) * 16.0;
        for i in 0..dots {
            let c = pos2(
                dots_rect.center().x - total / 2.0 + 7.0 + i as f32 * 30.0 + dx,
                dots_rect.center().y,
            );
            if i < self.digits.len() {
                painter.circle_filled(c, 7.0, color::CREAM);
            } else {
                let stroke = if wrong { color::DANGER } else { color::TEXT_3 };
                painter.circle_stroke(c, 6.25, egui::Stroke::new(1.5, stroke));
            }
        }
        let (msg_rect, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
        if let Some(rem) = self.locked_remaining() {
            painter.text(
                msg_rect.center(),
                Align2::CENTER_CENTER,
                crate::services::parental::lockout_text(rem),
                font::caption(),
                color::WARNING,
            );
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if let Some(m) = &self.message {
            painter.text(
                msg_rect.center(),
                Align2::CENTER_CENTER,
                m,
                font::caption(),
                color::DANGER,
            );
        }
        ui.add_space(12.0);

        // Keypad 3×4.
        let disabled = self.busy || self.locked_remaining().is_some();
        let keys = [
            "1", "2", "3", "4", "5", "6", "7", "8", "9", "clear", "0", "back",
        ];
        let (grid, _) =
            ui.allocate_exact_size(vec2(width, 4.0 * 72.0 + 3.0 * 16.0), Sense::hover());
        let col_w = 72.0 + 24.0;
        let left = grid.center().x - (3.0 * 72.0 + 2.0 * 24.0) / 2.0;
        focus::scope(ui, self.id.with("keys"), |ui| {
            for (i, k) in keys.iter().enumerate() {
                let (row, col) = (i / 3, i % 3);
                let c = pos2(
                    left + 36.0 + col as f32 * col_w,
                    grid.top() + 36.0 + row as f32 * 88.0,
                );
                let rect = Rect::from_center_size(c, vec2(72.0, 72.0));
                let opts = FocusOpts::default().autofocus_if(*k == "1");
                let (_, f) = focus::focusable(ui, self.id.with(("key", i)), rect, opts);
                let painter = ui.painter().clone();
                paint::focus_glow(&painter, rect, radius::FULL, f.anim);
                let fill = if f.focused {
                    color::SURFACE_3
                } else {
                    color::SURFACE_2
                };
                painter.rect_filled(
                    rect,
                    CornerRadius::same(radius::FULL),
                    fill.gamma_multiply(if disabled { 0.4 } else { 1.0 }),
                );
                let fg = if disabled {
                    color::TEXT_DISABLED
                } else {
                    color::TEXT
                };
                match *k {
                    "clear" => {
                        painter.text(c, Align2::CENTER_CENTER, "Clear", font::button(), fg);
                    }
                    "back" => {
                        icons::paint(&painter, c, icons::BACKSPACE, 24.0, Style::Regular, fg);
                    }
                    d => {
                        painter.text(c, Align2::CENTER_CENTER, d, font::heading(), fg);
                    }
                }
                if f.focused {
                    paint::focus_ring(&painter, rect, radius::FULL, 1.0);
                }
                if f.activated && !disabled {
                    match *k {
                        "clear" => self.digits.clear(),
                        "back" => {
                            self.digits.pop();
                        }
                        d => {
                            if let Some(ch) = d.chars().next()
                                && let PinPadEvent::Submitted(s) = self.push(ch)
                            {
                                event = PinPadEvent::Submitted(s);
                            }
                        }
                    }
                }
            }
        });
        event
    }
}

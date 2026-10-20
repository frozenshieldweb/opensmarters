// SPDX-License-Identifier: GPL-3.0-or-later
//! A self-contained "enter your PIN" prompt: loads the PIN state first (so a running
//! lockout shows "Try again in 0:27" at once), shows a [`PinPad`] with the stored PIN
//! length, verifies off the UI thread with `store.parental().verify_pin` (Argon2id,
//! rate-limited by the store) and reports the outcome.
//!
//! Use it to protect an action while parental control is on (Settings › Parental, adding /
//! editing / deleting profiles, importing a backup). When no PIN is set it reports
//! [`PinPromptResult::Verified`] immediately, so callers can always go through it.
//!
//! ```ignore
//! // field: gate: Option<PinPrompt>
//! if let Some(g) = &mut self.gate {
//!     match g.show_dialog(ui.ctx(), cx.services, "Enter your PIN", None) {
//!         PinPromptResult::Verified(_) => { self.gate = None; /* do the action */ }
//!         PinPromptResult::Cancelled => self.gate = None,
//!         _ => {}
//!     }
//! }
//! ```

use eframe::egui::{self, Color32, Context, Id, Sense, Ui, vec2};
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{PinOutcome, PinStatus, StoreError, Timestamp};

use super::{Button, Dialog, PinPad, PinPadEvent};
use crate::error::AppError;
use crate::services::Services;
use crate::services::parental::{lockout_text, pin_length, remaining_until, wrong_pin_text};
use crate::task::Loader;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::icons;
use crate::ui::theme::{color, font};

/// What the prompt reports each frame.
#[derive(Debug)]
pub enum PinPromptResult {
    /// Still waiting for the PIN (or checking it).
    Pending,
    /// The PIN was correct (`Some(pin)`), or no PIN is set (`None`).
    Verified(Option<SecretString>),
    /// The user cancelled.
    Cancelled,
    /// The user chose "Forgot your PIN?" (only offered with [`PinPrompt::with_forgot`]).
    Forgot,
}

/// The prompt state. Keep it in the screen while the prompt is open.
#[derive(Debug)]
pub struct PinPrompt {
    id: Id,
    status: Loader<(PinStatus, usize)>,
    pad: Option<PinPad>,
    verify: Loader<PinOutcome>,
    checking: Option<SecretString>,
    forgot: bool,
    started: bool,
}

impl PinPrompt {
    /// A prompt; `id` scopes its widgets.
    pub fn new(id: Id) -> Self {
        Self {
            id,
            status: Loader::new(),
            pad: None,
            verify: Loader::new(),
            checking: None,
            forgot: false,
            started: false,
        }
    }

    /// Offers a "Forgot your PIN?" button ([`PinPromptResult::Forgot`]).
    #[must_use]
    pub fn with_forgot(mut self) -> Self {
        self.forgot = true;
        self
    }

    fn start(&mut self, services: &Services) {
        if self.started {
            return;
        }
        self.started = true;
        let store = services.store.clone();
        self.status
            .start_blocking(services.rt(), services.egui(), move || {
                let status = store.parental().status().map_err(AppError::from)?;
                Ok((status, pin_length(&store)))
            });
    }

    /// Processes finished background work.
    fn poll(&mut self, services: &Services) -> Option<PinPromptResult> {
        self.start(services);
        if self.status.poll()
            && let Some((status, len)) = self.status.value().copied()
        {
            if !status.is_set {
                return Some(PinPromptResult::Verified(None));
            }
            let mut pad = PinPad::new(self.id.with("pad"), len);
            if let Some(until) = status.locked_until {
                pad.set_locked_until(remaining_until(until, Timestamp::now()));
            }
            self.pad = Some(pad);
        }
        if self.verify.poll() {
            match self.verify.take() {
                Some(PinOutcome::Ok) => {
                    return Some(PinPromptResult::Verified(self.checking.take()));
                }
                Some(PinOutcome::Wrong {
                    remaining_before_lockout,
                }) => {
                    self.checking = None;
                    if let Some(p) = &mut self.pad {
                        p.set_wrong(wrong_pin_text(remaining_before_lockout));
                    }
                }
                Some(PinOutcome::LockedOut { until }) => {
                    self.checking = None;
                    if let Some(p) = &mut self.pad {
                        p.set_locked_until(remaining_until(until, Timestamp::now()));
                    }
                }
                None => {}
            }
        } else if let Some(e) = self.verify.error() {
            tracing::warn!(error = %e, "PIN check failed");
            self.checking = None;
            self.verify = Loader::new();
            if let Some(p) = &mut self.pad {
                p.set_wrong("Couldn't check the PIN. Try again.");
            }
        }
        None
    }

    fn check(&mut self, services: &Services, pin: SecretString) {
        let store = services.store.clone();
        self.checking = Some(pin.clone());
        self.verify
            .start_blocking(services.rt(), services.egui(), move || {
                match store.parental().verify_pin(&pin) {
                    // Removed meanwhile: nothing to protect any more.
                    Err(StoreError::PinNotSet) => Ok(PinOutcome::Ok),
                    other => other.map_err(AppError::from),
                }
            });
    }

    /// The pad and its messages (shared by both layouts). Returns the result so far.
    fn body(&mut self, ui: &mut Ui, services: &Services, title: &str) -> PinPromptResult {
        if let Some(r) = self.poll(services) {
            return r;
        }
        let painter = ui.painter().clone();
        // The lock illustration only when the window is tall enough (TV mode at 1080p is
        // ≈ 620 pt tall: the keypad must fit).
        if ui.ctx().content_rect().height() >= 760.0 {
            let (ir, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
            assets::paint_contain(
                ui,
                &painter,
                Asset::Illustration(Illustration::ParentalLock),
                ir,
                Color32::WHITE,
            );
            ui.add_space(8.0);
        }
        if let Some(e) = self.status.error() {
            ui.label(
                egui::RichText::new(format!(
                    "Couldn't read the PIN settings: {}",
                    e.user_message()
                ))
                .font(font::body())
                .color(color::DANGER),
            );
            return PinPromptResult::Pending;
        }
        let Some(pad) = &mut self.pad else {
            // Loading (a few milliseconds): keep the layout height stable.
            ui.add_space(40.0 + 40.0 + 22.0 + 12.0 + 4.0 * 72.0 + 3.0 * 16.0);
            return PinPromptResult::Pending;
        };
        if let PinPadEvent::Submitted(pin) = pad.show(ui, title) {
            self.check(services, pin);
        }
        PinPromptResult::Pending
    }

    /// Shows the prompt inline (e.g. as a whole settings page), centred in `ui`.
    pub fn show_inline(
        &mut self,
        ui: &mut Ui,
        services: &Services,
        title: &str,
    ) -> PinPromptResult {
        let mut out = PinPromptResult::Pending;
        ui.vertical_centered(|ui| {
            out = self.body(ui, services, title);
            if self.forgot && self.pad.is_some() {
                ui.add_space(16.0);
                if Button::ghost("Forgot your PIN?")
                    .icon(icons::QUESTION)
                    .id(self.id.with("forgot"))
                    .show(ui)
                    .activated
                {
                    out = PinPromptResult::Forgot;
                }
            }
        });
        out
    }

    /// Shows the prompt as a modal dialog with a Cancel button (Esc should also cancel:
    /// close it from the screen's `on_back`).
    pub fn show_dialog(
        &mut self,
        ctx: &Context,
        services: &Services,
        title: &str,
        note: Option<&str>,
    ) -> PinPromptResult {
        let mut out = PinPromptResult::Pending;
        Dialog::new(self.id.with("dialog"), "")
            .width(440.0)
            .show_custom(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    out = self.body(ui, services, title);
                    if let Some(n) = note {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(n)
                                .font(font::caption())
                                .color(color::TEXT_3),
                        );
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let w = Button::ghost("Cancel").size(ui).x
                            + if self.forgot {
                                12.0 + Button::ghost("Forgot your PIN?")
                                    .icon(icons::QUESTION)
                                    .size(ui)
                                    .x
                            } else {
                                0.0
                            };
                        ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                        ui.spacing_mut().item_spacing.x = 12.0;
                        if Button::ghost("Cancel")
                            .id(self.id.with("cancel"))
                            .show(ui)
                            .activated
                        {
                            out = PinPromptResult::Cancelled;
                        }
                        if self.forgot
                            && Button::ghost("Forgot your PIN?")
                                .icon(icons::QUESTION)
                                .id(self.id.with("forgot"))
                                .show(ui)
                                .activated
                        {
                            out = PinPromptResult::Forgot;
                        }
                    });
                });
            });
        out
    }

    /// The running lockout as text ("Try again in 0:27"), if any (for callers that show
    /// it elsewhere).
    pub fn lockout(&self) -> Option<String> {
        let (status, _) = self.status.value()?;
        let until = status.locked_until?;
        let rem = remaining_until(until, Timestamp::now());
        (!rem.is_zero()).then(|| lockout_text(rem))
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! The "choose a PIN" state machine used by Settings › Parental control (set and change):
//! choose 4–8 digits, enter them again, done. Pure (no UI, no store) so it is unit-tested.

use opensmarters_core::secrecy::{ExposeSecret, SecretString};

use crate::services::parental::{MAX_PIN, MIN_PIN, is_valid_pin};

/// Where the user is.
#[derive(Debug)]
enum Step {
    Choose,
    Confirm(SecretString),
}

/// What [`PinSetup::enter`] decided.
#[derive(Debug)]
pub enum SetupOutcome {
    /// Now enter it again.
    Confirm,
    /// Not 4–8 digits (only possible from a variable pad's "Continue").
    Invalid,
    /// The second entry didn't match; back to choosing.
    Mismatch,
    /// Both entries match: save this PIN.
    Done(SecretString),
}

/// The choose → confirm flow.
#[derive(Debug)]
pub struct PinSetup {
    step: Step,
    error: Option<&'static str>,
}

impl Default for PinSetup {
    fn default() -> Self {
        Self::new()
    }
}

impl PinSetup {
    /// Starts at "choose".
    pub fn new() -> Self {
        Self {
            step: Step::Choose,
            error: None,
        }
    }

    /// True on the second step.
    pub fn is_confirming(&self) -> bool {
        matches!(self.step, Step::Confirm(_))
    }

    /// Digits the pad should accept: `(min, max)`; the confirm step expects exactly the
    /// chosen length.
    pub fn pad_range(&self) -> (usize, usize) {
        match &self.step {
            Step::Choose => (MIN_PIN, MAX_PIN),
            Step::Confirm(first) => {
                let n = first.expose_secret().len();
                (n, n)
            }
        }
    }

    /// The pad title.
    pub fn title(&self) -> &'static str {
        match self.step {
            Step::Choose => "Choose a PIN",
            Step::Confirm(_) => "Enter the same PIN again",
        }
    }

    /// The line under the pad.
    pub fn hint(&self) -> &'static str {
        match self.step {
            Step::Choose => "4 to 8 digits. Avoid birthdays and 1234.",
            Step::Confirm(_) => "To make sure it's the PIN you meant.",
        }
    }

    /// The last problem, if any ("The PINs didn't match…").
    pub fn error(&self) -> Option<&'static str> {
        self.error
    }

    /// Feeds a PIN from the pad.
    pub fn enter(&mut self, pin: SecretString) -> SetupOutcome {
        if !is_valid_pin(&pin) {
            self.error = Some("Use 4 to 8 digits.");
            return SetupOutcome::Invalid;
        }
        match std::mem::replace(&mut self.step, Step::Choose) {
            Step::Choose => {
                self.error = None;
                self.step = Step::Confirm(pin);
                SetupOutcome::Confirm
            }
            Step::Confirm(first) => {
                if first.expose_secret() == pin.expose_secret() {
                    self.error = None;
                    SetupOutcome::Done(first)
                } else {
                    self.error = Some("The PINs didn't match. Choose your PIN again.");
                    SetupOutcome::Mismatch
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(s: &str) -> SecretString {
        SecretString::from(s)
    }

    #[test]
    fn choose_confirm_done() {
        let mut f = PinSetup::new();
        assert_eq!(f.pad_range(), (4, 8));
        assert!(matches!(f.enter(pin("482915")), SetupOutcome::Confirm));
        assert!(f.is_confirming());
        assert_eq!(f.pad_range(), (6, 6), "confirm expects the chosen length");
        match f.enter(pin("482915")) {
            SetupOutcome::Done(p) => assert_eq!(p.expose_secret(), "482915"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mismatch_goes_back_to_choose() {
        let mut f = PinSetup::new();
        let _ = f.enter(pin("1111"));
        assert!(matches!(f.enter(pin("2222")), SetupOutcome::Mismatch));
        assert!(!f.is_confirming());
        assert!(f.error().is_some());
        assert_eq!(f.title(), "Choose a PIN");
        // A fresh choice clears the error.
        let _ = f.enter(pin("3333"));
        assert!(f.error().is_none());
    }

    #[test]
    fn invalid_pins_are_refused_without_changing_step() {
        let mut f = PinSetup::new();
        assert!(matches!(f.enter(pin("12")), SetupOutcome::Invalid));
        assert!(matches!(f.enter(pin("123456789")), SetupOutcome::Invalid));
        assert!(matches!(f.enter(pin("12a4")), SetupOutcome::Invalid));
        assert!(!f.is_confirming());
        let _ = f.enter(pin("1234"));
        assert!(matches!(f.enter(pin("12x4")), SetupOutcome::Invalid));
        assert!(f.is_confirming(), "a typo on confirm keeps the first entry");
    }
}

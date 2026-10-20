// SPDX-License-Identifier: GPL-3.0-or-later
//! Session additions for parental control (Phase 4C): the PIN length for PIN pads.

use std::sync::PoisonError;

use super::Session;

impl Session {
    /// Digits of the parental PIN (4–8): create PIN pads with
    /// `PinPad::new(id, session.pin_length())` so they submit on the last digit. Refreshed
    /// by [`Session::reload_parental`].
    pub fn pin_length(&self) -> usize {
        self.parental
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .pin_len
            .clamp(
                crate::services::parental::MIN_PIN,
                crate::services::parental::MAX_PIN,
            )
    }
}

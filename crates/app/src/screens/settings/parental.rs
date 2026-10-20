// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings → Parental control (stub; Phase 4 replaces this file): set/change/remove the
//! PIN (`store.parental()`), keyword list, manual category locks. After changes call
//! `session.reload_parental()` so the Live/Movies/Series screens pick them up.

use eframe::egui::{Id, Ui};

use super::{SettingsPageUi, page_heading};
use crate::screens::AppCtx;
use crate::ui::assets::Illustration;
use crate::ui::widgets::EmptyState;

/// The parental control page (placeholder).
#[derive(Debug, Default)]
pub struct ParentalPage;

impl SettingsPageUi for ParentalPage {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        page_heading(ui, "Parental control", None);
        let enabled = cx.session().is_some_and(|s| s.parental_enabled());
        let body = if enabled {
            "A PIN is set: categories that match the keyword list ask for it. Managing the PIN and the list arrives in the next update."
        } else {
            "Set a PIN to lock adult categories. This page arrives in the next update."
        };
        let _ = EmptyState::new(
            Id::unique("settings.parental.soon"),
            Illustration::ParentalLock,
            "Parental control is on its way",
            body,
        )
        .illustration_size(140.0)
        .show(ui);
    }
}

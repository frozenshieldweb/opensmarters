// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Recordings & downloads (stub). Phase 7 replaces this file: recordings folder,
//! recording padding, downloads, clips.

use eframe::egui::Ui;

use super::{SettingsPageUi, page_heading, paragraph};
use crate::screens::AppCtx;

/// The recordings & downloads page.
#[derive(Debug, Default)]
pub struct RecordingsPage {
    _todo: (),
}

impl SettingsPageUi for RecordingsPage {
    fn ui(&mut self, _cx: &mut AppCtx<'_>, ui: &mut Ui) {
        page_heading(
            ui,
            "Recordings & downloads",
            Some("Where recordings and downloads are saved, and how they run."),
        );
        paragraph(ui, "These settings are on their way.");
    }
}

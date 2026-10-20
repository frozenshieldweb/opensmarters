// SPDX-License-Identifier: GPL-3.0-or-later
//! Recordings & downloads (stub). Phase 7 replaces this file; the route, section, rail
//! item and factory entry already exist (`Route::Recordings { tab }`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{RecordingsTab, Route, Section};
use crate::ui::widgets;

/// Recordings screen (placeholder).
#[derive(Debug, Default)]
pub struct RecordingsScreen {
    tab: RecordingsTab,
}

impl RecordingsScreen {
    /// Creates the screen on `tab`.
    pub fn new(tab: RecordingsTab) -> Self {
        Self { tab }
    }
}

impl Screen for RecordingsScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        if widgets::coming_soon(
            ui,
            Id::unique("recordings.coming-soon"),
            "Recordings",
            self.tab.label(),
            "Films and episodes you download, and the channels you record, will be here. \
             This page is on its way.",
            None,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Recordings)
    }
}

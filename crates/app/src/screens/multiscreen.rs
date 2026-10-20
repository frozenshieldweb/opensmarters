// SPDX-License-Identifier: GPL-3.0-or-later
//! Multiscreen (stub). Phase 7 replaces this file; the route and factory entry already
//! exist (`Route::Multiscreen`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::widgets;

/// Multiscreen screen (placeholder).
#[derive(Debug, Default)]
pub struct MultiscreenScreen {}

impl MultiscreenScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {}
    }
}

impl Screen for MultiscreenScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        if widgets::coming_soon(
            ui,
            Id::unique("multiscreen.coming-soon"),
            "Multiscreen",
            "Up to four channels at once",
            "Watch several live channels side by side, as many as your account's connections \
             allow. This page is on its way.",
            None,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::live());
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Live)
    }
}

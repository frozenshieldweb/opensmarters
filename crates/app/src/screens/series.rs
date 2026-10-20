// SPDX-License-Identifier: GPL-3.0-or-later
//! Series (stub). Phase 4 replaces this file; the route, section and factory entry
//! already exist (`Route::Series`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::assets::Illustration;
use crate::ui::widgets;

/// Series screen (placeholder).
#[derive(Debug)]
pub struct SeriesScreen {
    /// Route parameter (kept for the real implementation).
    pub category: Option<String>,
}

impl SeriesScreen {
    /// Creates the screen.
    pub fn new(category: Option<String>) -> Self {
        Self { category }
    }
}

impl Screen for SeriesScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        let ill: Option<Illustration> = None;
        if widgets::coming_soon(
            ui,
            Id::unique("series.coming-soon"),
            "Series",
            "Series are on their way",
            "Seasons, episodes and “up next” will live here.",
            ill,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Series)
    }
}

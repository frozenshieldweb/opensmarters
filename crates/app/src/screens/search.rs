// SPDX-License-Identifier: GPL-3.0-or-later
//! Search (stub). Phase 4 replaces this file; the route, section and factory entry
//! already exist (`Route::Search`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::assets::Illustration;
use crate::ui::widgets;

/// Search screen (placeholder).
#[derive(Debug)]
pub struct SearchScreen {
    /// Route parameter (kept for the real implementation).
    pub query: String,
}

impl SearchScreen {
    /// Creates the screen.
    pub fn new(query: String) -> Self {
        Self { query }
    }
}

impl Screen for SearchScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        let ill: Option<Illustration> = Some(Illustration::EmptySearch);
        if widgets::coming_soon(
            ui,
            Id::unique("search.coming-soon"),
            "Search",
            "Search is on its way",
            "Find channels, films and series by name.",
            ill,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Search)
    }
}

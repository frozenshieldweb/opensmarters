// SPDX-License-Identifier: GPL-3.0-or-later
//! Movies (stub). Phase 4 replaces this file; the route, section and factory entry
//! already exist (`Route::Movies`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::assets::Illustration;
use crate::ui::widgets;

/// Movies screen (placeholder).
#[derive(Debug)]
pub struct MoviesScreen {
    /// Route parameter (kept for the real implementation).
    pub category: Option<String>,
}

impl MoviesScreen {
    /// Creates the screen.
    pub fn new(category: Option<String>) -> Self {
        Self { category }
    }
}

impl Screen for MoviesScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        let ill: Option<Illustration> = None;
        if widgets::coming_soon(
            ui,
            Id::unique("movies.coming-soon"),
            "Movies",
            "Movies are on their way",
            "Browse your provider's films by category, with posters, details and resume.",
            ill,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Movies)
    }
}

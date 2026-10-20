// SPDX-License-Identifier: GPL-3.0-or-later
//! Guide (stub). Phase 4 replaces this file; the route, section and factory entry
//! already exist (`Route::Guide`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::assets::Illustration;
use crate::ui::widgets;

/// Guide screen (placeholder).
#[derive(Debug, Default)]
pub struct GuideScreen {}

impl GuideScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {}
    }
}

impl Screen for GuideScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        let ill: Option<Illustration> = Some(Illustration::EmptyGuide);
        if widgets::coming_soon(
            ui,
            Id::unique("guide.coming-soon"),
            "Guide",
            "The guide is on its way",
            "A full programme grid for every channel, with catch-up where available.",
            ill,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Guide)
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Catch-up (stub). Phase 4 replaces this file; the route, section and factory entry
//! already exist (`Route::Catchup`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::assets::Illustration;
use crate::ui::widgets;

/// Catch-up screen (placeholder).
#[derive(Debug)]
pub struct CatchupScreen {
    /// Route parameter (kept for the real implementation).
    pub stream_id: Option<u64>,
}

impl CatchupScreen {
    /// Creates the screen.
    pub fn new(stream_id: Option<u64>) -> Self {
        Self { stream_id }
    }
}

impl Screen for CatchupScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        let ill: Option<Illustration> = Some(Illustration::EmptyGuide);
        if widgets::coming_soon(
            ui,
            Id::unique("catchup.coming-soon"),
            "Catch-up",
            "Catch-up is on its way",
            "Replay programmes from channels that keep an archive.",
            ill,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Catchup)
    }
}

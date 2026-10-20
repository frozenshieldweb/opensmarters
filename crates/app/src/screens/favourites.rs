// SPDX-License-Identifier: GPL-3.0-or-later
//! Favourites (stub). Phase 4 replaces this file; the route, section and factory entry
//! already exist (`Route::Favourites`, `screens::create`).

use eframe::egui::{self, Id};

use super::{AppCtx, Screen, shell};
use crate::nav::{Route, Section};
use crate::ui::assets::Illustration;
use crate::ui::widgets;

/// Favourites screen (placeholder).
#[derive(Debug, Default)]
pub struct FavouritesScreen {}

impl FavouritesScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {}
    }
}

impl Screen for FavouritesScreen {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        shell::status_cluster(cx, ui);
        let ill: Option<Illustration> = Some(Illustration::EmptyFavourites);
        if widgets::coming_soon(
            ui,
            Id::unique("favourites.coming-soon"),
            "Favourites",
            "Nothing saved yet",
            "Press ♥ on any channel, film or series to keep it here. This page is on its way.",
            ill,
            cx.layout.margin,
        ) {
            cx.nav.section(Route::Home);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Favourites)
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Movies (§8.4): the provider's VOD by category in a virtualised poster grid, with
//! in-category search, sort, parental locks, favourite hearts and watch progress. Enter
//! on a poster opens the detail page. The browse logic is shared with Series
//! ([`browse`]).
//!
//! Keys: arrows move through the categories, the toolbar (search, sort) and the grid;
//! PageUp/PageDown page the grid; Esc closes the sort menu / PIN pad, else goes back.

pub(crate) mod browse;

use std::sync::Arc;

use eframe::egui;
use opensmarters_core::{Category, CategoryId, VodStream};
use opensmarters_store::ContentKind;

use super::{AppCtx, Screen};
use crate::error::AppResult;
use crate::nav::{Route, Section};
use crate::services::Session;
use crate::services::vod;
use browse::{Browse, BrowseKind};

/// Movies in the browse screen.
#[derive(Debug)]
pub(crate) struct MovieKind;

impl BrowseKind for MovieKind {
    type Item = VodStream;
    const CONTENT: ContentKind = ContentKind::Movie;
    const TITLE: &'static str = "Movies";
    const PLURAL: &'static str = "movies";
    const SINGULAR: &'static str = "movie";
    const KEY: &'static str = "movies";
    const PROGRESS: bool = true;

    async fn categories(s: Arc<Session>) -> AppResult<Arc<Vec<Category>>> {
        s.vod_categories().await
    }

    async fn list(s: Arc<Session>, category: Option<CategoryId>) -> AppResult<Arc<Vec<VodStream>>> {
        s.vod_streams(category).await
    }

    fn poster(item: &VodStream) -> Option<&str> {
        item.stream_icon.as_deref()
    }

    fn meta(item: &VodStream) -> String {
        vod::movie_card_meta(item)
    }

    fn item_categories(item: &VodStream) -> Vec<&CategoryId> {
        item.category_id
            .iter()
            .chain(item.category_ids.iter())
            .collect()
    }

    fn detail(id: u64) -> Route {
        Route::MovieDetail { stream_id: id }
    }
}

/// The Movies screen.
#[derive(Debug)]
pub struct MoviesScreen {
    inner: Browse<MovieKind>,
}

impl MoviesScreen {
    /// Creates the screen, optionally on a category id (or `"all"`).
    pub fn new(category: Option<String>) -> Self {
        Self {
            inner: Browse::new(category),
        }
    }
}

impl Screen for MoviesScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.inner.on_enter(cx);
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        self.inner.on_back(cx)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        self.inner.ui(cx, ui);
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Movies)
    }
}

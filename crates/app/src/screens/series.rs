// SPDX-License-Identifier: GPL-3.0-or-later
//! Series (§8.4): the same browse experience as Movies (per-category loading, "All"
//! last and lazy, search, sort, parental locks, favourite hearts) for the provider's
//! series; Enter opens the series page with seasons and episodes.

use std::sync::Arc;

use eframe::egui;
use opensmarters_core::{Category, CategoryId, Series};
use opensmarters_store::ContentKind;

use super::movies::browse::{Browse, BrowseKind};
use super::{AppCtx, Screen};
use crate::error::AppResult;
use crate::nav::{Route, Section};
use crate::services::Session;
use crate::services::vod;

/// Series in the browse screen.
#[derive(Debug)]
pub(crate) struct SeriesKind;

impl BrowseKind for SeriesKind {
    type Item = Series;
    const CONTENT: ContentKind = ContentKind::Series;
    const TITLE: &'static str = "Series";
    const PLURAL: &'static str = "series";
    const SINGULAR: &'static str = "series";
    const KEY: &'static str = "series";
    const PROGRESS: bool = false;

    async fn categories(s: Arc<Session>) -> AppResult<Arc<Vec<Category>>> {
        s.series_categories().await
    }

    async fn list(s: Arc<Session>, category: Option<CategoryId>) -> AppResult<Arc<Vec<Series>>> {
        s.series_list(category).await
    }

    fn poster(item: &Series) -> Option<&str> {
        item.poster()
    }

    fn meta(item: &Series) -> String {
        vod::series_card_meta(item)
    }

    fn item_categories(item: &Series) -> Vec<&CategoryId> {
        item.category_id
            .iter()
            .chain(item.category_ids.iter())
            .collect()
    }

    fn detail(id: u64) -> Route {
        Route::SeriesDetail { series_id: id }
    }
}

/// The Series screen.
#[derive(Debug)]
pub struct SeriesScreen {
    inner: Browse<SeriesKind>,
}

impl SeriesScreen {
    /// Creates the screen, optionally on a category id (or `"all"`).
    pub fn new(category: Option<String>) -> Self {
        Self {
            inner: Browse::new(category),
        }
    }
}

impl Screen for SeriesScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.inner.on_enter(cx);
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        self.inner.on_back(cx)
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        self.inner.on_input(cx, ctx);
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        self.inner.ui(cx, ui);
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Series)
    }
}

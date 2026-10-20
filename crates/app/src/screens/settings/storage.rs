// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings → Storage & cache (stub; Phase 4 replaces this file): cache size cap
//! (`store.cache().set_max_bytes`), clear the response cache and the image cache
//! (`services::images::clear_disk_cache`), clear watch history.

use eframe::egui::{Id, Ui};

use super::{SettingsPageUi, page_heading};
use crate::screens::AppCtx;
use crate::ui::assets::Illustration;
use crate::ui::widgets::EmptyState;

/// The storage page (placeholder).
#[derive(Debug, Default)]
pub struct StoragePage;

impl SettingsPageUi for StoragePage {
    fn ui(&mut self, _cx: &mut AppCtx<'_>, ui: &mut Ui) {
        page_heading(ui, "Storage & cache", None);
        let _ = EmptyState::new(
            Id::unique("settings.storage.soon"),
            Illustration::EmptyGuide,
            "Storage settings are on their way",
            "Cache size, clearing the cache and watch history arrive in the next update.",
        )
        .illustration_size(140.0)
        .show(ui);
    }
}

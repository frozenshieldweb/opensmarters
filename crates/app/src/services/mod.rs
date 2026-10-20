// SPDX-License-Identifier: GPL-3.0-or-later
//! App-wide services shared by every screen (`cx.services`): the tokio runtime, the store,
//! the redactor, the image service, paths and preferences. Everything here is cheap to
//! clone into background tasks.
//!
//! The provider-specific part lives in [`session::Session`] (one per active profile,
//! `cx.session()`).

pub mod catalog;
#[cfg(feature = "demo")]
pub mod demo;
pub mod epg;
pub mod images;
pub mod prefs;
pub mod session;
pub mod vod;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use opensmarters_core::Redactor;
use opensmarters_store::Store;
use tokio::runtime::Handle;

pub use images::{ImageService, ImageSize, ImageSlot};
pub use prefs::Prefs;
pub use session::{AccountSummary, OpenFailure, Session};

/// Where the app keeps its files.
#[derive(Debug, Clone, Default)]
pub struct Paths {
    /// Data directory (database, image cache, logs). `None` for `--ephemeral` without
    /// `--data-dir` (nothing is written to disk).
    pub data_dir: Option<PathBuf>,
}

impl Paths {
    /// `<data>/images` (disk image cache).
    pub fn image_cache_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("images"))
    }

    /// `<data>/logs`.
    pub fn log_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("logs"))
    }

    /// The default per-user data directory (next to the default database).
    pub fn default_data_dir() -> Option<PathBuf> {
        opensmarters_store::default_db_path()
            .ok()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    }
}

/// The app-wide services. Clone freely (everything is behind `Arc`s).
#[derive(Clone)]
pub struct Services {
    rt: Handle,
    /// The database + keychain. **Never call it on the UI thread**: use
    /// [`Services::blocking`] or `Loader::start_blocking`.
    pub store: Arc<Store>,
    /// The app-wide redactor (credentials of every opened profile are registered).
    pub redactor: Redactor,
    /// Provider image loader (memory + disk cache).
    pub images: ImageService,
    /// Paths.
    pub paths: Paths,
    /// True for `--ephemeral` (in-memory store, memory keychain).
    pub ephemeral: bool,
    egui: egui::Context,
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Services")
            .field("paths", &self.paths)
            .field("ephemeral", &self.ephemeral)
            .finish_non_exhaustive()
    }
}

impl Services {
    /// Builds the services (called once from the eframe creation closure).
    pub fn new(
        rt: Handle,
        store: Arc<Store>,
        redactor: Redactor,
        paths: Paths,
        ephemeral: bool,
        egui: egui::Context,
    ) -> Self {
        let images = ImageService::new(
            rt.clone(),
            egui.clone(),
            redactor.clone(),
            paths.image_cache_dir(),
        );
        Self {
            rt,
            store,
            redactor,
            images,
            paths,
            ephemeral,
            egui,
        }
    }

    /// The tokio runtime handle.
    pub fn rt(&self) -> &Handle {
        &self.rt
    }

    /// The egui context (to request repaints from tasks).
    pub fn egui(&self) -> &egui::Context {
        &self.egui
    }

    /// Runs a blocking closure (store/keychain) on the runtime's blocking pool and
    /// forgets the result; errors are logged. Requests a repaint when done.
    pub fn blocking<F>(&self, what: &'static str, f: F)
    where
        F: FnOnce(&Store) -> Result<(), opensmarters_store::StoreError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        let ctx = self.egui.clone();
        self.rt.spawn_blocking(move || {
            if let Err(e) = f(&store) {
                tracing::warn!(error = %e, "{what} failed");
            }
            ctx.request_repaint();
        });
    }

    /// Spawns a future on the runtime.
    pub fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.rt.spawn(fut)
    }
}

/// A stable 64-bit FNV-1a hash (ids and cache file names must not change between Rust
/// releases, unlike `DefaultHasher`).
pub fn stable_hash(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h ^= 0x1f;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for b in p.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_is_stable() {
        assert_eq!(stable_hash(&["a"]), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(stable_hash(&["ab", "c"]), stable_hash(&["a", "bc"]));
    }
}

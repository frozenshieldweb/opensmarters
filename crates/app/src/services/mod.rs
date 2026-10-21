// SPDX-License-Identifier: GPL-3.0-or-later
//! App-wide services shared by every screen (`cx.services`): the tokio runtime, the store,
//! the redactor, the image service, paths and preferences. Everything here is cheap to
//! clone into background tasks.
//!
//! The provider-specific part lives in [`session::Session`] (one per active profile,
//! `cx.session()`).
//!
//! **Store access:** reads run on the blocking pool (`Loader::start_blocking`, or the
//! session's own helpers); **writes** go through one FIFO writer thread
//! ([`Services::blocking`] to fire and forget, [`Services::write`] to await the result),
//! so they are applied in the order they were made (see [`writer`]).

pub mod alerts;
pub mod backup;
pub mod catalog;
pub mod categories;
pub mod connections;
#[cfg(feature = "demo")]
pub mod demo;
pub mod epg;
pub mod fetch_queue;
pub mod images;
pub mod parental;
pub mod prefs;
pub mod reminders;
pub mod search_history;
pub mod session;
pub mod vod;
pub mod writer;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use opensmarters_core::Redactor;
use opensmarters_store::Store;
use tokio::runtime::Handle;

pub use images::{ImageService, ImageSize, ImageSlot};
pub use prefs::Prefs;
pub use session::{AccountSummary, InsecureReason, OpenFailure, Session};
pub use writer::WriteHandle;

use crate::error::{AppError, AppResult};

/// The background-error "what" used for a background task that panicked (the toast then
/// says something went wrong instead of blaming the disk).
pub const INTERNAL_FAILURE: &str = "finish a background task";

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
    errors: Arc<std::sync::Mutex<Vec<&'static str>>>,
    writer: writer::StoreWriter,
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
        let writer = writer::StoreWriter::start(Arc::clone(&store));
        Self {
            rt,
            store,
            redactor,
            images,
            paths,
            ephemeral,
            egui,
            errors: Arc::default(),
            writer,
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

    /// Queues a store write (or any store/keychain job whose result nobody waits for) on
    /// the FIFO writer thread: jobs run one at a time, **in the order they were
    /// submitted** (a later progress save never lands before an earlier one; two
    /// read-modify-write updates of one settings map never interleave). Errors — and a
    /// job that panics — are logged and reported to the app, which shows a toast
    /// ([`Services::take_background_errors`]). Requests a repaint when done.
    pub fn blocking<F>(&self, what: &'static str, f: F)
    where
        F: FnOnce(&Store) -> Result<(), opensmarters_store::StoreError> + Send + 'static,
    {
        let ctx = self.egui.clone();
        let errors = Arc::clone(&self.errors);
        self.writer.submit(Box::new(move |store: &Store| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || f(store)));
            let failed = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "{what} failed");
                    Some(what)
                }
                Err(_) => {
                    tracing::error!("{what} panicked");
                    Some(what)
                }
            };
            if let Some(what) = failed {
                push_error(&errors, what);
            }
            ctx.request_repaint();
        }));
    }

    /// Queues a store write on the FIFO writer thread (see [`Services::blocking`]) and
    /// returns a handle to await its result. Dropping the handle does not cancel the
    /// write. A panicking job yields [`AppError::Internal`].
    pub fn write<T, F>(&self, what: &'static str, f: F) -> WriteHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T, opensmarters_store::StoreError> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.writer.submit(Box::new(move |store: &Store| {
            let result =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || f(store))) {
                    Ok(r) => r.map_err(AppError::from),
                    Err(_) => {
                        tracing::error!("{what} panicked");
                        Err(AppError::Internal)
                    }
                };
            if let Err(e) = &result {
                tracing::debug!(error = %e, "{what} failed");
            }
            let _ = tx.send(result);
        }));
        WriteHandle::new(rx)
    }

    /// Runs a store **read** on the blocking pool (reads may run in parallel; use
    /// [`Services::write`] for anything that changes the store). A panic becomes
    /// [`AppError::Internal`].
    ///
    /// # Errors
    ///
    /// Store errors, or [`AppError::Internal`].
    pub async fn read<T, F>(&self, f: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T, opensmarters_store::StoreError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        match self.rt.spawn_blocking(move || f(&store)).await {
            Ok(r) => r.map_err(AppError::from),
            Err(e) if e.is_panic() => {
                tracing::error!("a store read panicked");
                Err(AppError::Internal)
            }
            Err(_) => Err(AppError::Cancelled),
        }
    }

    /// Reports a failed background job by its short description (e.g. `"save progress"`);
    /// the app shows it as a toast.
    pub fn report_background_error(&self, what: &'static str) {
        push_error(&self.errors, what);
        self.egui.request_repaint();
    }

    /// Drains the failed background jobs (descriptions given to [`Services::blocking`]).
    pub fn take_background_errors(&self) -> Vec<&'static str> {
        std::mem::take(
            &mut *self
                .errors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Spawns a future on the runtime. If it panics, the panic is logged and reported as
    /// a background error (toast) before it reaches the `JoinHandle` as usual.
    pub fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let errors = Arc::clone(&self.errors);
        let ctx = self.egui.clone();
        self.rt.spawn(writer::ReportPanic::new(
            fut,
            Box::new(move || {
                tracing::error!("a background task panicked");
                push_error(&errors, INTERNAL_FAILURE);
                ctx.request_repaint();
            }),
        ))
    }
}

fn push_error(errors: &std::sync::Mutex<Vec<&'static str>>, what: &'static str) {
    let mut q = errors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !q.contains(&what) && q.len() < 16 {
        q.push(what);
    }
}

/// The toast text for a failed background job (`what` as given to
/// [`Services::blocking`]). Plain language, no error details (those are in the log).
pub fn background_error_message(what: &str) -> String {
    if what == INTERNAL_FAILURE {
        return "Something went wrong in the background. Details are in the log.".to_owned();
    }
    let what = what.trim();
    let what = if what.is_empty() {
        "save a change"
    } else {
        what
    };
    format!("Couldn't {what}. Your disk may be full or busy; details are in the log.")
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
    fn background_error_text_is_plain() {
        assert_eq!(
            background_error_message("save progress"),
            "Couldn't save progress. Your disk may be full or busy; details are in the log."
        );
        assert!(background_error_message(" ").starts_with("Couldn't save a change."));
    }

    #[test]
    fn stable_hash_is_stable() {
        assert_eq!(stable_hash(&["a"]), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(stable_hash(&["ab", "c"]), stable_hash(&["a", "bc"]));
    }
}

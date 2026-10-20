// SPDX-License-Identifier: GPL-3.0-or-later
//! Background work for screens: [`Loader<T>`] runs a future (or blocking closure) on the
//! app's tokio runtime and delivers the result to the UI thread, which polls it each
//! frame. Completion wakes the UI (`request_repaint`).
//!
//! - **Staleness:** every [`Loader::start`] bumps a generation, aborts the previous task
//!   and opens a new channel; results of older generations can never be applied.
//! - **Stale-while-revalidate:** the previous value stays visible while reloading, and a
//!   task can send an early value (e.g. from an expired cache) with
//!   [`Emitter::partial`] before its final result.
//! - **Skeletons:** [`Loader::show_skeleton`] is true only after 150 ms without a value
//!   (no flashing on fast loads, §6.16).
//!
//! ```ignore
//! // in on_enter / when the category changes:
//! let session = cx.session().clone();
//! self.channels.start(cx.rt(), cx.egui(), async move |_emit| session.live_catalog().await);
//! // in ui():
//! self.channels.poll();
//! match self.channels.value() { Some(cat) => …, None if self.channels.show_skeleton() => …, None => {} }
//! if let Some(err) = self.channels.error() { … }
//! ```

use std::future::Future;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::error::AppError;

/// Delay before a skeleton is shown for a load without a value.
pub const SKELETON_DELAY: Duration = Duration::from_millis(150);

enum Update<T> {
    Partial(T),
    Done(Result<T, AppError>),
}

/// Sends intermediate values from inside a loader task.
pub struct Emitter<T> {
    generation: u64,
    tx: mpsc::Sender<(u64, Update<T>)>,
    ctx: egui::Context,
}

impl<T> std::fmt::Debug for Emitter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl<T> Emitter<T> {
    /// Delivers an early value (e.g. a stale cache hit) before the final result.
    pub fn partial(&self, value: T) {
        let _ = self.tx.send((self.generation, Update::Partial(value)));
        self.ctx.request_repaint();
    }
}

/// Loading state of one value (see the [module docs](self)).
pub struct Loader<T> {
    value: Option<T>,
    error: Option<AppError>,
    loading: bool,
    partial: bool,
    generation: u64,
    started: Option<Instant>,
    finished: Option<Instant>,
    rx: Option<mpsc::Receiver<(u64, Update<T>)>>,
    abort: Option<AbortHandle>,
}

impl<T> std::fmt::Debug for Loader<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader")
            .field("has_value", &self.value.is_some())
            .field("loading", &self.loading)
            .field("error", &self.error)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl<T> Default for Loader<T> {
    fn default() -> Self {
        Self {
            value: None,
            error: None,
            loading: false,
            partial: false,
            generation: 0,
            started: None,
            finished: None,
            rx: None,
            abort: None,
        }
    }
}

impl<T> Drop for Loader<T> {
    fn drop(&mut self) {
        if let Some(a) = self.abort.take() {
            a.abort();
        }
    }
}

impl<T: Send + 'static> Loader<T> {
    /// An idle loader with no value.
    pub fn new() -> Self {
        Self::default()
    }

    fn begin(&mut self, ctx: &egui::Context) -> Emitter<T> {
        self.cancel();
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        self.partial = false;
        self.error = None;
        self.started = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        // Repaint after the skeleton delay so the skeleton appears even if nothing else
        // triggers a frame.
        ctx.request_repaint_after(SKELETON_DELAY);
        Emitter {
            generation: self.generation,
            tx,
            ctx: ctx.clone(),
        }
    }

    /// Starts (or restarts) the load with an async task. The previous task is aborted and
    /// its results are ignored. The current value stays until the new one arrives.
    pub fn start<F, Fut>(&mut self, rt: &Handle, ctx: &egui::Context, f: F)
    where
        F: FnOnce(Emitter<T>) -> Fut,
        Fut: Future<Output = Result<T, AppError>> + Send + 'static,
    {
        let emitter = self.begin(ctx);
        let tx = emitter.tx.clone();
        let generation = emitter.generation;
        let ctx2 = ctx.clone();
        let fut = f(emitter);
        let handle = rt.spawn(async move {
            let result = fut.await;
            let _ = tx.send((generation, Update::Done(result)));
            ctx2.request_repaint();
        });
        self.abort = Some(handle.abort_handle());
    }

    /// Starts the load with a blocking closure (store/keychain calls, decoding) on the
    /// runtime's blocking pool.
    pub fn start_blocking<F>(&mut self, rt: &Handle, ctx: &egui::Context, f: F)
    where
        F: FnOnce() -> Result<T, AppError> + Send + 'static,
    {
        let emitter = self.begin(ctx);
        let tx = emitter.tx.clone();
        let generation = emitter.generation;
        let ctx2 = ctx.clone();
        let handle = rt.spawn_blocking(move || {
            let result = f();
            let _ = tx.send((generation, Update::Done(result)));
            ctx2.request_repaint();
        });
        self.abort = Some(handle.abort_handle());
    }

    /// Applies finished results. Call once per frame before reading. Returns true if the
    /// value or error changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let mut changed = false;
        let mut done = false;
        while let Ok((generation, update)) = rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            changed = true;
            match update {
                Update::Partial(v) => {
                    self.value = Some(v);
                    self.partial = true;
                }
                Update::Done(Ok(v)) => {
                    self.value = Some(v);
                    self.partial = false;
                    self.error = None;
                    done = true;
                }
                Update::Done(Err(AppError::Cancelled)) => done = true,
                Update::Done(Err(e)) => {
                    self.error = Some(e);
                    self.partial = false;
                    done = true;
                }
            }
        }
        if done {
            self.loading = false;
            self.finished = Some(Instant::now());
            self.rx = None;
            self.abort = None;
        }
        changed
    }
}

impl<T> Loader<T> {
    /// Aborts the running task (the value is kept).
    pub fn cancel(&mut self) {
        if let Some(a) = self.abort.take() {
            a.abort();
        }
        self.rx = None;
        self.loading = false;
    }

    /// The latest value (possibly stale while reloading).
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// The latest value, mutably.
    pub fn value_mut(&mut self) -> Option<&mut T> {
        self.value.as_mut()
    }

    /// Takes the value out.
    pub fn take(&mut self) -> Option<T> {
        self.value.take()
    }

    /// Replaces the value (e.g. after a local edit).
    pub fn set(&mut self, value: T) {
        self.value = Some(value);
        self.error = None;
    }

    /// The error of the last finished load, if it failed.
    pub fn error(&self) -> Option<&AppError> {
        self.error.as_ref()
    }

    /// True while a task runs.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// True if the value came from [`Emitter::partial`] and the final result is pending.
    pub fn is_partial(&self) -> bool {
        self.partial
    }

    /// True if never started.
    pub fn is_idle(&self) -> bool {
        self.generation == 0
    }

    /// True once a load has finished (successfully or not) at least once.
    pub fn has_finished(&self) -> bool {
        self.finished.is_some()
    }

    /// Generation counter (bumped by every start).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Loading without a value for longer than [`SKELETON_DELAY`].
    pub fn show_skeleton(&self) -> bool {
        self.loading
            && self.value.is_none()
            && self.started.is_some_and(|s| s.elapsed() >= SKELETON_DELAY)
    }

    /// Time since the last load finished.
    pub fn age(&self) -> Option<Duration> {
        self.finished.map(|f| f.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap_or_else(|e| panic!("runtime: {e}"))
    }

    fn wait_until(l: &mut Loader<u32>, f: impl Fn(&Loader<u32>) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            l.poll();
            if f(l) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out: {l:?}");
    }

    #[test]
    fn newer_generation_wins_over_slow_older_task() {
        let rt = rt();
        let ctx = egui::Context::default();
        let mut l: Loader<u32> = Loader::new();
        l.start(rt.handle(), &ctx, |_| async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(1)
        });
        let first = l.generation();
        l.start(rt.handle(), &ctx, |_| async { Ok(2) });
        assert!(l.generation() > first);
        wait_until(&mut l, |l| l.value() == Some(&2));
        std::thread::sleep(Duration::from_millis(400));
        l.poll();
        assert_eq!(l.value(), Some(&2), "the stale result must never land");
        assert!(!l.is_loading());
    }

    #[test]
    fn partial_then_final_and_errors_keep_value() {
        let rt = rt();
        let ctx = egui::Context::default();
        let mut l: Loader<u32> = Loader::new();
        l.start(rt.handle(), &ctx, |emit| async move {
            emit.partial(7);
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(8)
        });
        wait_until(&mut l, |l| l.value() == Some(&7) || l.value() == Some(&8));
        wait_until(&mut l, |l| l.value() == Some(&8) && !l.is_partial());
        l.start(rt.handle(), &ctx, |_| async { Err(AppError::msg("boom")) });
        wait_until(&mut l, |l| l.error().is_some());
        assert_eq!(l.value(), Some(&8), "a failed refresh keeps the old value");
        assert!(l.has_finished());
    }

    #[test]
    fn blocking_and_cancel() {
        let rt = rt();
        let ctx = egui::Context::default();
        let mut l: Loader<u32> = Loader::new();
        l.start_blocking(rt.handle(), &ctx, || Ok(3));
        wait_until(&mut l, |l| l.value() == Some(&3));
        l.start(rt.handle(), &ctx, |_| async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(4)
        });
        l.cancel();
        std::thread::sleep(Duration::from_millis(300));
        l.poll();
        assert_eq!(l.value(), Some(&3));
        assert!(!l.is_loading());
    }
}

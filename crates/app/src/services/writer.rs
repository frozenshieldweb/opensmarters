// SPDX-License-Identifier: GPL-3.0-or-later
//! The store writer: **one** thread that applies store writes in the order they were
//! submitted (FIFO).
//!
//! Writes used to go to tokio's multi-threaded blocking pool, where two jobs submitted
//! one after the other can run in either order: a periodic progress save could land
//! after the final one (or after "mark finished"), two read-modify-write updates of the
//! same settings map (search history, last category) could lose one another, and a
//! preference save could be overwritten by an older one. Everything that changes the
//! store goes through [`StoreWriter`] instead ([`super::Services::blocking`] for
//! fire-and-forget writes, [`super::Services::write`] to await a result). Reads stay on
//! the blocking pool and run in parallel.
//!
//! A panicking job is caught (the writer keeps running) and reported through the
//! background-error queue.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use opensmarters_store::Store;

use crate::error::{AppError, AppResult};

/// A unit of work for the writer thread.
pub(crate) type Job = Box<dyn FnOnce(&Store) + Send + 'static>;

/// Handle to the writer thread (cheap to clone; the thread ends when the last handle is
/// dropped, after draining what was submitted).
#[derive(Clone)]
pub(crate) struct StoreWriter {
    tx: Option<mpsc::Sender<Job>>,
    /// Fallback when the thread couldn't be started: run jobs under one lock on the
    /// caller's thread pool (still never two at once).
    fallback: Arc<(Arc<Store>, Mutex<()>)>,
}

impl std::fmt::Debug for StoreWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreWriter")
            .field("thread", &self.tx.is_some())
            .finish()
    }
}

impl StoreWriter {
    /// Starts the writer thread for `store`.
    pub(crate) fn start(store: Arc<Store>) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread_store = Arc::clone(&store);
        let spawned = std::thread::Builder::new()
            .name("store-writer".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    // Jobs catch their own panics (see `Services::blocking`); this is a
                    // last line of defence so one bad job never stops later writes.
                    if catch_unwind(AssertUnwindSafe(|| job(&thread_store))).is_err() {
                        tracing::error!("a store write panicked");
                    }
                }
            });
        let tx = match spawned {
            Ok(_) => Some(tx),
            Err(e) => {
                tracing::error!(error = %e, "store writer thread failed to start; writing inline");
                None
            }
        };
        Self {
            tx,
            fallback: Arc::new((store, Mutex::new(()))),
        }
    }

    /// Queues `job` (runs after every job submitted before it).
    pub(crate) fn submit(&self, job: Job) {
        let job = match &self.tx {
            Some(tx) => match tx.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (store, lock) = &*self.fallback;
        let _g = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if catch_unwind(AssertUnwindSafe(|| job(store))).is_err() {
            tracing::error!("a store write panicked");
        }
    }
}

/// The result of a write submitted with [`super::Services::write`]. Await it, or drop it
/// to fire and forget (the write still happens, in order).
#[must_use = "await the handle, or drop it explicitly to ignore the result"]
pub struct WriteHandle<T> {
    rx: tokio::sync::oneshot::Receiver<AppResult<T>>,
}

impl<T> std::fmt::Debug for WriteHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriteHandle")
    }
}

impl<T> WriteHandle<T> {
    pub(crate) fn new(rx: tokio::sync::oneshot::Receiver<AppResult<T>>) -> Self {
        Self { rx }
    }
}

impl<T> Future for WriteHandle<T> {
    type Output = AppResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx)
            .poll(cx)
            .map(|r| r.unwrap_or_else(|_| Err(AppError::Internal)))
    }
}

/// Wraps a future so a panic inside it is reported (`on_panic`) before it propagates to
/// the task's `JoinHandle` as usual.
pub(crate) struct ReportPanic<F> {
    inner: Pin<Box<F>>,
    on_panic: Option<Box<dyn FnOnce() + Send>>,
}

impl<F> ReportPanic<F> {
    pub(crate) fn new(inner: F, on_panic: Box<dyn FnOnce() + Send>) -> Self {
        Self {
            inner: Box::pin(inner),
            on_panic: Some(on_panic),
        }
    }
}

impl<F: Future> Future for ReportPanic<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        match catch_unwind(AssertUnwindSafe(|| this.inner.as_mut().poll(cx))) {
            Ok(p) => p,
            Err(payload) => {
                if let Some(report) = this.on_panic.take() {
                    report();
                }
                std::panic::resume_unwind(payload)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use opensmarters_store::{MemorySecretStore, StoreOptions};

    #[test]
    fn jobs_run_in_submission_order_even_when_a_job_panics() {
        let store = Arc::new(
            Store::open(
                StoreOptions::in_memory(),
                Arc::new(MemorySecretStore::new()),
            )
            .unwrap(),
        );
        let w = StoreWriter::start(store);
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..200u32 {
            let seen = Arc::clone(&seen);
            w.submit(Box::new(move |_s: &Store| {
                if i == 50 {
                    panic!("boom");
                }
                // Uneven work so an unordered pool would reorder them.
                if i % 7 == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                seen.lock().unwrap().push(i);
            }));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        w.submit(Box::new(move |_s: &Store| {
            let _ = tx.send(());
        }));
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let got = seen.lock().unwrap().clone();
        let want: Vec<u32> = (0..200).filter(|i| *i != 50).collect();
        assert_eq!(got, want);
    }
}

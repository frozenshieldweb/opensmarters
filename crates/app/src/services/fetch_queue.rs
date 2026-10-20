// SPDX-License-Identifier: GPL-3.0-or-later
//! A bounded work queue for "fetch what is on screen" jobs (EPG rows, artwork, lock
//! look-ups).
//!
//! Screens ask for data every frame for what they draw. Spawning one task per request
//! floods the provider when the user pages quickly (hundreds of queued requests, the
//! rows that are finally on screen wait behind all of them). This queue instead:
//!
//! - runs at most `workers` jobs at a time (the caller spawns a worker task only when
//!   [`FetchQueue::request`] says so — never one task per request);
//! - hands out the **most recently wanted** job first, so the rows on screen go before
//!   the ones the user scrolled past;
//! - drops a queued job that has not been asked for again within `wanted_for` (the row
//!   scrolled away) and tells the caller, so it can forget its "pending" marker;
//! - keeps at most `max_pending` queued jobs (the least recently wanted go first);
//! - deduplicates: one queued or running job per key (a queued job's payload can be
//!   upgraded with a merge function, e.g. "now/next" → "full guide row");
//! - can be closed (profile switch): queued jobs are dropped, workers stop.
//!
//! The pure bookkeeping is unit-tested; the async worker loop lives with each user.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Counters for logs, tests and the harness (outstanding requests must stay bounded).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueStats {
    /// Jobs waiting for a worker.
    pub pending: usize,
    /// Jobs running now.
    pub running: usize,
    /// Worker tasks alive.
    pub workers: usize,
    /// Jobs started since creation.
    pub started: u64,
    /// Queued jobs dropped because nobody wanted them any more (or the queue closed).
    pub dropped: u64,
    /// Highest number of jobs that ran at the same time.
    pub peak_running: usize,
}

/// What [`FetchQueue::request`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested<K, J> {
    /// The caller must spawn one worker task (see [`FetchQueue::worker`]).
    pub spawn_worker: bool,
    /// Queued jobs evicted to respect `max_pending` (forget their pending markers).
    pub evicted: Vec<(K, J)>,
}

struct Queued<J> {
    touched: Instant,
    job: J,
}

struct State<K, J> {
    pending: HashMap<K, Queued<J>>,
    running: HashSet<K>,
    workers: usize,
    closed: bool,
    stats: QueueStats,
}

/// See the [module docs](self). `K` identifies a job, `J` is its payload.
pub struct FetchQueue<K, J = ()> {
    state: Mutex<State<K, J>>,
    max_workers: usize,
    wanted_for: Duration,
    max_pending: usize,
}

impl<K, J> std::fmt::Debug for FetchQueue<K, J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchQueue")
            .field("max_workers", &self.max_workers)
            .field("wanted_for", &self.wanted_for)
            .finish_non_exhaustive()
    }
}

impl<K: Eq + Hash + Clone, J> FetchQueue<K, J> {
    /// A queue running at most `max_workers` jobs at once (at least 1).
    pub fn new(max_workers: usize, wanted_for: Duration, max_pending: usize) -> Self {
        Self {
            state: Mutex::new(State {
                pending: HashMap::new(),
                running: HashSet::new(),
                workers: 0,
                closed: false,
                stats: QueueStats::default(),
            }),
            max_workers: max_workers.max(1),
            wanted_for,
            max_pending: max_pending.max(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<K, J>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks for `key` now. A key already queued is marked wanted again and its payload
    /// becomes `merge(old, job)`; a running key is queued again only if it is asked for
    /// while running (it starts after the running job ends).
    pub fn request(&self, key: K, job: J, merge: impl FnOnce(&mut J, J)) -> Requested<K, J> {
        let mut st = self.lock();
        let mut evicted = Vec::new();
        if st.closed {
            return Requested {
                spawn_worker: false,
                evicted,
            };
        }
        let now = Instant::now();
        match st.pending.get_mut(&key) {
            Some(q) => {
                q.touched = now;
                merge(&mut q.job, job);
            }
            None => {
                st.pending.insert(key, Queued { touched: now, job });
            }
        }
        if st.pending.len() > self.max_pending {
            // Forget what nobody wants any more, then the least recently wanted.
            let wanted_for = self.wanted_for;
            let stale: Vec<K> = st
                .pending
                .iter()
                .filter(|(_, q)| q.touched.elapsed() >= wanted_for)
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                if let Some(q) = st.pending.remove(&k) {
                    evicted.push((k, q.job));
                }
            }
            while st.pending.len() > self.max_pending {
                let Some(oldest) = st
                    .pending
                    .iter()
                    .min_by_key(|(_, q)| q.touched)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some(q) = st.pending.remove(&oldest) {
                    evicted.push((oldest, q.job));
                }
            }
            st.stats.dropped += evicted.len() as u64;
        }
        let spawn_worker = st.workers < self.max_workers && !st.pending.is_empty();
        if spawn_worker {
            st.workers += 1;
        }
        Requested {
            spawn_worker,
            evicted,
        }
    }

    /// Marks a queued `key` as still wanted (call it for what is on screen).
    pub fn touch(&self, key: &K) {
        if let Some(q) = self.lock().pending.get_mut(key) {
            q.touched = Instant::now();
        }
    }

    /// True if `key` is queued or running.
    pub fn contains(&self, key: &K) -> bool {
        let st = self.lock();
        st.pending.contains_key(key) || st.running.contains(key)
    }

    /// Starts a worker. Only call it after [`FetchQueue::request`] returned
    /// `spawn_worker`; the worker slot is released when [`Worker::next`] returns `None`
    /// or the worker is dropped (also when its task panics).
    pub fn worker(&self) -> Worker<'_, K, J> {
        Worker {
            queue: self,
            current: None,
            released: false,
        }
    }

    /// Drops every queued job and stops the workers after their current job.
    pub fn close(&self) {
        let mut st = self.lock();
        st.closed = true;
        let n = st.pending.len() as u64;
        st.pending.clear();
        st.stats.dropped += n;
    }

    /// Drops every queued job (running ones finish) but keeps accepting new ones.
    pub fn clear_pending(&self) -> Vec<(K, J)> {
        let mut st = self.lock();
        let out: Vec<(K, J)> = st.pending.drain().map(|(k, q)| (k, q.job)).collect();
        st.stats.dropped += out.len() as u64;
        out
    }

    /// True once [`FetchQueue::close`] was called.
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Current counters.
    pub fn stats(&self) -> QueueStats {
        let st = self.lock();
        QueueStats {
            pending: st.pending.len(),
            running: st.running.len(),
            workers: st.workers,
            ..st.stats
        }
    }
}

/// A worker slot (see [`FetchQueue::worker`]).
pub struct Worker<'q, K: Eq + Hash + Clone, J> {
    queue: &'q FetchQueue<K, J>,
    current: Option<K>,
    released: bool,
}

impl<K: Eq + Hash + Clone, J> std::fmt::Debug for Worker<'_, K, J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("released", &self.released)
            .finish_non_exhaustive()
    }
}

impl<K: Eq + Hash + Clone, J> Worker<'_, K, J> {
    /// Finishes the previous job and returns the next one: the most recently wanted
    /// queued job that is not running. Jobs not wanted within `wanted_for` are removed
    /// and appended to `dropped`. `None` releases the worker slot (stop the loop).
    pub fn next(&mut self, dropped: &mut Vec<(K, J)>) -> Option<(K, J)> {
        if self.released {
            return None;
        }
        let mut st = self.queue.lock();
        if let Some(k) = self.current.take() {
            st.running.remove(&k);
        }
        if !st.closed {
            let wanted_for = self.queue.wanted_for;
            let stale: Vec<K> = st
                .pending
                .iter()
                .filter(|(_, q)| q.touched.elapsed() >= wanted_for)
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                if let Some(q) = st.pending.remove(&k) {
                    st.stats.dropped += 1;
                    dropped.push((k, q.job));
                }
            }
            let best = st
                .pending
                .iter()
                .filter(|(k, _)| !st.running.contains(*k))
                .max_by_key(|(_, q)| q.touched)
                .map(|(k, _)| k.clone());
            if let Some(k) = best
                && let Some(q) = st.pending.remove(&k)
            {
                st.running.insert(k.clone());
                st.stats.started += 1;
                st.stats.peak_running = st.stats.peak_running.max(st.running.len());
                self.current = Some(k.clone());
                return Some((k, q.job));
            }
        }
        st.workers = st.workers.saturating_sub(1);
        self.released = true;
        None
    }
}

impl<K: Eq + Hash + Clone, J> Drop for Worker<'_, K, J> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // The loop ended early (closed session, panic): release the slot.
        let mut st = self.queue.lock();
        if let Some(k) = self.current.take() {
            st.running.remove(&k);
        }
        st.workers = st.workers.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keep(old: &mut u8, new: u8) {
        *old = (*old).max(new);
    }

    #[test]
    fn spawns_at_most_the_worker_limit() {
        let q: FetchQueue<u32, u8> = FetchQueue::new(3, Duration::from_secs(10), 100);
        let spawned = (0..20)
            .filter(|i| q.request(*i, 0, keep).spawn_worker)
            .count();
        assert_eq!(
            spawned, 3,
            "one worker per free slot, never one per request"
        );
        assert_eq!(q.stats().pending, 20);
        assert_eq!(q.stats().workers, 3);
    }

    #[test]
    fn most_recently_wanted_runs_first_and_stale_jobs_are_dropped() {
        let q: FetchQueue<u32, u8> = FetchQueue::new(1, Duration::from_millis(60), 100);
        for i in 0..5 {
            q.request(i, 0, keep);
        }
        std::thread::sleep(Duration::from_millis(80));
        // Only 3 and 4 are still on screen.
        q.touch(&3);
        q.request(4, 1, keep);
        let mut w = q.worker();
        let mut dropped = Vec::new();
        let first = w.next(&mut dropped);
        assert_eq!(first, Some((4, 1)), "latest wanted first, payload merged");
        let mut gone: Vec<u32> = dropped.iter().map(|d| d.0).collect();
        gone.sort_unstable();
        assert_eq!(gone, vec![0, 1, 2], "scrolled-away rows are never fetched");
        assert_eq!(w.next(&mut dropped).map(|j| j.0), Some(3));
        assert_eq!(w.next(&mut dropped), None);
        let s = q.stats();
        assert_eq!((s.pending, s.running, s.workers), (0, 0, 0));
        assert_eq!(s.dropped, 3);
        assert_eq!(s.started, 2);
    }

    #[test]
    fn dedupes_running_keys_and_requeues_upgrades() {
        let q: FetchQueue<u32, u8> = FetchQueue::new(2, Duration::from_secs(10), 100);
        assert!(q.request(7, 0, keep).spawn_worker);
        let mut w = q.worker();
        let mut dropped = Vec::new();
        assert_eq!(w.next(&mut dropped), Some((7, 0)));
        assert!(q.contains(&7));
        // Asked again (with a bigger payload) while running: queued, but a second worker
        // must not start the same key.
        let r = q.request(7, 1, keep);
        assert!(r.spawn_worker);
        let mut w2 = q.worker();
        assert_eq!(w2.next(&mut dropped), None, "7 is still running");
        assert_eq!(w.next(&mut dropped), Some((7, 1)), "upgrade runs after");
        assert_eq!(w.next(&mut dropped), None);
        assert_eq!(q.stats().workers, 0);
    }

    #[test]
    fn bounded_pending_evicts_least_recently_wanted() {
        let q: FetchQueue<u32, ()> = FetchQueue::new(1, Duration::from_secs(10), 3);
        for i in 0..3 {
            assert!(q.request(i, (), |_, ()| {}).evicted.is_empty());
            std::thread::sleep(Duration::from_millis(2));
        }
        let r = q.request(9, (), |_, ()| {});
        assert_eq!(r.evicted, vec![(0, ())]);
        assert_eq!(q.stats().pending, 3);
    }

    #[test]
    fn close_drops_queue_and_dropping_a_worker_releases_its_slot() {
        let q: FetchQueue<u32, ()> = FetchQueue::new(1, Duration::from_secs(10), 10);
        assert!(q.request(1, (), |_, ()| {}).spawn_worker);
        q.request(2, (), |_, ()| {});
        {
            let mut w = q.worker();
            let mut d = Vec::new();
            assert!(w.next(&mut d).is_some());
            // The task "panics" here: the worker is dropped mid-job.
        }
        let s = q.stats();
        assert_eq!((s.running, s.workers, s.pending), (0, 0, 1));
        q.close();
        assert!(q.is_closed());
        assert!(!q.request(3, (), |_, ()| {}).spawn_worker);
        assert_eq!(q.stats().pending, 0);
    }
}

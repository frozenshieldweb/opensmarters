// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-session EPG cache for now/next lookups while rendering lists.
//!
//! Lookups never block: a missing or expired entry is marked pending and fetched in the
//! background by the session (at most [`MAX_CONCURRENT_FETCHES`] at a time); the UI is
//! woken when it arrives. Entries live [`EPG_TTL`] (failed ones are retried after
//! [`RETRY_AFTER`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::catalog::Programme;

/// How long a channel's EPG stays fresh.
pub const EPG_TTL: Duration = Duration::from_secs(10 * 60);
/// When to retry a failed fetch.
pub const RETRY_AFTER: Duration = Duration::from_secs(2 * 60);
/// Concurrent EPG requests to the provider.
pub const MAX_CONCURRENT_FETCHES: usize = 4;

#[derive(Debug, Clone)]
enum Slot {
    Pending,
    Ready {
        at: Instant,
        list: Arc<Vec<Programme>>,
    },
    Failed {
        at: Instant,
    },
}

/// Result of a non-blocking lookup.
#[derive(Debug, Clone)]
pub enum EpgState {
    /// Being fetched (show a skeleton line).
    Loading,
    /// The provider has no guide for this channel.
    Missing,
    /// Programmes sorted by start.
    Ready(Arc<Vec<Programme>>),
}

impl EpgState {
    /// The programmes, if ready.
    pub fn programmes(&self) -> Option<&[Programme]> {
        match self {
            EpgState::Ready(l) => Some(l),
            _ => None,
        }
    }
}

/// The cache. Owned by a [`super::Session`].
#[derive(Debug)]
pub struct EpgCache {
    slots: Mutex<HashMap<String, Slot>>,
    /// Limits concurrent provider requests.
    pub(crate) permits: Arc<tokio::sync::Semaphore>,
}

impl Default for EpgCache {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_FETCHES)),
        }
    }
}

impl EpgCache {
    /// Looks up `key`. Returns the state and whether the caller should start a fetch
    /// (the slot is then marked pending).
    pub fn lookup(&self, key: &str) -> (EpgState, bool) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        match slots.get(key) {
            Some(Slot::Ready { at, list }) if at.elapsed() < EPG_TTL => {
                (EpgState::Ready(Arc::clone(list)), false)
            }
            Some(Slot::Ready { list, .. }) => {
                // Expired: keep showing it while refreshing.
                let list = Arc::clone(list);
                slots.insert(key.to_owned(), Slot::Pending);
                (EpgState::Ready(list), true)
            }
            Some(Slot::Pending) => (EpgState::Loading, false),
            Some(Slot::Failed { at }) if at.elapsed() < RETRY_AFTER => (EpgState::Missing, false),
            Some(Slot::Failed { .. }) | None => {
                slots.insert(key.to_owned(), Slot::Pending);
                (EpgState::Loading, true)
            }
        }
    }

    /// Stores a fetched list (sorted by start here).
    pub fn put(&self, key: &str, mut list: Vec<Programme>) {
        list.sort_by_key(|p| p.start);
        let slot = if list.is_empty() {
            Slot::Failed { at: Instant::now() }
        } else {
            Slot::Ready {
                at: Instant::now(),
                list: Arc::new(list),
            }
        };
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), slot);
    }

    /// Marks a fetch as failed.
    pub fn fail(&self, key: &str) {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), Slot::Failed { at: Instant::now() });
    }

    /// Forgets everything.
    pub fn clear(&self) {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_marks_pending_once() {
        let c = EpgCache::default();
        let (s, fetch) = c.lookup("1");
        assert!(matches!(s, EpgState::Loading) && fetch);
        let (s, fetch) = c.lookup("1");
        assert!(matches!(s, EpgState::Loading) && !fetch);
        c.put("1", Vec::new());
        assert!(matches!(c.lookup("1").0, EpgState::Missing));
    }
}

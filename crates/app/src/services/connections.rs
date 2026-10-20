// SPDX-License-Identifier: GPL-3.0-or-later
//! The connection budget: how many provider streams this app may have open at once, and
//! who holds them (`session.connections()`).
//!
//! Providers count every open stream against `max_connections` (often 1–3) and answer 403
//! to **everything** once it is exceeded, for a minute or more. Every part of the app that
//! opens a stream takes a [`Lease`] first and drops it when the stream closes:
//!
//! | Holder | [`Purpose`] | How it acquires |
//! |---|---|---|
//! | The shared player (Live preview, fullscreen) | `Watch` | [`ConnectionBudget::acquire_for_viewing`]: may take a download's slot |
//! | A multiscreen tile | `Tile(n)` | same |
//! | A film/episode download | `Download(id)` | [`ConnectionBudget::try_acquire`]; **preemptible** |
//! | A live recording | `Recording(id)` | [`ConnectionBudget::try_acquire`]; never preempted |
//!
//! Saving the last minutes of a channel from the player's buffer needs no lease.
//!
//! ## Rules
//!
//! - **Limit** = the per-profile override (Settings, [`MAX_STREAMS_OVERRIDE`]) else the
//!   account's `max_connections` (Xtream) else 1 (M3U: unknown, so be careful).
//! - **Streams elsewhere** (other devices on the same account, from `active_cons` when it
//!   was last checked) count against background jobs only. Viewing ignores them: the user
//!   asked for it, and the provider's answer (403 → "Connection limit reached") is the
//!   authority. A stale count must never stop someone from watching.
//! - **Viewing beats downloads.** When nothing is free, viewing takes the newest
//!   download's slot: that lease is *revoked* at once ([`Lease::is_revoked`],
//!   [`Lease::revoked`]); the downloader must close its stream promptly and requeue the
//!   job (downloads resume). Recordings are never preempted; when they fill the budget,
//!   viewing gets [`Busy`] and the UI explains who holds the connections.
//! - A lease holds its slot until dropped (or revoked). Zapping keeps the same `Watch`
//!   lease: the player stops the old stream before opening the next.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use opensmarters_store::RecordingId;
use opensmarters_store::settings::Setting;

/// Per-profile "simultaneous streams" override (Settings), keyed by profile id text.
/// Absent = automatic (the account's `max_connections`, else 1).
pub const MAX_STREAMS_OVERRIDE: Setting<BTreeMap<String, u8>> =
    Setting::new("app.max_streams_override", BTreeMap::new);

/// Highest limit accepted from the provider or the user.
pub const MAX_LIMIT: u32 = 16;

/// Who holds a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// The shared player (Live TV preview or fullscreen).
    Watch,
    /// A multiscreen tile (0-based).
    Tile(u8),
    /// A film or episode download (preemptible).
    Download(RecordingId),
    /// A live recording.
    Recording(RecordingId),
}

impl Purpose {
    /// Downloads give their slot to viewing.
    pub fn is_preemptible(self) -> bool {
        matches!(self, Purpose::Download(_))
    }

    /// Something the user is watching right now.
    pub fn is_viewing(self) -> bool {
        matches!(self, Purpose::Watch | Purpose::Tile(_))
    }

    /// Plain-language label ("a download", "a recording"…).
    pub fn label(self) -> &'static str {
        match self {
            Purpose::Watch => "the player",
            Purpose::Tile(_) => "a multiscreen tile",
            Purpose::Download(_) => "a download",
            Purpose::Recording(_) => "a recording",
        }
    }
}

/// No connection is free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    /// The limit in effect.
    pub limit: u32,
    /// Who holds this app's connections.
    pub holders: Vec<Purpose>,
    /// Streams open on other devices (when last checked).
    pub elsewhere: u32,
}

impl Busy {
    /// A sentence for the UI, e.g. "All 2 connections are in use (2 recordings)."
    pub fn message(&self) -> String {
        let count = |p: fn(&Purpose) -> bool| self.holders.iter().filter(|h| p(h)).count();
        let rec = count(|h| matches!(h, Purpose::Recording(_)));
        let dl = count(|h| matches!(h, Purpose::Download(_)));
        let view = count(|h| h.is_viewing());
        let mut parts = Vec::new();
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        if rec > 0 {
            parts.push(plural(rec, "recording", "recordings"));
        }
        if dl > 0 {
            parts.push(plural(dl, "download", "downloads"));
        }
        if view > 0 {
            parts.push(plural(view, "stream playing", "streams playing"));
        }
        if self.elsewhere > 0 {
            parts.push(plural(
                self.elsewhere as usize,
                "on another device",
                "on other devices",
            ));
        }
        let all = if self.limit == 1 {
            "Your only connection is in use".to_owned()
        } else {
            format!("All {} connections are in use", self.limit)
        };
        if parts.is_empty() {
            format!("{all}.")
        } else {
            format!("{all} ({}).", parts.join(", "))
        }
    }
}

struct Slot {
    id: u64,
    purpose: Purpose,
    revoked: Arc<Revocation>,
}

#[derive(Default)]
struct Revocation {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

#[derive(Default)]
struct Inner {
    account_limit: Option<u32>,
    override_limit: Option<u32>,
    elsewhere: u32,
    next_id: u64,
    slots: Vec<Slot>,
}

impl Inner {
    fn limit(&self) -> u32 {
        self.override_limit
            .or(self.account_limit)
            .unwrap_or(1)
            .clamp(1, MAX_LIMIT)
    }

    fn used(&self) -> u32 {
        u32::try_from(self.slots.len()).unwrap_or(u32::MAX)
    }
}

/// The budget of one session (profile). See the [module docs](self).
pub struct ConnectionBudget {
    inner: Mutex<Inner>,
    changed: tokio::sync::Notify,
    epoch: AtomicU64,
}

impl std::fmt::Debug for ConnectionBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.lock();
        f.debug_struct("ConnectionBudget")
            .field("limit", &g.limit())
            .field("in_use", &g.used())
            .field("elsewhere", &g.elsewhere)
            .finish()
    }
}

/// A held connection. Dropping it frees the slot.
pub struct Lease {
    id: u64,
    purpose: Purpose,
    budget: Arc<ConnectionBudget>,
    revoked: Arc<Revocation>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("purpose", &self.purpose)
            .field("revoked", &self.is_revoked())
            .finish()
    }
}

impl Lease {
    /// Who holds it.
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// The slot was given to viewing: close the stream now.
    pub fn is_revoked(&self) -> bool {
        self.revoked.flag.load(Ordering::SeqCst)
    }

    /// Resolves when the lease is revoked (use in `tokio::select!` next to the download).
    pub async fn revoked(&self) {
        loop {
            let notified = self.revoked.notify.notified();
            if self.is_revoked() {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.release(self.id);
    }
}

impl ConnectionBudget {
    /// A budget with the account's limit (Xtream `max_connections`) and the user's
    /// override, if any.
    pub fn new(account_limit: Option<u32>, override_limit: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                account_limit: account_limit.filter(|n| *n > 0),
                override_limit: override_limit.filter(|n| *n > 0),
                ..Inner::default()
            }),
            changed: tokio::sync::Notify::new(),
            epoch: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    /// The limit in effect (override, else account, else 1; at most [`MAX_LIMIT`]).
    pub fn limit(&self) -> u32 {
        self.lock().limit()
    }

    /// The account's own limit, if known.
    pub fn account_limit(&self) -> Option<u32> {
        self.lock().account_limit
    }

    /// The user's override, if set.
    pub fn override_limit(&self) -> Option<u32> {
        self.lock().override_limit
    }

    /// Connections this app holds.
    pub fn in_use(&self) -> u32 {
        self.lock().used()
    }

    /// Streams open on other devices, when last checked.
    pub fn elsewhere(&self) -> u32 {
        self.lock().elsewhere
    }

    /// Who holds this app's connections.
    pub fn holders(&self) -> Vec<Purpose> {
        self.lock().slots.iter().map(|s| s.purpose).collect()
    }

    /// Free slots for a background job (streams elsewhere count).
    pub fn available_for_background(&self) -> u32 {
        let g = self.lock();
        g.limit().saturating_sub(g.used() + g.elsewhere)
    }

    /// Changes every time a lease is taken, released or revoked, or the limit changes
    /// (cheap to poll from the UI).
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Resolves at the next change (see [`epoch`](Self::epoch)).
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Sets the user's override (`None` = automatic).
    pub fn set_override(&self, limit: Option<u32>) {
        self.lock().override_limit = limit.filter(|n| *n > 0);
        self.bump();
    }

    /// Updates from a fresh account check: `max_connections` and `active_cons` (which
    /// includes this app's own streams; they are subtracted).
    pub fn set_account(&self, max_connections: Option<u32>, active_cons: Option<u32>) {
        let mut g = self.lock();
        if let Some(max) = max_connections.filter(|n| *n > 0) {
            g.account_limit = Some(max);
        }
        if let Some(active) = active_cons {
            g.elsewhere = active.saturating_sub(g.used());
        }
        drop(g);
        self.bump();
    }

    fn grant(self: &Arc<Self>, g: &mut Inner, purpose: Purpose) -> Lease {
        g.next_id += 1;
        let revoked = Arc::new(Revocation::default());
        g.slots.push(Slot {
            id: g.next_id,
            purpose,
            revoked: Arc::clone(&revoked),
        });
        Lease {
            id: g.next_id,
            purpose,
            budget: Arc::clone(self),
            revoked,
        }
    }

    fn busy(g: &Inner) -> Busy {
        Busy {
            limit: g.limit(),
            holders: g.slots.iter().map(|s| s.purpose).collect(),
            elsewhere: g.elsewhere,
        }
    }

    /// Takes a free slot, or fails with [`Busy`]. Background purposes (downloads,
    /// recordings) also count streams open elsewhere; viewing does not.
    ///
    /// # Errors
    ///
    /// [`Busy`] when no slot is free.
    pub fn try_acquire(self: &Arc<Self>, purpose: Purpose) -> Result<Lease, Busy> {
        let mut g = self.lock();
        let taken = g.used() + if purpose.is_viewing() { 0 } else { g.elsewhere };
        if taken >= g.limit() {
            return Err(Self::busy(&g));
        }
        let lease = self.grant(&mut g, purpose);
        drop(g);
        self.bump();
        Ok(lease)
    }

    /// Like [`try_acquire`](Self::try_acquire), but when nothing is free the newest
    /// download is revoked and its slot given to `purpose` (viewing beats downloads).
    ///
    /// # Errors
    ///
    /// [`Busy`] when every slot is held by something that cannot be preempted.
    pub fn acquire_for_viewing(self: &Arc<Self>, purpose: Purpose) -> Result<Lease, Busy> {
        let mut g = self.lock();
        if g.used() < g.limit() {
            let lease = self.grant(&mut g, purpose);
            drop(g);
            self.bump();
            return Ok(lease);
        }
        let Some(pos) = g.slots.iter().rposition(|s| s.purpose.is_preemptible()) else {
            return Err(Self::busy(&g));
        };
        let victim = g.slots.remove(pos);
        victim.revoked.flag.store(true, Ordering::SeqCst);
        victim.revoked.notify.notify_waiters();
        tracing::info!(victim = ?victim.purpose, ?purpose, "connection handed over");
        let lease = self.grant(&mut g, purpose);
        drop(g);
        self.bump();
        Ok(lease)
    }

    fn release(&self, id: u64) {
        let mut g = self.lock();
        let before = g.slots.len();
        g.slots.retain(|s| s.id != id);
        let changed = g.slots.len() != before;
        drop(g);
        if changed {
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_prefers_override_then_account_then_one() {
        assert_eq!(ConnectionBudget::new(None, None).limit(), 1);
        assert_eq!(ConnectionBudget::new(Some(3), None).limit(), 3);
        assert_eq!(ConnectionBudget::new(Some(3), Some(2)).limit(), 2);
        assert_eq!(ConnectionBudget::new(Some(0), None).limit(), 1);
        assert_eq!(ConnectionBudget::new(Some(500), None).limit(), MAX_LIMIT);
        let b = ConnectionBudget::new(Some(3), None);
        b.set_override(Some(1));
        assert_eq!(b.limit(), 1);
        b.set_override(None);
        assert_eq!(b.limit(), 3);
    }

    #[test]
    fn leases_free_their_slot_on_drop() {
        let b = ConnectionBudget::new(Some(2), None);
        let e0 = b.epoch();
        let a = b.try_acquire(Purpose::Watch).ok();
        let c = b.try_acquire(Purpose::Recording(RecordingId(1))).ok();
        assert!(a.is_some() && c.is_some());
        assert!(b.epoch() > e0);
        let busy = b
            .try_acquire(Purpose::Download(RecordingId(2)))
            .err()
            .unwrap_or_else(|| panic!("expected busy"));
        assert_eq!(busy.holders.len(), 2);
        assert_eq!(
            busy.message(),
            "All 2 connections are in use (1 recording, 1 stream playing)."
        );
        drop(a);
        assert_eq!(b.in_use(), 1);
        assert!(b.try_acquire(Purpose::Download(RecordingId(2))).is_ok());
    }

    #[test]
    fn viewing_preempts_the_newest_download_but_never_a_recording() {
        let b = ConnectionBudget::new(Some(2), None);
        let d1 = b.try_acquire(Purpose::Download(RecordingId(1)));
        let d2 = b.try_acquire(Purpose::Download(RecordingId(2)));
        let (Ok(d1), Ok(d2)) = (d1, d2) else {
            panic!("two free slots");
        };
        let watch = b.acquire_for_viewing(Purpose::Watch);
        assert!(watch.is_ok());
        assert!(d2.is_revoked(), "the newest download gives way");
        assert!(!d1.is_revoked());
        assert_eq!(b.in_use(), 2);
        // Dropping the revoked lease later does not free someone else's slot.
        drop(d2);
        assert_eq!(b.in_use(), 2);

        let r = ConnectionBudget::new(Some(1), None);
        let rec = r.try_acquire(Purpose::Recording(RecordingId(9)));
        assert!(rec.is_ok());
        let busy = r
            .acquire_for_viewing(Purpose::Watch)
            .err()
            .unwrap_or_else(|| panic!("recordings are not preempted"));
        assert_eq!(
            busy.message(),
            "Your only connection is in use (1 recording)."
        );
    }

    #[test]
    fn streams_elsewhere_only_limit_background_jobs() {
        let b = ConnectionBudget::new(Some(2), None);
        // 1 active at the provider, none of them ours.
        b.set_account(Some(2), Some(1));
        assert_eq!(b.elsewhere(), 1);
        assert_eq!(b.available_for_background(), 1);
        let w = b.try_acquire(Purpose::Watch);
        assert!(w.is_ok());
        assert!(b.try_acquire(Purpose::Download(RecordingId(1))).is_err());
        // Viewing ignores the (possibly stale) count from elsewhere.
        let tile = b.try_acquire(Purpose::Tile(1));
        assert!(tile.is_ok());
        // A later check subtracts our own two streams.
        b.set_account(None, Some(2));
        assert_eq!(b.elsewhere(), 0);
        drop(tile);
        drop(w);
        assert_eq!(b.in_use(), 0);
    }

    #[tokio::test]
    async fn revocation_wakes_the_holder() {
        let b = ConnectionBudget::new(Some(1), None);
        let Ok(dl) = b.try_acquire(Purpose::Download(RecordingId(1))) else {
            panic!("free slot");
        };
        let waiter = tokio::spawn(async move {
            dl.revoked().await;
            dl.purpose()
        });
        tokio::task::yield_now().await;
        let w = b.acquire_for_viewing(Purpose::Watch);
        assert!(w.is_ok());
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), waiter).await;
        assert!(matches!(got, Ok(Ok(Purpose::Download(_)))));
    }
}

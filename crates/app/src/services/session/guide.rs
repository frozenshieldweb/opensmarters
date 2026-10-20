// SPDX-License-Identifier: GPL-3.0-or-later
//! Guide (EPG grid) data of the session — and the **one** EPG cache every screen uses
//! (the Guide grid, Live TV's now/next, Home, Favourites, Search, the player banner).
//!
//! Two sources, picked per profile ([`Session::guide_uses_xmltv`]):
//!
//! - **Xtream API** (default for Xtream accounts): each channel's schedule is fetched
//!   **lazily, only for the rows on screen**. Rows live in a bounded LRU
//!   ([`ROW_CACHE_ROWS`]) for [`ROW_TTL`]. Two depths share the cache
//!   ([`RowDepth`]): the Guide ([`Session::guide_row`]) wants the full schedule
//!   (`get_simple_data_table`, else `get_short_epg` with 24 entries); now/next
//!   ([`Session::now_next`]) is happy with any row and otherwise asks for a short one
//!   (`get_short_epg`, [`BRIEF_PROGRAMMES`] entries). Requests go through one bounded
//!   queue ([`crate::services::fetch_queue`]): at most [`ROW_FETCHES`] at a time, the
//!   most recently shown rows first, and a request for a row that has not been on screen
//!   for [`ROW_WANTED_FOR`] is dropped before it reaches the provider — paging through
//!   10 000 channels never queues 10 000 requests.
//! - **XMLTV** (M3U `url-tvg`, or the profile's EPG URL override from the keychain):
//!   downloaded once per session with the capped HTTP client and parsed off the UI
//!   thread with [`opensmarters_core::xmltv::parse_filtered`], keeping only a time window
//!   ([`XMLTV_PAST`]..[`XMLTV_AHEAD`]), only channels of the playlist/catalogue, short
//!   texts and at most [`XMLTV_MAX_PROGRAMMES`] programmes, with tight document limits
//!   ([`xmltv_limits`]: [`XMLTV_MAX_BYTES`] decompressed, [`XMLTV_MAX_SEEN`] programmes
//!   looked at) — memory follows what is kept, not the size of the file. The same guide
//!   feeds Live TV's now/next.
//!
//! **Stale-while-revalidate:** an expired row keeps being returned (as `Ready`) while its
//! refresh is queued or running, and stays visible if the refresh fails (retried after
//! [`ROW_RETRY`]); it never turns back into a skeleton.
//!
//! EPG/XMLTV URLs embed credentials: they are registered with the redactor and never
//! logged or shown.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lru::LruCache;
use opensmarters_core::chrono::{DateTime, NaiveDateTime, Utc};
use opensmarters_core::epg::{ServerTimeZone, parse_server_datetime};
use opensmarters_core::xmltv::{self, Epg, XmltvLimits, XmltvProgramme};
use opensmarters_core::{ArchiveProgramme, EpgListing};

use super::Session;
use crate::error::{AppError, AppResult};
use crate::services::catalog::{LiveChannel, Programme};
use crate::services::epg::EpgState;
use crate::services::fetch_queue::{FetchQueue, QueueStats};
use crate::ui::text::{MAX_LINE_CHARS, cap, cap_paragraph};

/// How long a fetched row stays fresh.
pub const ROW_TTL: Duration = Duration::from_secs(10 * 60);
/// Retry a failed row after this long.
pub const ROW_RETRY: Duration = Duration::from_secs(2 * 60);
/// Rows kept in memory (each is one channel's schedule).
pub const ROW_CACHE_ROWS: usize = 320;
/// Concurrent EPG requests to the provider (Guide and now/next together).
pub const ROW_FETCHES: usize = 3;
/// A queued row request is dropped when its row has not been on screen for this long.
pub const ROW_WANTED_FOR: Duration = Duration::from_millis(1500);
/// Queued row requests kept at most (the least recently shown are dropped first).
pub const ROW_MAX_QUEUED: usize = 256;
/// Programmes asked for by a now/next lookup (`get_short_epg` limit).
pub const BRIEF_PROGRAMMES: u32 = 6;
/// XMLTV: programmes that ended more than this long ago are dropped.
pub const XMLTV_PAST: Duration = Duration::from_secs(36 * 3600);
/// XMLTV: programmes starting more than this far ahead are dropped.
pub const XMLTV_AHEAD: Duration = Duration::from_secs(7 * 24 * 3600);
/// XMLTV: at most this many programmes are kept (later ones are skipped, the guide is
/// marked as truncated).
pub const XMLTV_MAX_PROGRAMMES: usize = 400_000;
/// XMLTV: programmes looked at in the document at most (kept or not); the rest of the
/// document is skipped and the guide marked as truncated (bounds CPU on hostile files).
pub const XMLTV_MAX_SEEN: usize = 2_000_000;
/// XMLTV: largest document accepted once decompressed.
pub const XMLTV_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// XMLTV: description bytes kept per programme.
pub const XMLTV_DESC_BYTES: usize = 360;
/// XMLTV: title bytes kept per programme.
pub const XMLTV_TITLE_BYTES: usize = 240;

/// One channel's schedule, sorted by start, without overlaps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpgRow {
    /// Programmes (title/description capped).
    pub programmes: Vec<Programme>,
    /// Server wall-clock start of each programme (Xtream), for timeshift URLs.
    local_starts: Vec<Option<NaiveDateTime>>,
    /// From `get_simple_data_table` (archive flags are meaningful).
    pub full: bool,
}

impl EpgRow {
    /// From Xtream listings (any order; duplicates and overlaps are cleaned up).
    pub fn from_listings(listings: &[EpgListing], full: bool) -> Self {
        let mut items: Vec<(Programme, Option<NaiveDateTime>)> = listings
            .iter()
            .filter(|l| l.stop > l.start)
            .map(|l| {
                (
                    Programme::from_listing(l),
                    l.local_start.as_deref().and_then(parse_server_datetime),
                )
            })
            .collect();
        normalize(&mut items);
        let (programmes, local_starts) = items.into_iter().unzip();
        Self {
            programmes,
            local_starts,
            full,
        }
    }

    /// From app programmes (XMLTV; no server-local starts, no archive).
    pub fn from_programmes(list: Vec<Programme>) -> Self {
        let mut items: Vec<(Programme, Option<NaiveDateTime>)> = list
            .into_iter()
            .filter(|p| p.stop > p.start)
            .map(|p| (p, None))
            .collect();
        normalize(&mut items);
        let (programmes, local_starts) = items.into_iter().unzip();
        Self {
            programmes,
            local_starts,
            full: false,
        }
    }

    /// Index of the programme on air at `t`.
    pub fn index_at(&self, t: DateTime<Utc>) -> Option<usize> {
        let idx = self.programmes.partition_point(|p| p.start <= t);
        idx.checked_sub(1)
            .filter(|&i| self.programmes.get(i).is_some_and(|p| t < p.stop))
    }

    /// Index of the first programme ending after `t`.
    pub fn first_ending_after(&self, t: DateTime<Utc>) -> usize {
        self.programmes.partition_point(|p| p.stop <= t)
    }

    /// The catch-up view of programme `i` when it has an archive and has started.
    pub fn archive(
        &self,
        i: usize,
        tz: &ServerTimeZone,
        now: DateTime<Utc>,
    ) -> Option<ArchiveProgramme> {
        let p = self.programmes.get(i)?;
        if !p.has_archive || p.start > now {
            return None;
        }
        let start_local = self
            .local_starts
            .get(i)
            .copied()
            .flatten()
            .unwrap_or_else(|| tz.to_local(p.start));
        Some(ArchiveProgramme {
            title: p.title.clone(),
            description: p.description.clone(),
            start: p.start,
            stop: p.stop,
            start_local,
        })
    }

    /// True when at least one programme can be replayed.
    pub fn has_any_archive(&self, now: DateTime<Utc>) -> bool {
        self.programmes
            .iter()
            .any(|p| p.has_archive && p.start <= now)
    }
}

/// Sorts by start, drops repeated starts and clips overlaps (the earlier programme ends
/// where the next begins), so grid cells never overlap.
fn normalize(items: &mut Vec<(Programme, Option<NaiveDateTime>)>) {
    items.sort_by_key(|(p, _)| p.start);
    items.dedup_by(|b, a| a.0.start == b.0.start);
    for i in 1..items.len() {
        let next_start = items[i].0.start;
        let prev = &mut items[i - 1].0;
        if prev.stop > next_start {
            prev.stop = next_start;
        }
    }
    items.retain(|(p, _)| p.stop > p.start);
}

/// A row lookup.
#[derive(Debug, Clone)]
pub enum RowState {
    /// Being fetched.
    Loading,
    /// No guide for this channel.
    Missing,
    /// The schedule (possibly stale while it is refreshed).
    Ready(Arc<EpgRow>),
}

/// How much of a channel's schedule a cached row holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowDepth {
    /// A few programmes from now (`get_short_epg`): enough for now/next.
    Brief,
    /// The full schedule for the Guide (`get_simple_data_table`, else 24 short entries).
    Guide,
}

/// State of the session's XMLTV guide.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum XmltvStatus {
    /// Not requested yet.
    #[default]
    Idle,
    /// Downloading/parsing.
    Loading,
    /// Ready.
    Ready {
        /// Channels with programmes.
        channels: usize,
        /// Programmes kept.
        programmes: usize,
        /// Some programmes were skipped (the guide is very large).
        truncated: bool,
    },
    /// Failed (plain-language message).
    Failed(String),
}

#[derive(Debug, Clone)]
enum Slot {
    /// No data yet; a fetch of `depth` is queued or running.
    Pending { depth: RowDepth },
    /// A schedule. Stale after `valid_until` (still returned); `refreshing` = a fetch of
    /// that depth is queued or running; no new fetch before `hold_until` (after a
    /// failure).
    Ready {
        row: Arc<EpgRow>,
        depth: RowDepth,
        valid_until: Instant,
        refreshing: Option<RowDepth>,
        hold_until: Instant,
    },
    /// Nothing for this channel; retried after `until`.
    Failed { until: Instant },
}

/// One queued row request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RowJob {
    stream_id: u64,
    depth: RowDepth,
}

/// Per-session state of the guide/catch-up extensions.
#[derive(Debug)]
pub(super) struct ExtState {
    rows: Mutex<LruCache<String, Slot>>,
    queue: FetchQueue<String, RowJob>,
    xmltv_status: Mutex<XmltvStatus>,
    xmltv_started: std::sync::atomic::AtomicBool,
    pub(super) timeshift_override: Mutex<Option<opensmarters_store::TimeshiftFormat>>,
    /// A format saved to the profile in this session (the session's profile copy is older).
    pub(super) timeshift_saved: Mutex<Option<opensmarters_store::TimeshiftFormat>>,
}

impl Default for ExtState {
    fn default() -> Self {
        Self {
            rows: Mutex::new(LruCache::new(
                NonZeroUsize::new(ROW_CACHE_ROWS).unwrap_or(NonZeroUsize::MIN),
            )),
            queue: FetchQueue::new(ROW_FETCHES, ROW_WANTED_FOR, ROW_MAX_QUEUED),
            xmltv_status: Mutex::new(XmltvStatus::Idle),
            xmltv_started: std::sync::atomic::AtomicBool::new(false),
            timeshift_override: Mutex::new(None),
            timeshift_saved: Mutex::new(None),
        }
    }
}

impl ExtState {
    fn rows(&self) -> std::sync::MutexGuard<'_, LruCache<String, Slot>> {
        self.rows.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Looks up `key` wanting at least `want`. Returns what to show and, when a fetch
    /// must be queued, its depth (the slot is then marked pending/refreshing).
    fn lookup(&self, key: &str, want: RowDepth) -> (RowState, Option<RowDepth>) {
        let now = Instant::now();
        let mut rows = self.rows();
        let Some(slot) = rows.get_mut(key) else {
            rows.put(key.to_owned(), Slot::Pending { depth: want });
            return (RowState::Loading, Some(want));
        };
        match slot {
            Slot::Ready {
                row,
                depth,
                valid_until,
                refreshing,
                hold_until,
            } => {
                let expired = now >= *valid_until;
                let shallow = *depth < want;
                if !(expired || shallow) || now < *hold_until {
                    return (RowState::Ready(Arc::clone(row)), None);
                }
                let target = if expired { want.max(*depth) } else { want };
                if refreshing.is_some_and(|r| r >= target) {
                    // Already queued: keep showing the stale/short row meanwhile.
                    return (RowState::Ready(Arc::clone(row)), None);
                }
                *refreshing = Some(target);
                (RowState::Ready(Arc::clone(row)), Some(target))
            }
            Slot::Pending { depth } => {
                if *depth >= want {
                    (RowState::Loading, None)
                } else {
                    *depth = want;
                    (RowState::Loading, Some(want))
                }
            }
            Slot::Failed { until } if now < *until => (RowState::Missing, None),
            Slot::Failed { .. } => {
                *slot = Slot::Pending { depth: want };
                (RowState::Loading, Some(want))
            }
        }
    }

    /// Stores a fetched row of `depth` (an empty one counts as a failure).
    fn put(&self, key: &str, row: Arc<EpgRow>, depth: RowDepth) {
        if row.programmes.is_empty() {
            self.fail(key, depth);
            return;
        }
        let now = Instant::now();
        let mut rows = self.rows();
        // A deeper fetch still queued for this key stays marked.
        let still = |r: Option<RowDepth>| r.filter(|d| *d > depth);
        let slot = match rows.peek(key) {
            Some(Slot::Ready {
                row: old,
                depth: d0,
                valid_until,
                refreshing,
                hold_until,
            }) if *d0 > depth && now < *valid_until => Slot::Ready {
                // A shorter answer arrived after a full one: keep the full row.
                row: Arc::clone(old),
                depth: *d0,
                valid_until: *valid_until,
                refreshing: still(*refreshing),
                hold_until: *hold_until,
            },
            Some(Slot::Ready { refreshing, .. }) => Slot::Ready {
                row,
                depth,
                valid_until: now + ROW_TTL,
                refreshing: still(*refreshing),
                hold_until: now,
            },
            Some(Slot::Pending { depth: d }) => Slot::Ready {
                row,
                depth,
                valid_until: now + ROW_TTL,
                refreshing: still(Some(*d)),
                hold_until: now,
            },
            _ => Slot::Ready {
                row,
                depth,
                valid_until: now + ROW_TTL,
                refreshing: None,
                hold_until: now,
            },
        };
        rows.put(key.to_owned(), slot);
    }

    /// A fetch of `depth` failed: keep any row we have (retry later), else mark missing.
    fn fail(&self, key: &str, depth: RowDepth) {
        let now = Instant::now();
        let mut rows = self.rows();
        let still = |r: Option<RowDepth>| r.filter(|d| *d > depth);
        let slot = match rows.peek(key) {
            Some(Slot::Ready {
                row,
                depth: d0,
                valid_until,
                refreshing,
                ..
            }) => Slot::Ready {
                row: Arc::clone(row),
                depth: *d0,
                valid_until: *valid_until,
                refreshing: still(*refreshing),
                hold_until: now + ROW_RETRY,
            },
            Some(Slot::Pending { depth: d }) if *d > depth => Slot::Pending { depth: *d },
            _ => Slot::Failed {
                until: now + ROW_RETRY,
            },
        };
        rows.put(key.to_owned(), slot);
    }

    /// A queued fetch of `depth` was dropped (its row left the screen): forget the
    /// pending marker so the next lookup asks again.
    fn dropped(&self, key: &str, depth: RowDepth) {
        let mut rows = self.rows();
        match rows.peek_mut(key) {
            Some(Slot::Pending { depth: d }) if *d <= depth => {
                rows.pop(key);
            }
            Some(Slot::Ready { refreshing, .. }) if refreshing.is_some_and(|r| r <= depth) => {
                *refreshing = None;
            }
            _ => {}
        }
    }

    /// A fresh full (`get_simple_data_table`) row, if cached.
    fn fresh_full(&self, key: &str) -> Option<Arc<EpgRow>> {
        match self.rows().peek(key) {
            Some(Slot::Ready {
                row, valid_until, ..
            }) if row.full && Instant::now() < *valid_until => Some(Arc::clone(row)),
            _ => None,
        }
    }

    fn cached_rows(&self) -> Vec<(String, Arc<EpgRow>)> {
        self.rows()
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Ready { row, .. } => Some((k.clone(), Arc::clone(row))),
                _ => None,
            })
            .collect()
    }

    fn cached_xmltv(&self, key: &str) -> Option<Arc<EpgRow>> {
        match self.rows().get(key) {
            Some(Slot::Ready { row, .. }) => Some(Arc::clone(row)),
            _ => None,
        }
    }

    fn put_xmltv(&self, key: String, row: Arc<EpgRow>) {
        let now = Instant::now();
        self.rows().put(
            key,
            Slot::Ready {
                row,
                depth: RowDepth::Guide,
                valid_until: now + Duration::from_secs(30 * 24 * 3600),
                refreshing: None,
                hold_until: now,
            },
        );
    }

    /// Forgets every row and queued request (keeps the XMLTV state and formats).
    pub(super) fn clear_rows(&self) {
        let _ = self.queue.clear_pending();
        self.rows().clear();
    }

    /// Stops all row work (the session was replaced or closed).
    pub(super) fn close(&self) {
        self.queue.close();
    }

    fn set_status(&self, s: XmltvStatus) {
        *self
            .xmltv_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = s;
    }
}

impl Session {
    /// The guide/catch-up extension state.
    pub(super) fn ext_state(&self) -> Arc<ExtState> {
        Arc::clone(&self.ext)
    }

    /// The panel's time zone (server-local EPG strings, catch-up day tabs). UTC for M3U.
    pub fn server_time_zone(&self) -> ServerTimeZone {
        self.xtream()
            .map(|c| c.server_time_zone())
            .unwrap_or_default()
    }

    /// True when the guide comes from an XMLTV file (M3U `url-tvg` or the profile's EPG
    /// URL override) rather than the Xtream API.
    pub fn guide_uses_xmltv(&self) -> bool {
        !self.is_xtream() || self.profile.has_epg_url_override
    }

    /// The XMLTV guide's state (see [`Session::start_guide`]).
    pub fn xmltv_status(&self) -> XmltvStatus {
        self.ext
            .xmltv_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Starts loading the XMLTV guide in the background if this profile uses one (no-op
    /// otherwise, or when already started).
    pub fn start_guide(self: &Arc<Self>) {
        if !self.guide_uses_xmltv() || self.is_closed() {
            return;
        }
        if self
            .ext
            .xmltv_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let this = Arc::clone(self);
        self.services.spawn(async move {
            let _ = this.xmltv().await;
            this.services.egui().request_repaint();
        });
    }

    /// Non-blocking schedule lookup for a guide row. Call it every frame for the rows on
    /// screen: it queues a background fetch when needed (see the module docs).
    pub fn guide_row(self: &Arc<Self>, channel: &LiveChannel) -> RowState {
        if self.guide_uses_xmltv() {
            return self.xmltv_row(channel);
        }
        self.api_row(channel, RowDepth::Guide)
    }

    /// Non-blocking now/next data for a channel (Live TV, Home, Favourites, Search, the
    /// player banner): any cached row of the guide cache, else a short `get_short_epg`
    /// request through the same bounded queue. Call it every frame for what is on screen.
    pub fn epg(self: &Arc<Self>, channel: &LiveChannel) -> EpgState {
        let state = if self.guide_uses_xmltv() {
            self.xmltv_row(channel)
        } else {
            self.api_row(channel, RowDepth::Brief)
        };
        match state {
            RowState::Loading => EpgState::Loading,
            RowState::Missing => EpgState::Missing,
            RowState::Ready(r) => EpgState::Ready(r),
        }
    }

    /// Outstanding EPG requests (pending, running, dropped, peak concurrency): for logs,
    /// tests and the dev harness.
    pub fn epg_queue_stats(&self) -> QueueStats {
        self.ext.queue.stats()
    }

    fn api_row(self: &Arc<Self>, channel: &LiveChannel, want: RowDepth) -> RowState {
        let Some(stream_id) = channel.stream_id() else {
            return RowState::Missing;
        };
        if self.is_closed() {
            return match self.ext.cached_xmltv(&channel.key) {
                Some(r) => RowState::Ready(r),
                None => RowState::Missing,
            };
        }
        let (state, fetch) = self.ext.lookup(&channel.key, want);
        match fetch {
            Some(depth) => self.request_row(&channel.key, stream_id, depth),
            None => self.ext.queue.touch(&channel.key),
        }
        state
    }

    fn request_row(self: &Arc<Self>, key: &str, stream_id: u64, depth: RowDepth) {
        let r = self
            .ext
            .queue
            .request(key.to_owned(), RowJob { stream_id, depth }, |old, new| {
                old.depth = old.depth.max(new.depth);
            });
        for (k, job) in r.evicted {
            self.ext.dropped(&k, job.depth);
        }
        if r.spawn_worker {
            self.spawn_row_worker();
        }
    }

    /// One worker of the row queue: runs jobs until the queue is empty (at most
    /// [`ROW_FETCHES`] workers exist).
    fn spawn_row_worker(self: &Arc<Self>) {
        let this = Arc::clone(self);
        self.services.spawn(async move {
            let ext = Arc::clone(&this.ext);
            let mut worker = ext.queue.worker();
            let mut dropped = Vec::new();
            loop {
                let next = worker.next(&mut dropped);
                for (k, job) in dropped.drain(..) {
                    ext.dropped(&k, job.depth);
                }
                let Some((key, job)) = next else { break };
                // The profile was switched while this waited: never call the old provider.
                if this.is_closed() {
                    break;
                }
                match this.fetch_row(job.stream_id, job.depth).await {
                    Ok(row) => ext.put(&key, Arc::new(row), job.depth),
                    Err(e) => {
                        tracing::debug!(error = %e, "guide row fetch failed");
                        ext.fail(&key, job.depth);
                    }
                }
                let s = ext.queue.stats();
                tracing::debug!(
                    target: "opensmarters::queue",
                    pending = s.pending,
                    running = s.running,
                    peak = s.peak_running,
                    started = s.started,
                    dropped = s.dropped,
                    "epg queue"
                );
                this.services.egui().request_repaint();
            }
        });
    }

    /// The full schedule of an Xtream channel (`get_simple_data_table`, with archive
    /// flags), from the row cache when fresh. Used by Catch-up.
    ///
    /// # Errors
    ///
    /// Not an Xtream channel; provider/network errors.
    pub async fn full_row(self: &Arc<Self>, channel: &LiveChannel) -> AppResult<Arc<EpgRow>> {
        if let Some(row) = self.ext.fresh_full(&channel.key) {
            return Ok(row);
        }
        let stream_id = channel
            .stream_id()
            .ok_or_else(|| AppError::msg("Catch-up needs an Xtream Codes account."))?;
        if self.is_closed() {
            return Err(AppError::Cancelled);
        }
        let client = self.client()?;
        let list = client.simple_data_table(stream_id).await?;
        let row = Arc::new(EpgRow::from_listings(&list.items, true));
        if !row.programmes.is_empty() {
            self.ext
                .put(&channel.key, Arc::clone(&row), RowDepth::Guide);
        }
        Ok(row)
    }

    /// Every schedule currently cached (for searching programme titles).
    pub fn cached_guide_rows(&self) -> Vec<(String, Arc<EpgRow>)> {
        self.ext.cached_rows()
    }

    /// The XMLTV guide, if loaded (no download).
    pub fn loaded_xmltv(&self) -> Option<Arc<Epg>> {
        self.xmltv.get().cloned().flatten()
    }

    async fn fetch_row(self: &Arc<Self>, stream_id: u64, depth: RowDepth) -> AppResult<EpgRow> {
        let client = self.client()?;
        if depth == RowDepth::Brief {
            let short = client.short_epg(stream_id, Some(BRIEF_PROGRAMMES)).await?;
            return Ok(EpgRow::from_listings(&short.items, false));
        }
        let table = client.simple_data_table(stream_id).await?;
        if !table.items.is_empty() {
            return Ok(EpgRow::from_listings(&table.items, true));
        }
        if self.is_closed() {
            return Err(AppError::Cancelled);
        }
        let short = client.short_epg(stream_id, Some(24)).await?;
        Ok(EpgRow::from_listings(&short.items, false))
    }

    fn xmltv_row(self: &Arc<Self>, channel: &LiveChannel) -> RowState {
        let Some(epg_id) = channel.epg_id.as_deref() else {
            return RowState::Missing;
        };
        match self.xmltv.get() {
            None => {
                self.start_guide();
                RowState::Loading
            }
            Some(None) => RowState::Missing,
            Some(Some(epg)) => {
                let key = format!("x:{}", epg_id.to_lowercase());
                if let Some(row) = self.ext.cached_xmltv(&key) {
                    return RowState::Ready(row);
                }
                let list: Vec<Programme> = epg
                    .programmes_for(epg_id)
                    .iter()
                    .map(xmltv_programme)
                    .collect();
                if list.is_empty() {
                    return RowState::Missing;
                }
                let row = Arc::new(EpgRow::from_programmes(list));
                self.ext.put_xmltv(key, Arc::clone(&row));
                RowState::Ready(row)
            }
        }
    }

    /// Downloads and parses the profile's XMLTV guide with bounded memory (see the module
    /// docs). `None` when the profile has no guide URL or it failed (status says why).
    pub(super) async fn load_guide_xmltv(&self) -> Option<Arc<Epg>> {
        let st = self.ext_state();
        st.set_status(XmltvStatus::Loading);
        let url = match self.guide_url().await {
            Ok(Some(u)) => u,
            Ok(None) => {
                st.set_status(XmltvStatus::Failed(
                    "This playlist doesn't name a programme guide.".into(),
                ));
                return None;
            }
            Err(e) => {
                st.set_status(XmltvStatus::Failed(e.user_message()));
                return None;
            }
        };
        let Ok(parsed) = opensmarters_core::url::Url::parse(url.trim()) else {
            st.set_status(XmltvStatus::Failed(
                "The guide address isn't a valid web address.".into(),
            ));
            return None;
        };
        if self.is_closed() {
            st.set_status(XmltvStatus::Idle);
            return None;
        }
        let bytes = match self.http.get_bytes(&parsed).await {
            Ok(b) => b,
            Err(e) => {
                tracing::info!(error = %e, "guide download failed");
                st.set_status(XmltvStatus::Failed(AppError::from(e).user_message()));
                return None;
            }
        };
        let ids = self.known_epg_ids();
        let now = Utc::now();
        let parsed =
            match tokio::task::spawn_blocking(move || parse_guide(&bytes, ids.as_ref(), now)).await
            {
                Ok(r) => r,
                Err(e) if e.is_panic() => Err(AppError::Internal),
                Err(_) => Err(AppError::Cancelled),
            };
        match parsed {
            Ok((epg, truncated)) => {
                st.set_status(XmltvStatus::Ready {
                    channels: epg.programmes.len(),
                    programmes: epg.programme_count(),
                    truncated,
                });
                Some(Arc::new(epg))
            }
            Err(e) => {
                tracing::info!(error = %e, "guide could not be parsed");
                st.set_status(XmltvStatus::Failed(e.user_message()));
                None
            }
        }
    }

    /// The guide URL: the keychain override, else the playlist's first `url-tvg`.
    /// Registered with the redactor (it may carry credentials).
    async fn guide_url(&self) -> AppResult<Option<String>> {
        let url = if self.profile.has_epg_url_override {
            let id = self.profile.id;
            super::blocking(self.store(), move |s| s.profiles().epg_url_override(id))
                .await?
                .map(|s| {
                    use opensmarters_core::secrecy::ExposeSecret;
                    s.expose_secret().to_owned()
                })
        } else {
            None
        };
        let url = url.or_else(|| self.playlist().and_then(|p| p.tvg_urls.first().cloned()));
        if let Some(u) = &url {
            self.services.redactor.add_secret(u.trim());
        }
        Ok(url)
    }

    /// Lower-cased EPG ids of the channels we know (playlist entries or the cached
    /// Xtream stream list), to skip guide data for channels we don't have.
    fn known_epg_ids(&self) -> Option<HashSet<String>> {
        if let Some(p) = self.playlist() {
            let ids: HashSet<String> = p
                .entries
                .iter()
                .filter_map(|e| e.tvg_id.as_deref())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            return (!ids.is_empty()).then_some(ids);
        }
        let streams = self.memo_get::<Vec<opensmarters_core::LiveStream>>("xt:live_streams:all")?;
        let ids: HashSet<String> = streams
            .iter()
            .filter_map(|s| s.epg_channel_id.as_deref())
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        (!ids.is_empty()).then_some(ids)
    }
}

/// An app programme from an XMLTV one.
pub fn xmltv_programme(p: &XmltvProgramme) -> Programme {
    Programme {
        title: cap(p.title(None), MAX_LINE_CHARS).trim().to_owned(),
        description: cap_paragraph(p.description(None).unwrap_or_default(), 1200),
        start: p.start,
        stop: p
            .stop
            .unwrap_or(p.start + opensmarters_core::chrono::Duration::minutes(30)),
        has_archive: false,
    }
}

fn truncate_bytes(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
}

/// The document limits used for guide downloads (SR-11): far below the core defaults,
/// sized for a real panel (≈10 000 channels, a week of programmes).
pub fn xmltv_limits() -> XmltvLimits {
    XmltvLimits {
        max_decompressed_bytes: XMLTV_MAX_BYTES,
        max_channels: 100_000,
        // Only kept programmes count (see `parse_filtered`); the filter stops keeping at
        // `XMLTV_MAX_PROGRAMMES`.
        max_programmes: XMLTV_MAX_PROGRAMMES + 1,
        max_text_len: 1024,
        max_values_per_field: 4,
        max_token_bytes: 1024 * 1024,
    }
}

/// Parses an XMLTV document keeping a window around `now`, known channels only, trimmed
/// texts and a programme cap. Returns the guide and whether programmes were skipped
/// because of a cap.
///
/// # Errors
///
/// Not XMLTV, too large, malformed before any data.
pub fn parse_guide(
    bytes: &[u8],
    known_ids: Option<&HashSet<String>>,
    now: DateTime<Utc>,
) -> AppResult<(Epg, bool)> {
    parse_guide_with(bytes, known_ids, now, &xmltv_limits(), XMLTV_MAX_SEEN)
}

fn parse_guide_with(
    bytes: &[u8],
    known_ids: Option<&HashSet<String>>,
    now: DateTime<Utc>,
    limits: &XmltvLimits,
    max_seen: usize,
) -> AppResult<(Epg, bool)> {
    let from = now - opensmarters_core::chrono::Duration::from_std(XMLTV_PAST).unwrap_or_default();
    let until =
        now + opensmarters_core::chrono::Duration::from_std(XMLTV_AHEAD).unwrap_or_default();
    let mut kept = 0usize;
    let mut seen = 0usize;
    let mut truncated = false;
    let epg = xmltv::parse_filtered(bytes, limits, |p| {
        seen += 1;
        if seen > max_seen {
            truncated = true;
            return false;
        }
        let end = p
            .stop
            .unwrap_or(p.start + opensmarters_core::chrono::Duration::minutes(30));
        if end < from || p.start > until {
            return false;
        }
        if let Some(ids) = known_ids
            && !ids.contains(&p.channel.trim().to_lowercase())
        {
            return false;
        }
        if kept >= XMLTV_MAX_PROGRAMMES {
            truncated = true;
            return false;
        }
        kept += 1;
        // Keep one title and one description, short; drop the rest.
        p.titles.truncate(1);
        if let Some(t) = p.titles.first_mut() {
            truncate_bytes(&mut t.text, XMLTV_TITLE_BYTES);
        }
        p.descriptions.truncate(1);
        if let Some(d) = p.descriptions.first_mut() {
            truncate_bytes(&mut d.text, XMLTV_DESC_BYTES);
        }
        p.sub_titles.clear();
        p.categories.clear();
        p.episode_nums.clear();
        p.icon = None;
        p.catchup_id = None;
        true
    })
    .map_err(|e| AppError::msg(format!("The programme guide couldn't be read ({e}).")))?;
    Ok((epg, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_core::chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 1, h, m, 0)
            .single()
            .unwrap_or_default()
    }

    fn prog(title: &str, a: DateTime<Utc>, b: DateTime<Utc>) -> Programme {
        Programme {
            title: title.into(),
            description: String::new(),
            start: a,
            stop: b,
            has_archive: true,
        }
    }

    fn row(n: usize) -> Arc<EpgRow> {
        Arc::new(EpgRow::from_programmes(
            (0..n)
                .map(|i| prog("P", t(8 + i as u32, 0), t(9 + i as u32, 0)))
                .collect(),
        ))
    }

    #[test]
    fn rows_are_sorted_deduplicated_and_clipped() {
        let row = EpgRow::from_programmes(vec![
            prog("C", t(10, 0), t(11, 0)),
            prog("A", t(8, 0), t(9, 30)), // overlaps B: clipped to 9:00
            prog("B", t(9, 0), t(10, 0)),
            prog("B2", t(9, 0), t(9, 45)), // same start as B: dropped
            prog("Empty", t(12, 0), t(12, 0)),
        ]);
        let titles: Vec<&str> = row.programmes.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles.len(), 3);
        assert_eq!(titles[0], "A");
        assert_eq!(row.programmes[0].stop, t(9, 0));
        assert_eq!(row.index_at(t(9, 59)), Some(1));
        assert_eq!(row.index_at(t(11, 0)), None);
        assert_eq!(row.first_ending_after(t(9, 0)), 1);
        assert_eq!(row.first_ending_after(t(23, 0)), 3);
    }

    #[test]
    fn archive_uses_server_local_start() {
        let tz = ServerTimeZone::from_name("Europe/Paris").unwrap_or_default();
        let row = EpgRow::from_programmes(vec![
            prog("Past", t(8, 0), t(9, 0)),
            prog("Future", t(20, 0), t(21, 0)),
        ]);
        let a = row.archive(0, &tz, t(12, 0));
        assert_eq!(
            a.map(|a| a.start_local.to_string()).as_deref(),
            Some("2026-03-01 09:00:00")
        );
        assert!(row.archive(1, &tz, t(12, 0)).is_none(), "not started yet");
        assert!(row.has_any_archive(t(12, 0)));
    }

    fn expire(st: &ExtState, key: &str) {
        if let Some(Slot::Ready { valid_until, .. }) = st.rows().peek_mut(key) {
            *valid_until = Instant::now();
        }
    }

    #[test]
    fn expired_rows_stay_visible_while_refreshing_and_after_a_failed_refresh() {
        let st = ExtState::default();
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Loading, Some(RowDepth::Brief))
        ));
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Loading, None)
        ));
        st.put("1", row(3), RowDepth::Brief);
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Ready(_), None)
        ));
        expire(&st, "1");
        // First lookup after expiry: stale data + one refresh.
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Ready(_), Some(RowDepth::Brief))
        ));
        // Every later lookup keeps returning the stale row (no skeleton, no second fetch).
        for _ in 0..5 {
            assert!(matches!(
                st.lookup("1", RowDepth::Brief),
                (RowState::Ready(_), None)
            ));
        }
        // The refresh failed: still the old row, and no new fetch until the retry delay.
        st.fail("1", RowDepth::Brief);
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Ready(_), None)
        ));
        // A refresh dropped from the queue (row scrolled away) also keeps the data.
        expire(&st, "1");
        if let Some(Slot::Ready { hold_until, .. }) = st.rows().peek_mut("1") {
            *hold_until = Instant::now();
        }
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Ready(_), Some(RowDepth::Brief))
        ));
        st.dropped("1", RowDepth::Brief);
        assert!(matches!(
            st.lookup("1", RowDepth::Brief),
            (RowState::Ready(_), Some(RowDepth::Brief))
        ));
    }

    #[test]
    fn brief_rows_serve_now_next_and_are_upgraded_for_the_guide() {
        let st = ExtState::default();
        let _ = st.lookup("7", RowDepth::Brief);
        st.put("7", row(2), RowDepth::Brief);
        // The guide wants more: it sees the short row at once and asks for the full one.
        assert!(matches!(
            st.lookup("7", RowDepth::Guide),
            (RowState::Ready(_), Some(RowDepth::Guide))
        ));
        assert!(matches!(
            st.lookup("7", RowDepth::Guide),
            (RowState::Ready(_), None)
        ));
        st.put("7", row(5), RowDepth::Guide);
        // A late short answer never replaces the full row.
        st.put("7", row(1), RowDepth::Brief);
        match st.lookup("7", RowDepth::Brief) {
            (RowState::Ready(r), None) => assert_eq!(r.programmes.len(), 5),
            other => panic!("unexpected {other:?}"),
        }
        // A pending slot dropped from the queue is forgotten.
        let _ = st.lookup("8", RowDepth::Brief);
        st.dropped("8", RowDepth::Brief);
        assert!(matches!(
            st.lookup("8", RowDepth::Brief),
            (RowState::Loading, Some(RowDepth::Brief))
        ));
        // Empty answers mean "no guide" (retried later).
        st.put("8", Arc::new(EpgRow::default()), RowDepth::Brief);
        assert!(matches!(
            st.lookup("8", RowDepth::Brief),
            (RowState::Missing, None)
        ));
    }

    #[test]
    fn guide_parse_keeps_window_and_known_channels() {
        let now = Utc
            .with_ymd_and_hms(2026, 3, 10, 12, 0, 0)
            .single()
            .unwrap_or_default();
        let mut doc = String::from("<tv>");
        for (ch, day) in [("a", 1), ("a", 10), ("A", 10), ("b", 10), ("a", 30)] {
            doc.push_str(&format!(
                "<programme channel=\"{ch}\" start=\"202603{day:02}120000 +0000\" \
                 stop=\"202603{day:02}130000 +0000\"><title>{}</title><desc>{}</desc>\
                 <category>x</category></programme>",
                "T".repeat(600),
                "é".repeat(400)
            ));
        }
        doc.push_str("</tv>");
        let ids: HashSet<String> = ["a".to_owned()].into_iter().collect();
        let (epg, truncated) =
            parse_guide(doc.as_bytes(), Some(&ids), now).unwrap_or_else(|e| panic!("{e}"));
        assert!(!truncated);
        // Day 1 and day 30 are outside the window; "b" is unknown; "a"/"A" both kept.
        assert_eq!(epg.programme_count(), 2);
        let p = &epg.programmes_for("a")[0];
        assert!(p.categories.is_empty());
        let d = p.description(None).unwrap_or_default();
        assert!(d.len() <= XMLTV_DESC_BYTES + '…'.len_utf8());
        assert!(d.ends_with('…'));
        assert!(p.title(None).len() <= XMLTV_TITLE_BYTES + '…'.len_utf8());
        assert!(parse_guide(b"not xml at all", None, now).is_err());
    }

    #[test]
    fn guide_limits_are_tight_and_seen_cap_truncates() {
        let l = xmltv_limits();
        assert!(l.max_decompressed_bytes <= 256 * 1024 * 1024);
        assert!(l.max_token_bytes <= 1024 * 1024);
        let now = Utc
            .with_ymd_and_hms(2026, 3, 10, 12, 0, 0)
            .single()
            .unwrap_or_default();
        let mut doc = String::from("<tv>");
        for h in 0..10 {
            doc.push_str(&format!(
                "<programme channel=\"a\" start=\"20260310{h:02}0000 +0000\" \
                 stop=\"20260310{h:02}3000 +0000\"><title>T</title></programme>"
            ));
        }
        doc.push_str("</tv>");
        let (epg, truncated) =
            parse_guide_with(doc.as_bytes(), None, now, &l, 4).unwrap_or_else(|e| panic!("{e}"));
        assert!(truncated, "programmes past the seen cap are skipped");
        assert_eq!(epg.programme_count(), 4);
        // A document larger than the decompressed cap is refused.
        let tiny = XmltvLimits {
            max_decompressed_bytes: 64,
            ..l
        };
        assert!(parse_guide_with(doc.as_bytes(), None, now, &tiny, 100).is_err());
    }
}

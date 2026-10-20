// SPDX-License-Identifier: GPL-3.0-or-later
//! Guide (EPG grid) data of the session.
//!
//! Two sources, picked per profile ([`Session::guide_uses_xmltv`]):
//!
//! - **Xtream API** (default for Xtream accounts): each channel's schedule is fetched
//!   **lazily, only for the rows on screen** ([`Session::guide_row`]): first
//!   `get_simple_data_table` (past days with archive flags + upcoming), else
//!   `get_short_epg`. Rows live in a bounded LRU ([`ROW_CACHE_ROWS`]) for [`ROW_TTL`];
//!   at most [`ROW_FETCHES`] requests run at once, and a queued request for a row that
//!   scrolled away before its turn is dropped, so flinging through 10 000 channels does
//!   not fire 10 000 requests.
//! - **XMLTV** (M3U `url-tvg`, or the profile's EPG URL override from the keychain):
//!   downloaded once per session with the capped HTTP client and parsed off the UI
//!   thread with [`opensmarters_core::xmltv::parse_filtered`], keeping only a time window
//!   ([`XMLTV_PAST`]..[`XMLTV_AHEAD`]), only channels of the playlist/catalogue, short
//!   texts and at most [`XMLTV_MAX_PROGRAMMES`] programmes — memory follows what is kept,
//!   not the size of the file. The same guide feeds Live TV's now/next.
//!
//! EPG/XMLTV URLs embed credentials: they are registered with the redactor and never
//! logged or shown.

use std::collections::{HashMap, HashSet};
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
use crate::ui::text::{MAX_LINE_CHARS, cap, cap_paragraph};

/// How long a fetched row stays fresh.
pub const ROW_TTL: Duration = Duration::from_secs(10 * 60);
/// Retry a failed row after this long.
pub const ROW_RETRY: Duration = Duration::from_secs(2 * 60);
/// Rows kept in memory (each is one channel's schedule).
pub const ROW_CACHE_ROWS: usize = 320;
/// Concurrent row requests to the provider.
pub const ROW_FETCHES: usize = 3;
/// A queued row request is dropped when its row has not been on screen for this long.
pub const ROW_WANTED_FOR: Duration = Duration::from_millis(1500);
/// XMLTV: programmes that ended more than this long ago are dropped.
pub const XMLTV_PAST: Duration = Duration::from_secs(36 * 3600);
/// XMLTV: programmes starting more than this far ahead are dropped.
pub const XMLTV_AHEAD: Duration = Duration::from_secs(7 * 24 * 3600);
/// XMLTV: at most this many programmes are kept (later ones are skipped, the guide is
/// marked as truncated).
pub const XMLTV_MAX_PROGRAMMES: usize = 400_000;
/// XMLTV: description bytes kept per programme.
pub const XMLTV_DESC_BYTES: usize = 360;

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
    /// The schedule.
    Ready(Arc<EpgRow>),
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
    Pending,
    Ready { at: Instant, row: Arc<EpgRow> },
    Failed { at: Instant },
}

/// Per-session state of the guide/catch-up extensions (kept in the session memo).
#[derive(Debug)]
pub(super) struct ExtState {
    rows: Mutex<LruCache<String, Slot>>,
    wanted: Mutex<HashMap<String, Instant>>,
    permits: Arc<tokio::sync::Semaphore>,
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
            wanted: Mutex::new(HashMap::new()),
            permits: Arc::new(tokio::sync::Semaphore::new(ROW_FETCHES)),
            xmltv_status: Mutex::new(XmltvStatus::Idle),
            xmltv_started: std::sync::atomic::AtomicBool::new(false),
            timeshift_override: Mutex::new(None),
            timeshift_saved: Mutex::new(None),
        }
    }
}

impl ExtState {
    fn lookup(&self, key: &str) -> (RowState, bool) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        match rows.get(key).cloned() {
            Some(Slot::Ready { at, row }) if at.elapsed() < ROW_TTL => {
                (RowState::Ready(row), false)
            }
            Some(Slot::Ready { row, .. }) => {
                // Expired: keep showing it while refreshing.
                rows.put(key.to_owned(), Slot::Pending);
                (RowState::Ready(row), true)
            }
            Some(Slot::Pending) => (RowState::Loading, false),
            Some(Slot::Failed { at }) if at.elapsed() < ROW_RETRY => (RowState::Missing, false),
            Some(Slot::Failed { .. }) | None => {
                rows.put(key.to_owned(), Slot::Pending);
                (RowState::Loading, true)
            }
        }
    }

    fn put(&self, key: &str, row: Arc<EpgRow>) {
        let slot = if row.programmes.is_empty() {
            Slot::Failed { at: Instant::now() }
        } else {
            Slot::Ready {
                at: Instant::now(),
                row,
            }
        };
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .put(key.to_owned(), slot);
    }

    fn fail(&self, key: &str) {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .put(key.to_owned(), Slot::Failed { at: Instant::now() });
    }

    /// Forgets a pending slot (its row scrolled away before the request ran).
    fn forget_pending(&self, key: &str) {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(rows.peek(key), Some(Slot::Pending)) {
            rows.pop(key);
        }
    }

    fn touch(&self, key: &str) {
        let mut w = self.wanted.lock().unwrap_or_else(PoisonError::into_inner);
        if w.len() > 4096 {
            w.retain(|_, at| at.elapsed() < ROW_WANTED_FOR);
        }
        w.insert(key.to_owned(), Instant::now());
    }

    fn still_wanted(&self, key: &str) -> bool {
        self.wanted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .is_some_and(|at| at.elapsed() < ROW_WANTED_FOR)
    }

    /// A fresh full (`get_simple_data_table`) row, if cached.
    fn fresh_full(&self, key: &str) -> Option<Arc<EpgRow>> {
        match self
            .rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .peek(key)
        {
            Some(Slot::Ready { at, row }) if row.full && at.elapsed() < ROW_TTL => {
                Some(Arc::clone(row))
            }
            _ => None,
        }
    }

    fn cached_rows(&self) -> Vec<(String, Arc<EpgRow>)> {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Ready { row, .. } => Some((k.clone(), Arc::clone(row))),
                _ => None,
            })
            .collect()
    }

    fn set_status(&self, s: XmltvStatus) {
        *self
            .xmltv_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = s;
    }
}

const EXT_KEY: &str = "ext:guide-state";
/// The memo TTL of the extension state (it lives as long as the session in practice).
const EXT_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

impl Session {
    /// The guide/catch-up extension state (created on first use).
    pub(super) fn ext_state(&self) -> Arc<ExtState> {
        if let Some(s) = self.memo_get::<ExtState>(EXT_KEY) {
            return s;
        }
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = memo
            .get(EXT_KEY)
            .and_then(|e| Arc::clone(&e.value).downcast::<ExtState>().ok())
        {
            return s;
        }
        let s = Arc::new(ExtState::default());
        memo.insert(
            EXT_KEY.to_owned(),
            super::MemoEntry {
                valid_until: Instant::now() + EXT_TTL,
                value: Arc::clone(&s) as Arc<dyn std::any::Any + Send + Sync>,
            },
        );
        s
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
        self.ext_state()
            .xmltv_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Starts loading the XMLTV guide in the background if this profile uses one (no-op
    /// otherwise, or when already started).
    pub fn start_guide(self: &Arc<Self>) {
        if !self.guide_uses_xmltv() {
            return;
        }
        let st = self.ext_state();
        if st
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
    /// screen: it schedules a background fetch when needed (see the module docs).
    pub fn guide_row(self: &Arc<Self>, channel: &LiveChannel) -> RowState {
        let st = self.ext_state();
        if self.guide_uses_xmltv() {
            return self.xmltv_row(&st, channel);
        }
        let Some(stream_id) = channel.stream_id() else {
            return RowState::Missing;
        };
        st.touch(&channel.key);
        let (state, fetch) = st.lookup(&channel.key);
        if fetch {
            let this = Arc::clone(self);
            let key = channel.key.clone();
            self.services.spawn(async move {
                let st = this.ext_state();
                let permits = Arc::clone(&st.permits);
                let Ok(_permit) = permits.acquire_owned().await else {
                    return;
                };
                if !st.still_wanted(&key) {
                    st.forget_pending(&key);
                    return;
                }
                match this.fetch_row(stream_id).await {
                    Ok(row) => st.put(&key, Arc::new(row)),
                    Err(e) => {
                        tracing::debug!(error = %e, "guide row fetch failed");
                        st.fail(&key);
                    }
                }
                this.services.egui().request_repaint();
            });
        }
        state
    }

    /// The full schedule of an Xtream channel (`get_simple_data_table`, with archive
    /// flags), from the row cache when fresh. Used by Catch-up.
    ///
    /// # Errors
    ///
    /// Not an Xtream channel; provider/network errors.
    pub async fn full_row(self: &Arc<Self>, channel: &LiveChannel) -> AppResult<Arc<EpgRow>> {
        let st = self.ext_state();
        if let Some(row) = st.fresh_full(&channel.key) {
            return Ok(row);
        }
        let stream_id = channel
            .stream_id()
            .ok_or_else(|| AppError::msg("Catch-up needs an Xtream Codes account."))?;
        let client = self.client()?;
        let list = client.simple_data_table(stream_id).await?;
        let row = Arc::new(EpgRow::from_listings(&list.items, true));
        if !row.programmes.is_empty() {
            st.put(&channel.key, Arc::clone(&row));
        }
        Ok(row)
    }

    /// Every schedule currently cached (for searching programme titles).
    pub fn cached_guide_rows(&self) -> Vec<(String, Arc<EpgRow>)> {
        self.ext_state().cached_rows()
    }

    /// The XMLTV guide, if loaded (no download).
    pub fn loaded_xmltv(&self) -> Option<Arc<Epg>> {
        self.xmltv.get().cloned().flatten()
    }

    async fn fetch_row(self: &Arc<Self>, stream_id: u64) -> AppResult<EpgRow> {
        let client = self.client()?;
        let table = client.simple_data_table(stream_id).await?;
        if !table.items.is_empty() {
            return Ok(EpgRow::from_listings(&table.items, true));
        }
        let short = client.short_epg(stream_id, Some(24)).await?;
        Ok(EpgRow::from_listings(&short.items, false))
    }

    fn xmltv_row(self: &Arc<Self>, st: &ExtState, channel: &LiveChannel) -> RowState {
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
                if let Some(Slot::Ready { row, .. }) = st
                    .rows
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&key)
                {
                    return RowState::Ready(Arc::clone(row));
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
                st.rows.lock().unwrap_or_else(PoisonError::into_inner).put(
                    key,
                    Slot::Ready {
                        at: Instant::now(),
                        row: Arc::clone(&row),
                    },
                );
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
        let parsed = tokio::task::spawn_blocking(move || parse_guide(&bytes, ids.as_ref(), now))
            .await
            .map_err(|_| AppError::Cancelled)
            .and_then(|r| r);
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

/// Parses an XMLTV document keeping a window around `now`, known channels only, trimmed
/// texts and a programme cap. Returns the guide and whether programmes were skipped
/// because of the cap.
///
/// # Errors
///
/// Not XMLTV, too large, malformed before any data.
pub fn parse_guide(
    bytes: &[u8],
    known_ids: Option<&HashSet<String>>,
    now: DateTime<Utc>,
) -> AppResult<(Epg, bool)> {
    let from = now - opensmarters_core::chrono::Duration::from_std(XMLTV_PAST).unwrap_or_default();
    let until =
        now + opensmarters_core::chrono::Duration::from_std(XMLTV_AHEAD).unwrap_or_default();
    let limits = XmltvLimits {
        max_programmes: XMLTV_MAX_PROGRAMMES + 1,
        max_text_len: 2048,
        max_values_per_field: 4,
        ..XmltvLimits::default()
    };
    let mut kept = 0usize;
    let mut truncated = false;
    let epg = xmltv::parse_filtered(bytes, &limits, |p| {
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
                 stop=\"202603{day:02}130000 +0000\"><title>T</title><desc>{}</desc>\
                 <category>x</category></programme>",
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
        assert!(parse_guide(b"not xml at all", None, now).is_err());
    }
}

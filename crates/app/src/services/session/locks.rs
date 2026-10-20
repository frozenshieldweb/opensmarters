// SPDX-License-Identifier: GPL-3.0-or-later
//! Parental locks where content is **used**, not only in the list that led there
//! (SECURITY.md SR-02).
//!
//! - **Every category of an item counts** ([`Session::is_item_locked`]): a channel or
//!   film listed under an open category *and* a locked one is locked.
//! - **Requests refuse locked items**: `live_request`, `movie_request`,
//!   `episode_request` / `series_episode_request`, `catchup_request(_with)` and
//!   `resume_request` return [`AppError::Locked`] with the category to unlock. The UI
//!   asks for the PIN (`widgets::PinPrompt`), calls [`Session::unlock_category`] and
//!   retries. When the item's categories aren't known yet (a Continue-watching film
//!   whose list was never loaded), the error has no category: the session starts a
//!   look-up in the background (one cached `get_vod_info` / `get_series_info`), and
//!   [`Session::verify_item`] awaits it; if it still can't tell, the PIN unlocks just
//!   that item ([`Session::unlock_item`]). Unknown = locked (fail closed) while a PIN is
//!   set.
//! - **Lists** use [`Session::filter_unlocked`] / [`Session::retain_unlocked`] /
//!   [`Session::is_locked`] with any [`Lockable`] item (channels, films, series,
//!   favourites, resume points, recent channels).
//! - **Where categories come from**: every list and detail the session loads
//!   (categories, VOD/series lists and infos, the live catalogue) is indexed on the way
//!   in; category names feed the keyword locks.
//! - **Re-lock policy**: unlocks last for the session and expire after
//!   [`RELOCK_IDLE`] (30 min) without user input (the session manager reports input
//!   every frame, [`Session::note_activity`]); switching profiles or restarting the app
//!   locks everything again (unlocks are never stored). [`Session::relock_all`] locks
//!   at once.
//! - **Artwork of locked categories stays in memory** (SR-13): their image URLs are
//!   registered with the image service as private (no disk cache, existing files are
//!   deleted).

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use opensmarters_core::catalog::CategorySelection;
use opensmarters_core::{Category, CategoryId, LiveStream, Series, SeriesInfo, VodInfo, VodStream};
use opensmarters_store::{
    CategoryLock, ContentKind, Favourite, PlaybackKind, PlaybackProgress, RecentChannel,
};

use super::{ParentalState, Session};
use crate::error::{AppError, AppResult, LockedItem};
use crate::services::catalog::{LiveCatalog, LiveChannel};
use crate::services::fetch_queue::FetchQueue;

/// Unlocked categories and items lock again after this long without user input.
pub const RELOCK_IDLE: Duration = Duration::from_secs(30 * 60);
/// Concurrent category look-ups (`get_vod_info`/`get_series_info`, usually cache hits).
pub const RESOLVE_WORKERS: usize = 2;
/// A look-up that failed is not retried before this.
pub const RESOLVE_RETRY: Duration = Duration::from_secs(120);

/// The lock state of one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemLock {
    /// Nothing locks it (or no PIN is set).
    Unlocked,
    /// It is in this locked category.
    Locked {
        /// The category to unlock.
        category: CategoryId,
        /// Its name, when known.
        name: Option<String>,
    },
    /// Its categories aren't known yet (a look-up runs in the background).
    Unknown,
}

/// An item whose parental lock can be checked. Implemented for channels, films,
/// series, favourites, resume points and recent channels.
pub trait Lockable {
    /// The section whose locks apply.
    fn lock_kind(&self) -> ContentKind;
    /// The item id (stream id / series id / channel key).
    fn lock_id(&self) -> String;
    /// The item's categories when the item carries them (catalogue items); `None` = look
    /// them up in what the session loaded.
    fn lock_categories(&self) -> Option<Vec<CategoryId>>;
}

impl Lockable for LiveChannel {
    fn lock_kind(&self) -> ContentKind {
        ContentKind::Live
    }
    fn lock_id(&self) -> String {
        self.key.clone()
    }
    fn lock_categories(&self) -> Option<Vec<CategoryId>> {
        Some(self.categories.clone())
    }
}

fn all_categories(primary: Option<&CategoryId>, more: &[CategoryId]) -> Vec<CategoryId> {
    let mut out: Vec<CategoryId> = primary.cloned().into_iter().collect();
    for c in more {
        if !out.contains(c) {
            out.push(c.clone());
        }
    }
    out
}

impl Lockable for VodStream {
    fn lock_kind(&self) -> ContentKind {
        ContentKind::Movie
    }
    fn lock_id(&self) -> String {
        self.stream_id.to_string()
    }
    fn lock_categories(&self) -> Option<Vec<CategoryId>> {
        Some(all_categories(
            self.category_id.as_ref(),
            &self.category_ids,
        ))
    }
}

impl Lockable for Series {
    fn lock_kind(&self) -> ContentKind {
        ContentKind::Series
    }
    fn lock_id(&self) -> String {
        self.series_id.to_string()
    }
    fn lock_categories(&self) -> Option<Vec<CategoryId>> {
        Some(all_categories(
            self.category_id.as_ref(),
            &self.category_ids,
        ))
    }
}

impl Lockable for Favourite {
    fn lock_kind(&self) -> ContentKind {
        self.kind
    }
    fn lock_id(&self) -> String {
        self.item_id.clone()
    }
    fn lock_categories(&self) -> Option<Vec<CategoryId>> {
        None
    }
}

impl Lockable for RecentChannel {
    fn lock_kind(&self) -> ContentKind {
        ContentKind::Live
    }
    fn lock_id(&self) -> String {
        self.stream_id.clone()
    }
    fn lock_categories(&self) -> Option<Vec<CategoryId>> {
        None
    }
}

impl Lockable for PlaybackProgress {
    fn lock_kind(&self) -> ContentKind {
        match self.kind {
            PlaybackKind::Movie => ContentKind::Movie,
            PlaybackKind::Episode => ContentKind::Series,
            PlaybackKind::Catchup => ContentKind::Live,
        }
    }
    fn lock_id(&self) -> String {
        match self.kind {
            PlaybackKind::Movie => self.item_id.clone(),
            // Episodes are locked by their series.
            PlaybackKind::Episode => self.snapshot.series_id.clone().unwrap_or_default(),
            // "<stream_id>@<start>": the channel.
            PlaybackKind::Catchup => self
                .item_id
                .split_once('@')
                .map_or_else(|| self.item_id.clone(), |(sid, _)| sid.to_owned()),
        }
    }
    fn lock_categories(&self) -> Option<Vec<CategoryId>> {
        None
    }
}

/// What a background look-up fetches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Resolve {
    /// The category list of a section (names for keyword locks).
    Categories(ContentKind),
    /// One item's categories.
    Item(ContentKind, String),
}

#[derive(Debug, Default)]
struct Unlocks {
    categories: HashSet<(ContentKind, String)>,
    items: HashSet<(ContentKind, String)>,
}

/// Per-session lock bookkeeping (see the module docs).
#[derive(Debug)]
pub(super) struct LockState {
    names: RwLock<HashMap<ContentKind, HashMap<String, String>>>,
    items: RwLock<HashMap<(ContentKind, u64), Vec<CategoryId>>>,
    unlocks: RwLock<Unlocks>,
    last_activity: Mutex<Instant>,
    resolve: FetchQueue<Resolve, ()>,
    resolve_failed: Mutex<HashMap<Resolve, Instant>>,
}

impl Default for LockState {
    fn default() -> Self {
        Self {
            names: RwLock::default(),
            items: RwLock::default(),
            unlocks: RwLock::default(),
            last_activity: Mutex::new(Instant::now()),
            resolve: FetchQueue::new(RESOLVE_WORKERS, Duration::from_secs(5), 512),
            resolve_failed: Mutex::default(),
        }
    }
}

impl LockState {
    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .elapsed()
    }

    pub(super) fn close(&self) {
        self.resolve.close();
    }
}

impl ParentalState {
    /// True if a lock can apply to `kind` at all (fast path for the common case).
    fn any_lock(&self, kind: ContentKind) -> bool {
        self.pin_set
            && (self
                .matcher
                .as_ref()
                .is_some_and(|m| m.keywords().next().is_some())
                || self
                    .locks
                    .get(&kind)
                    .is_some_and(|m| m.values().any(|l| *l == CategoryLock::Locked)))
    }

    /// Whether a category is locked by its settings (ignoring session unlocks): `None`
    /// when that depends on a keyword match and the name is unknown.
    pub(super) fn restricted(
        &self,
        kind: ContentKind,
        id: &str,
        name: Option<&str>,
    ) -> Option<bool> {
        if !self.pin_set {
            return Some(false);
        }
        match self.locks.get(&kind).and_then(|m| m.get(id)) {
            Some(CategoryLock::Locked) => Some(true),
            Some(CategoryLock::Unlocked) => Some(false),
            None => match &self.matcher {
                None => Some(false),
                Some(m) if m.keywords().next().is_none() => Some(false),
                Some(m) => name.map(|n| m.is_match(n)),
            },
        }
    }
}

impl Session {
    fn parental_state(&self) -> std::sync::RwLockReadGuard<'_, ParentalState> {
        self.parental.read().unwrap_or_else(PoisonError::into_inner)
    }

    // ---------------------------------------------------------------------------------
    // Unlocks and the idle re-lock
    // ---------------------------------------------------------------------------------

    /// Reports user input (the session manager calls it every frame with input). After
    /// [`RELOCK_IDLE`] without input, every unlock of this session expires.
    pub fn note_activity(&self) {
        let idle = {
            let mut last = self
                .lock_state
                .last_activity
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let idle = last.elapsed();
            *last = Instant::now();
            idle
        };
        if idle >= RELOCK_IDLE {
            self.relock_all();
        }
    }

    /// Locks every category and item unlocked in this session again.
    pub fn relock_all(&self) {
        let mut u = self
            .lock_state
            .unlocks
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        u.categories.clear();
        u.items.clear();
    }

    fn unlocks_live(&self) -> bool {
        self.lock_state.idle_for() < RELOCK_IDLE
    }

    fn category_unlocked(&self, kind: ContentKind, id: &str) -> bool {
        self.unlocks_live()
            && self
                .lock_state
                .unlocks
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .categories
                .contains(&(kind, id.to_owned()))
    }

    fn item_unlocked(&self, kind: ContentKind, item: &str) -> bool {
        self.unlocks_live()
            && self
                .lock_state
                .unlocks
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .items
                .contains(&(kind, item.to_owned()))
    }

    /// Unlocks a category until the session ends or [`RELOCK_IDLE`] passes without input
    /// (after a correct PIN).
    pub fn unlock_category(&self, kind: ContentKind, id: &CategoryId) {
        self.note_activity();
        self.lock_state
            .unlocks
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .categories
            .insert((kind, id.as_str().to_owned()));
    }

    /// Unlocks one item (after a correct PIN for an [`AppError::Locked`] without a
    /// category), with the same expiry as [`Session::unlock_category`].
    pub fn unlock_item(&self, kind: ContentKind, item: &str) {
        self.note_activity();
        self.lock_state
            .unlocks
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .items
            .insert((kind, item.to_owned()));
    }

    // ---------------------------------------------------------------------------------
    // Categories
    // ---------------------------------------------------------------------------------

    /// True if a category needs the PIN (manual lock, or a keyword match unless manually
    /// unlocked), and has not been unlocked in this session.
    pub fn is_category_locked(&self, kind: ContentKind, id: &CategoryId, name: &str) -> bool {
        let restricted = self
            .parental_state()
            .restricted(kind, id.as_str(), Some(name))
            .unwrap_or(false);
        restricted && !self.category_unlocked(kind, id.as_str())
    }

    /// The selection is locked (All/Favourites never are).
    pub fn is_selection_locked(
        &self,
        kind: ContentKind,
        sel: &CategorySelection,
        name: &str,
    ) -> bool {
        match sel {
            CategorySelection::Category(id) => self.is_category_locked(kind, id, name),
            _ => false,
        }
    }

    /// The display name of a category, from the lists the session loaded (M3U groups are
    /// their own names).
    pub fn category_name(&self, kind: ContentKind, id: &CategoryId) -> Option<String> {
        if kind == ContentKind::Live && !self.is_xtream() {
            return Some(id.as_str().to_owned());
        }
        self.lock_state
            .names
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&kind)
            .and_then(|m| m.get(id.as_str()).cloned())
    }

    // ---------------------------------------------------------------------------------
    // Items
    // ---------------------------------------------------------------------------------

    /// The lock state of an item with these categories (every one counts).
    fn categories_lock(&self, kind: ContentKind, categories: &[CategoryId]) -> ItemLock {
        let mut unknown = false;
        let mut names_needed = false;
        {
            let p = self.parental_state();
            if !p.any_lock(kind) {
                return ItemLock::Unlocked;
            }
            for c in categories {
                let name = self.category_name(kind, c);
                match p.restricted(kind, c.as_str(), name.as_deref()) {
                    Some(true) if !self.category_unlocked(kind, c.as_str()) => {
                        return ItemLock::Locked {
                            category: c.clone(),
                            name,
                        };
                    }
                    Some(_) => {}
                    None => {
                        unknown = true;
                        names_needed = true;
                    }
                }
            }
        }
        if names_needed {
            self.schedule_resolve(Resolve::Categories(kind));
        }
        if unknown {
            ItemLock::Unknown
        } else {
            ItemLock::Unlocked
        }
    }

    /// The lock state of an item: its own `categories` when given, else what the
    /// session indexed for it. Unknown items get a background look-up.
    pub fn item_lock(
        &self,
        kind: ContentKind,
        item: &str,
        categories: Option<&[CategoryId]>,
    ) -> ItemLock {
        if !self.parental_state().any_lock(kind) || self.item_unlocked(kind, item) {
            return ItemLock::Unlocked;
        }
        if let Some(cats) = categories {
            return self.categories_lock(kind, cats);
        }
        match self.item_categories(kind, item) {
            Some(cats) => self.categories_lock(kind, &cats),
            None => {
                self.schedule_resolve(Resolve::Item(kind, item.to_owned()));
                ItemLock::Unknown
            }
        }
    }

    /// True if any of `categories` is locked (and not unlocked in this session). A
    /// category whose name isn't known yet counts as locked while a PIN is set (its list
    /// is then loaded in the background).
    pub fn is_item_locked(&self, kind: ContentKind, categories: &[CategoryId]) -> bool {
        !matches!(self.categories_lock(kind, categories), ItemLock::Unlocked)
    }

    /// The first locked category among `categories`, if any.
    pub fn locked_category(
        &self,
        kind: ContentKind,
        categories: &[CategoryId],
    ) -> Option<CategoryId> {
        match self.categories_lock(kind, categories) {
            ItemLock::Locked { category, .. } => Some(category),
            _ => None,
        }
    }

    /// True if `item` must not be shown or played without the PIN (unknown counts as
    /// locked while a PIN is set).
    pub fn is_locked<T: Lockable + ?Sized>(&self, item: &T) -> bool {
        !matches!(
            self.item_lock(
                item.lock_kind(),
                &item.lock_id(),
                item.lock_categories().as_deref()
            ),
            ItemLock::Unlocked
        )
    }

    /// The items that may be shown (Home rails, Favourites, "More like this", zap lists).
    pub fn filter_unlocked<'a, T: Lockable + 'a>(
        &self,
        items: impl IntoIterator<Item = &'a T>,
    ) -> Vec<&'a T> {
        items.into_iter().filter(|i| !self.is_locked(*i)).collect()
    }

    /// Removes the items that may not be shown.
    pub fn retain_unlocked<T: Lockable>(&self, items: &mut Vec<T>) {
        items.retain(|i| !self.is_locked(i));
    }

    /// `Ok` if the item may be played; else the typed [`AppError::Locked`] the request
    /// builders return.
    ///
    /// # Errors
    ///
    /// [`AppError::Locked`].
    pub fn check_unlocked(
        &self,
        kind: ContentKind,
        item: &str,
        categories: Option<&[CategoryId]>,
    ) -> AppResult<()> {
        match self.item_lock(kind, item, categories) {
            ItemLock::Unlocked => Ok(()),
            ItemLock::Locked { category, name } => Err(AppError::Locked(LockedItem {
                kind,
                item: item.to_owned(),
                category: Some(category),
                category_name: name,
            })),
            ItemLock::Unknown => Err(AppError::Locked(LockedItem {
                kind,
                item: item.to_owned(),
                category: None,
                category_name: None,
            })),
        }
    }

    /// Looks up the categories of an item now (cached `get_vod_info`/`get_series_info`,
    /// the category list, the live catalogue) and checks it. Use it after an
    /// [`AppError::Locked`] without a category, then retry the request.
    ///
    /// # Errors
    ///
    /// [`AppError::Locked`] (with the category when it is known now).
    pub async fn verify_item(self: &Arc<Self>, kind: ContentKind, item: &str) -> AppResult<()> {
        if !self.parental_state().any_lock(kind) {
            return Ok(());
        }
        let _ = self.resolve(&Resolve::Categories(kind)).await;
        let _ = self.resolve(&Resolve::Item(kind, item.to_owned())).await;
        self.check_unlocked(kind, item, None)
    }

    fn item_categories(&self, kind: ContentKind, item: &str) -> Option<Vec<CategoryId>> {
        if kind == ContentKind::Live
            && let Some(cat) = self.last_live_catalog()
            && let Some(ch) = cat.get(item)
        {
            return Some(ch.categories.clone());
        }
        let id = item.parse::<u64>().ok()?;
        self.lock_state
            .items
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(kind, id))
            .cloned()
    }

    // ---------------------------------------------------------------------------------
    // Background look-ups
    // ---------------------------------------------------------------------------------

    fn schedule_resolve(&self, what: Resolve) {
        let live = matches!(
            what,
            Resolve::Categories(ContentKind::Live) | Resolve::Item(ContentKind::Live, _)
        );
        // M3U profiles have no films/series; their live groups are in the playlist.
        if self.is_closed() || (!self.is_xtream() && !live) {
            return;
        }
        if let Some(at) = self
            .lock_state
            .resolve_failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&what)
            && at.elapsed() < RESOLVE_RETRY
        {
            return;
        }
        let Some(this) = self.me.upgrade() else {
            return;
        };
        let r = self.lock_state.resolve.request(what, (), |_, ()| {});
        if r.spawn_worker {
            self.services.spawn(async move {
                let st = &this.lock_state;
                let mut worker = st.resolve.worker();
                let mut dropped = Vec::new();
                while let Some((what, ())) = worker.next(&mut dropped) {
                    if this.is_closed() {
                        break;
                    }
                    let ok = this.resolve(&what).await.is_ok();
                    if !ok {
                        st.resolve_failed
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(what, Instant::now());
                    }
                    this.services.egui().request_repaint();
                }
            });
        }
    }

    async fn resolve(self: &Arc<Self>, what: &Resolve) -> AppResult<()> {
        if self.is_closed() {
            return Err(AppError::Cancelled);
        }
        match what {
            Resolve::Categories(ContentKind::Live) | Resolve::Item(ContentKind::Live, _) => {
                self.live_catalog().await.map(|_| ())
            }
            Resolve::Categories(ContentKind::Movie) => self.vod_categories().await.map(|_| ()),
            Resolve::Categories(ContentKind::Series) => self.series_categories().await.map(|_| ()),
            Resolve::Item(ContentKind::Movie, id) => {
                let id = id
                    .parse::<u64>()
                    .map_err(|_| AppError::msg("Unknown film."))?;
                let info = self.vod_info(id).await?;
                // An answer without a category still settles the question.
                self.index_item_categories(
                    ContentKind::Movie,
                    id,
                    info.movie_data
                        .as_ref()
                        .and_then(|m| m.category_id.clone())
                        .into_iter()
                        .collect(),
                );
                Ok(())
            }
            Resolve::Item(ContentKind::Series, id) => {
                let id = id
                    .parse::<u64>()
                    .map_err(|_| AppError::msg("Unknown series."))?;
                let info = self.series_info(id).await?;
                self.index_item_categories(
                    ContentKind::Series,
                    id,
                    info.info.category_id.clone().into_iter().collect(),
                );
                Ok(())
            }
        }
    }

    // ---------------------------------------------------------------------------------
    // Indexing what the session loads
    // ---------------------------------------------------------------------------------

    /// Adds `cats` to what is known of an item (creating the entry: an item with no
    /// category is then known to have none).
    fn index_item_categories(&self, kind: ContentKind, id: u64, cats: Vec<CategoryId>) {
        let mut items = self
            .lock_state
            .items
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = items.entry((kind, id)).or_default();
        for c in cats {
            if !entry.contains(&c) {
                entry.push(c);
            }
        }
    }

    fn note_categories(&self, kind: ContentKind, cats: &[Category]) {
        let mut names = self
            .lock_state
            .names
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let m = names.entry(kind).or_default();
        for c in cats {
            m.insert(c.id.as_str().to_owned(), c.name.clone());
        }
    }

    /// True if artwork of an item in these categories must stay out of the disk cache
    /// (a locked-by-settings category, unlocked or not; unknown names count while a PIN
    /// is set).
    fn private_art(&self, p: &ParentalState, kind: ContentKind, cats: &[CategoryId]) -> bool {
        p.pin_set
            && cats.iter().any(|c| {
                let name = self.category_name(kind, c);
                p.restricted(kind, c.as_str(), name.as_deref()) != Some(false)
            })
    }

    /// Called for every list/detail decoded from the disk cache or fetched (not for memo
    /// hits): indexes categories and registers private artwork.
    pub(super) fn after_load(&self, key: &str, value: &(dyn Any + Send + Sync)) {
        let list_category = |prefix: &str| {
            key.strip_prefix(prefix)
                .filter(|c| *c != "all")
                .map(CategoryId::from)
        };
        if let Some(cats) = value.downcast_ref::<Vec<Category>>() {
            let kind = match key {
                "xt:live_categories" => Some(ContentKind::Live),
                "xt:vod_categories" => Some(ContentKind::Movie),
                "xt:series_categories" => Some(ContentKind::Series),
                _ => None,
            };
            if let Some(kind) = kind {
                self.note_categories(kind, cats);
            }
            return;
        }
        let p = self.parental_state();
        let mut private: Vec<String> = Vec::new();
        if let Some(list) = value.downcast_ref::<Vec<VodStream>>() {
            let extra = list_category("xt:vod_streams:");
            {
                let mut items = self
                    .lock_state
                    .items
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                for s in list {
                    let entry = items.entry((ContentKind::Movie, s.stream_id)).or_default();
                    for c in s
                        .category_id
                        .iter()
                        .chain(&s.category_ids)
                        .chain(extra.as_ref())
                    {
                        if !entry.contains(c) {
                            entry.push(c.clone());
                        }
                    }
                }
            }
            for s in list {
                let cats = all_categories(s.category_id.as_ref(), &s.category_ids);
                if self.private_art(&p, ContentKind::Movie, &cats)
                    || extra.as_ref().is_some_and(|c| {
                        self.private_art(&p, ContentKind::Movie, std::slice::from_ref(c))
                    })
                {
                    private.extend(s.stream_icon.clone());
                }
            }
        } else if let Some(list) = value.downcast_ref::<Vec<Series>>() {
            let extra = list_category("xt:series:");
            {
                let mut items = self
                    .lock_state
                    .items
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                for s in list {
                    let entry = items.entry((ContentKind::Series, s.series_id)).or_default();
                    for c in s
                        .category_id
                        .iter()
                        .chain(&s.category_ids)
                        .chain(extra.as_ref())
                    {
                        if !entry.contains(c) {
                            entry.push(c.clone());
                        }
                    }
                }
            }
            for s in list {
                let cats = all_categories(s.category_id.as_ref(), &s.category_ids);
                if self.private_art(&p, ContentKind::Series, &cats)
                    || extra.as_ref().is_some_and(|c| {
                        self.private_art(&p, ContentKind::Series, std::slice::from_ref(c))
                    })
                {
                    private.extend(s.cover.clone());
                    private.extend(s.stream_icon.clone());
                    private.extend(s.backdrop_path.iter().cloned());
                }
            }
        } else if let Some(info) = value.downcast_ref::<VodInfo>() {
            if let Some(id) = key
                .strip_prefix("xt:vod_info:")
                .and_then(|s| s.parse::<u64>().ok())
            {
                let cats: Vec<CategoryId> = info
                    .movie_data
                    .as_ref()
                    .and_then(|m| m.category_id.clone())
                    .into_iter()
                    .collect();
                drop(p);
                self.index_item_categories(ContentKind::Movie, id, cats);
                let all = self.item_categories(ContentKind::Movie, &id.to_string());
                let p = self.parental_state();
                if self.private_art(&p, ContentKind::Movie, all.as_deref().unwrap_or_default()) {
                    private.extend(info.info.movie_image.clone());
                    private.extend(info.info.backdrop_path.iter().cloned());
                }
            }
        } else if let Some(info) = value.downcast_ref::<SeriesInfo>() {
            if let Some(id) = key
                .strip_prefix("xt:series_info:")
                .and_then(|s| s.parse::<u64>().ok())
            {
                let cats: Vec<CategoryId> = info.info.category_id.clone().into_iter().collect();
                drop(p);
                self.index_item_categories(ContentKind::Series, id, cats);
                let all = self.item_categories(ContentKind::Series, &id.to_string());
                let p = self.parental_state();
                if self.private_art(&p, ContentKind::Series, all.as_deref().unwrap_or_default()) {
                    private.extend(info.info.cover.clone());
                    private.extend(info.info.backdrop_path.iter().cloned());
                    for e in info.episodes.values().flatten() {
                        private.extend(e.info.movie_image.clone());
                    }
                }
            }
        } else if let Some(list) = value.downcast_ref::<Vec<LiveStream>>() {
            let mut items = self
                .lock_state
                .items
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            for s in list {
                items.insert(
                    (ContentKind::Live, s.stream_id),
                    all_categories(s.category_id.as_ref(), &s.category_ids),
                );
            }
        }
        if !private.is_empty() {
            self.services
                .images
                .mark_private(private.iter().map(String::as_str));
        }
    }

    /// Registers the logos of channels in locked categories as private artwork.
    pub(super) fn note_live_catalog(&self, catalog: &LiveCatalog) {
        if self.is_xtream() {
            self.note_categories(ContentKind::Live, &catalog.categories);
        }
        let p = self.parental_state();
        if !p.pin_set {
            return;
        }
        let private: Vec<&str> = catalog
            .channels
            .iter()
            .filter(|c| self.private_art(&p, ContentKind::Live, &c.categories))
            .filter_map(|c| c.logo.as_deref())
            .collect();
        if !private.is_empty() {
            self.services.images.mark_private(private);
        }
    }

    /// Re-applies the artwork rule to everything already loaded (after the parental
    /// settings changed).
    pub(super) fn rescan_private_art(&self) {
        let entries: Vec<(String, Arc<dyn Any + Send + Sync>)> = self
            .memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(k, _)| {
                k.starts_with("xt:vod_streams:")
                    || k.starts_with("xt:series:")
                    || k.starts_with("xt:vod_info:")
                    || k.starts_with("xt:series_info:")
            })
            .map(|(k, e)| (k.clone(), Arc::clone(&e.value)))
            .collect();
        for (k, v) in entries {
            self.after_load(&k, v.as_ref());
        }
        if let Some(cat) = self.last_live_catalog() {
            self.note_live_catalog(&cat);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use opensmarters_core::parental::KeywordMatcher;

    #[test]
    fn unlocks_expire_after_thirty_minutes_without_input() {
        use opensmarters_core::secrecy::SecretString;
        use opensmarters_store::{
            MemorySecretStore, NewProfile, NewProfileSource, PinHashParams, ProfileSettings, Store,
            StoreOptions,
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let panel = rt
            .block_on(fake_panel::spawn(fake_panel::Config::api_only()))
            .unwrap();
        let store = Arc::new(
            Store::open(
                StoreOptions::in_memory().with_pin_hash(PinHashParams {
                    m_cost_kib: 64,
                    t_cost: 1,
                    p_cost: 1,
                }),
                Arc::new(MemorySecretStore::new()),
            )
            .unwrap(),
        );
        store
            .parental()
            .set_pin(&SecretString::from("1234"))
            .unwrap();
        let services = crate::services::Services::new(
            rt.handle().clone(),
            store,
            opensmarters_core::Redactor::new(),
            crate::services::Paths::default(),
            true,
            eframe::egui::Context::default(),
        );
        let id = services
            .store
            .profiles()
            .create(NewProfile {
                name: "T".into(),
                source: NewProfileSource::Xtream {
                    server_url: panel.base_url().to_owned(),
                    username: "demo".into(),
                    password: SecretString::from("demo"),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap()
            .id;
        let session = rt
            .block_on(Session::open(services, id))
            .unwrap_or_else(|f| panic!("{}", f.error));
        let adult = CategoryId::from("7");
        assert!(session.is_category_locked(ContentKind::Live, &adult, "Adult Night"));
        session.unlock_category(ContentKind::Live, &adult);
        session.unlock_item(ContentKind::Movie, "42");
        assert!(!session.is_category_locked(ContentKind::Live, &adult, "Adult Night"));
        // Input keeps it unlocked.
        session.note_activity();
        assert!(!session.is_category_locked(ContentKind::Live, &adult, "Adult Night"));
        // 31 minutes without input: locked again, even before the next input …
        let ago = Instant::now()
            .checked_sub(RELOCK_IDLE + Duration::from_secs(60))
            .unwrap();
        *session.lock_state.last_activity.lock().unwrap() = ago;
        assert!(session.is_category_locked(ContentKind::Live, &adult, "Adult Night"));
        assert!(!session.item_unlocked(ContentKind::Movie, "42"));
        // … and the next input clears the unlocks for good.
        session.note_activity();
        assert!(session.is_category_locked(ContentKind::Live, &adult, "Adult Night"));
        assert!(!session.item_unlocked(ContentKind::Movie, "42"));
    }

    fn state(pin: bool) -> ParentalState {
        let mut locks = HashMap::new();
        let mut live = HashMap::new();
        live.insert("9".to_owned(), CategoryLock::Locked);
        live.insert("10".to_owned(), CategoryLock::Unlocked);
        locks.insert(ContentKind::Live, live);
        ParentalState {
            pin_set: pin,
            matcher: Some(KeywordMatcher::new(["adult"])),
            locks,
            pin_len: 4,
        }
    }

    #[test]
    fn restriction_rules() {
        let p = state(true);
        assert_eq!(p.restricted(ContentKind::Live, "9", None), Some(true));
        assert_eq!(
            p.restricted(ContentKind::Live, "10", Some("Adult")),
            Some(false),
            "a manual unlock beats the keyword"
        );
        assert_eq!(
            p.restricted(ContentKind::Live, "1", Some("Adult night")),
            Some(true)
        );
        assert_eq!(
            p.restricted(ContentKind::Live, "1", Some("News")),
            Some(false)
        );
        assert_eq!(
            p.restricted(ContentKind::Live, "1", None),
            None,
            "name unknown"
        );
        assert!(p.any_lock(ContentKind::Movie), "keywords apply everywhere");
        let off = state(false);
        assert_eq!(off.restricted(ContentKind::Live, "9", None), Some(false));
        assert!(!off.any_lock(ContentKind::Live));
        let mut no_kw = state(true);
        no_kw.matcher = Some(KeywordMatcher::new(Vec::<String>::new()));
        assert!(!no_kw.any_lock(ContentKind::Movie));
        assert!(no_kw.any_lock(ContentKind::Live), "manual lock on live");
    }

    #[test]
    fn lockable_ids() {
        let p = PlaybackProgress {
            kind: PlaybackKind::Catchup,
            item_id: "42@1700000000".into(),
            position_secs: 0.0,
            duration_secs: None,
            finished: false,
            ever_finished: false,
            updated_at: opensmarters_store::Timestamp::from_unix_millis(0),
            snapshot: Default::default(),
        };
        assert_eq!(p.lock_id(), "42");
        assert_eq!(p.lock_kind(), ContentKind::Live);
        assert_eq!(
            all_categories(
                Some(&CategoryId::from("1")),
                &[CategoryId::from("1"), CategoryId::from("2")]
            ),
            vec![CategoryId::from("1"), CategoryId::from("2")]
        );
    }
}

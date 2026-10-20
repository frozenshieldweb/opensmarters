// SPDX-License-Identifier: GPL-3.0-or-later
//! The active profile's connection to its provider: Xtream client or M3U playlist, the
//! login info, stream URL builders, catalogue loading with caching, EPG, favourites and
//! parental state.
//!
//! Open one with [`Session::open`] (the app does this when a profile is chosen); screens
//! get it as `cx.session()` (an `Arc<Session>`, cheap to clone into tasks).
//!
//! ## Caching (stale-while-revalidate)
//!
//! Catalogue calls go memory memo → encrypted store cache → provider:
//! - fresh cache hit: returned;
//! - expired hit: returned at once, refreshed in the background (next call is fresh);
//! - miss: fetched, stored, returned; a failed fetch with an expired copy returns the copy.
//!
//! TTLs: categories and stream lists [`LIST_TTL`] (6 h), VOD/series info [`INFO_TTL`],
//! EPG [`super::epg::EPG_TTL`] (10 min, memory only). [`Session::refresh`] drops it all.
//!
//! ## Secrets
//!
//! The password and M3U URL are registered with the app-wide redactor on open. Stream
//! URLs only exist inside [`PlayableUrl`]s.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use opensmarters_core::catalog::CategorySelection;
use opensmarters_core::chrono::{DateTime, Utc};
use opensmarters_core::m3u::{self, M3uLimits, Playlist};
use opensmarters_core::net::HttpClient;
use opensmarters_core::parental::KeywordMatcher;
use opensmarters_core::secrecy::{ExposeSecret, SecretString};
use opensmarters_core::urls::{
    LiveContainer as CoreContainer, StreamUrls, TimeshiftFormat as CoreTimeshift,
};
use opensmarters_core::xtream::Client;
use opensmarters_core::{
    AccountStatus, ArchiveProgramme, Category, CategoryId, DEFAULT_USER_AGENT, Episode, HttpConfig,
    IPTV_SMARTERS_UA, LoginInfo, Series, SeriesInfo, VodInfo, VodStream, XtreamAccount,
};
use opensmarters_player::PlayableUrl;
use opensmarters_store::{
    CacheScope, CategoryLock, ContentKind, ItemSnapshot, LiveContainer, PlaybackKind,
    PlaybackProgress, Profile, ProfileId, ProfileKind, ProfileSource, Store, TimeshiftFormat,
    UserAgent as UaSetting,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::Services;
use super::catalog::{ChannelSource, LiveCatalog, LiveChannel, Programme, now_and_next};
use super::epg::{EpgCache, EpgState};
use crate::error::{AppError, AppResult};
use crate::playback::{MediaKind, PlaybackItem, PlaybackRequest, ZapContext, stream_headers};

// Phase 4B extensions (guide rows, bounded XMLTV, catch-up formats) live in child modules
// so parallel work on this file merges cleanly.
pub mod catchup;
pub mod guide;

/// TTL of category and stream lists.
pub const LIST_TTL: Duration = Duration::from_secs(6 * 3600);
/// TTL of VOD/series detail.
pub const INFO_TTL: Duration = Duration::from_secs(6 * 3600);
/// TTL of the cached login (used only when the provider is unreachable).
pub const LOGIN_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Maximum size of a local M3U file.
pub const MAX_M3U_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Cache key of a VOD list (`None` = All). On large panels "All" is tens of MB and takes
/// ~20 s: load per category by default and only use All lazily.
pub fn vod_streams_key(category: Option<&CategoryId>) -> String {
    format!(
        "xt:vod_streams:{}",
        category.map_or("all", CategoryId::as_str)
    )
}

/// Cache key of a series list (`None` = All).
pub fn series_key(category: Option<&CategoryId>) -> String {
    format!("xt:series:{}", category.map_or("all", CategoryId::as_str))
}

/// The User-Agent string for a profile setting.
pub fn user_agent_string(ua: &UaSetting) -> String {
    match ua {
        UaSetting::Default => DEFAULT_USER_AGENT.to_owned(),
        UaSetting::SmartersCompat => IPTV_SMARTERS_UA.to_owned(),
        UaSetting::Custom(s) => s.clone(),
    }
}

/// Store → core live container.
pub fn core_container(c: LiveContainer) -> CoreContainer {
    match c {
        LiveContainer::Ts => CoreContainer::Ts,
        LiveContainer::M3u8 => CoreContainer::M3u8,
    }
}

/// Core → store live container.
pub fn store_container(c: CoreContainer) -> LiveContainer {
    match c {
        CoreContainer::Ts => LiveContainer::Ts,
        CoreContainer::M3u8 => LiveContainer::M3u8,
    }
}

/// Store → core timeshift format.
pub fn core_timeshift(f: TimeshiftFormat) -> CoreTimeshift {
    match f {
        TimeshiftFormat::Standard => CoreTimeshift::Standard,
        TimeshiftFormat::SmartersCompat => CoreTimeshift::SmartersCompat,
    }
}

/// What the account page shows (Xtream `user_info`), without secrets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccountSummary {
    /// `status`.
    pub status: Option<AccountStatus>,
    /// Expiry (`None` = unlimited or unknown).
    pub exp_date: Option<DateTime<Utc>>,
    /// Trial account.
    pub is_trial: bool,
    /// Active connections.
    pub active_cons: Option<u32>,
    /// Allowed connections.
    pub max_connections: Option<u32>,
    /// `allowed_output_formats`.
    pub allowed_output_formats: Vec<String>,
    /// Account creation date.
    pub created_at: Option<DateTime<Utc>>,
    /// Provider message (capped).
    pub message: Option<String>,
    /// The server clock at login.
    pub server_now: Option<DateTime<Utc>>,
    /// Server time zone name.
    pub timezone: Option<String>,
}

impl AccountSummary {
    /// From a login response.
    pub fn from_login(info: &LoginInfo) -> Self {
        let u = &info.user_info;
        Self {
            status: u.status.clone(),
            exp_date: u.exp_date,
            is_trial: u.is_trial,
            active_cons: u.active_cons,
            max_connections: u.max_connections,
            allowed_output_formats: u.allowed_output_formats.clone(),
            created_at: u.created_at,
            message: u
                .message
                .as_deref()
                .map(|m| crate::ui::text::cap(m, 200).into_owned())
                .filter(|m| !m.trim().is_empty()),
            server_now: info.server_info.timestamp_now,
            timezone: info.server_info.timezone.clone(),
        }
    }

    /// Days until expiry (negative if expired).
    pub fn days_left(&self, now: DateTime<Utc>) -> Option<i64> {
        self.exp_date.map(|e| (e - now).num_days())
    }
}

/// Why a profile could not be opened.
#[derive(Debug, Clone)]
pub struct OpenFailure {
    /// The error (plain-language via [`AppError::user_message`]).
    pub error: AppError,
    /// Account info, when the provider answered (expired/banned accounts).
    pub account: Option<AccountSummary>,
}

impl From<AppError> for OpenFailure {
    fn from(error: AppError) -> Self {
        Self {
            error,
            account: None,
        }
    }
}

enum Source {
    Xtream {
        client: Arc<Client>,
        login: Box<LoginInfo>,
        urls: StreamUrls,
    },
    M3u {
        playlist: RwLock<Arc<Playlist>>,
    },
}

#[derive(Debug, Default)]
struct ParentalState {
    pin_set: bool,
    matcher: Option<KeywordMatcher>,
    locks: HashMap<ContentKind, HashMap<String, CategoryLock>>,
    unlocked: HashSet<(ContentKind, String)>,
}

struct MemoEntry {
    valid_until: Instant,
    value: Arc<dyn Any + Send + Sync>,
}

/// The active profile's provider connection. See the [module docs](self).
pub struct Session {
    profile: Profile,
    services: Services,
    source: Source,
    user_agent: String,
    http: HttpClient,
    offline: AtomicBool,
    account: Option<AccountSummary>,
    insecure: bool,
    memo: Mutex<HashMap<String, MemoEntry>>,
    key_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    favourites: RwLock<HashMap<ContentKind, HashSet<String>>>,
    parental: RwLock<ParentalState>,
    epg: EpgCache,
    xmltv: tokio::sync::OnceCell<Option<Arc<opensmarters_core::xmltv::Epg>>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.profile.id)
            .field("kind", &self.profile.kind())
            .field("offline", &self.is_offline())
            .finish_non_exhaustive()
    }
}

async fn blocking<T: Send + 'static>(
    store: &Arc<Store>,
    f: impl FnOnce(&Store) -> Result<T, opensmarters_store::StoreError> + Send + 'static,
) -> AppResult<T> {
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|_| AppError::Cancelled)?
        .map_err(AppError::from)
}

impl Session {
    /// Opens `profile_id`: reads its secrets from the keychain, logs in (Xtream) or loads
    /// the playlist (M3U), and loads favourites/parental state.
    ///
    /// When the provider is unreachable but a previous login is cached, the session opens
    /// **offline** (cached catalogue only).
    ///
    /// # Errors
    ///
    /// [`OpenFailure`] with a plain-language error and, for refused Xtream accounts, the
    /// account summary (expired, banned…).
    pub async fn open(
        services: Services,
        profile_id: ProfileId,
    ) -> Result<Arc<Session>, OpenFailure> {
        let store = Arc::clone(&services.store);
        let profile = blocking(&store, move |s| s.profiles().get(profile_id))
            .await?
            .ok_or_else(|| AppError::msg("This profile no longer exists."))?;
        let user_agent = user_agent_string(&profile.settings.user_agent);
        let http_config = HttpConfig {
            user_agent: user_agent.clone(),
            ..HttpConfig::default()
        };
        let redactor = services.redactor.clone();
        let http = HttpClient::new(&http_config, redactor.clone()).map_err(AppError::from)?;

        let mut offline = false;
        let mut account = None;
        let (source, insecure) = match &profile.source {
            ProfileSource::Xtream {
                server_url,
                username,
            } => {
                let password = blocking(&store, move |s| s.profiles().password(profile_id))
                    .await?
                    .ok_or_else(|| {
                        AppError::msg(
                            "This account's password is missing from the system keychain. \
                             Edit the account and enter it again.",
                        )
                    })?;
                let acc =
                    XtreamAccount::new(server_url, username.clone()).map_err(AppError::from)?;
                let insecure = !acc.server().is_https();
                let client = Arc::new(
                    Client::with_redactor(acc, password.clone(), &http_config, redactor.clone())
                        .map_err(AppError::from)?,
                );
                let login = match client.account_info().await {
                    Ok(info) => {
                        let summary = AccountSummary::from_login(&info);
                        if let Err(status) = info.check(Utc::now()) {
                            return Err(OpenFailure {
                                error: AppError::Provider(opensmarters_core::Error::Auth {
                                    status,
                                }),
                                account: Some(summary),
                            });
                        }
                        account = Some(summary);
                        // Cache the parsed login (it never contains the password) for
                        // offline starts.
                        if let Ok(bytes) = serde_json::to_vec(&info) {
                            let _ = blocking(&store, move |s| {
                                s.cache()
                                    .put(
                                        CacheScope::Profile(profile_id),
                                        "xt:login",
                                        &bytes,
                                        LOGIN_TTL,
                                    )
                                    .map(|_| ())
                            })
                            .await;
                        }
                        info
                    }
                    Err(e) if AppError::from(e.clone()).is_network() => {
                        let cached = blocking(&store, move |s| {
                            s.cache()
                                .get_stale_ok(CacheScope::Profile(profile_id), "xt:login")
                        })
                        .await
                        .ok()
                        .flatten()
                        .and_then(|c| serde_json::from_slice::<LoginInfo>(&c.data).ok());
                        match cached {
                            Some(info) => {
                                tracing::warn!("provider unreachable; opening the profile offline");
                                offline = true;
                                client.set_server_time_zone(info.time_zone());
                                account = Some(AccountSummary::from_login(&info));
                                info
                            }
                            None => return Err(AppError::from(e).into()),
                        }
                    }
                    Err(e) => return Err(AppError::from(e).into()),
                };
                let urls = client.stream_urls(&login);
                (
                    Source::Xtream {
                        client,
                        login: Box::new(login),
                        urls,
                    },
                    insecure,
                )
            }
            ProfileSource::M3uUrl => {
                let url = blocking(&store, move |s| s.profiles().m3u_url(profile_id))
                    .await?
                    .ok_or_else(|| {
                        AppError::msg(
                            "This playlist's address is missing from the system keychain. \
                             Edit the account and enter it again.",
                        )
                    })?;
                redactor.add_secret(url.expose_secret());
                let insecure = url
                    .expose_secret()
                    .trim()
                    .to_ascii_lowercase()
                    .starts_with("http://");
                let (playlist, was_offline) = load_m3u_url(&store, profile_id, &http, &url).await?;
                offline = was_offline;
                (
                    Source::M3u {
                        playlist: RwLock::new(Arc::new(playlist)),
                    },
                    insecure,
                )
            }
            ProfileSource::M3uFile { path } => {
                let playlist = load_m3u_file(path.clone()).await?;
                (
                    Source::M3u {
                        playlist: RwLock::new(Arc::new(playlist)),
                    },
                    false,
                )
            }
        };

        // Favourites and parental state.
        let favourites = blocking(&store, move |s| {
            let mut out = HashMap::new();
            for kind in ContentKind::ALL {
                out.insert(kind, s.favourites().ids(profile_id, kind)?);
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
        let parental = load_parental(&store, profile_id).await.unwrap_or_default();

        // Mark as used (last profile) in the background.
        services.blocking("mark profile used", move |s| {
            s.profiles().mark_used(profile_id)
        });

        Ok(Arc::new(Session {
            profile,
            services,
            source,
            user_agent,
            http,
            offline: AtomicBool::new(offline),
            account,
            insecure,
            memo: Mutex::new(HashMap::new()),
            key_locks: Mutex::new(HashMap::new()),
            favourites: RwLock::new(favourites),
            parental: RwLock::new(parental),
            epg: EpgCache::default(),
            xmltv: tokio::sync::OnceCell::new(),
        }))
    }

    // ---------------------------------------------------------------------------------
    // Accessors
    // ---------------------------------------------------------------------------------

    /// The profile.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The profile id.
    pub fn profile_id(&self) -> ProfileId {
        self.profile.id
    }

    /// Xtream or M3U.
    pub fn kind(&self) -> ProfileKind {
        self.profile.kind()
    }

    /// True for Xtream profiles (VOD, series, EPG and catch-up available).
    pub fn is_xtream(&self) -> bool {
        matches!(self.source, Source::Xtream { .. })
    }

    /// The services.
    pub fn services(&self) -> &Services {
        &self.services
    }

    /// The store.
    pub fn store(&self) -> &Arc<Store> {
        &self.services.store
    }

    /// The Xtream client (for agents that need an endpoint not wrapped here).
    pub fn xtream(&self) -> Option<&Arc<Client>> {
        match &self.source {
            Source::Xtream { client, .. } => Some(client),
            Source::M3u { .. } => None,
        }
    }

    /// The login response (Xtream).
    pub fn login(&self) -> Option<&LoginInfo> {
        match &self.source {
            Source::Xtream { login, .. } => Some(login),
            Source::M3u { .. } => None,
        }
    }

    /// The parsed playlist (M3U profiles).
    pub fn playlist(&self) -> Option<Arc<Playlist>> {
        match &self.source {
            Source::M3u { playlist, .. } => Some(Arc::clone(
                &playlist.read().unwrap_or_else(PoisonError::into_inner),
            )),
            Source::Xtream { .. } => None,
        }
    }

    /// Account status/expiry/connections (Xtream).
    pub fn account(&self) -> Option<&AccountSummary> {
        self.account.as_ref()
    }

    /// True if the account talks plain HTTP (show the unencrypted warning).
    pub fn is_insecure(&self) -> bool {
        self.insecure
    }

    /// True if the provider was unreachable at open (cached data only).
    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::Relaxed)
    }

    /// The profile's User-Agent string (API and streams).
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The live containers the account allows (Xtream `allowed_output_formats`).
    pub fn allowed_live_containers(&self) -> Vec<LiveContainer> {
        match &self.source {
            Source::Xtream { login, .. } => CoreContainer::allowed(&login.user_info)
                .into_iter()
                .map(store_container)
                .collect(),
            Source::M3u { .. } => vec![LiveContainer::Ts, LiveContainer::M3u8],
        }
    }

    /// The live container to use: the profile's preference if allowed.
    pub fn live_container(&self) -> CoreContainer {
        let pref = core_container(self.profile.settings.live_container);
        match &self.source {
            Source::Xtream { login, .. } => CoreContainer::preferred(pref, &login.user_info),
            Source::M3u { .. } => pref,
        }
    }

    /// The capped HTTP client with this profile's User-Agent (XMLTV, extra downloads).
    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    /// `url` if it carries none of the registered secrets (safe to store in snapshots).
    pub fn safe_url(&self, url: Option<&str>) -> Option<String> {
        let u = url?;
        (self.services.redactor.redact(u) == u).then(|| u.to_owned())
    }

    // ---------------------------------------------------------------------------------
    // Caching
    // ---------------------------------------------------------------------------------

    fn memo_get<T: Send + Sync + 'static>(&self, key: &str) -> Option<Arc<T>> {
        let memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        let e = memo.get(key)?;
        if e.valid_until < Instant::now() {
            return None;
        }
        Arc::clone(&e.value).downcast::<T>().ok()
    }

    fn memo_put<T: Send + Sync + 'static>(&self, key: &str, value: Arc<T>, ttl: Duration) {
        self.memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                key.to_owned(),
                MemoEntry {
                    valid_until: Instant::now() + ttl,
                    value,
                },
            );
    }

    fn key_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.key_locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.to_owned())
                .or_default(),
        )
    }

    async fn disk_put<T: Serialize>(&self, key: &str, value: &T, ttl: Duration) {
        let Ok(bytes) = serde_json::to_vec(value) else {
            return;
        };
        let key = key.to_owned();
        let profile = self.profile.id;
        if let Err(e) = blocking(self.store(), move |s| {
            s.cache()
                .put(CacheScope::Profile(profile), &key, &bytes, ttl)
                .map(|_| ())
        })
        .await
        {
            tracing::debug!(error = %e, "cache write failed");
        }
    }

    /// Generic cached fetch (see the module docs). `fetch` may run twice (once now, once
    /// for a background refresh), so it is a `Fn`.
    pub async fn cached<T, F, Fut>(
        self: &Arc<Self>,
        key: &str,
        ttl: Duration,
        fetch: F,
    ) -> AppResult<Arc<T>>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static,
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AppResult<T>> + Send + 'static,
    {
        let lock = self.key_lock(key);
        let _guard = lock.lock().await;
        if let Some(v) = self.memo_get::<T>(key) {
            return Ok(v);
        }
        let profile = self.profile.id;
        let k = key.to_owned();
        let disk = blocking(self.store(), move |s| {
            s.cache().get_stale_ok(CacheScope::Profile(profile), &k)
        })
        .await
        .ok()
        .flatten();
        let stale = match disk {
            Some(hit) => match serde_json::from_slice::<T>(&hit.data) {
                Ok(v) if hit.is_fresh => {
                    let v = Arc::new(v);
                    self.memo_put(key, Arc::clone(&v), ttl);
                    return Ok(v);
                }
                Ok(v) => Some(Arc::new(v)),
                Err(_) => None,
            },
            None => None,
        };
        if let Some(v) = stale {
            // Stale-while-revalidate: answer now, refresh in the background.
            self.memo_put(key, Arc::clone(&v), Duration::from_secs(60));
            if !self.is_offline() {
                let this = Arc::clone(self);
                let key = key.to_owned();
                self.services.spawn(async move {
                    match fetch().await {
                        Ok(fresh) => {
                            this.disk_put(&key, &fresh, ttl).await;
                            this.memo_put(&key, Arc::new(fresh), ttl);
                            this.services.egui().request_repaint();
                        }
                        Err(e) => tracing::debug!(error = %e, "background refresh failed"),
                    }
                });
            }
            return Ok(v);
        }
        let fresh = fetch().await?;
        self.disk_put(key, &fresh, ttl).await;
        let fresh = Arc::new(fresh);
        self.memo_put(key, Arc::clone(&fresh), ttl);
        Ok(fresh)
    }

    /// A cached value (memory, or the disk cache even if expired) **without** any network
    /// call, e.g. to show the VOD "All" list only when it was fetched before (it can take
    /// tens of seconds on large panels).
    pub async fn peek_cached<T>(self: &Arc<Self>, key: &str) -> Option<Arc<T>>
    where
        T: DeserializeOwned + Send + Sync + 'static,
    {
        if let Some(v) = self.memo_get::<T>(key) {
            return Some(v);
        }
        let profile = self.profile.id;
        let k = key.to_owned();
        let hit = blocking(self.store(), move |s| {
            s.cache().get_stale_ok(CacheScope::Profile(profile), &k)
        })
        .await
        .ok()
        .flatten()?;
        let v = Arc::new(serde_json::from_slice::<T>(&hit.data).ok()?);
        self.memo_put(key, Arc::clone(&v), Duration::from_secs(60));
        Some(v)
    }

    /// Drops every cached response of this profile (memory, disk, EPG). The next calls
    /// hit the provider.
    pub async fn refresh(&self) {
        self.memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.epg.clear();
        let profile = self.profile.id;
        let _ = blocking(self.store(), move |s| {
            s.cache().invalidate_profile(profile).map(|_| ())
        })
        .await;
    }

    fn client(&self) -> AppResult<Arc<Client>> {
        self.xtream()
            .cloned()
            .ok_or_else(|| AppError::msg("This needs an Xtream Codes account."))
    }

    // ---------------------------------------------------------------------------------
    // Catalogue
    // ---------------------------------------------------------------------------------

    /// All live channels and categories (Xtream "All" list filtered locally, or the M3U
    /// playlist's live entries).
    ///
    /// # Errors
    ///
    /// Provider/network errors (with no cached copy).
    pub async fn live_catalog(self: &Arc<Self>) -> AppResult<Arc<LiveCatalog>> {
        if let Some(c) = self.memo_get::<LiveCatalog>("live_catalog") {
            return Ok(c);
        }
        let catalog = match &self.source {
            Source::Xtream { .. } => {
                let client = self.client()?;
                let cats = {
                    let client = Arc::clone(&client);
                    self.cached("xt:live_categories", LIST_TTL, move || {
                        let client = Arc::clone(&client);
                        async move { Ok(client.live_categories().await?.items) }
                    })
                    .await?
                };
                let streams = self
                    .cached("xt:live_streams:all", LIST_TTL, move || {
                        let client = Arc::clone(&client);
                        async move { Ok(client.live_streams(None).await?.items) }
                    })
                    .await?;
                LiveCatalog::from_xtream(&cats, &streams)
            }
            Source::M3u { .. } => {
                let playlist = self.playlist().ok_or(AppError::Cancelled)?;
                LiveCatalog::from_playlist(&playlist)
            }
        };
        let catalog = Arc::new(catalog);
        self.memo_put(
            "live_catalog",
            Arc::clone(&catalog),
            Duration::from_secs(60),
        );
        Ok(catalog)
    }

    /// VOD categories (Xtream).
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn vod_categories(self: &Arc<Self>) -> AppResult<Arc<Vec<Category>>> {
        let client = self.client()?;
        self.cached("xt:vod_categories", LIST_TTL, move || {
            let client = Arc::clone(&client);
            async move { Ok(client.vod_categories().await?.items) }
        })
        .await
    }

    /// Movies of a category (`None` = all).
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn vod_streams(
        self: &Arc<Self>,
        category: Option<CategoryId>,
    ) -> AppResult<Arc<Vec<VodStream>>> {
        let client = self.client()?;
        let key = vod_streams_key(category.as_ref());
        self.cached(&key, LIST_TTL, move || {
            let client = Arc::clone(&client);
            let category = category.clone();
            async move { Ok(client.vod_streams(category.as_ref()).await?.items) }
        })
        .await
    }

    /// Series categories (Xtream).
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn series_categories(self: &Arc<Self>) -> AppResult<Arc<Vec<Category>>> {
        let client = self.client()?;
        self.cached("xt:series_categories", LIST_TTL, move || {
            let client = Arc::clone(&client);
            async move { Ok(client.series_categories().await?.items) }
        })
        .await
    }

    /// Series of a category (`None` = all).
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn series_list(
        self: &Arc<Self>,
        category: Option<CategoryId>,
    ) -> AppResult<Arc<Vec<Series>>> {
        let client = self.client()?;
        let key = series_key(category.as_ref());
        self.cached(&key, LIST_TTL, move || {
            let client = Arc::clone(&client);
            let category = category.clone();
            async move { Ok(client.series(category.as_ref()).await?.items) }
        })
        .await
    }

    /// Movie detail.
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn vod_info(self: &Arc<Self>, stream_id: u64) -> AppResult<Arc<VodInfo>> {
        let client = self.client()?;
        self.cached(&format!("xt:vod_info:{stream_id}"), INFO_TTL, move || {
            let client = Arc::clone(&client);
            async move { Ok(client.vod_info(stream_id).await?) }
        })
        .await
    }

    /// Series detail with seasons and episodes.
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn series_info(self: &Arc<Self>, series_id: u64) -> AppResult<Arc<SeriesInfo>> {
        let client = self.client()?;
        self.cached(
            &format!("xt:series_info:{series_id}"),
            INFO_TTL,
            move || {
                let client = Arc::clone(&client);
                async move { Ok(client.series_info(series_id).await?) }
            },
        )
        .await
    }

    /// The full EPG of a channel with archive flags (`get_simple_data_table`, Xtream),
    /// for catch-up. Not cached on disk (changes constantly).
    ///
    /// # Errors
    ///
    /// Provider/network errors.
    pub async fn epg_table(self: &Arc<Self>, stream_id: u64) -> AppResult<Arc<Vec<Programme>>> {
        let key = format!("epg_table:{stream_id}");
        if let Some(v) = self.memo_get::<Vec<Programme>>(&key) {
            return Ok(v);
        }
        let client = self.client()?;
        let list = client.simple_data_table(stream_id).await?;
        let mut out: Vec<Programme> = list.items.iter().map(Programme::from_listing).collect();
        out.sort_by_key(|p| p.start);
        let out = Arc::new(out);
        self.memo_put(&key, Arc::clone(&out), super::epg::EPG_TTL);
        Ok(out)
    }

    /// Non-blocking EPG lookup for a channel (schedules a background fetch when needed).
    pub fn epg(self: &Arc<Self>, channel: &LiveChannel) -> EpgState {
        let (state, fetch) = self.epg.lookup(&channel.key);
        if fetch {
            let this = Arc::clone(self);
            let channel = channel.clone();
            self.services.spawn(async move {
                let permits = Arc::clone(&this.epg.permits);
                let Ok(_permit) = permits.acquire_owned().await else {
                    return;
                };
                match this.fetch_epg(&channel).await {
                    Ok(list) => this.epg.put(&channel.key, list),
                    Err(e) => {
                        tracing::debug!(error = %e, "EPG fetch failed");
                        this.epg.fail(&channel.key);
                    }
                }
                this.services.egui().request_repaint();
            });
        }
        state
    }

    /// Now/next for a channel at `now` (non-blocking, see [`Session::epg`]).
    pub fn now_next(
        self: &Arc<Self>,
        channel: &LiveChannel,
        now: DateTime<Utc>,
    ) -> (EpgState, Option<Programme>, Option<Programme>) {
        let state = self.epg(channel);
        let (n, x) = match state.programmes() {
            Some(list) => {
                let (n, x) = now_and_next(list, now);
                (n.cloned(), x.cloned())
            }
            None => (None, None),
        };
        (state, n, x)
    }

    async fn fetch_epg(self: &Arc<Self>, channel: &LiveChannel) -> AppResult<Vec<Programme>> {
        match (&self.source, &channel.source) {
            (Source::Xtream { client, .. }, ChannelSource::Xtream { stream_id }) => {
                let list = client.short_epg(*stream_id, Some(6)).await?;
                Ok(list.items.iter().map(Programme::from_listing).collect())
            }
            _ => {
                let Some(epg_id) = channel.epg_id.as_deref() else {
                    return Ok(Vec::new());
                };
                let Some(xmltv) = self.xmltv().await else {
                    return Ok(Vec::new());
                };
                let now = Utc::now();
                Ok(xmltv
                    .programmes_for(epg_id)
                    .iter()
                    .filter(|p| {
                        p.stop
                            .is_none_or(|s| s > now - opensmarters_core::chrono::Duration::hours(1))
                    })
                    .take(8)
                    .map(|p| Programme {
                        title: crate::ui::text::cap(p.title(None), 160).into_owned(),
                        description: crate::ui::text::cap_paragraph(
                            p.description(None).unwrap_or_default(),
                            1200,
                        ),
                        start: p.start,
                        stop: p
                            .stop
                            .unwrap_or(p.start + opensmarters_core::chrono::Duration::minutes(30)),
                        has_archive: false,
                    })
                    .collect())
            }
        }
    }

    /// The profile's XMLTV guide (EPG URL override, else the M3U `url-tvg`), downloaded
    /// once per session and parsed with bounded memory (see [`guide`]).
    async fn xmltv(&self) -> Option<Arc<opensmarters_core::xmltv::Epg>> {
        self.xmltv
            .get_or_init(|| self.load_guide_xmltv())
            .await
            .clone()
    }

    /// Fresh `(active_cons, max_connections)` from the provider (one API call). Used after
    /// a stream failure to tell the user the connection limit is exhausted.
    ///
    /// # Errors
    ///
    /// Not an Xtream account; provider/network errors.
    pub async fn connection_usage(&self) -> AppResult<(Option<u32>, Option<u32>)> {
        let client = self.client()?;
        let info = client.account_info().await?;
        Ok((info.user_info.active_cons, info.user_info.max_connections))
    }

    // ---------------------------------------------------------------------------------
    // Playback requests
    // ---------------------------------------------------------------------------------

    /// A live channel request. `zap` gives the list for channel up/down.
    ///
    /// # Errors
    ///
    /// The stream URL was rejected (e.g. an M3U entry with a `file:` URL).
    pub fn live_request(
        &self,
        channel: &LiveChannel,
        zap: Option<ZapContext>,
    ) -> AppResult<PlaybackRequest> {
        let (url, headers) = match (&self.source, &channel.source) {
            (Source::Xtream { urls, .. }, ChannelSource::Xtream { stream_id }) => {
                let url = urls.live(*stream_id, self.live_container());
                let headers = stream_headers(url.as_str(), &self.user_agent, None, None);
                (PlayableUrl::parse(url.as_str())?, headers)
            }
            (
                _,
                ChannelSource::M3u {
                    url,
                    user_agent,
                    referrer,
                },
            ) => {
                let playable = PlayableUrl::parse(url).map_err(|_| {
                    AppError::msg("This channel's address isn't a web stream, so it won't be played for your safety.")
                })?;
                let headers = stream_headers(
                    url,
                    &self.user_agent,
                    user_agent.as_deref(),
                    referrer.as_deref(),
                );
                (playable, headers)
            }
            _ => {
                return Err(AppError::msg(
                    "This channel can't be played with this account.",
                ));
            }
        };
        Ok(PlaybackRequest {
            kind: MediaKind::Live,
            url,
            title: channel.name.clone(),
            subtitle: None,
            resume_from: None,
            item: PlaybackItem::Live {
                key: channel.key.clone(),
                number: channel.number,
            },
            artwork: self.safe_url(channel.logo.as_deref()),
            headers,
            zap,
            duration_hint: None,
        })
    }

    /// A movie request.
    ///
    /// # Errors
    ///
    /// Not an Xtream account, or the URL was rejected.
    pub fn movie_request(
        &self,
        stream_id: u64,
        container_extension: Option<&str>,
        title: &str,
        subtitle: Option<String>,
        artwork: Option<&str>,
        resume_from: Option<f64>,
    ) -> AppResult<PlaybackRequest> {
        let Source::Xtream { urls, .. } = &self.source else {
            return Err(AppError::msg("Movies need an Xtream Codes account."));
        };
        let url = urls.movie(stream_id, container_extension);
        Ok(PlaybackRequest {
            kind: MediaKind::Movie,
            headers: stream_headers(url.as_str(), &self.user_agent, None, None),
            url: PlayableUrl::parse(url.as_str())?,
            title: crate::ui::text::cap(title, 160).into_owned(),
            subtitle,
            resume_from,
            item: PlaybackItem::Movie {
                stream_id,
                container_extension: container_extension.map(str::to_owned),
            },
            artwork: self.safe_url(artwork),
            zap: None,
            duration_hint: None,
        })
    }

    /// An episode request.
    ///
    /// # Errors
    ///
    /// Not an Xtream account, or the URL was rejected.
    pub fn episode_request(
        &self,
        series_id: u64,
        series_name: &str,
        episode: &Episode,
        artwork: Option<&str>,
        resume_from: Option<f64>,
    ) -> AppResult<PlaybackRequest> {
        let Source::Xtream { urls, .. } = &self.source else {
            return Err(AppError::msg("Series need an Xtream Codes account."));
        };
        let url = urls.episode(episode.id, episode.container_extension.as_deref());
        let se = match (episode.season, episode.episode_num) {
            (Some(s), Some(e)) => format!("S{s} · E{e}"),
            (None, Some(e)) => format!("E{e}"),
            _ => String::new(),
        };
        let subtitle = [crate::ui::text::cap(series_name, 120).into_owned(), se]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        Ok(PlaybackRequest {
            kind: MediaKind::Episode,
            headers: stream_headers(url.as_str(), &self.user_agent, None, None),
            url: PlayableUrl::parse(url.as_str())?,
            title: crate::ui::text::cap(&episode.title, 160).into_owned(),
            subtitle: Some(subtitle),
            resume_from,
            item: PlaybackItem::Episode {
                episode_id: episode.id,
                series_id,
                season: episode.season,
                episode: episode.episode_num,
                container_extension: episode.container_extension.clone(),
            },
            artwork: self.safe_url(artwork.or(episode.info.movie_image.as_deref())),
            zap: None,
            duration_hint: episode.info.duration_secs.map(|d| d as f64),
        })
    }

    /// A catch-up request for an archived programme of `channel`, using the profile's
    /// timeshift format.
    ///
    /// # Errors
    ///
    /// Not an Xtream channel, or the URL was rejected.
    pub fn catchup_request(
        &self,
        channel: &LiveChannel,
        programme: &ArchiveProgramme,
        resume_from: Option<f64>,
    ) -> AppResult<PlaybackRequest> {
        let (Source::Xtream { urls, .. }, Some(stream_id)) = (&self.source, channel.stream_id())
        else {
            return Err(AppError::msg("Catch-up needs an Xtream Codes account."));
        };
        let url = urls.timeshift(
            stream_id,
            programme,
            core_timeshift(self.profile.settings.timeshift_format),
            self.live_container(),
        );
        Ok(PlaybackRequest {
            kind: MediaKind::Catchup,
            headers: stream_headers(url.as_str(), &self.user_agent, None, None),
            url: PlayableUrl::parse(url.as_str())?,
            title: crate::ui::text::cap(&programme.title, 160).into_owned(),
            subtitle: Some(channel.name.clone()),
            resume_from,
            item: PlaybackItem::Catchup {
                stream_id,
                start: programme.start.timestamp(),
                duration_minutes: programme.duration_minutes(),
            },
            artwork: self.safe_url(channel.logo.as_deref()),
            zap: None,
            duration_hint: Some(f64::from(programme.duration_minutes()) * 60.0),
        })
    }

    /// Rebuilds a request from a stored resume point (Continue watching).
    ///
    /// # Errors
    ///
    /// Unknown ids, catch-up items (not resumable from history), rejected URLs.
    pub fn resume_request(&self, p: &PlaybackProgress) -> AppResult<PlaybackRequest> {
        let resume = Some(p.position_secs).filter(|s| *s > 0.0);
        match p.kind {
            PlaybackKind::Movie => {
                let id = p
                    .item_id
                    .parse::<u64>()
                    .map_err(|_| AppError::msg("This item can't be resumed."))?;
                let mut r = self.movie_request(
                    id,
                    p.snapshot.container_extension.as_deref(),
                    &p.snapshot.title,
                    None,
                    p.snapshot.poster_url.as_deref(),
                    resume,
                )?;
                r.duration_hint = p.duration_secs;
                Ok(r)
            }
            PlaybackKind::Episode => {
                let id = p
                    .item_id
                    .parse::<u64>()
                    .map_err(|_| AppError::msg("This episode can't be resumed."))?;
                let series_id = p
                    .snapshot
                    .series_id
                    .as_deref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or_default();
                let episode = Episode {
                    id,
                    episode_num: p.snapshot.episode,
                    title: p.snapshot.title.clone(),
                    container_extension: p.snapshot.container_extension.clone(),
                    season: p.snapshot.season,
                    added: None,
                    direct_source: None,
                    info: Default::default(),
                };
                let mut r = self.episode_request(
                    series_id,
                    "",
                    &episode,
                    p.snapshot.poster_url.as_deref(),
                    resume,
                )?;
                r.duration_hint = p.duration_secs;
                Ok(r)
            }
            PlaybackKind::Catchup => Err(AppError::msg(
                "Catch-up programmes can be resumed from the Catch-up page.",
            )),
        }
    }

    // ---------------------------------------------------------------------------------
    // Favourites
    // ---------------------------------------------------------------------------------

    /// True if `id` is a favourite of `kind`.
    pub fn is_favourite(&self, kind: ContentKind, id: &str) -> bool {
        self.favourites
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&kind)
            .is_some_and(|s| s.contains(id))
    }

    /// The favourite ids of `kind`.
    pub fn favourite_ids(&self, kind: ContentKind) -> HashSet<String> {
        self.favourites
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&kind)
            .cloned()
            .unwrap_or_default()
    }

    /// Toggles a favourite (updated in memory at once, persisted in the background).
    /// Returns the new state.
    pub fn toggle_favourite(
        &self,
        kind: ContentKind,
        id: &str,
        mut snapshot: ItemSnapshot,
    ) -> bool {
        let now_fav = {
            let mut favs = self
                .favourites
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let set = favs.entry(kind).or_default();
            if set.remove(id) {
                false
            } else {
                set.insert(id.to_owned());
                true
            }
        };
        snapshot.icon_url = self.safe_url(snapshot.icon_url.as_deref());
        let profile = self.profile.id;
        let id = id.to_owned();
        self.services.blocking("update favourites", move |s| {
            if now_fav {
                s.favourites()
                    .add(profile, kind, &id, &snapshot)
                    .map(|_| ())
            } else {
                s.favourites().remove(profile, kind, &id).map(|_| ())
            }
        });
        now_fav
    }

    /// Re-reads favourites from the store (after another screen edited them).
    pub async fn reload_favourites(&self) {
        let profile = self.profile.id;
        if let Ok(f) = blocking(self.store(), move |s| {
            let mut out = HashMap::new();
            for kind in ContentKind::ALL {
                out.insert(kind, s.favourites().ids(profile, kind)?);
            }
            Ok(out)
        })
        .await
        {
            *self
                .favourites
                .write()
                .unwrap_or_else(PoisonError::into_inner) = f;
        }
    }

    // ---------------------------------------------------------------------------------
    // Parental control
    // ---------------------------------------------------------------------------------

    /// True if a parental PIN is set (locks are active).
    pub fn parental_enabled(&self) -> bool {
        self.parental
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .pin_set
    }

    /// True if a category needs the PIN (manual lock, or a keyword match unless manually
    /// unlocked), and has not been unlocked in this session.
    pub fn is_category_locked(&self, kind: ContentKind, id: &CategoryId, name: &str) -> bool {
        let p = self.parental.read().unwrap_or_else(PoisonError::into_inner);
        if !p.pin_set || p.unlocked.contains(&(kind, id.as_str().to_owned())) {
            return false;
        }
        match p.locks.get(&kind).and_then(|m| m.get(id.as_str())) {
            Some(CategoryLock::Locked) => true,
            Some(CategoryLock::Unlocked) => false,
            None => p.matcher.as_ref().is_some_and(|m| m.is_match(name)),
        }
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

    /// Unlocks a category for the rest of this session (after a correct PIN).
    pub fn unlock_category(&self, kind: ContentKind, id: &CategoryId) {
        self.parental
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .unlocked
            .insert((kind, id.as_str().to_owned()));
    }

    /// Re-reads the PIN state, keywords and locks (after Settings → Parental changes).
    pub async fn reload_parental(&self) {
        if let Ok(mut fresh) = load_parental(self.store(), self.profile.id).await {
            let mut p = self
                .parental
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            fresh.unlocked = std::mem::take(&mut p.unlocked);
            *p = fresh;
        }
    }
}

async fn load_parental(store: &Arc<Store>, profile: ProfileId) -> AppResult<ParentalState> {
    blocking(store, move |s| {
        let par = s.parental();
        let pin_set = par.is_pin_set()?;
        let keywords = par.keywords()?;
        let mut locks = HashMap::new();
        for kind in ContentKind::ALL {
            locks.insert(kind, par.category_locks(profile, kind)?);
        }
        Ok(ParentalState {
            pin_set,
            matcher: Some(KeywordMatcher::new(keywords)),
            locks,
            unlocked: HashSet::new(),
        })
    })
    .await
}

async fn load_m3u_url(
    store: &Arc<Store>,
    profile: ProfileId,
    http: &HttpClient,
    url: &SecretString,
) -> AppResult<(Playlist, bool)> {
    let cached = blocking(store, move |s| {
        s.cache()
            .get_stale_ok(CacheScope::Profile(profile), "m3u:playlist")
    })
    .await
    .ok()
    .flatten();
    if let Some(hit) = &cached
        && hit.is_fresh
        && let Ok(p) = m3u::parse(&hit.data, &M3uLimits::default())
    {
        return Ok((p, false));
    }
    let parsed_url = opensmarters_core::url::Url::parse(url.expose_secret().trim())
        .map_err(|_| AppError::msg("The playlist address isn't a valid web address."))?;
    match http.get_bytes(&parsed_url).await {
        Ok(bytes) => {
            let playlist = m3u::parse(&bytes, &M3uLimits::default())?;
            let _ = blocking(store, move |s| {
                s.cache()
                    .put(
                        CacheScope::Profile(profile),
                        "m3u:playlist",
                        &bytes,
                        LIST_TTL,
                    )
                    .map(|_| ())
            })
            .await;
            Ok((playlist, false))
        }
        Err(e) => {
            if let Some(hit) = cached
                && let Ok(p) = m3u::parse(&hit.data, &M3uLimits::default())
            {
                tracing::warn!("playlist download failed; using the cached copy");
                return Ok((p, true));
            }
            Err(e.into())
        }
    }
}

async fn load_m3u_file(path: std::path::PathBuf) -> AppResult<Playlist> {
    tokio::task::spawn_blocking(move || -> AppResult<Playlist> {
        let meta = std::fs::metadata(&path).map_err(|_| {
            AppError::msg("The playlist file can't be opened. Was it moved or deleted?")
        })?;
        if !meta.is_file() {
            return Err(AppError::msg("The playlist path is not a file."));
        }
        if meta.len() > MAX_M3U_FILE_BYTES {
            return Err(AppError::msg("The playlist file is larger than 64 MB."));
        }
        let bytes =
            std::fs::read(&path).map_err(|_| AppError::msg("The playlist file can't be read."))?;
        Ok(m3u::parse(&bytes, &M3uLimits::default())?)
    })
    .await
    .map_err(|_| AppError::Cancelled)?
}

/// Validates a login without creating a profile (onboarding "Sign in"): Xtream login, or
/// M3U download/parse. Returns a summary for the confirmation message.
///
/// # Errors
///
/// Plain-language failures (wrong credentials, unreachable, not a playlist…).
pub async fn test_xtream_login(
    server: &str,
    username: &str,
    password: SecretString,
    user_agent: &str,
    redactor: opensmarters_core::Redactor,
) -> Result<LoginInfo, OpenFailure> {
    let account = XtreamAccount::new(server, username.to_owned()).map_err(AppError::from)?;
    let config = HttpConfig {
        user_agent: user_agent.to_owned(),
        ..HttpConfig::default()
    };
    let client =
        Client::with_redactor(account, password, &config, redactor).map_err(AppError::from)?;
    let info = client.account_info().await.map_err(AppError::from)?;
    if let Err(status) = info.check(Utc::now()) {
        return Err(OpenFailure {
            error: AppError::Provider(opensmarters_core::Error::Auth { status }),
            account: Some(AccountSummary::from_login(&info)),
        });
    }
    Ok(info)
}

/// Downloads and parses an M3U URL for onboarding. Returns the number of entries.
///
/// # Errors
///
/// Network errors, not a playlist.
pub async fn test_m3u_url(
    url: &SecretString,
    user_agent: &str,
    redactor: opensmarters_core::Redactor,
) -> AppResult<usize> {
    redactor.add_secret(url.expose_secret());
    let config = HttpConfig {
        user_agent: user_agent.to_owned(),
        ..HttpConfig::default()
    };
    let http = HttpClient::new(&config, redactor)?;
    let parsed = opensmarters_core::url::Url::parse(url.expose_secret().trim())
        .map_err(|_| AppError::msg("That isn't a valid web address."))?;
    let bytes = http.get_bytes(&parsed).await?;
    let playlist = m3u::parse(&bytes, &M3uLimits::default())?;
    Ok(playlist.entries.len())
}

/// Parses a local M3U file for onboarding. Returns the number of entries.
///
/// # Errors
///
/// Unreadable file, too large, not a playlist.
pub async fn test_m3u_file(path: std::path::PathBuf) -> AppResult<usize> {
    Ok(load_m3u_file(path).await?.entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_presets() {
        assert!(user_agent_string(&UaSetting::Default).starts_with("opensmarters/"));
        assert_eq!(
            user_agent_string(&UaSetting::SmartersCompat),
            "IPTVSmartersPro"
        );
        assert_eq!(user_agent_string(&UaSetting::Custom("X/1".into())), "X/1");
    }

    #[test]
    fn container_mapping_round_trips() {
        for c in [LiveContainer::Ts, LiveContainer::M3u8] {
            assert_eq!(store_container(core_container(c)), c);
        }
    }
}

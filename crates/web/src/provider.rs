// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider access for one account: credentials (sealed at rest, decrypted only to build
//! a request), the Xtream Codes API and M3U / XMLTV downloads through the SSRF-safe
//! [`Net`](crate::net::Net), parsing with `opensmarters-core`, normalised catalogue
//! models, and an in-memory cache per account.
//!
//! Cache lifetimes: login 10 min, lists and VOD / series info 6 h, EPG rows 10 min, the
//! M3U playlist and its XMLTV guide 6 h. A failed refresh falls back to the expired copy.
//! "Refresh" in the API drops everything ([`ProviderCache::drop_account`]). Nothing here
//! is written to disk (an M3U playlist holds credential-bearing stream addresses).

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use chrono::{DateTime, NaiveDateTime, Utc};
use opensmarters_core::catalog::SearchQuery;
use opensmarters_core::epg::ServerTimeZone;
use opensmarters_core::m3u::{self, EntryKind, M3uLimits, Playlist};
use opensmarters_core::model::{archive_programmes, parse_epg_listings};
use opensmarters_core::url::Url;
use opensmarters_core::urls::stream_base;
use opensmarters_core::xmltv::{self, XmltvLimits};
use opensmarters_core::{
    AccountStatus, ArchiveProgramme, AuthFailure, Category, EpgListing, LiveStream, LoginInfo,
    SeriesInfo, ServerUrl, VodInfo, VodStream, de,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::db::AccountRow;
use crate::error::{ApiError, ApiResult};
use crate::net::{NetError, Purpose};
use crate::state::AppState;
use crate::text;

const LOGIN_TTL: Duration = Duration::from_secs(600);
const LIST_TTL: Duration = Duration::from_secs(6 * 3600);
const EPG_TTL: Duration = Duration::from_secs(600);
const MAX_SLOTS: usize = 4000;

// ------------------------------------------------------------------------------------
// Credentials
// ------------------------------------------------------------------------------------

/// Decrypted provider credentials. `Debug` prints no secret.
pub enum Creds {
    /// Xtream Codes.
    Xtream {
        /// API base (no credentials).
        server: ServerUrl,
        /// User name.
        username: String,
        /// Password.
        password: SecretString,
    },
    /// M3U playlist.
    M3u {
        /// Playlist address (secret: usually carries credentials).
        url: SecretString,
        /// XMLTV address override (secret).
        epg: Option<SecretString>,
    },
}

impl std::fmt::Debug for Creds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Creds::Xtream { server, .. } => write!(f, "Creds::Xtream({server}, ***)"),
            Creds::M3u { .. } => f.write_str("Creds::M3u(***)"),
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct SecretBlob {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m3u_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epg_url: Option<String>,
}

impl Drop for SecretBlob {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        for s in [&mut self.password, &mut self.m3u_url, &mut self.epg_url]
            .into_iter()
            .flatten()
        {
            s.zeroize();
        }
    }
}

/// Seals credentials for an account row.
///
/// # Errors
///
/// 500 on a crypto failure.
pub fn seal(
    state: &AppState,
    user_id: &str,
    account_id: &str,
    creds: &Creds,
) -> ApiResult<Vec<u8>> {
    let blob = match creds {
        Creds::Xtream { password, .. } => SecretBlob {
            password: Some(password.expose_secret().to_owned()),
            m3u_url: None,
            epg_url: None,
        },
        Creds::M3u { url, epg } => SecretBlob {
            password: None,
            m3u_url: Some(url.expose_secret().to_owned()),
            epg_url: epg.as_ref().map(|e| e.expose_secret().to_owned()),
        },
    };
    let json =
        Zeroizing::new(serde_json::to_vec(&blob).map_err(|_| ApiError::internal("seal json"))?);
    state
        .keys
        .seal_account(user_id, account_id, &json)
        .map_err(|_| ApiError::internal("seal"))
}

/// Decrypts an account's credentials and registers them with the log redactor.
///
/// # Errors
///
/// 409 `reauth_required` when the secret can't be decrypted (the key changed).
pub fn open(state: &AppState, row: &AccountRow) -> ApiResult<Creds> {
    let reauth = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "reauth_required",
            "This provider's saved details can't be read any more (the server's secret key changed). Edit the provider and enter them again.",
        )
    };
    let plain = state
        .keys
        .open_account(&row.user_id, &row.id, &row.secret)
        .map_err(|_| reauth())?;
    let blob: SecretBlob = serde_json::from_slice(&plain).map_err(|_| reauth())?;
    let redactor = state.net.redactor();
    match row.kind.as_str() {
        "xtream" => {
            let server = ServerUrl::parse(row.server.as_deref().unwrap_or_default())
                .map_err(|_| reauth())?;
            let username = row.username.clone().unwrap_or_default();
            let password = blob.password.as_deref().ok_or_else(reauth)?;
            redactor.add_secret(password);
            redactor.add_secret(&username);
            Ok(Creds::Xtream {
                server,
                username,
                password: SecretString::from(password.to_owned()),
            })
        }
        _ => {
            let url = blob.m3u_url.as_deref().ok_or_else(reauth)?;
            redactor.add_secret(url);
            if let Some(e) = blob.epg_url.as_deref() {
                redactor.add_secret(e);
            }
            Ok(Creds::M3u {
                url: SecretString::from(url.to_owned()),
                epg: blob
                    .epg_url
                    .as_deref()
                    .map(|e| SecretString::from(e.to_owned())),
            })
        }
    }
}

/// `{server}/player_api.php?username=…&password=…[&…]`. **Contains the password.**
pub fn api_url(
    server: &ServerUrl,
    username: &str,
    password: &SecretString,
    params: &[(&str, &str)],
) -> Url {
    let mut url = server.endpoint("player_api.php");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("username", username)
            .append_pair("password", password.expose_secret());
        for (k, v) in params {
            q.append_pair(k, v);
        }
    }
    url
}

/// `{server}/xmltv.php?username=…&password=…`. **Contains the password.**
pub fn xmltv_url(server: &ServerUrl, username: &str, password: &SecretString) -> Url {
    let mut url = server.endpoint("xmltv.php");
    url.query_pairs_mut()
        .append_pair("username", username)
        .append_pair("password", password.expose_secret());
    url
}

/// Fetches and parses the Xtream login answer (no cache, no checks).
///
/// # Errors
///
/// [`NetError`]s and parse failures, mapped by [`login_error`].
pub async fn fetch_login(
    state: &AppState,
    server: &ServerUrl,
    username: &str,
    password: &SecretString,
) -> Result<LoginInfo, LoginFailure> {
    let (body, _) = state
        .net
        .get(&api_url(server, username, password, &[]), Purpose::Api)
        .await
        .map_err(LoginFailure::Net)?;
    LoginInfo::from_json(&body).map_err(|_| LoginFailure::NotXtream)
}

/// Why checking an account failed.
#[derive(Debug)]
pub enum LoginFailure {
    /// Transport problem.
    Net(NetError),
    /// The answer isn't Xtream JSON.
    NotXtream,
    /// The provider refused the account.
    Auth(AuthFailure),
}

/// Maps a failed account check to the API's `provider_rejected` / `provider_unreachable`
/// / `blocked_address` answers.
pub fn login_error(f: &LoginFailure) -> ApiError {
    let rejected = |m: &str| ApiError::new(StatusCode::BAD_REQUEST, "provider_rejected", m);
    match f {
        LoginFailure::Net(NetError::Blocked(r)) => {
            ApiError::new(StatusCode::BAD_REQUEST, "blocked_address", r.message())
        }
        LoginFailure::Net(NetError::Http(401 | 403)) => {
            rejected("The provider refused these details. Check the user name and password.")
        }
        LoginFailure::Net(NetError::Http(404)) => {
            rejected("No provider answers at that address. Check the server address.")
        }
        LoginFailure::Net(NetError::InsecureRedirect) => rejected(
            "The provider redirected a secure connection to an unencrypted one, which isn't allowed. Try the http:// address.",
        ),
        LoginFailure::Net(NetError::TooLarge(_)) => {
            rejected("The provider sent more data than this server accepts.")
        }
        LoginFailure::Net(NetError::Http(_))
        | LoginFailure::Net(NetError::Network(_))
        | LoginFailure::Net(NetError::Timeout)
        | LoginFailure::Net(NetError::InvalidUrl) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_unreachable",
            "The provider can't be reached right now. Check the address, or try again in a moment.",
        ),
        LoginFailure::NotXtream => rejected(
            "That address doesn't answer like an Xtream Codes server. Check the server address.",
        ),
        LoginFailure::Auth(AuthFailure::InvalidCredentials) => {
            rejected("The provider didn't accept this user name and password.")
        }
        LoginFailure::Auth(AuthFailure::Expired { .. })
        | LoginFailure::Auth(AuthFailure::Status(AccountStatus::Expired)) => {
            rejected("This subscription has expired.")
        }
        LoginFailure::Auth(AuthFailure::Status(AccountStatus::Banned)) => {
            rejected("The provider has banned this account.")
        }
        LoginFailure::Auth(AuthFailure::Status(AccountStatus::Disabled)) => {
            rejected("The provider has disabled this account.")
        }
        LoginFailure::Auth(_) => rejected("The provider says this account isn't active."),
    }
}

/// Whether an account sends anything over plain `http` (SECURITY.md SR-12): its panel,
/// playlist or guide address, or, for Xtream, the stream addresses the panel hands out
/// (`server_info`, when the login answer is known). The UI keeps a warning on such
/// providers: their passwords and what is watched cross the network unencrypted between
/// this server and the provider.
pub fn insecure_of(creds: &Creds, login: Option<&LoginInfo>) -> bool {
    let plain = |s: &str| {
        s.trim()
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
    };
    match creds {
        Creds::Xtream { server, .. } => {
            !server.is_https()
                || login.is_some_and(|l| stream_base(&l.server_info, server).scheme() != "https")
        }
        Creds::M3u { url, epg } => {
            plain(url.expose_secret()) || epg.as_ref().is_some_and(|e| plain(e.expose_secret()))
        }
    }
}

/// `active`, `expired`, `banned`, `disabled` or `unknown`.
pub fn status_of(info: &LoginInfo) -> &'static str {
    let now = info.server_now(Utc::now());
    let expired = info.user_info.exp_date.is_some_and(|e| e <= now);
    match &info.user_info.status {
        Some(AccountStatus::Expired) => "expired",
        Some(AccountStatus::Banned) => "banned",
        Some(AccountStatus::Disabled) => "disabled",
        _ if expired => "expired",
        Some(AccountStatus::Active) => "active",
        _ if info.user_info.auth => "active",
        _ => "unknown",
    }
}

/// Downloads and parses an M3U playlist (used to check a new account too).
///
/// # Errors
///
/// `provider_rejected` / `provider_unreachable` / `blocked_address` answers.
pub async fn fetch_playlist(state: &AppState, url: &SecretString) -> ApiResult<Playlist> {
    let parsed = Url::parse(url.expose_secret().trim()).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The playlist address isn't a valid web address.",
        )
    })?;
    let rejected = |m: &str| ApiError::new(StatusCode::BAD_REQUEST, "provider_rejected", m);
    let (body, _) = state.net.get(&parsed, Purpose::Playlist).await.map_err(|e| match e {
        NetError::Blocked(r) => ApiError::new(StatusCode::BAD_REQUEST, "blocked_address", r.message()),
        NetError::Http(401 | 403) => rejected("The provider refused this playlist address."),
        NetError::Http(404) => rejected("There's no playlist at this address."),
        NetError::TooLarge(_) => rejected("This playlist is larger than this server accepts."),
        NetError::InsecureRedirect => rejected(
            "The provider redirected a secure connection to an unencrypted one, which isn't allowed.",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_unreachable",
            "The playlist can't be downloaded right now. Check the address, or try again in a moment.",
        ),
    })?;
    let playlist = tokio::task::spawn_blocking(move || m3u::parse(&body, &M3uLimits::default()))
        .await
        .map_err(|_| ApiError::internal("m3u parse task"))?
        .map_err(|_| rejected("This address doesn't point to an M3U playlist."))?;
    if playlist.entries.is_empty() {
        return Err(rejected("This playlist has no channels or videos."));
    }
    Ok(playlist)
}

// ------------------------------------------------------------------------------------
// Normalised catalogue models
// ------------------------------------------------------------------------------------

/// A category.
#[derive(Debug, Clone)]
pub struct Cat {
    /// Id.
    pub id: String,
    /// Name (clean).
    pub name: String,
}

/// A live channel.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Id: the Xtream stream id, or `m3u:<hash>` for playlists.
    pub key: String,
    /// Name (clean).
    pub name: String,
    /// Channel number.
    pub number: Option<u64>,
    /// Provider logo address (becomes an image token in responses).
    pub logo: Option<String>,
    /// Category ids.
    pub categories: Vec<String>,
    /// Catch-up days.
    pub archive_days: Option<u32>,
    /// XMLTV id.
    pub epg_id: Option<String>,
    /// Xtream stream id.
    pub stream_id: Option<u64>,
    /// M3U entry index.
    pub entry: Option<usize>,
    /// Normalised name for search.
    pub search: String,
}

/// All live channels of an account.
#[derive(Debug, Default)]
pub struct LiveData {
    /// Channels in provider order.
    pub channels: Vec<Channel>,
    /// Categories in provider order.
    pub categories: Vec<Cat>,
    index: HashMap<String, usize>,
}

impl LiveData {
    fn new(channels: Vec<Channel>, categories: Vec<Cat>) -> Self {
        let mut index = HashMap::with_capacity(channels.len());
        for (i, c) in channels.iter().enumerate() {
            index.entry(c.key.clone()).or_insert(i);
        }
        LiveData {
            channels,
            categories,
            index,
        }
    }

    /// A channel by key.
    pub fn get(&self, key: &str) -> Option<&Channel> {
        self.index.get(key).and_then(|i| self.channels.get(*i))
    }
}

/// A film.
#[derive(Debug, Clone)]
pub struct Movie {
    /// Id: Xtream stream id or `m3u:<hash>`.
    pub id: String,
    /// Name.
    pub name: String,
    /// Provider poster address.
    pub poster: Option<String>,
    /// Year.
    pub year: Option<i32>,
    /// Rating 0–10.
    pub rating: Option<f64>,
    /// Added (Unix seconds).
    pub added: Option<i64>,
    /// Category ids.
    pub categories: Vec<String>,
    /// Container extension (Xtream).
    pub ext: Option<String>,
    /// M3U entry index.
    pub entry: Option<usize>,
    /// Normalised name for search.
    pub search: String,
}

/// A series.
#[derive(Debug, Clone)]
pub struct Show {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Provider poster address.
    pub poster: Option<String>,
    /// Year.
    pub year: Option<i32>,
    /// Rating 0–10.
    pub rating: Option<f64>,
    /// Last update (Unix seconds).
    pub updated: Option<i64>,
    /// Category ids.
    pub categories: Vec<String>,
    /// Normalised name for search.
    pub search: String,
}

/// One programme.
#[derive(Debug, Clone, Serialize)]
pub struct Programme {
    /// Unix seconds.
    pub start: i64,
    /// Unix seconds.
    pub end: i64,
    /// Title.
    pub title: String,
    /// Description.
    pub description: Option<String>,
}

/// A channel's full schedule as the panel sent it (`get_simple_data_table`), with the
/// archive flags and the server-local start strings that catch-up needs.
#[derive(Debug, Default)]
pub struct Table {
    /// Listings by start time (repeated starts dropped).
    pub listings: Vec<EpgListing>,
    /// The panel's time zone when the table was fetched.
    pub tz: ServerTimeZone,
}

impl Table {
    /// The panel's wall-clock start of the programme starting at `start` (Unix seconds),
    /// for the timeshift address: the panel's own `start` string when it sent one.
    pub fn local_start(&self, start: i64) -> Option<NaiveDateTime> {
        self.listings
            .iter()
            .find(|l| l.start.timestamp() == start)
            .map(|l| l.server_local_start(&self.tz))
    }

    /// The programmes that can be replayed at `now`: the panel's archive flag set, ended,
    /// and started at most `days` days ago; newest first.
    pub fn archive(&self, days: u32, now: DateTime<Utc>) -> Vec<Programme> {
        let from = now - chrono::Duration::days(i64::from(days));
        let mut out: Vec<Programme> = archive_programmes(&self.listings, &self.tz, now)
            .iter()
            .filter(|p| p.stop <= now && p.start >= from)
            .map(archived)
            .collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.start));
        out
    }
}

/// A parsed M3U account.
#[derive(Debug)]
pub struct M3uData {
    /// The playlist (holds stream addresses: memory only).
    pub playlist: Playlist,
    /// Live channels.
    pub live: Arc<LiveData>,
    /// Films.
    pub movies: Arc<Vec<Movie>>,
}

/// An XMLTV guide indexed by lower-cased channel id.
#[derive(Debug, Default)]
pub struct Guide {
    by_channel: HashMap<String, Vec<Programme>>,
}

impl Guide {
    /// Programmes of an XMLTV channel id.
    pub fn programmes(&self, id: &str) -> &[Programme] {
        self.by_channel
            .get(&id.to_lowercase())
            .map_or(&[], Vec::as_slice)
    }
}

fn search_key(name: &str) -> String {
    opensmarters_core::catalog::normalize_for_search(name)
}

/// Whether a normalised name matches a query.
pub fn matches(query: &SearchQuery, normalised: &str) -> bool {
    query.matches_normalized(normalised)
}

fn categories_of(
    primary: Option<&opensmarters_core::CategoryId>,
    all: &[opensmarters_core::CategoryId],
) -> Vec<String> {
    let mut out: Vec<String> = all.iter().map(|c| c.as_str().to_owned()).collect();
    if let Some(p) = primary
        && !out.iter().any(|c| c == p.as_str())
    {
        out.insert(0, p.as_str().to_owned());
    }
    out
}

fn cats(list: Vec<Category>) -> Vec<Cat> {
    list.into_iter()
        .map(|c| Cat {
            id: c.id.as_str().to_owned(),
            name: text::line(&c.name, text::MAX_NAME),
        })
        .collect()
}

fn channel_from_xtream(s: LiveStream) -> Channel {
    let name = text::line(&s.name, text::MAX_NAME);
    Channel {
        key: s.stream_id.to_string(),
        search: search_key(&name),
        name,
        number: s.num,
        logo: s.stream_icon,
        categories: categories_of(s.category_id.as_ref(), &s.category_ids),
        archive_days: s
            .tv_archive
            .then(|| s.tv_archive_duration.unwrap_or(1).max(1)),
        epg_id: s.epg_channel_id,
        stream_id: Some(s.stream_id),
        entry: None,
    }
}

fn movie_from_xtream(m: VodStream) -> Movie {
    let name = text::line(&m.name, text::MAX_NAME);
    Movie {
        id: m.stream_id.to_string(),
        search: search_key(&name),
        year: text::year(None, &name),
        name,
        poster: m.stream_icon,
        rating: text::rating10(m.rating, m.rating_5based),
        added: m.added.map(|a| a.timestamp()),
        categories: categories_of(m.category_id.as_ref(), &m.category_ids),
        ext: m.container_extension,
        entry: None,
    }
}

fn show_from_xtream(s: opensmarters_core::Series) -> Show {
    let name = text::line(&s.name, text::MAX_NAME);
    Show {
        id: s.series_id.to_string(),
        search: search_key(&name),
        year: text::year(s.release_date.as_deref(), &name),
        poster: s.poster().map(str::to_owned),
        name,
        rating: text::rating10(s.rating, s.rating_5based),
        updated: s.last_modified.map(|t| t.timestamp()),
        categories: categories_of(s.category_id.as_ref(), &s.category_ids),
    }
}

/// FNV-1a 64: a stable hash for M3U keys (never the URL: it carries credentials).
fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn is_web(url: &str) -> bool {
    let l = url.trim().get(..8).unwrap_or(url).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

fn m3u_data(playlist: Playlist) -> M3uData {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut channels = Vec::new();
    let mut movies = Vec::new();
    let mut groups: Vec<Cat> = Vec::new();
    let mut group_seen = std::collections::HashSet::new();
    for (i, e) in playlist.entries.iter().enumerate() {
        if !is_web(&e.url) || e.kind == EntryKind::Series {
            continue;
        }
        let base = format!(
            "m3u:{:016x}",
            fnv(&[
                e.tvg_id.as_deref().unwrap_or_default(),
                &e.title,
                e.group_title.as_deref().unwrap_or_default()
            ])
        );
        let n = seen.entry(base.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 { base } else { format!("{base}#{n}") };
        let name = text::line(&e.title, text::MAX_NAME);
        let group: Vec<String> = e
            .group_title
            .iter()
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty())
            .collect();
        match e.kind {
            EntryKind::Live => {
                for g in &group {
                    if group_seen.insert(g.clone()) {
                        groups.push(Cat {
                            id: g.clone(),
                            name: text::line(g, text::MAX_NAME),
                        });
                    }
                }
                channels.push(Channel {
                    key,
                    search: search_key(&name),
                    name,
                    number: e.tvg_chno.map(u64::from),
                    logo: e.tvg_logo.clone(),
                    categories: group,
                    archive_days: None,
                    epg_id: e.tvg_id.clone().filter(|t| !t.trim().is_empty()),
                    stream_id: None,
                    entry: Some(i),
                });
            }
            EntryKind::Movie => movies.push(Movie {
                id: key,
                search: search_key(&name),
                year: text::year(None, &name),
                name,
                poster: e.tvg_logo.clone(),
                rating: None,
                added: None,
                categories: group,
                ext: None,
                entry: Some(i),
            }),
            EntryKind::Series => {}
        }
    }
    M3uData {
        live: Arc::new(LiveData::new(channels, groups)),
        movies: Arc::new(movies),
        playlist,
    }
}

fn programme(l: &EpgListing) -> Programme {
    Programme {
        start: l.start.timestamp(),
        end: l.stop.timestamp(),
        title: text::line(&l.title, 240),
        description: text::opt_paragraph(Some(&l.description), 1000),
    }
}

fn archived(p: &ArchiveProgramme) -> Programme {
    Programme {
        start: p.start.timestamp(),
        end: p.stop.timestamp(),
        title: text::line(&p.title, 240),
        description: text::opt_paragraph(Some(&p.description), 1000),
    }
}

// ------------------------------------------------------------------------------------
// Cache
// ------------------------------------------------------------------------------------

type Value = Arc<dyn Any + Send + Sync>;

struct Slot {
    at: Instant,
    value: Value,
}

/// Cached provider data of one account.
pub struct AccountCache {
    slots: Mutex<HashMap<String, Slot>>,
    flights: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Bounds concurrent per-channel EPG requests.
    pub epg_permits: tokio::sync::Semaphore,
}

impl std::fmt::Debug for AccountCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCache").finish_non_exhaustive()
    }
}

impl AccountCache {
    fn new() -> Self {
        AccountCache {
            slots: Mutex::new(HashMap::new()),
            flights: Mutex::new(HashMap::new()),
            epg_permits: tokio::sync::Semaphore::new(4),
        }
    }

    /// A cached value regardless of age.
    pub fn peek<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots
            .get(key)
            .and_then(|s| Arc::clone(&s.value).downcast::<T>().ok())
    }

    fn fresh<T: Any + Send + Sync>(&self, key: &str, ttl: Duration) -> Option<Arc<T>> {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots
            .get(key)
            .filter(|s| s.at.elapsed() < ttl)
            .and_then(|s| Arc::clone(&s.value).downcast::<T>().ok())
    }

    fn store(&self, key: &str, value: Value) {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if slots.len() >= MAX_SLOTS {
            let mut ages: Vec<(Instant, String)> =
                slots.iter().map(|(k, s)| (s.at, k.clone())).collect();
            ages.sort();
            for (_, k) in ages.into_iter().take(MAX_SLOTS / 10) {
                slots.remove(&k);
            }
            self.flights
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|_, f| Arc::strong_count(f) > 1);
        }
        slots.insert(
            key.to_owned(),
            Slot {
                at: Instant::now(),
                value,
            },
        );
    }

    /// The cached value if younger than `ttl`, else `fetch` (one fetch per key at a
    /// time); a failed fetch falls back to the expired copy.
    ///
    /// # Errors
    ///
    /// The fetch error when there is no copy at all.
    pub async fn get<T, F, Fut>(&self, key: &str, ttl: Duration, fetch: F) -> ApiResult<Arc<T>>
    where
        T: Any + Send + Sync,
        F: FnOnce() -> Fut,
        Fut: Future<Output = ApiResult<T>>,
    {
        if let Some(v) = self.fresh::<T>(key, ttl) {
            return Ok(v);
        }
        let flight = {
            let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(flights.entry(key.to_owned()).or_default())
        };
        let _guard = flight.lock().await;
        if let Some(v) = self.fresh::<T>(key, ttl) {
            return Ok(v);
        }
        match fetch().await {
            Ok(v) => {
                let v = Arc::new(v);
                self.store(key, Arc::clone(&v) as Value);
                Ok(v)
            }
            Err(e) => self.peek::<T>(key).ok_or(e),
        }
    }
}

/// The caches of every account.
#[derive(Debug, Default)]
pub struct ProviderCache {
    accounts: Mutex<HashMap<String, Arc<AccountCache>>>,
}

impl ProviderCache {
    /// The cache of an account.
    pub fn account(&self, id: &str) -> Arc<AccountCache> {
        let mut map = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            map.entry(id.to_owned())
                .or_insert_with(|| Arc::new(AccountCache::new())),
        )
    }

    /// Drops everything cached for an account.
    pub fn drop_account(&self, id: &str) {
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}

// ------------------------------------------------------------------------------------
// The provider of one account
// ------------------------------------------------------------------------------------

/// Provider access for one (already user-scoped) account row.
#[derive(Clone)]
pub struct Provider {
    state: AppState,
    /// The account.
    pub row: AccountRow,
    cache: Arc<AccountCache>,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider")
            .field("account", &self.row.id)
            .finish_non_exhaustive()
    }
}

fn parse_error(e: &opensmarters_core::Error) -> ApiError {
    match e {
        opensmarters_core::Error::Auth { .. } => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            "Your provider refused the request. The subscription may have expired; check this provider in Settings.",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "provider_error",
            "Your provider sent an answer this server couldn't read.",
        ),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("parse task"))
}

impl Provider {
    /// Provider access for `row` (which the caller looked up with the user's id).
    pub fn new(state: &AppState, row: AccountRow) -> Self {
        let cache = state.providers.account(&row.id);
        Provider {
            state: state.clone(),
            row,
            cache,
        }
    }

    /// `true` for Xtream accounts.
    pub fn is_xtream(&self) -> bool {
        self.row.kind == "xtream"
    }

    /// The cache.
    pub fn cache(&self) -> &AccountCache {
        &self.cache
    }

    /// Decrypts the credentials.
    ///
    /// # Errors
    ///
    /// 409 `reauth_required`.
    pub fn creds(&self) -> ApiResult<Creds> {
        open(&self.state, &self.row)
    }

    async fn xtream(&self, params: &[(&str, &str)]) -> ApiResult<Vec<u8>> {
        let Creds::Xtream {
            server,
            username,
            password,
        } = self.creds()?
        else {
            return Err(ApiError::bad_request(
                "This provider isn't an Xtream Codes account.",
            ));
        };
        let url = api_url(&server, &username, &password, params);
        let (body, _) = self
            .state
            .net
            .get(&url, Purpose::Api)
            .await
            .map_err(|e| ApiError::provider(&e))?;
        Ok(body)
    }

    async fn xtream_list<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        params: &[(&str, &str)],
    ) -> ApiResult<Vec<T>> {
        let body = self.xtream(params).await?;
        let listing = blocking(move || de::parse_list::<T>(&body))
            .await?
            .map_err(|e| parse_error(&e))?;
        Ok(listing.items)
    }

    /// The Xtream login answer (cached 10 min).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn login(&self) -> ApiResult<Arc<LoginInfo>> {
        self.cache
            .get("login", LOGIN_TTL, || async {
                let body = self.xtream(&[]).await?;
                let info = LoginInfo::from_json(&body).map_err(|e| parse_error(&e))?;
                self.note_insecure(&info);
                Ok(info)
            })
            .await
    }

    /// Records a change of the account's "unencrypted" flag seen in a fresh login answer
    /// (a panel that starts or stops handing out `http` stream addresses).
    fn note_insecure(&self, info: &LoginInfo) {
        let Ok(creds) = self.creds() else { return };
        let insecure = insecure_of(&creds, Some(info));
        if self.row.insecure == Some(insecure) {
            return;
        }
        let db = self.state.db.clone();
        let (u, a) = (self.row.user_id.clone(), self.row.id.clone());
        tokio::spawn(async move {
            let _ = db
                .call(move |c| crate::db::set_account_insecure(c, &u, &a, insecure))
                .await;
        });
    }

    /// The parsed M3U playlist (cached 6 h).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn m3u(&self) -> ApiResult<Arc<M3uData>> {
        self.cache
            .get("m3u", LIST_TTL, || async {
                let Creds::M3u { url, .. } = self.creds()? else {
                    return Err(ApiError::bad_request(
                        "This provider isn't an M3U playlist.",
                    ));
                };
                let playlist = fetch_playlist(&self.state, &url).await.map_err(|e| {
                    if e.status == StatusCode::BAD_REQUEST {
                        ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", e.message)
                    } else {
                        e
                    }
                })?;
                blocking(move || m3u_data(playlist)).await
            })
            .await
    }

    /// Caches a playlist that was just downloaded (account check), so the first list
    /// doesn't download it again.
    pub async fn prime_m3u(&self, playlist: Playlist) {
        if let Ok(data) = blocking(move || m3u_data(playlist)).await {
            self.cache.store("m3u", Arc::new(data) as Value);
        }
    }

    /// Live channels and categories (cached 6 h).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn live(&self) -> ApiResult<Arc<LiveData>> {
        if !self.is_xtream() {
            return Ok(Arc::clone(&self.m3u().await?.live));
        }
        self.cache
            .get("live", LIST_TTL, || async {
                let categories: Vec<Category> = self
                    .xtream_list(&[("action", "get_live_categories")])
                    .await?;
                let streams: Vec<LiveStream> =
                    self.xtream_list(&[("action", "get_live_streams")]).await?;
                blocking(move || {
                    LiveData::new(
                        streams.into_iter().map(channel_from_xtream).collect(),
                        cats(categories),
                    )
                })
                .await
            })
            .await
    }

    /// Film categories (empty for M3U).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn movie_categories(&self) -> ApiResult<Arc<Vec<Cat>>> {
        if !self.is_xtream() {
            return Ok(Arc::new(Vec::new()));
        }
        self.cache
            .get("vod_cats", LIST_TTL, || async {
                Ok(cats(
                    self.xtream_list(&[("action", "get_vod_categories")])
                        .await?,
                ))
            })
            .await
    }

    /// Series categories (empty for M3U).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn series_categories(&self) -> ApiResult<Arc<Vec<Cat>>> {
        if !self.is_xtream() {
            return Ok(Arc::new(Vec::new()));
        }
        self.cache
            .get("series_cats", LIST_TTL, || async {
                Ok(cats(
                    self.xtream_list(&[("action", "get_series_categories")])
                        .await?,
                ))
            })
            .await
    }

    /// Films of a category (`None` = all).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn movies(&self, category: Option<&str>) -> ApiResult<Arc<Vec<Movie>>> {
        if !self.is_xtream() {
            let all = Arc::clone(&self.m3u().await?.movies);
            return Ok(match category {
                None => all,
                Some(c) => Arc::new(
                    all.iter()
                        .filter(|m| m.categories.iter().any(|x| x == c))
                        .cloned()
                        .collect(),
                ),
            });
        }
        let key = format!("vod:{}", category.unwrap_or("*"));
        self.cache
            .get(&key, LIST_TTL, || async {
                let mut params = vec![("action", "get_vod_streams")];
                if let Some(c) = category {
                    params.push(("category_id", c));
                }
                let items: Vec<VodStream> = self.xtream_list(&params).await?;
                blocking(move || items.into_iter().map(movie_from_xtream).collect()).await
            })
            .await
    }

    /// Series of a category (`None` = all). Empty for M3U.
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn series(&self, category: Option<&str>) -> ApiResult<Arc<Vec<Show>>> {
        if !self.is_xtream() {
            return Ok(Arc::new(Vec::new()));
        }
        let key = format!("series:{}", category.unwrap_or("*"));
        self.cache
            .get(&key, LIST_TTL, || async {
                let mut params = vec![("action", "get_series")];
                if let Some(c) = category {
                    params.push(("category_id", c));
                }
                let items: Vec<opensmarters_core::Series> = self.xtream_list(&params).await?;
                blocking(move || items.into_iter().map(show_from_xtream).collect()).await
            })
            .await
    }

    /// A film already in any cached list (no request).
    pub fn peek_movie(&self, id: &str) -> Option<Movie> {
        let slots = self
            .cache
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut keys: Vec<&String> = slots.keys().filter(|k| k.starts_with("vod:")).collect();
        keys.sort_by_key(|k| k.as_str() != "vod:*");
        for k in keys {
            if let Some(list) = slots
                .get(k)
                .and_then(|s| Arc::clone(&s.value).downcast::<Vec<Movie>>().ok())
                && let Some(m) = list.iter().find(|m| m.id == id)
            {
                return Some(m.clone());
            }
        }
        if let Some(m3u) = slots
            .get("m3u")
            .and_then(|s| Arc::clone(&s.value).downcast::<M3uData>().ok())
        {
            return m3u.movies.iter().find(|m| m.id == id).cloned();
        }
        None
    }

    /// A series already in any cached list (no request).
    pub fn peek_show(&self, id: &str) -> Option<Show> {
        let slots = self
            .cache
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut keys: Vec<&String> = slots.keys().filter(|k| k.starts_with("series:")).collect();
        keys.sort_by_key(|k| k.as_str() != "series:*");
        for k in keys {
            if let Some(list) = slots
                .get(k)
                .and_then(|s| Arc::clone(&s.value).downcast::<Vec<Show>>().ok())
                && let Some(s) = list.iter().find(|s| s.id == id)
            {
                return Some(s.clone());
            }
        }
        None
    }

    /// The series id of an episode, from any cached series info.
    pub fn peek_episode_series(&self, episode_id: u64) -> Option<(String, Arc<SeriesInfo>)> {
        let slots = self
            .cache
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slots
            .iter()
            .filter_map(|(k, s)| {
                let id = k.strip_prefix("series_info:")?;
                let info = Arc::clone(&s.value).downcast::<SeriesInfo>().ok()?;
                info.episode(episode_id)
                    .is_some()
                    .then(|| (id.to_owned(), info))
            })
            .next()
    }

    /// `get_vod_info` (cached 6 h).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn vod_info(&self, id: u64) -> ApiResult<Arc<VodInfo>> {
        let key = format!("vod_info:{id}");
        self.cache
            .get(&key, LIST_TTL, || async {
                let id = id.to_string();
                let body = self
                    .xtream(&[("action", "get_vod_info"), ("vod_id", &id)])
                    .await?;
                blocking(move || de::parse_object::<VodInfo>(&body))
                    .await?
                    .map_err(|e| parse_error(&e))
            })
            .await
    }

    /// `get_series_info` (cached 6 h).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn series_info(&self, id: u64) -> ApiResult<Arc<SeriesInfo>> {
        let key = format!("series_info:{id}");
        self.cache
            .get(&key, LIST_TTL, || async {
                let id = id.to_string();
                let body = self
                    .xtream(&[("action", "get_series_info"), ("series_id", &id)])
                    .await?;
                blocking(move || de::parse_object::<SeriesInfo>(&body))
                    .await?
                    .map_err(|e| parse_error(&e))
            })
            .await
    }

    /// Now / next rows of an Xtream channel (`get_short_epg`, cached 10 min).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn short_epg(&self, stream_id: u64) -> ApiResult<Arc<Vec<Programme>>> {
        let key = format!("epg:{stream_id}");
        self.cache
            .get(&key, EPG_TTL, || async {
                let _permit = self
                    .cache
                    .epg_permits
                    .acquire()
                    .await
                    .map_err(|_| ApiError::internal("epg permits"))?;
                let tz = self
                    .login()
                    .await
                    .map(|l| l.time_zone())
                    .unwrap_or_default();
                let id = stream_id.to_string();
                let body = self
                    .xtream(&[
                        ("action", "get_short_epg"),
                        ("stream_id", &id),
                        ("limit", "4"),
                    ])
                    .await?;
                let listing = blocking(move || parse_epg_listings(&body, &tz))
                    .await?
                    .map_err(|e| parse_error(&e))?;
                let mut rows: Vec<Programme> = listing.items.iter().map(programme).collect();
                rows.sort_by_key(|p| p.start);
                Ok(rows)
            })
            .await
    }

    /// The full schedule of an Xtream channel with its archive flags
    /// (`get_simple_data_table`, cached 10 min).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn table(&self, stream_id: u64) -> ApiResult<Arc<Table>> {
        let key = format!("table:{stream_id}");
        self.cache
            .get(&key, EPG_TTL, || async {
                let tz = self
                    .login()
                    .await
                    .map(|l| l.time_zone())
                    .unwrap_or_default();
                let id = stream_id.to_string();
                let body = self
                    .xtream(&[("action", "get_simple_data_table"), ("stream_id", &id)])
                    .await?;
                let listing = blocking(move || parse_epg_listings(&body, &tz))
                    .await?
                    .map_err(|e| parse_error(&e))?;
                let mut listings = listing.items;
                listings.sort_by_key(|l| l.start);
                listings.dedup_by_key(|l| l.start);
                Ok(Table { listings, tz })
            })
            .await
    }

    /// The schedule of an Xtream channel for the guide (from [`Provider::table`]).
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn schedule(&self, stream_id: u64) -> ApiResult<Vec<Programme>> {
        Ok(self
            .table(stream_id)
            .await?
            .listings
            .iter()
            .map(programme)
            .collect())
    }

    /// The XMLTV guide of an M3U account (its EPG address, else the playlist's
    /// `url-tvg`), parsed for −12 h … +48 h (cached 6 h). Empty without a guide.
    ///
    /// # Errors
    ///
    /// Provider errors.
    pub async fn guide(&self) -> ApiResult<Arc<Guide>> {
        self.cache
            .get("xmltv", LIST_TTL, || async {
                let Creds::M3u { epg, .. } = self.creds()? else {
                    return Ok(Guide::default());
                };
                let data = self.m3u().await?;
                let address: Option<Zeroizing<String>> = epg
                    .map(|e| Zeroizing::new(e.expose_secret().to_owned()))
                    .or_else(|| {
                        data.playlist
                            .tvg_urls
                            .first()
                            .map(|u| Zeroizing::new(u.clone()))
                    });
                let Some(address) = address else {
                    return Ok(Guide::default());
                };
                self.state.net.redactor().add_secret(&address);
                let Ok(url) = Url::parse(address.trim()) else {
                    return Ok(Guide::default());
                };
                let (body, _) = self
                    .state
                    .net
                    .get(&url, Purpose::Epg)
                    .await
                    .map_err(|e| ApiError::provider(&e))?;
                let wanted: std::collections::HashSet<String> = data
                    .live
                    .channels
                    .iter()
                    .filter_map(|c| c.epg_id.as_deref().map(str::to_lowercase))
                    .collect();
                blocking(move || parse_guide(&body, &wanted)).await
            })
            .await
    }
}

fn parse_guide(body: &[u8], wanted: &std::collections::HashSet<String>) -> Guide {
    let now = Utc::now();
    let from = now - chrono::Duration::hours(12);
    let to = now + chrono::Duration::hours(48);
    let limits = XmltvLimits {
        max_decompressed_bytes: 256 * 1024 * 1024,
        max_programmes: 400_000,
        max_text_len: 1024,
        max_token_bytes: 1024 * 1024,
        ..XmltvLimits::default()
    };
    let parsed = xmltv::parse_filtered(body, &limits, |p| {
        let end = p.stop.unwrap_or(p.start + chrono::Duration::hours(1));
        if end < from || p.start > to || !wanted.contains(&p.channel.to_lowercase()) {
            return false;
        }
        p.categories.clear();
        p.sub_titles.clear();
        p.icon = None;
        p.episode_nums.clear();
        true
    });
    let Ok(epg) = parsed else {
        return Guide::default();
    };
    let mut by_channel: HashMap<String, Vec<Programme>> = HashMap::new();
    for (channel, progs) in &epg.programmes {
        let rows: Vec<Programme> = progs
            .iter()
            .take(2000)
            .map(|p| Programme {
                start: p.start.timestamp(),
                end: p
                    .stop
                    .unwrap_or(p.start + chrono::Duration::hours(1))
                    .timestamp(),
                title: text::line(p.title(None), 240),
                description: text::opt_paragraph(p.description(None), 1000),
            })
            .collect();
        by_channel
            .entry(channel.to_lowercase())
            .or_default()
            .extend(rows);
    }
    for rows in by_channel.values_mut() {
        rows.sort_by_key(|p| p.start);
    }
    Guide { by_channel }
}

/// The programme on air at `now` and the next one.
pub fn now_next(rows: &[Programme], now: i64) -> Vec<Programme> {
    let idx = rows.partition_point(|p| p.start <= now);
    let mut out = Vec::new();
    if let Some(cur) = idx
        .checked_sub(1)
        .and_then(|i| rows.get(i))
        .filter(|p| p.end > now)
    {
        out.push(cur.clone());
    }
    if let Some(next) = rows.get(idx) {
        out.push(next.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_keys_are_stable_and_never_urls() {
        let body = b"#EXTM3U\n#EXTINF:-1 tvg-id=\"a\" group-title=\"News\",One\nhttp://h/u/p/1\n#EXTINF:-1 tvg-id=\"a\" group-title=\"News\",One\nhttp://h/u/p/2\n#EXTINF:-1,Bad\nfile:///etc/passwd\n#EXTINF:-1 group-title=\"Films\",Film\nhttp://h/movie/u/p/3.mkv\n";
        let pl = m3u::parse(body, &M3uLimits::default()).unwrap_or_default();
        let data = m3u_data(pl);
        assert_eq!(data.live.channels.len(), 2);
        let k1 = &data.live.channels[0].key;
        assert!(k1.starts_with("m3u:"));
        assert_eq!(&data.live.channels[1].key, &format!("{k1}#2"));
        assert!(!k1.contains("http"));
        assert_eq!(data.live.categories.len(), 1);
        assert_eq!(data.movies.len(), 1);
        assert_eq!(fnv(&["a", "b"]), fnv(&["a", "b"]));
        assert_ne!(fnv(&["ab", ""]), fnv(&["a", "b"]));
    }

    #[test]
    fn archive_window_and_order() {
        let tz = ServerTimeZone::Utc;
        let body = br#"{"epg_listings":[
            {"title":"T2xk","start":"2026-10-10 10:00:00","start_timestamp":1760090400,"stop_timestamp":1760094000,"has_archive":1},
            {"title":"WWVzdGVyZGF5","start":"2026-10-21 10:00:00","start_timestamp":1761040800,"stop_timestamp":1761044400,"has_archive":1},
            {"title":"Tm8gYXJjaGl2ZQ==","start_timestamp":1761044400,"stop_timestamp":1761048000,"has_archive":0},
            {"title":"VG9kYXk=","start":"2026-10-22 08:00:00","start_timestamp":1761120000,"stop_timestamp":1761123600,"has_archive":"1"},
            {"title":"T24gbm93","start_timestamp":1761123600,"stop_timestamp":1761130800,"has_archive":1}
        ]}"#;
        let mut listings = parse_epg_listings(body, &tz).unwrap_or_default().items;
        listings.sort_by_key(|l| l.start);
        let table = Table { listings, tz };
        // 2026-10-22 09:30Z: "On now" hasn't ended; "Old" is 12 days back.
        let now = DateTime::from_timestamp(1_761_125_400, 0).unwrap_or_default();
        let got: Vec<(i64, String)> = table
            .archive(7, now)
            .into_iter()
            .map(|p| (p.start, p.title))
            .collect();
        assert_eq!(
            got,
            vec![
                (1_761_120_000, "Today".to_owned()),
                (1_761_040_800, "Yesterday".to_owned())
            ]
        );
        assert_eq!(table.archive(30, now).len(), 3);
        assert_eq!(
            table.local_start(1_761_040_800).map(|d| d.to_string()),
            Some("2026-10-21 10:00:00".to_owned())
        );
        assert_eq!(table.local_start(5), None);
    }

    #[test]
    fn insecure_addresses() {
        let xt = |server: &str| Creds::Xtream {
            server: ServerUrl::parse(server).unwrap_or_else(|_| unreachable!()),
            username: "u".into(),
            password: SecretString::from("p".to_owned()),
        };
        let m3u = |url: &str, epg: Option<&str>| Creds::M3u {
            url: SecretString::from(url.to_owned()),
            epg: epg.map(|e| SecretString::from(e.to_owned())),
        };
        assert!(insecure_of(&xt("http://panel.example"), None));
        assert!(!insecure_of(&xt("https://panel.example"), None));
        let login = |proto: &str| {
            LoginInfo::from_json(
                format!(
                    r#"{{"user_info":{{"auth":1,"status":"Active"}},"server_info":{{"url":"panel.example","port":"80","https_port":"443","server_protocol":"{proto}"}}}}"#
                )
                .as_bytes(),
            )
            .unwrap_or_else(|_| unreachable!())
        };
        // An https panel that hands out http stream addresses.
        assert!(insecure_of(
            &xt("https://panel.example"),
            Some(&login("http"))
        ));
        assert!(!insecure_of(
            &xt("https://panel.example"),
            Some(&login("https"))
        ));
        assert!(insecure_of(&m3u("HTTP://lists.example/a.m3u", None), None));
        assert!(!insecure_of(
            &m3u("https://lists.example/a.m3u", None),
            None
        ));
        assert!(insecure_of(
            &m3u(
                "https://lists.example/a.m3u",
                Some(" http://guide.example/x.xml")
            ),
            None
        ));
    }

    #[test]
    fn now_and_next() {
        let p = |s, e| Programme {
            start: s,
            end: e,
            title: String::new(),
            description: None,
        };
        let rows = vec![p(0, 10), p(10, 20), p(20, 30)];
        let nn = now_next(&rows, 15);
        assert_eq!((nn[0].start, nn[1].start), (10, 20));
        assert_eq!(now_next(&rows, 35).len(), 0);
        assert_eq!(now_next(&rows, -5).len(), 1);
    }
}

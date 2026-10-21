// SPDX-License-Identifier: GPL-3.0-or-later
//! "Check for updates" (Settings › About): asks GitHub's Releases API for the latest
//! published version number and says whether a newer one exists. It **never downloads or
//! installs anything**: updating stays manual (the user runs the new setup, which updates
//! in place). The installer itself has no network code at all.
//!
//! ## The request
//!
//! One `GET https://api.github.com/repos/<owner>/<repo>/releases/latest`, where owner/repo
//! come from this build's `CARGO_PKG_REPOSITORY` ([`Repo::from_url`]; anything that isn't
//! `https://github.com/<owner>/<repo>` means the feature isn't available in this build).
//! Through [`opensmarters_core::net::HttpClient`] with [`http_config`]: https only (an
//! https → http redirect is refused), 256 KiB response cap, connect 10 s / idle 15 s /
//! total 20 s, ≤ 3 redirects, no cookies, no `Referer`, `User-Agent:
//! opensmarters/<version>` (GitHub requires one) and `Accept: application/vnd.github+json`.
//! Nothing else is sent; GitHub sees the IP address.
//!
//! ## The answer is untrusted input
//!
//! Only `tag_name`, `draft` and `prerelease` are read ([`parse_release`]). The tag must be
//! ≤ 64 bytes and exactly `v?MAJOR.MINOR.PATCH(-PRE)?` with numeric parts that fit a
//! `u32` ([`Version::parse_tag`]); drafts and pre-releases are skipped. **No URL from the
//! response is ever used** (`html_url`, asset links…), and release notes are never shown:
//! the "Get the update" link is built here ([`release_link`]) from `CARGO_PKG_HOMEPAGE`
//! (when it is an https URL) or the repository's release page for that version.
//!
//! ## Policy
//!
//! - Manual: the About page's **Check for updates** button ([`check_now`]).
//! - Automatic: only with the preference `app.check_updates_automatically` (off by
//!   default; [`crate::services::prefs::CHECK_UPDATES_AUTOMATICALLY`]), first ~10 s after
//!   startup, then at most once every 7 days ([`auto_check_due`]); a failed automatic check
//!   is silent (logged) and retried after a day. A newer version found automatically shows
//!   one toast, once per version.
//! - The last check time, the latest version seen, the version dismissed with **Not now**
//!   and the version already announced are machine-local ([`STATE`], excluded from backups
//!   by [`crate::services::backup::LOCAL_ONLY_KEYS`]); the header indicator
//!   (`shell::status_cluster`) shows while a newer, non-dismissed version is known.
//!
//! Dev/test seam (`harness` feature only, so release builds can't be redirected):
//! `OPENSMARTERS_UPDATE_URL=<full API URL>` (https, or http to 127.0.0.1/localhost), or
//! `fake[?query]` with `--demo` for the fake panel's `/__updates/releases/latest` route
//! (`?tag=v0.3.0`, `?status=429`, `?delay_ms=8000`…).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use opensmarters_core::net::HttpClient;
use opensmarters_core::url::Url;
use opensmarters_core::{DEFAULT_USER_AGENT, HttpConfig, Redactor};
use opensmarters_store::settings::Setting;
use serde::{Deserialize, Serialize};

use super::Services;
use crate::nav::{Route, SettingsPage};
use crate::screens::AppCtx;
use crate::ui::widgets::ToastKind;

/// Where the source lives (from the workspace manifest).
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// The download page, if the manifest names one (else the release page is used).
pub const HOMEPAGE: &str = env!("CARGO_PKG_HOMEPAGE");
/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Largest answer read (GitHub's "latest release" object is a few KiB).
pub const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
/// Longest `tag_name` accepted.
pub const MAX_TAG_LEN: usize = 64;
/// The `Accept` header GitHub recommends for its REST API.
pub const ACCEPT: &str = "application/vnd.github+json";
/// Automatic checks: at most once per this many seconds (7 days).
pub const AUTO_EVERY_SECS: i64 = 7 * 86_400;
/// After a failed check, wait this long before the next automatic one (1 day).
pub const RETRY_AFTER_FAILURE_SECS: i64 = 86_400;
/// The first automatic check waits this long after startup.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);
/// The toast key of "a new version is available".
pub const TOAST_KEY: &str = "updates.available";

/// The machine-local update state (never exported: see `backup::LOCAL_ONLY_KEYS`).
pub const STATE: Setting<UpdateState> = Setting::new("app.updates", UpdateState::default);

/// Env var for the dev/test override of the API URL (`harness` builds only).
#[cfg(feature = "harness")]
pub const URL_OVERRIDE_VAR: &str = "OPENSMARTERS_UPDATE_URL";

// ---------------------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------------------

/// A strictly parsed `MAJOR.MINOR.PATCH(-PRE)?` version (no build metadata).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Patch.
    pub patch: u32,
    /// Pre-release identifiers (`rc.1`), if any.
    pub pre: Option<String>,
}

/// A numeric part: ASCII digits, no leading zero (except `0`), fits a `u32`.
fn numeric(s: &str) -> Option<u32> {
    let ok = !s.is_empty()
        && s.len() <= 10
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    if ok { s.parse().ok() } else { None }
}

/// Pre-release identifiers: dot-separated, each non-empty `[0-9A-Za-z-]+`, numeric ones
/// without leading zeros.
fn valid_pre(pre: &str) -> bool {
    !pre.is_empty()
        && pre.split('.').all(|id| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!id.bytes().all(|b| b.is_ascii_digit()) || id == "0" || !id.starts_with('0'))
        })
}

impl Version {
    /// Parses `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-PRE` (no `v`, no `+build`).
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() > MAX_TAG_LEN || !s.is_ascii() {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = numeric(parts.next()?)?;
        let minor = numeric(parts.next()?)?;
        let patch = numeric(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        if let Some(p) = pre
            && !valid_pre(p)
        {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            pre: pre.map(str::to_owned),
        })
    }

    /// Parses a release tag: ≤ [`MAX_TAG_LEN`] bytes, an optional leading `v`, then
    /// [`Version::parse`]. Anything else (spaces, markup, build metadata) is refused.
    pub fn parse_tag(tag: &str) -> Option<Self> {
        if tag.len() > MAX_TAG_LEN {
            return None;
        }
        Self::parse(tag.strip_prefix('v').unwrap_or(tag))
    }

    /// This build's version.
    pub fn current() -> Option<Self> {
        Self::parse(CURRENT)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

/// SemVer precedence of two pre-release identifiers.
fn cmp_identifier(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let num = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    match (num(a), num(b)) {
        // No leading zeros, so a longer number is larger.
        (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                // A pre-release comes before its release.
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => {
                    let mut x = a.split('.');
                    let mut y = b.split('.');
                    loop {
                        match (x.next(), y.next()) {
                            (None, None) => return Ordering::Equal,
                            (None, Some(_)) => return Ordering::Less,
                            (Some(_), None) => return Ordering::Greater,
                            (Some(i), Some(j)) => match cmp_identifier(i, j) {
                                Ordering::Equal => {}
                                o => return o,
                            },
                        }
                    }
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------------------
// Where to ask, and the link to show
// ---------------------------------------------------------------------------------------

/// A GitHub repository (`owner/name`), validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    owner: String,
    name: String,
}

impl Repo {
    /// `https://github.com/<owner>/<repo>` (an optional trailing `/` or `.git`), with
    /// GitHub's character rules (owner: 1–39 letters, digits and inner hyphens; repo:
    /// 1–100 letters, digits, `.`, `_`, `-`). Anything else is `None`.
    pub fn from_url(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix("https://github.com/")?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, name) = rest.split_once('/')?;
        let owner_ok = (1..=39).contains(&owner.len())
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !owner.starts_with('-')
            && !owner.ends_with('-');
        let name_ok = (1..=100).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            && name != "."
            && name != "..";
        (owner_ok && name_ok).then(|| Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// This build's repository, if it is on GitHub.
    pub fn this_build() -> Option<Self> {
        Self::from_url(REPOSITORY)
    }

    /// `https://api.github.com/repos/<owner>/<repo>/releases/latest`.
    pub fn latest_release_api(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            self.owner, self.name
        )
    }

    /// `https://github.com/<owner>/<repo>/releases/tag/v<version>`.
    pub fn release_page(&self, v: &Version) -> String {
        format!(
            "https://github.com/{}/{}/releases/tag/v{v}",
            self.owner, self.name
        )
    }
}

/// A plain https URL suitable for `platform::open_url` (host, no user info, printable
/// ASCII without quotes or shell characters, ≤ 2 KiB), else `None`.
fn plain_https(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty()
        || s.len() > 2048
        || !s
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '`' | '\\' | '^' | '|'))
    {
        return None;
    }
    let u = Url::parse(s).ok()?;
    let ok = u.scheme() == "https"
        && u.host_str().is_some_and(|h| !h.is_empty())
        && u.username().is_empty()
        && u.password().is_none();
    ok.then(|| u.as_str().to_owned())
}

/// The "Get the update" link, built **locally** (never from the response): `homepage`
/// when it is a plain https URL (the download page), else the repository's release page
/// for `v`. `None` if neither exists.
pub fn release_link(homepage: &str, repo: Option<&Repo>, v: &Version) -> Option<String> {
    plain_https(homepage).or_else(|| repo.map(|r| r.release_page(v)))
}

/// Where the check asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    url: Url,
    repo: Option<Repo>,
}

/// True for a URL the check may ask: https with a host and no user info, or (only when
/// `allow_loopback_http`, i.e. the harness override and tests) plain http to
/// 127.0.0.1 / localhost.
pub fn allowed_source_url(url: &Url, allow_loopback_http: bool) -> bool {
    let host_ok = url.host_str().is_some_and(|h| !h.is_empty());
    let creds = !url.username().is_empty() || url.password().is_some();
    let scheme_ok = match url.scheme() {
        "https" => true,
        "http" => allow_loopback_http && matches!(url.host_str(), Some("127.0.0.1" | "localhost")),
        _ => false,
    };
    host_ok && !creds && scheme_ok
}

impl Source {
    /// GitHub's API for this build's repository (`None`: not a GitHub repository, so the
    /// feature isn't available in this build).
    pub fn production() -> Option<Self> {
        let repo = Repo::this_build()?;
        let url = Url::parse(&repo.latest_release_api()).ok()?;
        Some(Self {
            url,
            repo: Some(repo),
        })
    }

    /// Another API URL (tests, the harness override). The release link still comes from
    /// this build's repository. `None` if [`allowed_source_url`] refuses it.
    pub fn custom(url: &str, allow_loopback_http: bool) -> Option<Self> {
        let url = Url::parse(url.trim()).ok()?;
        allowed_source_url(&url, allow_loopback_http).then(|| Self {
            url,
            repo: Repo::this_build(),
        })
    }

    /// The source this build uses: the harness override if set (`harness` builds),
    /// else [`Source::production`].
    pub fn resolve() -> Option<Self> {
        #[cfg(feature = "harness")]
        if let Ok(v) = std::env::var(URL_OVERRIDE_VAR)
            && !v.trim().is_empty()
        {
            let source = override_url(v.trim(), demo_panel().as_deref())
                .and_then(|u| Self::custom(&u, true));
            if source.is_none() {
                tracing::warn!("{URL_OVERRIDE_VAR} isn't usable; update checks are off");
            }
            return source;
        }
        Self::production()
    }

    /// The API URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The repository used to build the release link.
    pub fn repo(&self) -> Option<&Repo> {
        self.repo.as_ref()
    }
}

#[cfg(all(feature = "harness", feature = "demo"))]
static DEMO_PANEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Registers the demo panel's base URL for `OPENSMARTERS_UPDATE_URL=fake` (`--demo`).
#[cfg(all(feature = "harness", feature = "demo"))]
pub fn set_demo_panel(base_url: &str) {
    let _ = DEMO_PANEL.set(base_url.trim_end_matches('/').to_owned());
}

#[cfg(feature = "harness")]
fn demo_panel() -> Option<String> {
    #[cfg(feature = "demo")]
    {
        DEMO_PANEL.get().cloned()
    }
    #[cfg(not(feature = "demo"))]
    {
        None
    }
}

/// The URL for an override value: `fake` / `fake?<query>` → the demo panel's route (needs
/// `--demo`), anything else as given.
#[cfg(feature = "harness")]
pub fn override_url(value: &str, demo_base: Option<&str>) -> Option<String> {
    let path = "/__updates/releases/latest";
    match value.strip_prefix("fake") {
        Some("") => demo_base.map(|b| format!("{b}{path}")),
        Some(q) if q.starts_with('?') => demo_base.map(|b| format!("{b}{path}{q}")),
        _ => Some(value.to_owned()),
    }
}

// ---------------------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------------------

/// What GitHub's "latest release" says, after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latest {
    /// A published release.
    Release(Version),
    /// The answer is a draft or a pre-release: nothing to offer.
    NoStableRelease,
}

/// The result of a successful check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing newer (`latest` = the latest release, if one was offered).
    UpToDate {
        /// The latest published version.
        latest: Option<Version>,
    },
    /// A newer version exists.
    Available {
        /// It.
        version: Version,
        /// The locally built link ([`release_link`]).
        url: Option<String>,
    },
}

/// Why a check failed (plain-language messages; details go to the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckError {
    /// No connection, DNS, TLS or a timeout.
    Offline,
    /// GitHub answered 403 or 429 (unauthenticated rate limit).
    RateLimited,
    /// Anything else (404, 5xx, a refused redirect, a body too large or not understood).
    Failed,
}

impl CheckError {
    /// The message shown on the About page.
    pub fn message(self) -> &'static str {
        match self {
            Self::Offline => "Couldn't reach GitHub. Check your connection and try again.",
            Self::RateLimited => "GitHub is busy right now. Try again later.",
            Self::Failed => "Couldn't check for updates.",
        }
    }

    /// Maps a transport error.
    pub fn from_http(e: &opensmarters_core::Error) -> Self {
        use opensmarters_core::Error as E;
        match e {
            E::Timeout | E::Network { .. } => Self::Offline,
            E::Http { status: 403 | 429 } => Self::RateLimited,
            _ => Self::Failed,
        }
    }
}

/// Reads GitHub's "latest release" JSON: only `tag_name` (validated by
/// [`Version::parse_tag`]), `draft` and `prerelease` (non-booleans count as false).
/// Nothing else in the body is looked at.
///
/// # Errors
///
/// [`CheckError::Failed`] if the body isn't a JSON object with a valid `tag_name`.
pub fn parse_release(body: &[u8]) -> Result<Latest, CheckError> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| CheckError::Failed)?;
    let obj = v.as_object().ok_or(CheckError::Failed)?;
    let flag = |k: &str| {
        obj.get(k)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    if flag("draft") || flag("prerelease") {
        return Ok(Latest::NoStableRelease);
    }
    obj.get("tag_name")
        .and_then(serde_json::Value::as_str)
        .and_then(Version::parse_tag)
        .map(Latest::Release)
        .ok_or(CheckError::Failed)
}

/// Compares the answer with `current` and builds the link locally.
pub fn decide(latest: Latest, current: &Version, homepage: &str, repo: Option<&Repo>) -> Outcome {
    match latest {
        Latest::Release(v) if v > *current => Outcome::Available {
            url: release_link(homepage, repo, &v),
            version: v,
        },
        Latest::Release(v) => Outcome::UpToDate { latest: Some(v) },
        Latest::NoStableRelease => Outcome::UpToDate { latest: None },
    }
}

/// The HTTP settings of the check (see the module docs).
pub fn http_config() -> HttpConfig {
    HttpConfig {
        user_agent: DEFAULT_USER_AGENT.to_owned(),
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(15),
        request_timeout: Duration::from_secs(20),
        max_response_bytes: MAX_RESPONSE_BYTES,
        max_redirects: 3,
        allow_https_downgrade: false,
    }
}

/// Runs one check against `source` (one GET, nothing downloaded or run). Failures are
/// logged (redacted writer) and mapped to a [`CheckError`].
///
/// # Errors
///
/// See [`CheckError`].
pub async fn check(
    source: &Source,
    current: &Version,
    redactor: Redactor,
) -> Result<Outcome, CheckError> {
    let http = HttpClient::new(&http_config(), redactor).map_err(|e| {
        tracing::warn!(error = %e, "update check: no HTTP client");
        CheckError::Failed
    })?;
    let body = http
        .get_bytes_accept(source.url(), ACCEPT)
        .await
        .map_err(|e| {
            tracing::info!(error = %e, "update check failed");
            CheckError::from_http(&e)
        })?;
    let latest = parse_release(&body).inspect_err(|_| {
        tracing::info!("update check: the answer isn't a valid release");
    })?;
    let outcome = decide(latest, current, HOMEPAGE, source.repo());
    match &outcome {
        Outcome::Available { version, .. } => {
            tracing::info!(%version, %current, "update check: a newer version exists");
        }
        Outcome::UpToDate { latest } => {
            tracing::info!(latest = ?latest.as_ref().map(ToString::to_string), %current, "update check: up to date");
        }
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------------------
// Policy and persisted state
// ---------------------------------------------------------------------------------------

/// Machine-local state ([`STATE`]). Versions are stored as text and re-validated on load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// Unix time of the last successful check.
    pub last_success: Option<i64>,
    /// Unix time of the last failed check (cleared by a success).
    pub last_failure: Option<i64>,
    /// The latest version GitHub reported.
    pub latest_seen: Option<String>,
    /// The version dismissed with "Not now".
    pub dismissed: Option<String>,
    /// The version already announced (toast or About page), so it is announced once.
    pub announced: Option<String>,
}

/// `now - t` lies in `0..window` (a timestamp in the future, e.g. after the clock was set
/// back, doesn't count, so it can't block checks).
fn within(now: i64, t: i64, window: i64) -> bool {
    (0..window).contains(&now.saturating_sub(t))
}

/// True when an automatic check should run now.
pub fn auto_check_due(enabled: bool, st: &UpdateState, now: i64) -> bool {
    if !enabled {
        return false;
    }
    if st
        .last_failure
        .is_some_and(|t| within(now, t, RETRY_AFTER_FAILURE_SECS))
    {
        return false;
    }
    !st.last_success
        .is_some_and(|t| within(now, t, AUTO_EVERY_SECS))
}

impl UpdateState {
    /// Drops versions that don't parse (a hand-edited database).
    pub fn sanitized(mut self) -> Self {
        for v in [
            &mut self.latest_seen,
            &mut self.dismissed,
            &mut self.announced,
        ] {
            *v = v
                .take()
                .and_then(|s| Version::parse(&s))
                .map(|v| v.to_string());
        }
        self
    }

    /// The latest version seen, if valid.
    pub fn latest(&self) -> Option<Version> {
        self.latest_seen.as_deref().and_then(Version::parse)
    }

    /// True if `v` was dismissed with "Not now".
    pub fn is_dismissed(&self, v: &Version) -> bool {
        self.dismissed.as_deref() == Some(v.to_string().as_str())
    }

    /// The version the header indicator shows: newer than `current` and not dismissed.
    pub fn indicator(&self, current: &Version) -> Option<Version> {
        self.latest()
            .filter(|v| v > current && !self.is_dismissed(v))
    }

    /// True if `v` hasn't been announced or dismissed yet (the automatic toast).
    pub fn should_announce(&self, v: &Version) -> bool {
        let s = v.to_string();
        self.announced.as_deref() != Some(s.as_str()) && !self.is_dismissed(v)
    }

    /// Records a successful check at `now`.
    pub fn record_success(&mut self, outcome: &Outcome, now: i64) {
        self.last_success = Some(now);
        self.last_failure = None;
        match outcome {
            Outcome::Available { version, .. } => self.latest_seen = Some(version.to_string()),
            Outcome::UpToDate { latest: Some(v) } => self.latest_seen = Some(v.to_string()),
            Outcome::UpToDate { latest: None } => {}
        }
    }

    /// Records a failed check at `now`.
    pub fn record_failure(&mut self, now: i64) {
        self.last_failure = Some(now);
    }
}

/// "just now", "5 minutes ago", "yesterday", "3 days ago".
pub fn checked_ago(checked_at: i64, now: i64) -> String {
    let d = now.saturating_sub(checked_at).max(0);
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match d {
        0..60 => "just now".to_owned(),
        60..3600 => plural(d / 60, "minute"),
        3600..86_400 => plural(d / 3600, "hour"),
        86_400..172_800 => "yesterday".to_owned(),
        _ => plural(d / 86_400, "day"),
    }
}

// ---------------------------------------------------------------------------------------
// The app-wide service (`cx.services.updates`)
// ---------------------------------------------------------------------------------------

/// Who started a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The About page's button: errors are shown there.
    Manual,
    /// The weekly check: silent on failure, one toast per new version.
    Automatic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Load {
    NotStarted,
    Loading,
    Ready,
}

#[derive(Debug)]
struct Inner {
    source: Option<Source>,
    current: Option<Version>,
    started: Instant,
    load: Load,
    /// A check result landed before the stored state did: keep it.
    touched: bool,
    state: UpdateState,
    checking: Option<Origin>,
    /// The last manual check's error (this session).
    error: Option<CheckError>,
    toast: Option<Version>,
    open_about: bool,
}

/// What the About page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Never checked on this computer.
    NeverChecked,
    /// The last check found nothing newer.
    UpToDate {
        /// Unix time of that check.
        checked_at: i64,
    },
    /// A newer version is known.
    Available {
        /// It.
        version: Version,
        /// The locally built link.
        url: Option<String>,
        /// "Not now" was chosen for it.
        dismissed: bool,
    },
    /// The last manual check failed.
    Failed(CheckError),
}

/// A snapshot for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// False when this build can't check (not a GitHub repository).
    pub supported: bool,
    /// A check is running.
    pub checking: bool,
    /// What to say.
    pub status: Status,
}

/// The update checker shared by the app (`cx.services.updates`). Cheap to clone.
#[derive(Debug, Clone)]
pub struct Updates {
    inner: Arc<Mutex<Inner>>,
}

impl Default for Updates {
    fn default() -> Self {
        Self::new()
    }
}

impl Updates {
    /// Resolves the source ([`Source::resolve`]); nothing is read or sent yet.
    pub fn new() -> Self {
        Self::with_source(Source::resolve())
    }

    /// With an explicit source (tests).
    pub fn with_source(source: Option<Source>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                source,
                current: Version::current(),
                started: Instant::now(),
                load: Load::NotStarted,
                touched: false,
                state: UpdateState::default(),
                checking: None,
                error: None,
                toast: None,
                open_about: false,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// True if this build can check for updates.
    pub fn supported(&self) -> bool {
        let g = self.lock();
        g.source.is_some() && g.current.is_some()
    }

    /// The UI snapshot.
    pub fn view(&self) -> View {
        let g = self.lock();
        let supported = g.source.is_some() && g.current.is_some();
        let status = match (&g.error, g.state.latest(), &g.current) {
            (Some(e), _, _) => Status::Failed(*e),
            (None, Some(v), Some(cur)) if v > *cur => Status::Available {
                url: release_link(HOMEPAGE, g.source.as_ref().and_then(Source::repo), &v),
                dismissed: g.state.is_dismissed(&v),
                version: v,
            },
            _ => match g.state.last_success {
                Some(t) => Status::UpToDate { checked_at: t },
                None => Status::NeverChecked,
            },
        };
        View {
            supported,
            checking: g.checking.is_some(),
            status,
        }
    }

    /// The version the header indicator shows, if any.
    pub fn indicator(&self) -> Option<Version> {
        let g = self.lock();
        g.current.as_ref().and_then(|c| g.state.indicator(c))
    }

    /// The header indicator was activated: open Settings › About on the next frame.
    pub fn request_open_about(&self) {
        self.lock().open_about = true;
    }

    /// "Not now": stop showing the indicator for the latest version.
    pub fn dismiss(&self, services: &Services) {
        let st = {
            let mut g = self.lock();
            g.touched = true;
            g.state.dismissed = g.state.latest_seen.clone();
            g.state.clone()
        };
        persist(services, st);
    }

    /// Starts a check unless one runs (or the build can't check).
    pub fn start(&self, services: &Services, origin: Origin) {
        let (source, current) = {
            let mut g = self.lock();
            let (Some(source), Some(current)) = (g.source.clone(), g.current.clone()) else {
                return;
            };
            if g.checking.is_some() {
                return;
            }
            g.checking = Some(origin);
            if origin == Origin::Manual {
                g.error = None;
            }
            (source, current)
        };
        services.egui().request_repaint();
        let me = self.clone();
        let svc = services.clone();
        let redactor = services.redactor.clone();
        services.spawn(async move {
            let result = check(&source, &current, redactor).await;
            me.finish(&svc, origin, result, unix_now());
        });
    }

    /// Applies a finished check (also used by tests).
    pub fn finish(
        &self,
        services: &Services,
        origin: Origin,
        result: Result<Outcome, CheckError>,
        now: i64,
    ) {
        let st = {
            let mut g = self.lock();
            g.checking = None;
            g.touched = true;
            match &result {
                Ok(outcome) => {
                    g.error = None;
                    g.state.record_success(outcome, now);
                    if let Outcome::Available { version, .. } = outcome {
                        if origin == Origin::Automatic && g.state.should_announce(version) {
                            g.toast = Some(version.clone());
                        }
                        g.state.announced = Some(version.to_string());
                    }
                }
                Err(e) => {
                    g.state.record_failure(now);
                    if origin == Origin::Manual {
                        g.error = Some(*e);
                    }
                }
            }
            g.state.clone()
        };
        persist(services, st);
        services.egui().request_repaint();
    }

    /// Loads the stored state once (in the background).
    fn ensure_loaded(&self, services: &Services) {
        {
            let mut g = self.lock();
            if g.load != Load::NotStarted {
                return;
            }
            g.load = Load::Loading;
        }
        let me = self.clone();
        let svc = services.clone();
        services.spawn(async move {
            let stored = match svc.read(|s| s.settings().get(&STATE)).await {
                Ok(st) => st.sanitized(),
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't read the update-check state");
                    UpdateState::default()
                }
            };
            let mut g = me.lock();
            if !g.touched {
                g.state = stored;
            }
            g.load = Load::Ready;
            drop(g);
            svc.egui().request_repaint();
        });
    }

    /// True once the stored state is loaded.
    pub fn loaded(&self) -> bool {
        self.lock().load == Load::Ready
    }

    fn auto_due(&self, enabled: bool, now: i64) -> bool {
        let g = self.lock();
        g.load == Load::Ready
            && g.checking.is_none()
            && g.source.is_some()
            && auto_check_due(enabled, &g.state, now)
    }

    /// Time left before the first automatic check may run.
    fn first_check_wait(&self) -> Duration {
        FIRST_CHECK_DELAY.saturating_sub(self.lock().started.elapsed())
    }

    /// The version an automatic check just found, to announce once (taken by
    /// [`on_frame`], which shows the toast).
    pub fn take_toast(&self) -> Option<Version> {
        self.lock().toast.take()
    }

    fn take_open_about(&self) -> bool {
        std::mem::take(&mut self.lock().open_about)
    }

    /// The stored state (tests).
    pub fn state(&self) -> UpdateState {
        self.lock().state.clone()
    }
}

fn persist(services: &Services, st: UpdateState) {
    services.blocking("remember the update check", move |s| {
        s.settings().set(&STATE, &st)
    });
}

fn unix_now() -> i64 {
    opensmarters_core::chrono::Utc::now().timestamp()
}

/// "Check for updates" (the About page).
pub fn check_now(cx: &AppCtx<'_>) {
    cx.services.updates.start(cx.services, Origin::Manual);
}

/// The text of the automatic toast.
pub fn toast_text(v: &Version) -> String {
    format!("opensmarters {v} is available. Get it from Settings › About.")
}

/// Per-frame hook (from the app, before the screen): loads the stored state, runs the
/// automatic check when due, shows the one-time toast, and opens Settings › About when the
/// header indicator was activated.
pub fn on_frame(cx: &mut AppCtx<'_>) {
    let updates = cx.services.updates.clone();
    updates.ensure_loaded(cx.services);
    if updates.take_open_about() {
        cx.nav.section(Route::Settings {
            page: SettingsPage::About,
        });
    }
    if let Some(v) = updates.take_toast() {
        cx.toasts
            .push(ToastKind::Info, toast_text(&v), Some(TOAST_KEY));
    }
    if !cx.prefs.check_updates_automatically || !updates.supported() {
        return;
    }
    let wait = updates.first_check_wait();
    if !wait.is_zero() {
        cx.egui.request_repaint_after(wait);
        return;
    }
    if updates.auto_due(true, unix_now()) {
        updates.start(cx.services, Origin::Automatic);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn tags_parse_strictly() {
        assert_eq!(Version::parse_tag("v0.3.0"), Some(v("0.3.0")));
        assert_eq!(Version::parse_tag("0.3.0"), Some(v("0.3.0")), "v-less");
        assert_eq!(
            Version::parse_tag("v1.2.3-rc.1"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 3,
                pre: Some("rc.1".into())
            })
        );
        assert_eq!(
            Version::parse_tag("v4294967295.0.0").map(|v| v.major),
            Some(u32::MAX)
        );
        for bad in [
            "",
            "v",
            "vv1.0.0",
            "V1.0.0",
            "1.0",
            "1.0.0.0",
            "1.0.x",
            "a.b.c",
            "01.0.0",
            "1.00.0",
            "v4294967296.0.0",
            "v99999999999.0.0",
            "1.0.0-",
            "1.0.0-rc..1",
            "1.0.0-rc.01",
            "1.0.0+build.5",
            "1.0.0-rc_1",
            " 1.0.0",
            "1.0.0 ",
            "1.0.0\n",
            "v1.0.0\"><script>alert(1)</script>",
            "v1.0.0<b>",
            "v1.0.0/../../x",
            "v１.0.0",
            "-1.0.0",
            "+1.0.0",
        ] {
            assert_eq!(Version::parse_tag(bad), None, "{bad:?}");
        }
        // Overlong: refused even if otherwise well-formed.
        let long = format!("v1.0.0-{}", "a".repeat(60));
        assert!(long.len() > MAX_TAG_LEN);
        assert_eq!(Version::parse_tag(&long), None);
        let fits = format!("v1.0.0-{}", "a".repeat(MAX_TAG_LEN - 7));
        assert_eq!(fits.len(), MAX_TAG_LEN);
        assert!(Version::parse_tag(&fits).is_some());
    }

    #[test]
    fn versions_compare_like_semver() {
        assert!(v("0.3.0") > v("0.2.0"));
        assert!(v("0.2.1") > v("0.2.0"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.10.0") > v("0.9.0"), "numeric, not text");
        assert!(v("1.0.0-rc.1") < v("1.0.0"), "pre-release < release");
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
        assert!(v("1.0.0-beta.11") < v("1.0.0-rc.1"));
        assert_eq!(v("1.2.3").cmp(&v("1.2.3")), std::cmp::Ordering::Equal);
        assert_eq!(v("1.2.3-rc.1").to_string(), "1.2.3-rc.1");
        assert!(Version::current().is_some(), "this build's version parses");
    }

    #[test]
    fn repository_urls() {
        let r = Repo::from_url("https://github.com/frozenshieldweb/opensmarters").unwrap();
        assert_eq!(
            r.latest_release_api(),
            "https://api.github.com/repos/frozenshieldweb/opensmarters/releases/latest"
        );
        assert_eq!(
            Repo::from_url("https://github.com/a-b/c.d_e-f.git/"),
            Repo::from_url("https://github.com/a-b/c.d_e-f")
        );
        assert!(Repo::from_url("https://github.com/a-b/c.d_e-f").is_some());
        for bad in [
            "",
            "http://github.com/a/b",
            "https://gitlab.com/a/b",
            "https://github.com.evil.example/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com/-a/b",
            "https://github.com/a/..",
            "https://github.com/a b/c",
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#x",
        ] {
            assert!(Repo::from_url(bad).is_none(), "{bad}");
        }
        assert!(Repo::this_build().is_some(), "this build is on GitHub");
    }

    #[test]
    fn the_link_is_built_locally() {
        let repo = Repo::from_url("https://github.com/frozenshieldweb/opensmarters").unwrap();
        let ver = v("0.3.0");
        assert_eq!(
            release_link("", Some(&repo), &ver).as_deref(),
            Some("https://github.com/frozenshieldweb/opensmarters/releases/tag/v0.3.0")
        );
        assert_eq!(
            release_link("https://example.org/download/", Some(&repo), &ver).as_deref(),
            Some("https://example.org/download/")
        );
        // A homepage that isn't a plain https URL is ignored.
        for bad in [
            "http://example.org/",
            "https://user:pw@example.org/",
            "https://example.org/\"&calc",
            "javascript:alert(1)",
            "file:///C:/x",
        ] {
            assert_eq!(
                release_link(bad, Some(&repo), &ver).as_deref(),
                Some("https://github.com/frozenshieldweb/opensmarters/releases/tag/v0.3.0"),
                "{bad}"
            );
        }
        assert_eq!(release_link("", None, &ver), None);
        // The decision never uses anything from the answer but the version.
        let body = br#"{"tag_name":"v0.3.0","html_url":"https://evil.example.invalid/x",
            "assets":[{"browser_download_url":"https://evil.example.invalid/x.exe"}],
            "body":"<script>alert(1)</script>"}"#;
        let latest = parse_release(body).unwrap();
        let out = decide(latest, &v("0.2.0"), "", Some(&repo));
        assert_eq!(
            out,
            Outcome::Available {
                version: ver,
                url: Some(
                    "https://github.com/frozenshieldweb/opensmarters/releases/tag/v0.3.0".into()
                ),
            }
        );
    }

    #[test]
    fn release_json_is_read_leniently_but_validated() {
        assert_eq!(
            parse_release(br#"{"tag_name":"v1.2.3","draft":false,"prerelease":false}"#),
            Ok(Latest::Release(v("1.2.3")))
        );
        assert_eq!(
            parse_release(br#"{"tag_name":"v9.0.0","draft":true}"#),
            Ok(Latest::NoStableRelease)
        );
        assert_eq!(
            parse_release(br#"{"tag_name":"v9.0.0","prerelease":true}"#),
            Ok(Latest::NoStableRelease)
        );
        // Odd flag types count as false; unknown fields are ignored.
        assert_eq!(
            parse_release(br#"{"tag_name":"1.0.0","draft":"yes","x":[1,2]}"#),
            Ok(Latest::Release(v("1.0.0")))
        );
        for bad in [
            &b"not json"[..],
            b"[]",
            b"{}",
            br#"{"tag_name":42}"#,
            br#"{"tag_name":"latest"}"#,
            br#"{"tag_name":"v1.0.0\"><script>"}"#,
            br#"{"message":"Not Found"}"#,
        ] {
            assert_eq!(parse_release(bad), Err(CheckError::Failed));
        }
    }

    #[test]
    fn decisions() {
        let cur = v("0.2.0");
        assert_eq!(
            decide(Latest::Release(v("0.2.0")), &cur, "", None),
            Outcome::UpToDate {
                latest: Some(v("0.2.0"))
            }
        );
        assert_eq!(
            decide(Latest::Release(v("0.1.9")), &cur, "", None),
            Outcome::UpToDate {
                latest: Some(v("0.1.9"))
            }
        );
        assert_eq!(
            decide(Latest::NoStableRelease, &cur, "", None),
            Outcome::UpToDate { latest: None }
        );
        assert!(matches!(
            decide(Latest::Release(v("0.2.1")), &cur, "", None),
            Outcome::Available { url: None, .. }
        ));
        // A pre-release build is older than its release.
        assert!(matches!(
            decide(Latest::Release(v("0.3.0")), &v("0.3.0-rc.1"), "", None),
            Outcome::Available { .. }
        ));
    }

    #[test]
    fn errors_map_to_plain_messages() {
        use opensmarters_core::Error as E;
        assert_eq!(CheckError::from_http(&E::Timeout), CheckError::Offline);
        assert_eq!(
            CheckError::from_http(&E::Network {
                message: "dns".into()
            }),
            CheckError::Offline
        );
        assert_eq!(
            CheckError::from_http(&E::Http { status: 429 }),
            CheckError::RateLimited
        );
        assert_eq!(
            CheckError::from_http(&E::Http { status: 403 }),
            CheckError::RateLimited
        );
        for e in [
            E::Http { status: 404 },
            E::Http { status: 500 },
            E::TooLarge { limit: 1 },
            E::InsecureRedirect,
        ] {
            assert_eq!(CheckError::from_http(&e), CheckError::Failed, "{e}");
        }
        assert_eq!(
            CheckError::Offline.message(),
            "Couldn't reach GitHub. Check your connection and try again."
        );
        assert_eq!(
            CheckError::RateLimited.message(),
            "GitHub is busy right now. Try again later."
        );
        assert_eq!(CheckError::Failed.message(), "Couldn't check for updates.");
    }

    #[test]
    fn sources_are_https_only() {
        let p = Source::production().unwrap();
        assert_eq!(p.url().scheme(), "https");
        assert_eq!(p.url().host_str(), Some("api.github.com"));
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(allowed_source_url(&u("https://api.github.com/x"), false));
        assert!(!allowed_source_url(&u("http://api.github.com/x"), false));
        assert!(!allowed_source_url(&u("http://127.0.0.1:9/x"), false));
        assert!(allowed_source_url(&u("http://127.0.0.1:9/x"), true));
        assert!(allowed_source_url(&u("http://localhost:9/x"), true));
        assert!(!allowed_source_url(&u("http://10.0.0.1/x"), true));
        assert!(!allowed_source_url(
            &u("https://u:p@api.github.com/x"),
            false
        ));
        assert!(!allowed_source_url(&u("file:///C:/x"), true));
        assert!(Source::custom("http://example.org/", true).is_none());
        assert!(Source::custom("https://example.org/", false).is_some());
    }

    #[cfg(feature = "harness")]
    #[test]
    fn harness_override_values() {
        let base = Some("http://127.0.0.1:5000");
        assert_eq!(
            override_url("fake", base).as_deref(),
            Some("http://127.0.0.1:5000/__updates/releases/latest")
        );
        assert_eq!(
            override_url("fake?tag=v1.0.0", base).as_deref(),
            Some("http://127.0.0.1:5000/__updates/releases/latest?tag=v1.0.0")
        );
        assert_eq!(override_url("fake", None), None, "needs --demo");
        assert_eq!(
            override_url("https://example.org/x", None).as_deref(),
            Some("https://example.org/x")
        );
        assert_eq!(override_url("fakery", base).as_deref(), Some("fakery"));
    }

    #[test]
    fn automatic_policy() {
        let now = 1_800_000_000;
        let day = 86_400;
        let mut st = UpdateState::default();
        assert!(!auto_check_due(false, &st, now), "off by default");
        assert!(auto_check_due(true, &st, now), "never checked");
        st.last_success = Some(now - 6 * day);
        assert!(!auto_check_due(true, &st, now), "checked this week");
        st.last_success = Some(now - 7 * day);
        assert!(auto_check_due(true, &st, now), "a week ago");
        st.last_success = Some(now + 30 * day);
        assert!(
            auto_check_due(true, &st, now),
            "clock set back: not blocked"
        );
        st.last_success = Some(now - 8 * day);
        st.last_failure = Some(now - 3600);
        assert!(!auto_check_due(true, &st, now), "failed an hour ago");
        st.last_failure = Some(now - day);
        assert!(auto_check_due(true, &st, now), "retried after a day");
        st.record_success(&Outcome::UpToDate { latest: None }, now);
        assert_eq!(st.last_failure, None);
        assert!(!auto_check_due(true, &st, now));
    }

    #[test]
    fn indicator_and_dismissal() {
        let cur = v("0.2.0");
        let mut st = UpdateState::default();
        assert_eq!(st.indicator(&cur), None);
        st.record_success(
            &Outcome::Available {
                version: v("0.3.0"),
                url: None,
            },
            1,
        );
        assert_eq!(st.indicator(&cur), Some(v("0.3.0")));
        assert!(st.should_announce(&v("0.3.0")));
        st.announced = Some("0.3.0".into());
        assert!(!st.should_announce(&v("0.3.0")), "announced once");
        st.dismissed = st.latest_seen.clone();
        assert_eq!(st.indicator(&cur), None, "Not now");
        // A newer version shows again.
        st.record_success(
            &Outcome::Available {
                version: v("0.4.0"),
                url: None,
            },
            2,
        );
        assert_eq!(st.indicator(&cur), Some(v("0.4.0")));
        assert!(st.should_announce(&v("0.4.0")));
        // After updating, nothing shows.
        assert_eq!(st.indicator(&v("0.4.0")), None);
        // A hand-edited database can't inject text.
        let hostile = UpdateState {
            latest_seen: Some("9.9.9\"><b>".into()),
            dismissed: Some("1.0.0".into()),
            ..UpdateState::default()
        }
        .sanitized();
        assert_eq!(hostile.latest_seen, None);
        assert_eq!(hostile.dismissed.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn ago_texts() {
        let now = 1_000_000;
        assert_eq!(checked_ago(now, now), "just now");
        assert_eq!(checked_ago(now + 50, now), "just now");
        assert_eq!(checked_ago(now - 60, now), "1 minute ago");
        assert_eq!(checked_ago(now - 600, now), "10 minutes ago");
        assert_eq!(checked_ago(now - 7200, now), "2 hours ago");
        assert_eq!(checked_ago(now - 90_000, now), "yesterday");
        assert_eq!(checked_ago(now - 3 * 86_400, now), "3 days ago");
    }

    #[test]
    fn the_http_settings_are_tight() {
        let c = http_config();
        assert_eq!(c.max_response_bytes, 256 * 1024);
        assert_eq!(c.connect_timeout, Duration::from_secs(10));
        assert_eq!(c.request_timeout, Duration::from_secs(20));
        assert!(!c.allow_https_downgrade);
        assert_eq!(c.user_agent, format!("opensmarters/{CURRENT}"));
    }
}

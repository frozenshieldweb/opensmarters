// SPDX-License-Identifier: GPL-3.0-or-later
//! # opensmarters-core
//!
//! Protocol and parsing core of opensmarters: everything that talks to, or reads data
//! from, an IPTV provider. It has no UI and no FFI, and contains no `unsafe` code.
//!
//! **All provider data is treated as hostile.** Every parser in this crate is lenient
//! (bad items are skipped, never fatal), bounded (size, count and length limits with
//! typed errors) and panic-free on arbitrary input; this is checked with `proptest`
//! in the normal test suite and with `cargo-fuzz` targets in `fuzz/`.
//!
//! ## Modules
//!
//! | Module | What it gives you |
//! |---|---|
//! | [`model`] | Typed domain models: [`XtreamAccount`], [`LoginInfo`], [`Category`], [`LiveStream`], [`VodStream`], [`Series`], [`VodInfo`], [`SeriesInfo`], [`EpgListing`], [`ArchiveProgramme`], … |
//! | [`de`] | Lenient `serde` helpers for the loosely-typed Xtream JSON and the resilient list parser ([`de::parse_list`] → [`Listing`]) |
//! | [`xtream`] | Async Xtream Codes client ([`xtream::Client`]) over `reqwest` + `rustls` |
//! | [`net`] | HTTP configuration ([`HttpConfig`]: User-Agent, timeouts, response cap) and a capped downloader ([`net::HttpClient`]) for M3U/XMLTV |
//! | [`urls`] | Live / movie / episode / timeshift stream URL builders ([`urls::StreamUrls`]) |
//! | [`m3u`] | Line-based M3U/M3U8 playlist parser and `get.php` → Xtream detection |
//! | [`xmltv`] | Streaming XMLTV parser (plain or gzip) with zip-bomb defence |
//! | [`epg`] | Lenient base64 EPG text decoding and server-timezone helpers |
//! | [`redact`] | Credential redaction for log lines ([`redact::redact_url`], [`Redactor`], [`redact::RedactingWriter`]) |
//! | [`parental`] | Whole-word, case-insensitive keyword matcher for adult-category locking |
//! | [`catalog`] | Search (diacritics-insensitive, no regex), natural sorting, All/Favourites pseudo-categories |
//!
//! ## Typical Xtream flow
//!
//! ```no_run
//! use opensmarters_core::{HttpConfig, XtreamAccount, secrecy::SecretString};
//! use opensmarters_core::urls::LiveContainer;
//! use opensmarters_core::xtream::Client;
//!
//! # async fn demo() -> Result<(), opensmarters_core::Error> {
//! // The password comes from the OS keychain; it is never stored in a model.
//! let password = SecretString::from("from-the-keychain".to_owned());
//! let account = XtreamAccount::new(" panel.example.com:8080/ ", "demo")?;
//! let client = Client::new(account, password.clone(), &HttpConfig::default())?;
//!
//! // Auth + `status == Active` + expiry against the server clock; typed `Error::Auth` otherwise.
//! let login = client.login().await?;
//!
//! // `None` = the "All" pseudo-category: `category_id` is omitted on the wire.
//! let categories = client.live_categories().await?;
//! let channels = client.live_streams(None).await?;
//! if channels.skipped > 0 {
//!     // Malformed items were skipped, not fatal. `channels.errors` has details.
//! }
//!
//! // Stream URLs use `server_info` (with fallback to the login URL), percent-encoded.
//! let urls = client.stream_urls(&login);
//! let container = LiveContainer::preferred(LiveContainer::Ts, &login.user_info);
//! if let Some(first) = channels.items.first() {
//!     let url = urls.live(first.stream_id, container);
//!     // Never log `url` directly: it carries the password. Use the redactor.
//!     tracing::info!(url = %client.redactor().redact(url.as_str()), "playing");
//! }
//! # let _ = categories;
//! # Ok(()) }
//! ```
//!
//! ## M3U playlists
//!
//! ```no_run
//! use opensmarters_core::{HttpConfig, Redactor, m3u, net::HttpClient, url::Url};
//!
//! # async fn demo(user_input: &str, redactor: Redactor) -> Result<(), Box<dyn std::error::Error>> {
//! // An Xtream `get.php` link? Offer an Xtream login instead (VOD, series, EPG, catch-up).
//! if let Some(link) = m3u::detect_xtream(user_input) {
//!     let _ = (link.server, link.username, link.password);
//! }
//! redactor.add_secret(user_input); // the playlist URL itself is a secret
//! let http = HttpClient::new(&HttpConfig::default(), redactor)?;
//! let body = http.get_bytes(&Url::parse(user_input)?).await?;
//! let playlist = m3u::parse(&body, &m3u::M3uLimits::default())?;
//! for entry in &playlist.entries {
//!     // entry.kind is Live / Movie / Series; entry.url can be any scheme:
//!     // the player must check it against its allowlist.
//!     let _ = (&entry.title, entry.kind, &entry.url, &entry.user_agent);
//! }
//! # Ok(()) }
//! ```
//!
//! ## XMLTV
//!
//! [`xmltv::parse_bytes`] takes plain or gzip input (from [`xtream::Client::xmltv_url`],
//! [`m3u::Playlist::tvg_urls`] or a user URL), decompresses under a cap and returns an
//! [`xmltv::Epg`] indexed by channel id, with [`xmltv::Epg::now_and_next`].
//!
//! ## Logging
//!
//! Register every secret with one app-wide [`Redactor`] (pass it to
//! [`xtream::Client::with_redactor`], add M3U URLs with [`Redactor::add_secret`]) and
//! write logs through [`redact::RedactingWriter`], so that mpv/ffmpeg lines echoing
//! stream URLs are scrubbed too.
//!
//! ## Security notes for callers
//!
//! - Passwords are [`secrecy::SecretString`]s and are never part of any model or error
//!   message. Errors returned by this crate never contain credentials.
//! - Stream and API URLs *do* contain credentials (that is how Xtream works). Treat any
//!   [`url::Url`] returned by [`urls::StreamUrls`] or [`xtream::Client::xmltv_url`] as a
//!   secret, and pass every log line through a [`Redactor`].
//! - URL schemes from playlists are **not** validated here; the player must check them
//!   against its allowlist before playback.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod catalog;
pub mod de;
pub mod epg;
mod error;
pub mod m3u;
pub mod model;
pub mod net;
pub mod parental;
pub mod redact;
pub mod urls;
pub mod xmltv;
pub mod xtream;

pub use de::{ItemError, Listing};
pub use error::{AuthFailure, Error};
pub use model::{
    AccountStatus, ArchiveProgramme, Category, CategoryId, EpgListing, Episode, LiveStream,
    LoginInfo, Series, SeriesInfo, ServerInfo, ServerUrl, UserInfo, VodInfo, VodStream,
    XtreamAccount,
};
pub use net::{DEFAULT_USER_AGENT, HttpConfig, IPTV_SMARTERS_UA};
pub use redact::Redactor;

/// Re-export of `chrono`, whose types appear throughout the public API.
pub use chrono;
/// Re-export of `chrono-tz`, used by [`epg::ServerTimeZone`].
pub use chrono_tz;
/// Re-export of the `secrecy` crate, so callers use the same `SecretString` type.
pub use secrecy;
/// Re-export of the `url` crate, so callers use the same `Url` type.
pub use url;

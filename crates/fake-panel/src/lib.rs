//! # fake-panel
//!
//! A **dev/test-only** fake IPTV provider: an Xtream Codes–style `player_api.php`, M3U
//! (`get.php`), XMLTV, generated artwork and real (ffmpeg-generated) test streams, all
//! with **synthetic, deterministic data**. It binds to `127.0.0.1` only.
//!
//! It is an independent implementation of the *server* side of `docs/PROTOCOL.md`
//! (it does not depend on `opensmarters-core`), so client tests against it are meaningful.
//!
//! ## Using it from another crate's tests
//!
//! ```no_run
//! # async fn demo() -> Result<(), fake_panel::Error> {
//! use fake_panel::{Config, MediaMode, Quirks, users};
//!
//! // API-only tests: don't wait for ffmpeg media generation.
//! let panel = fake_panel::spawn(Config::api_only()).await?;
//! let login = format!(
//!     "{}/player_api.php?username={}&password={}",
//!     panel.base_url(), users::DEMO.username, users::DEMO.password,
//! );
//! // ... point the client at `panel.base_url()` ...
//!
//! // Streams too (generates media into <workspace>/testdata/generated on first use):
//! let mut cfg = Config::default();
//! cfg.quirks = "mixed-types,malformed-item".parse()?;
//! let panel = fake_panel::spawn(cfg).await?;
//! assert_eq!(panel.catalogue().live.len(), 250);
//! for r in panel.requests() { println!("{} {}", r.method, r.path); }
//! panel.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! [`spawn`] binds an ephemeral port (unless [`Config::port`] is set) and serves on the
//! caller's tokio runtime until the [`Handle`] is dropped or [`Handle::shutdown`] is called.
//!
//! ## Endpoints
//!
//! | Endpoint | Notes |
//! |---|---|
//! | `GET /player_api.php?username&password` | Login (`user_info` + `server_info`) |
//! | `…&action=get_live_categories` / `get_vod_categories` / `get_series_categories` | |
//! | `…&action=get_live_streams` / `get_vod_streams` / `get_series` `[&category_id=]` | `ALL`/omitted → everything |
//! | `…&action=get_vod_info&vod_id=` / `get_series_info&series_id=` | |
//! | `…&action=get_short_epg&stream_id=[&limit=4]` / `get_simple_data_table&stream_id=` | base64 `title`/`description` |
//! | `GET /get.php?username&password&type=m3u_plus\|m3u&output=ts\|m3u8` | live + `/movie/` + `/series/` entries |
//! | `GET /xmltv.php?username&password`, `GET /xmltv.xml.gz` | XMLTV, `+HHMM` offsets; gz served as a file |
//! | `GET /nasty.m3u` | Torture playlist (see [`m3u`](crate#m3u) below) |
//! | `GET /images/{logo,poster,cover,backdrop,episode}/{id}.png` | Generated PNGs |
//! | `GET /live/{u}/{p}/{id}.ts\|.m3u8`, `GET /{u}/{p}/{id}` | Range-capable; `.m3u8` → HLS with `/hls/{token}/…` segments |
//! | `GET /movie/{u}/{p}/{id}.{ext}`, `GET /series/{u}/{p}/{id}.{ext}` | Range-capable |
//! | `GET /timeshift/{u}/{p}/{minutes}/{start}/{id}.ts\|.m3u8` | `start` = `YYYY-MM-DD:HH-MM` **or** `YYYY-MM-DD:HH:MM:SS` |
//! | `GET /streaming/timeshift.php?username&password&stream&start&duration` | Same, query form |
//! | `GET /__debug/last-requests[?limit=&path_prefix=]`, `DELETE` to clear | [`RequestRecord`] JSON |
//!
//! Stream URLs validate credentials (403), ids (404) and media availability (503 with a
//! message, e.g. when ffmpeg is missing).
//!
//! ## Users
//!
//! See [`users`]: `demo/demo` (Active), `expired/expired`, `banned/banned` (`auth:0`),
//! `disabled/disabled`, `trial/trial`, `unlimited/unlimited` (`exp_date:null`) and
//! `special` / `p@ss/w#rd %&+` (clients must percent-encode). Wrong credentials →
//! `{"user_info":{"auth":0}}`.
//!
//! ## Quirks
//!
//! See [`Quirks`]: `mixed-types`, `html-content-type`, `backdrop-string`,
//! `episodes-array`, `category-all-literal` (default on), `slow`, `huge`, `stream-host`,
//! `malformed-item`, `timeshift-503`, `timeshift-smarters-only`, `stall` (a live `.ts`
//! that freezes and never closes), and for recordings and downloads `slow-media`
//! (≈ 256 KiB/s, `Range` still works), `live-endless` (live `.ts` loops forever at its real
//! bitrate), `media-html` / `media-playlist` (a `200 OK` that isn't video).
//!
//! ## M3U
//!
//! `/nasty.m3u` contains a BOM, CRLF, odd durations, tricky attribute values, `#EXTGRP`,
//! `#EXTVLCOPT`, mixed schemes, an entry with no URL, and URLs that **must not be played**
//! (`file://`, `lavfi://`, `av://`, `edl://`, `memory://`, UNC and relative paths), all in
//! the group `Must Not Play`.

#![forbid(unsafe_code)]

mod api;
pub mod catalogue;
mod debug;
mod epg;
mod images;
mod m3u;
mod media;
mod names;
mod quirks;
mod rng;
mod server;
mod shape;
mod streams;
mod time;
pub mod users;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use catalogue::Catalogue;
pub use debug::{RequestRecord, StartFormat, TimeshiftRequest};
pub use media::{MediaMode, MediaStatus, expected_files};
pub use quirks::Quirks;
pub use time::TIMEZONE;

/// Default catalogue seed.
pub const DEFAULT_SEED: u64 = 0x0F5E_ED00_2026;

/// Errors from [`spawn`] and quirk parsing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Could not bind `127.0.0.1:{port}`.
    #[error("failed to bind 127.0.0.1:{port}: {source}")]
    Bind {
        /// Requested port.
        port: u16,
        /// Underlying error.
        source: std::io::Error,
    },
    /// Could not read the bound address.
    #[error("failed to read the bound address: {0}")]
    LocalAddr(std::io::Error),
    /// Unknown quirk name.
    #[error("unknown quirk {0:?}; known quirks: {known}", known = Quirks::NAMES.join(", "))]
    UnknownQuirk(String),
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Port on 127.0.0.1; `0` (default) picks a free one.
    pub port: u16,
    /// Where generated media lives. Default: `<workspace>/testdata/generated`.
    pub media_dir: PathBuf,
    /// How to handle missing media.
    pub media: MediaMode,
    /// Quirk switches.
    pub quirks: Quirks,
    /// Catalogue/EPG seed.
    pub seed: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 0,
            media_dir: default_media_dir(),
            media: MediaMode::Generate,
            quirks: Quirks::default(),
            seed: DEFAULT_SEED,
        }
    }
}

impl Config {
    /// Default config that never runs ffmpeg (stream endpoints work only if the media was
    /// generated before, otherwise they answer 503). Fast for API-only tests.
    pub fn api_only() -> Self {
        Self {
            media: MediaMode::Never,
            ..Self::default()
        }
    }

    /// Builder-style quirk setter.
    #[must_use]
    pub fn with_quirks(mut self, quirks: Quirks) -> Self {
        self.quirks = quirks;
        self
    }
}

/// `<workspace>/testdata/generated` (resolved from this crate's manifest directory),
/// which is gitignored.
pub fn default_media_dir() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map_or_else(|| PathBuf::from("testdata"), PathBuf::from)
        .join("testdata")
        .join("generated")
}

/// A running fake panel. Dropping it stops the server.
pub struct Handle {
    base_url: String,
    addr: SocketAddr,
    state: server::AppState,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle")
            .field("base_url", &self.base_url)
            .field("quirks", &self.state.quirks)
            .field("media", &self.state.media.status())
            .finish_non_exhaustive()
    }
}

impl Handle {
    /// `http://127.0.0.1:{port}` (no trailing slash): use as the client's server URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Base of stream URLs as advertised by `server_info` (`http://localhost:{port}` with
    /// the `stream-host` quirk).
    pub fn stream_base_url(&self) -> String {
        self.state.stream_base()
    }

    /// Bound socket address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Bound port.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// `base_url` + `path_and_query` (which should start with `/`).
    pub fn url(&self, path_and_query: &str) -> String {
        format!("{}{path_and_query}", self.base_url)
    }

    /// The catalogue being served (to compute expected counts in tests).
    pub fn catalogue(&self) -> &Catalogue {
        &self.state.catalogue
    }

    /// Active quirks.
    pub fn quirks(&self) -> Quirks {
        self.state.quirks
    }

    /// Media readiness.
    pub fn media_status(&self) -> MediaStatus {
        self.state.media.status()
    }

    /// Requests seen so far (oldest first, excluding `/__debug/*`; capped at 2000).
    pub fn requests(&self) -> Vec<RequestRecord> {
        self.state.log.snapshot()
    }

    /// Forget logged requests.
    pub fn clear_requests(&self) {
        self.state.log.clear();
    }

    /// Stop serving and wait for the server task to finish.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Start a fake panel on the current tokio runtime.
///
/// With [`MediaMode::Generate`] this waits for ffmpeg the first time media is generated
/// (about 10–20 s); afterwards startup is a few milliseconds.
pub async fn spawn(config: Config) -> Result<Handle, Error> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
        .await
        .map_err(|source| Error::Bind {
            port: config.port,
            source,
        })?;
    let addr = listener.local_addr().map_err(Error::LocalAddr)?;

    let media = Arc::new(media::MediaLibrary::new(config.media_dir.clone()));
    match config.media {
        MediaMode::Generate => {
            let m = Arc::clone(&media);
            if tokio::task::spawn_blocking(move || m.prepare(true))
                .await
                .is_err()
            {
                tracing::warn!("media preparation task panicked");
            }
        }
        MediaMode::Background => {
            if media.files_present() {
                media.prepare(false);
            } else {
                media.mark_generating();
                let m = Arc::clone(&media);
                tokio::task::spawn_blocking(move || m.prepare(true));
            }
        }
        MediaMode::Never => media.prepare(false),
    }

    let stream_host = if config.quirks.stream_host {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let state = Arc::new(server::State {
        catalogue: Catalogue::generate(config.seed, config.quirks.huge),
        quirks: config.quirks,
        seed: config.seed,
        port: addr.port(),
        api_base: base_url.clone(),
        stream_host: stream_host.to_owned(),
        media,
        images: images::ImageCache::default(),
        log: debug::RequestLog::default(),
        slow_counter: AtomicU64::new(0),
    });
    let app = server::router(Arc::clone(&state));
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let shutdown = async {
            let _ = rx.await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            tracing::warn!("fake-panel server error: {e}");
        }
    });
    tracing::info!(
        base_url = %base_url,
        quirks = %config.quirks,
        media = ?state.media.status(),
        "fake-panel listening"
    );
    Ok(Handle {
        base_url,
        addr,
        state,
        shutdown: Some(tx),
        task: Some(task),
    })
}

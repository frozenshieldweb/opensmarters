// SPDX-License-Identifier: GPL-3.0-or-later
//! # opensmarters-web
//!
//! The server of **opensmarters web**, the self-hosted web edition of opensmarters
//! (`docs/WEB_PROMPT.md`): one binary that serves the browser UI, a JSON API, an image
//! proxy and HLS streams, and talks to IPTV providers on the browser's behalf.
//!
//! ```text
//! browser ──https──▶ reverse proxy (TLS) ──▶ opensmarters-web :8080
//!                                             ├─ /, /app/…            the UI (web/dist, embedded)
//!                                             ├─ /api/…               JSON API (api)
//!                                             ├─ /api/img/<token>     image proxy (images)
//!                                             └─ /api/streams/<sid>/… HLS from ffmpeg (streams)
//!                                                   │
//!                                                   └─ provider (net: SSRF-safe client)
//! ```
//!
//! | Module | Role |
//! |---|---|
//! | [`config`] | Environment variables → [`Config`] |
//! | [`state`] | Shared state ([`AppState`]) |
//! | [`db`] | SQLite (users, sessions, invites, accounts with sealed secrets, favourites, progress, settings) |
//! | [`crypto`] | Key derivation, sealing, token hashes, image tokens |
//! | [`auth`] | Passwords (Argon2id), sessions, extractors, client address, rate limiter |
//! | [`net`] | The SSRF-safe outbound client used for every provider request |
//! | [`provider`] | Xtream / M3U / XMLTV access through `opensmarters-core`, cache, normalised models |
//! | [`streams`] | The ffmpeg HLS pipeline (stdin only), limits, idle stop |
//! | [`images`] | Signed, cached, validated image proxy |
//! | [`ui`] | The embedded UI, SPA fallback, page security headers |
//! | [`api`] | The handlers of the API contract (WEB_PROMPT §4) |
//! | `demo` | `--demo`: the fake panel in-process and a demo user (feature `demo`) |
//!
//! Embedding: [`start`] binds, spawns the server and returns a [`Server`] handle (the
//! integration tests use it with a temporary data folder and
//! [`Config::dev_loopback_allow`] set to the fake panel's address).

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
#[cfg(feature = "demo")]
pub mod demo;
pub mod error;
pub mod images;
pub mod net;
pub mod provider;
pub mod state;
pub mod streams;
pub mod text;
pub mod ui;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use opensmarters_core::Redactor;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate as _, SizeAbove};

pub use config::Config;
pub use state::AppState;

/// The whole HTTP application.
pub fn router(state: AppState) -> Router {
    // gzip for JSON (film lists run to megabytes) and the UI's files; never for video
    // segments, playlists or images (no gain, and segments are already compressed).
    let compress = CompressionLayer::new().compress_when(
        SizeAbove::new(1024)
            .and(NotForContentType::IMAGES)
            .and(NotForContentType::const_new("video/"))
            .and(NotForContentType::const_new(
                "application/vnd.apple.mpegurl",
            ))
            .and(NotForContentType::SSE),
    );
    Router::new()
        .nest("/api", api::router().layer(middleware::from_fn(api::guard)))
        .fallback(ui::serve)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(compress)
        .with_state(state)
}

/// A running server.
#[derive(Debug)]
pub struct Server {
    /// The bound address.
    pub addr: SocketAddr,
    /// The shared state (tests inspect streams, the database…).
    pub state: AppState,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    reaper: JoinHandle<()>,
}

impl Server {
    /// `http://<addr>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops accepting requests, stops every stream (ffmpeg killed, provider
    /// connections closed) and waits for the server task.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        for s in self.state.streams.list() {
            self.state.streams.stop(&s.id).await;
        }
        self.reaper.abort();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// Waits until the server stops (it runs until [`Server::shutdown`] or an error).
    pub async fn wait(mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.reaper.abort();
    }
}

/// Builds the state, binds `config.listen` and serves on the current runtime.
///
/// # Errors
///
/// Start-up failures (data folder, database, address in use…).
pub async fn start(config: Config, redactor: Redactor) -> anyhow::Result<Server> {
    let state = AppState::new(config, redactor).await?;
    start_with_state(state).await
}

/// Like [`start`] with a prepared state.
///
/// # Errors
///
/// Binding failures.
pub async fn start_with_state(state: AppState) -> anyhow::Result<Server> {
    let listener = TcpListener::bind(state.config.listen).await?;
    let addr = listener.local_addr()?;
    let reaper = tokio::spawn(Arc::clone(&state.streams).run_reaper());
    let app = router(state.clone());
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let serve = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = rx.await;
        });
        if let Err(e) = serve.await {
            tracing::error!(error = %e, "server error");
        }
    });
    tracing::info!(%addr, ui_built = ui::ui_built(), "listening");
    state.announce_setup();
    Ok(Server {
        addr,
        state,
        shutdown: Some(tx),
        task: Some(task),
        reaper,
    })
}

//! Shared state, router and small HTTP helpers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State as AxState};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::Value;

use crate::catalogue::Catalogue;
use crate::debug::RequestLog;
use crate::images::ImageCache;
use crate::media::MediaLibrary;
use crate::rng::mix;
use crate::shape::Shape;
use crate::{Quirks, api, debug, epg, images, m3u, streams, updates, users};

/// Everything a handler needs.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) catalogue: Catalogue,
    pub(crate) quirks: Quirks,
    pub(crate) seed: u64,
    pub(crate) port: u16,
    /// `http://127.0.0.1:{port}`: API, images, XMLTV.
    pub(crate) api_base: String,
    /// `server_info.url`: `127.0.0.1`, or `localhost` with the `stream-host` quirk.
    pub(crate) stream_host: String,
    pub(crate) media: Arc<MediaLibrary>,
    pub(crate) images: ImageCache,
    pub(crate) log: RequestLog,
    pub(crate) slow_counter: AtomicU64,
}

pub(crate) type AppState = Arc<State>;

impl State {
    pub(crate) fn shape(&self) -> Shape {
        Shape::new(self.quirks.mixed_types, self.seed)
    }

    /// Base URL for stream links, built from `server_info` like clients do.
    pub(crate) fn stream_base(&self) -> String {
        format!("http://{}:{}", self.stream_host, self.port)
    }
}

/// Decoded query pairs; the first occurrence of a key wins.
#[derive(Debug, Default)]
pub(crate) struct Params(Vec<(String, String)>);

impl Params {
    pub(crate) fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }

    pub(crate) fn opt(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The value, or `""` when missing.
    pub(crate) fn get(&self, key: &str) -> &str {
        self.opt(key).unwrap_or_default()
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.opt(key).is_some()
    }
}

/// Serialise JSON with the content type the quirks ask for.
pub(crate) fn json_response(st: &State, v: &Value) -> Response {
    let ct = if st.quirks.html_content_type {
        "text/html; charset=UTF-8"
    } else {
        "application/json"
    };
    match serde_json::to_string(v) {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, ct),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            body,
        )
            .into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "serialisation failed").into_response(),
    }
}

/// `slow` quirk: 0–3 s of deterministic pseudo-random latency.
async fn slow(AxState(st): AxState<AppState>, req: Request, next: Next) -> Response {
    if st.quirks.slow {
        let n = st.slow_counter.fetch_add(1, Ordering::Relaxed);
        let ms = mix(st.seed, n) % 3001;
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    next.run(req).await
}

async fn index(AxState(st): AxState<AppState>) -> Response {
    let mut s = String::from(
        "fake-panel: dev-only fake Xtream Codes panel with synthetic data\n\n\
         GET /player_api.php?username=&password=[&action=...]\n\
         GET /get.php?username=&password=&type=m3u_plus&output=ts|m3u8\n\
         GET /xmltv.php?username=&password=   GET /xmltv.xml.gz\n\
         GET /nasty.m3u\n\
         GET /images/{logo|poster|cover|backdrop|episode}/{id}.png\n\
         GET /live/{u}/{p}/{id}.ts|.m3u8   /movie/{u}/{p}/{id}.{ext}   /series/{u}/{p}/{id}.{ext}\n\
         GET /timeshift/{u}/{p}/{minutes}/{YYYY-MM-DD:HH-MM | YYYY-MM-DD:HH:MM:SS}/{id}.ts|.m3u8\n\
         GET /streaming/timeshift.php?username=&password=&stream=&start=&duration=\n\
         GET|DELETE /__debug/last-requests\n\
         GET /__updates/releases/latest[?tag=&status=&draft=&prerelease=&big=&delay_ms=]\n\nusers:\n",
    );
    for u in users::USERS {
        s.push_str(&format!(
            "  {} / {}  ({:?})\n",
            u.username, u.password, u.kind
        ));
    }
    s.push_str(&format!(
        "\nquirks: {}\nmedia: {:?} ({})\n",
        st.quirks,
        st.media.status(),
        st.media.dir().display()
    ));
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], s).into_response()
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "fake-panel: not found").into_response()
}

pub(crate) fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/player_api.php", get(api::player_api))
        .route("/get.php", get(m3u::get_php))
        .route("/xmltv.php", get(epg::xmltv_php))
        .route("/xmltv.xml.gz", get(epg::xmltv_gz))
        .route_layer(middleware::from_fn_with_state(state.clone(), slow));
    Router::new()
        .merge(api)
        .route("/", get(index))
        .route("/nasty.m3u", get(m3u::nasty))
        .route("/images/{kind}/{file}", get(images::handler))
        .route("/live/{u}/{p}/{file}", get(streams::live))
        .route("/movie/{u}/{p}/{file}", get(streams::movie))
        .route("/series/{u}/{p}/{file}", get(streams::series))
        .route(
            "/timeshift/{u}/{p}/{duration}/{start}/{file}",
            get(streams::timeshift),
        )
        .route("/streaming/timeshift.php", get(streams::timeshift_php))
        .route("/hls/{token}/{clip}/{segment}", get(streams::hls_segment))
        .route("/{u}/{p}/{file}", get(streams::live_short))
        .route(
            "/__debug/last-requests",
            get(debug::list).delete(debug::clear),
        )
        .route("/__updates/releases/latest", get(updates::latest))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), debug::record))
        .with_state(state)
}

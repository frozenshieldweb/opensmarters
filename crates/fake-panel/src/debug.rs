//! Request log for tests: `GET /__debug/last-requests` (and [`Handle::requests`](crate::Handle::requests)).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::extract::{MatchedPath, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::server::{AppState, Params};

const LOG_CAP: usize = 2000;

/// Which catch-up `start` format a timeshift request used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StartFormat {
    /// `YYYY-MM-DD:HH-MM` (most clients).
    #[serde(rename = "YYYY-MM-DD:HH-MM")]
    DashMinutes,
    /// `YYYY-MM-DD:HH:MM:SS` (IPTV Smarters).
    #[serde(rename = "YYYY-MM-DD:HH:MM:SS")]
    ColonSeconds,
}

/// Details of a parsed timeshift request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeshiftRequest {
    /// `"path"` for `/timeshift/…`, `"timeshift.php"` for `/streaming/timeshift.php`.
    pub via: String,
    /// Requested stream id.
    pub stream_id: u64,
    /// The `start` value exactly as received (after percent-decoding).
    pub start_raw: String,
    /// Which format `start_raw` matched.
    pub start_format: StartFormat,
    /// Start as server-local `YYYY-MM-DD HH:MM:SS`.
    pub start_local: String,
    /// Start as epoch seconds.
    pub start_timestamp: i64,
    /// Duration as received.
    pub duration_raw: String,
    /// Duration in minutes (may be fractional; the Smarters client does not round).
    pub duration_minutes: f64,
    /// Requested container (`ts` or `m3u8`).
    pub extension: String,
}

/// One logged request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    /// Monotonic sequence number.
    pub seq: u64,
    /// Arrival time, epoch milliseconds.
    pub at_ms: i64,
    /// HTTP method.
    pub method: String,
    /// Raw (still percent-encoded) path.
    pub path: String,
    /// Raw query string.
    pub query: Option<String>,
    /// Matched route pattern, e.g. `/live/{u}/{p}/{file}`.
    pub route: Option<String>,
    /// `User-Agent` header.
    pub user_agent: Option<String>,
    /// `Range` header.
    pub range: Option<String>,
    /// Response status.
    pub status: u16,
    /// Present for successfully parsed timeshift requests.
    pub timeshift: Option<TimeshiftRequest>,
}

/// Bounded in-memory request log.
#[derive(Debug, Default)]
pub(crate) struct RequestLog {
    seq: AtomicU64,
    entries: Mutex<VecDeque<RequestRecord>>,
}

impl RequestLog {
    fn push(&self, mut r: RequestRecord) {
        r.seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let mut e = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if e.len() >= LOG_CAP {
            e.pop_front();
        }
        e.push_back(r);
    }

    pub(crate) fn snapshot(&self) -> Vec<RequestRecord> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    pub(crate) fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn header_str(req: &Request, name: header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Middleware recording every request except `/__debug/*`.
pub(crate) async fn record(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    if path.starts_with("/__debug") {
        return next.run(req).await;
    }
    let method = req.method().to_string();
    let query = req.uri().query().map(str::to_owned);
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let user_agent = header_str(&req, header::USER_AGENT);
    let range = header_str(&req, header::RANGE);
    let at_ms = crate::time::now_ms();
    let res = next.run(req).await;
    tracing::debug!(
        method = %method,
        route = route.as_deref().unwrap_or("<fallback>"),
        status = res.status().as_u16(),
        "request"
    );
    let timeshift = res.extensions().get::<TimeshiftRequest>().cloned();
    st.log.push(RequestRecord {
        seq: 0,
        at_ms,
        method,
        path,
        query,
        route,
        user_agent,
        range,
        status: res.status().as_u16(),
        timeshift,
    });
    res
}

#[derive(Serialize)]
struct ListBody {
    requests: Vec<RequestRecord>,
}

/// `GET /__debug/last-requests[?limit=N][&path_prefix=/timeshift/]`, oldest first.
pub(crate) async fn list(
    State(st): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let p = Params::from_pairs(pairs);
    let mut requests = st.log.snapshot();
    if let Some(prefix) = p.opt("path_prefix") {
        requests.retain(|r| r.path.starts_with(prefix));
    }
    if let Some(limit) = p.opt("limit").and_then(|l| l.parse::<usize>().ok()) {
        let skip = requests.len().saturating_sub(limit);
        requests.drain(..skip);
    }
    Json(ListBody { requests }).into_response()
}

/// `DELETE /__debug/last-requests`.
pub(crate) async fn clear(State(st): State<AppState>) -> StatusCode {
    st.log.clear();
    StatusCode::NO_CONTENT
}

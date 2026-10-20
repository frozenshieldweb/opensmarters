//! Stream endpoints: live (`.ts` / `.m3u8`), movie, series episode, timeshift, HLS segments.
//!
//! Files are served with `tower-http`'s `ServeFile`, so `Range` requests (206) work.
//! Credentials in the path are validated (403 otherwise); unknown ids are 404; missing
//! media is 503 with an explanation.
//!
//! Recording/download quirks: `slow-media` paces film/episode bodies (Range still
//! works), `live-endless` loops a live clip forever at its real bitrate, `media-html` /
//! `media-playlist` answer `200 OK` with something that isn't video.

use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use futures_util::StreamExt as _;
use tower::ServiceExt as _;
use tower_http::services::ServeFile;

use crate::catalogue::LIVE_CLIPS;
use crate::debug::{StartFormat, TimeshiftRequest};
use crate::media::{MediaStatus, is_segment_name};
use crate::rng::{hash_str, mix};
use crate::server::{AppState, Params, State as PanelState};
use crate::time::{local_string, local_to_utc};
use crate::users::{self, TestUser, USERS};

const TS: &str = "video/mp2t";
const HLS: &str = "application/vnd.apple.mpegurl";
/// `slow-media` pace.
const SLOW_BYTES_PER_SEC: f64 = 256.0 * 1024.0;
/// Length of the generated live clips (see `media.rs`), for `live-endless` pacing.
const LIVE_CLIP_SECS: f64 = 20.0;
/// `live-endless` chunk size.
const LIVE_CHUNK: usize = 32 * 1024;

fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, "fake-panel: invalid credentials").into_response()
}

fn not_found(what: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        format!("fake-panel: {what} not found"),
    )
        .into_response()
}

fn split_file(file: &str) -> (&str, &str) {
    file.rsplit_once('.').unwrap_or((file, ""))
}

fn media_gate(st: &PanelState) -> Option<Response> {
    match st.media.status() {
        MediaStatus::Ready => None,
        MediaStatus::Generating => Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                "fake-panel: test media is still being generated with ffmpeg; retry shortly",
            )
                .into_response(),
        ),
        MediaStatus::Unavailable(msg) => Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("fake-panel: test media unavailable: {msg}"),
            )
                .into_response(),
        ),
    }
}

async fn serve_file(path: PathBuf, content_type: &'static str, req: Request) -> Response {
    let res = match ServeFile::new(path).oneshot(req).await {
        Ok(res) => res,
        Err(never) => match never {},
    };
    let mut res = res.map(Body::new);
    if res.status().is_success() {
        res.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    res
}

/// `slow-media`: re-sends `res`'s body at [`SLOW_BYTES_PER_SEC`] (headers untouched, so
/// `206`/`Content-Range` stay valid).
fn throttled(res: Response) -> Response {
    let (parts, body) = res.into_parts();
    let paced = body.into_data_stream().then(|chunk| async move {
        if let Ok(c) = &chunk {
            tokio::time::sleep(Duration::from_secs_f64(c.len() as f64 / SLOW_BYTES_PER_SEC)).await;
        }
        chunk
    });
    Response::from_parts(parts, Body::from_stream(paced))
}

/// `live-endless`: the clip at `path` looped forever at about its real bitrate.
async fn endless_live(path: PathBuf) -> Response {
    let data = match tokio::fs::read(&path).await {
        Ok(d) if !d.is_empty() => Bytes::from(d),
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "fake-panel: live clip unreadable",
            )
                .into_response();
        }
    };
    let rate = (data.len() as f64 / LIVE_CLIP_SECS).max(1.0);
    let stream = futures_util::stream::unfold((data, 0usize), move |(data, pos)| async move {
        let end = (pos + LIVE_CHUNK).min(data.len());
        let chunk = data.slice(pos..end);
        tokio::time::sleep(Duration::from_secs_f64(chunk.len() as f64 / rate)).await;
        let next = if end >= data.len() { 0 } else { end };
        Some((Ok::<_, Infallible>(chunk), (data, next)))
    });
    ([(header::CONTENT_TYPE, TS)], Body::from_stream(stream)).into_response()
}

/// `media-html` / `media-playlist`: a `200 OK` that isn't video, or `None`.
fn bogus_media(st: &PanelState, live: bool) -> Option<Response> {
    if st.quirks.media_html {
        return Some(
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                "<!DOCTYPE html><html><body><h1>Service temporarily unavailable</h1></body></html>",
            )
                .into_response(),
        );
    }
    if st.quirks.media_playlist && !live {
        return Some(
            (
                [(header::CONTENT_TYPE, TS)],
                "#EXTM3U
#EXTINF:-1,Not a film
file:///C:/Windows/win.ini
",
            )
                .into_response(),
        );
    }
    None
}

/// Opaque per-user token used in HLS segment URLs (like real panels' `/hls/<token>/…`).
pub(crate) fn hls_token(st: &PanelState, user: &TestUser) -> String {
    let h = mix(
        st.seed,
        hash_str(&format!("{}\n{}", user.username, user.password)),
    );
    format!("{h:016x}")
}

fn token_user(st: &PanelState, token: &str) -> Option<&'static TestUser> {
    USERS
        .iter()
        .find(|u| u.kind.can_stream() && hls_token(st, u) == token)
}

fn playlist_response(st: &PanelState, user: &TestUser, clip: usize) -> Response {
    let prefix = format!("/hls/{}/{clip}", hls_token(st, user));
    match st.media.playlist(clip, &prefix) {
        Some(body) => ([(header::CONTENT_TYPE, HLS)], body).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "fake-panel: HLS playlist unavailable",
        )
            .into_response(),
    }
}

async fn serve_live(st: &PanelState, u: &str, p: &str, file: &str, req: Request) -> Response {
    let Some(user) = users::find_streamable(u, p) else {
        return forbidden();
    };
    let (stem, ext) = split_file(file);
    let Some(ch) = stem
        .parse()
        .ok()
        .and_then(|id| st.catalogue.live_channel(id))
    else {
        return not_found("channel");
    };
    if !matches!(ext, "" | "ts" | "m3u8") {
        return not_found("container");
    }
    if let Some(r) = media_gate(st) {
        return r;
    }
    if ext == "m3u8" {
        return playlist_response(st, user, ch.clip);
    }
    if let Some(r) = bogus_media(st, true) {
        return r;
    }
    if st.quirks.live_endless {
        return endless_live(st.media.live_ts(ch.clip)).await;
    }
    let res = serve_file(st.media.live_ts(ch.clip), TS, req).await;
    if st.quirks.slow_media {
        throttled(res)
    } else {
        res
    }
}

/// `GET /live/{u}/{p}/{id}.{ts|m3u8}`.
pub(crate) async fn live(
    State(st): State<AppState>,
    Path((u, p, file)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    serve_live(&st, &u, &p, &file, req).await
}

/// `GET /{u}/{p}/{id}` — the short live form some playlists use.
pub(crate) async fn live_short(
    State(st): State<AppState>,
    Path((u, p, file)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    serve_live(&st, &u, &p, &file, req).await
}

fn movie_content_type(ext: &str) -> &'static str {
    match ext {
        "mp4" | "m4v" => "video/mp4",
        "avi" => "video/x-msvideo",
        _ => "video/x-matroska",
    }
}

/// `GET /movie/{u}/{p}/{id}.{ext}` — serves the file matching the movie's own
/// `container_extension` whatever extension was requested (like lenient panels).
pub(crate) async fn movie(
    State(st): State<AppState>,
    Path((u, p, file)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    if users::find_streamable(&u, &p).is_none() {
        return forbidden();
    }
    let (stem, _) = split_file(&file);
    let Some(m) = stem.parse().ok().and_then(|id| st.catalogue.movie(id)) else {
        return not_found("movie");
    };
    if let Some(r) = media_gate(&st) {
        return r;
    }
    if let Some(r) = bogus_media(&st, false) {
        return r;
    }
    let ext = m.container_extension;
    let res = serve_file(st.media.movie_file(ext), movie_content_type(ext), req).await;
    if st.quirks.slow_media {
        throttled(res)
    } else {
        res
    }
}

/// `GET /series/{u}/{p}/{episode_id}.{ext}`.
pub(crate) async fn series(
    State(st): State<AppState>,
    Path((u, p, file)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    if users::find_streamable(&u, &p).is_none() {
        return forbidden();
    }
    let (stem, _) = split_file(&file);
    let Some((_, ep)) = stem.parse().ok().and_then(|id| st.catalogue.episode(id)) else {
        return not_found("episode");
    };
    if let Some(r) = media_gate(&st) {
        return r;
    }
    if let Some(r) = bogus_media(&st, false) {
        return r;
    }
    let ext = ep.container_extension;
    let res = serve_file(st.media.movie_file(ext), movie_content_type(ext), req).await;
    if st.quirks.slow_media {
        throttled(res)
    } else {
        res
    }
}

/// Parse a catch-up start in one of the two accepted formats.
pub(crate) fn parse_start(s: &str) -> Option<(StartFormat, NaiveDateTime)> {
    let s = s.trim();
    // Both formats are fixed-width; checking the length keeps chrono from accepting
    // looser variants (single-digit fields, trailing garbage).
    match s.len() {
        16 => NaiveDateTime::parse_from_str(s, "%Y-%m-%d:%H-%M")
            .ok()
            .map(|d| (StartFormat::DashMinutes, d)),
        19 => NaiveDateTime::parse_from_str(s, "%Y-%m-%d:%H:%M:%S")
            .ok()
            .map(|d| (StartFormat::ColonSeconds, d)),
        _ => None,
    }
}

struct TimeshiftArgs<'a> {
    via: &'static str,
    username: &'a str,
    password: &'a str,
    duration: &'a str,
    start: &'a str,
    stream: &'a str,
    ext: &'a str,
}

async fn timeshift_common(st: &PanelState, a: TimeshiftArgs<'_>, req: Request) -> Response {
    let Some(user) = users::find_streamable(a.username, a.password) else {
        return forbidden();
    };
    let Some(ch) = a
        .stream
        .trim()
        .parse()
        .ok()
        .and_then(|id| st.catalogue.live_channel(id))
    else {
        return not_found("channel");
    };
    let Some((format, local)) = parse_start(a.start) else {
        return (
            StatusCode::BAD_REQUEST,
            "fake-panel: timeshift start must be YYYY-MM-DD:HH-MM or YYYY-MM-DD:HH:MM:SS",
        )
            .into_response();
    };
    let Some(minutes) = a
        .duration
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|m| m.is_finite() && *m > 0.0)
    else {
        return (
            StatusCode::BAD_REQUEST,
            "fake-panel: timeshift duration must be a positive number of minutes",
        )
            .into_response();
    };
    let ext = match a.ext {
        "" | "ts" => "ts",
        "m3u8" => "m3u8",
        _ => return not_found("container"),
    };
    let start_ts = local_to_utc(local);
    let info = TimeshiftRequest {
        via: a.via.to_owned(),
        stream_id: ch.stream_id,
        start_raw: a.start.to_owned(),
        start_format: format,
        start_local: local_string(start_ts),
        start_timestamp: start_ts,
        duration_raw: a.duration.to_owned(),
        duration_minutes: minutes,
        extension: ext.to_owned(),
    };
    let refused = st.quirks.timeshift_503
        || (st.quirks.timeshift_smarters_only && format == StartFormat::DashMinutes);
    let mut res = if !ch.tv_archive {
        not_found("archive for this channel")
    } else if refused {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "fake-panel: timeshift unavailable (quirk)",
        )
            .into_response()
    } else if let Some(r) = media_gate(st) {
        r
    } else if ext == "m3u8" {
        playlist_response(st, user, ch.clip)
    } else {
        serve_file(st.media.live_ts(ch.clip), TS, req).await
    };
    res.extensions_mut().insert(info);
    res
}

/// `GET /timeshift/{u}/{p}/{duration}/{start}/{id}.{ts|m3u8}`.
pub(crate) async fn timeshift(
    State(st): State<AppState>,
    Path((u, p, duration, start, file)): Path<(String, String, String, String, String)>,
    req: Request,
) -> Response {
    let (stem, ext) = split_file(&file);
    let args = TimeshiftArgs {
        via: "path",
        username: &u,
        password: &p,
        duration: &duration,
        start: &start,
        stream: stem,
        ext,
    };
    timeshift_common(&st, args, req).await
}

/// `GET /streaming/timeshift.php?username=&password=&stream=&start=&duration=`.
pub(crate) async fn timeshift_php(
    State(st): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
    req: Request,
) -> Response {
    let q = Params::from_pairs(pairs);
    let args = TimeshiftArgs {
        via: "timeshift.php",
        username: q.get("username"),
        password: q.get("password"),
        duration: q.get("duration"),
        start: q.get("start"),
        stream: q.get("stream"),
        ext: q.opt("extension").unwrap_or("ts"),
    };
    timeshift_common(&st, args, req).await
}

/// `GET /hls/{token}/{clip}/{segment}`.
pub(crate) async fn hls_segment(
    State(st): State<AppState>,
    Path((token, clip, segment)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    if token_user(&st, &token).is_none() {
        return forbidden();
    }
    let Some(clip) = clip.parse::<usize>().ok().filter(|c| *c < LIVE_CLIPS) else {
        return not_found("clip");
    };
    if !is_segment_name(&segment) {
        return not_found("segment");
    }
    if let Some(r) = media_gate(&st) {
        return r;
    }
    serve_file(st.media.hls_segment(clip, &segment), TS, req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_formats() {
        assert_eq!(
            parse_start("2026-10-20:14-30").map(|x| x.0),
            Some(StartFormat::DashMinutes)
        );
        assert_eq!(
            parse_start("2026-10-20:14:30:00").map(|x| x.0),
            Some(StartFormat::ColonSeconds)
        );
        assert!(parse_start("2026-10-20 14:30:00").is_none());
        assert!(parse_start("2026-10-20:14:30").is_none());
        assert!(parse_start("1729430000").is_none());
    }
}

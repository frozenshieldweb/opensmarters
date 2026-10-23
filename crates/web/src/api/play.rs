// SPDX-License-Identifier: GPL-3.0-or-later
//! Playback: `POST /api/play`, the HLS files, `DELETE /api/streams/{sid}`.
//!
//! Catch-up: panels accept one or both of the two timeshift start formats
//! (`docs/PROTOCOL.md`, "Catch-up"). The account's remembered format is tried first; when
//! the provider refuses it before sending any video, the other one is tried once, and the
//! one that worked is remembered for the account. When both are refused the programme
//! isn't available right now (some panels refuse every catch-up link).

use std::time::Duration;

use axum::Json;
use axum::body::Body as HttpBody;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use opensmarters_core::XtreamAccount;
use opensmarters_core::url::Url;
use opensmarters_core::urls::{LiveContainer, StreamUrls, TimeshiftFormat};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Body, empty, provider, settings};
use crate::auth::CurrentUser;
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::provider::{Creds, Provider};
use crate::state::AppState;
use crate::streams::{StartSpec, Started, valid_file};

#[derive(Deserialize)]
pub struct PlayBody {
    account: String,
    kind: String,
    id: String,
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    catchup_start: Option<i64>,
    #[serde(default)]
    catchup_minutes: Option<u32>,
    #[serde(default)]
    hevc: bool,
    /// Optional extension: the series of an episode (else it is found in the cached
    /// series details).
    #[serde(default)]
    series_id: Option<String>,
}

struct Target {
    url: Url,
    user_agent: Option<String>,
    live: bool,
    duration: Option<f64>,
    /// Catch-up: the format of `url` and the same programme in the other format.
    catchup: Option<Catchup>,
    /// Seconds of the programme the provider's link already skips (catch-up starts
    /// at a later minute instead of transcoding from the beginning).
    skipped: f64,
}

struct Catchup {
    format: TimeshiftFormat,
    other: Url,
}

/// The stored name of a timeshift format.
fn format_name(f: TimeshiftFormat) -> &'static str {
    match f {
        TimeshiftFormat::Standard => "standard",
        TimeshiftFormat::SmartersCompat => "smarters",
    }
}

/// The account's remembered timeshift format (the standard one by default).
fn remembered(name: Option<&str>) -> TimeshiftFormat {
    match name {
        Some("smarters") => TimeshiftFormat::SmartersCompat,
        _ => TimeshiftFormat::Standard,
    }
}

fn other_format(f: TimeshiftFormat) -> TimeshiftFormat {
    match f {
        TimeshiftFormat::Standard => TimeshiftFormat::SmartersCompat,
        TimeshiftFormat::SmartersCompat => TimeshiftFormat::Standard,
    }
}

fn catchup_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "provider_error",
        "This programme isn't available from your provider right now.",
    )
}

fn no_such(what: &str) -> ApiError {
    ApiError::not_found(format!("This provider has no such {what}."))
}

fn parse_id(id: &str, what: &str) -> ApiResult<u64> {
    id.trim().parse().map_err(|_| no_such(what))
}

async fn xtream_urls(
    p: &Provider,
) -> ApiResult<(StreamUrls, std::sync::Arc<opensmarters_core::LoginInfo>)> {
    let login = p.login().await?;
    let Creds::Xtream {
        server,
        username,
        password,
    } = p.creds()?
    else {
        return Err(ApiError::bad_request(
            "This provider isn't an Xtream Codes account.",
        ));
    };
    let account = XtreamAccount::from_parts(server, username);
    Ok((StreamUrls::new(&login, &account, password), login))
}

fn m3u_url(url: &str) -> ApiResult<Url> {
    Url::parse(url.trim()).map_err(|_| no_such("stream"))
}

async fn target(
    state: &AppState,
    p: &Provider,
    user_id: &str,
    body: &PlayBody,
) -> ApiResult<Target> {
    let unsupported =
        || ApiError::bad_request("This kind of playback isn't available for this provider.");
    match (body.kind.as_str(), p.is_xtream()) {
        ("live", true) => {
            let sid = parse_id(&body.id, "channel")?;
            let (urls, login) = xtream_urls(p).await?;
            let format = settings::load(state, user_id).await?.live_format;
            let preferred = if format == "m3u8" {
                LiveContainer::M3u8
            } else {
                LiveContainer::Ts
            };
            let container = LiveContainer::preferred(preferred, &login.user_info);
            Ok(Target {
                url: urls.live(sid, container),
                user_agent: None,
                live: true,
                duration: None,
                catchup: None,
                skipped: 0.0,
            })
        }
        ("live", false) | ("movie", false) => {
            let data = p.m3u().await?;
            let entry = if body.kind == "live" {
                data.live.get(&body.id).and_then(|c| c.entry)
            } else {
                data.movies
                    .iter()
                    .find(|m| m.id == body.id)
                    .and_then(|m| m.entry)
            };
            let entry = entry
                .and_then(|i| data.playlist.entries.get(i))
                .ok_or_else(|| {
                    no_such(if body.kind == "live" {
                        "channel"
                    } else {
                        "film"
                    })
                })?;
            Ok(Target {
                url: m3u_url(&entry.url)?,
                user_agent: entry.user_agent.clone(),
                live: body.kind == "live",
                duration: entry.duration.filter(|d| *d > 0.0 && body.kind != "live"),
                catchup: None,
                skipped: 0.0,
            })
        }
        ("movie", true) => {
            let vod_id = parse_id(&body.id, "film")?;
            let (urls, _) = xtream_urls(p).await?;
            let info = p.vod_info(vod_id).await.ok();
            let ext = p.peek_movie(&body.id).and_then(|m| m.ext).or_else(|| {
                info.as_ref()
                    .and_then(|i| i.movie_data.as_ref()?.container_extension.clone())
            });
            Ok(Target {
                url: urls.movie(vod_id, ext.as_deref()),
                user_agent: None,
                live: false,
                duration: info.and_then(|i| i.info.duration_secs).map(|d| d as f64),
                catchup: None,
                skipped: 0.0,
            })
        }
        ("episode", true) => {
            let episode_id = parse_id(&body.id, "episode")?;
            let (urls, _) = xtream_urls(p).await?;
            let info = match body
                .series_id
                .as_deref()
                .and_then(|s| s.trim().parse::<u64>().ok())
            {
                Some(series) => p.series_info(series).await.ok(),
                None => p.peek_episode_series(episode_id).map(|(_, info)| info),
            };
            let episode = info.as_ref().and_then(|i| i.episode(episode_id));
            Ok(Target {
                url: urls.episode(
                    episode_id,
                    episode.and_then(|e| e.container_extension.as_deref()),
                ),
                user_agent: None,
                live: false,
                duration: episode.and_then(|e| e.info.duration_secs).map(|d| d as f64),
                catchup: None,
                skipped: 0.0,
            })
        }
        ("catchup", true) => {
            let sid = parse_id(&body.id, "channel")?;
            let start = body
                .catchup_start
                .ok_or_else(|| ApiError::bad_request("Catch-up needs catchup_start."))?;
            let minutes = body
                .catchup_minutes
                .filter(|m| (1..=24 * 60).contains(m))
                .ok_or_else(|| {
                    ApiError::bad_request("Catch-up needs catchup_minutes (1 to 1440).")
                })?;
            let start_utc = chrono::DateTime::from_timestamp(start, 0)
                .ok_or_else(|| ApiError::bad_request("catchup_start isn't a valid time."))?;
            let (urls, login) = xtream_urls(p).await?;
            // The panel's own wall-clock start of the programme when the schedule has it
            // (catch-up lists and the guide load it), else the start in its time zone.
            let local = match p.table(sid).await {
                Ok(t) => t.local_start(start),
                Err(_) => None,
            }
            .unwrap_or_else(|| login.time_zone().to_local(start_utc));
            // Seeking ahead asks the panel for the archive from a later minute, so the
            // server doesn't download and skip everything before it.
            let offset = position(body.start);
            let skip_min = ((offset / 60.0).floor() as u32).min(minutes.saturating_sub(1));
            let at = local + chrono::Duration::minutes(i64::from(skip_min));
            let left = minutes - skip_min;
            let format = remembered(p.row.timeshift_format.as_deref());
            let link = |f| urls.timeshift_at(sid, at, left, f, LiveContainer::Ts);
            Ok(Target {
                url: link(format),
                user_agent: None,
                live: false,
                duration: Some(f64::from(minutes) * 60.0),
                catchup: Some(Catchup {
                    format,
                    other: link(other_format(format)),
                }),
                skipped: f64::from(skip_min) * 60.0,
            })
        }
        ("episode" | "catchup", false) => Err(unsupported()),
        _ => Err(ApiError::bad_request(
            "The kind must be live, movie, episode or catchup.",
        )),
    }
}

/// A requested start position (seconds), or 0.
fn position(start: Option<f64>) -> f64 {
    start
        .filter(|s| s.is_finite() && *s > 0.0 && *s < 7.0 * 24.0 * 3600.0)
        .unwrap_or(0.0)
}

/// `POST /api/play`.
pub async fn play(
    State(state): State<AppState>,
    user: CurrentUser,
    Body(body): Body<PlayBody>,
) -> ApiResult<Json<Value>> {
    if body.id.is_empty() || body.id.len() > 128 {
        return Err(ApiError::bad_request("The item id isn't valid."));
    }
    let p = provider(&state, &user.user.id, &body.account).await?;
    let target = target(&state, &p, &user.user.id, &body).await?;
    let provider_key = match (&p.row.server, &p.row.username) {
        (Some(s), Some(u)) if p.is_xtream() => format!("xtream:{s}\n{u}"),
        _ => format!("account:{}", p.row.id),
    };
    let provider_limit = p
        .cache()
        .peek::<opensmarters_core::LoginInfo>("login")
        .and_then(|l| l.user_info.max_connections)
        .or_else(|| p.row.max_connections.and_then(|m| u32::try_from(m).ok()));
    let start = if target.live {
        0.0
    } else {
        position(body.start)
    };
    let spec = |url: Url| StartSpec {
        user_id: user.user.id.clone(),
        session: user.session,
        account_id: p.row.id.clone(),
        provider_key: provider_key.clone(),
        provider_limit,
        url,
        user_agent: target.user_agent.clone(),
        live: target.live,
        duration_hint: if target.skipped > 0.0 {
            target.duration.map(|d| (d - target.skipped).max(1.0))
        } else {
            target.duration
        },
        start: start - target.skipped,
        hevc: body.hevc,
    };
    let mut started = match &target.catchup {
        None => {
            state
                .streams
                .start(&state.net, spec(target.url.clone()))
                .await?
        }
        Some(c) => {
            catchup(
                &state,
                &p,
                &user,
                c,
                spec(target.url.clone()),
                spec(c.other.clone()),
            )
            .await?
        }
    };
    if target.skipped > 0.0 {
        started.start += target.skipped;
        started.duration = target.duration;
    }
    Ok(Json(json!({
        "stream": started.id,
        "url": format!("/api/streams/{}/index.m3u8", started.id),
        "live": started.live,
        "duration": started.duration,
        "start": started.start,
    })))
}

/// Starts a catch-up stream in the remembered format, falling back once to the other
/// format when the provider refuses before sending any video, and remembers the one
/// that worked for the account.
async fn catchup(
    state: &AppState,
    p: &Provider,
    user: &CurrentUser,
    c: &Catchup,
    first: StartSpec,
    second: StartSpec,
) -> ApiResult<Started> {
    let (started, format) = match state.streams.start_detailed(&state.net, first).await {
        Ok(s) => (s, c.format),
        Err(e) if e.refused => {
            let other = other_format(c.format);
            tracing::info!(
                account = %p.row.id,
                refused = format_name(c.format),
                trying = format_name(other),
                "catch-up link refused; trying the other format"
            );
            match state.streams.start_detailed(&state.net, second).await {
                Ok(s) => (s, other),
                Err(e) if e.refused => return Err(catchup_unavailable()),
                Err(e) => return Err(e.error),
            }
        }
        Err(e) => return Err(e.error),
    };
    if remembered(p.row.timeshift_format.as_deref()) != format {
        // Best effort: the stream already plays.
        let (u, a, f) = (user.user.id.clone(), p.row.id.clone(), format_name(format));
        if let Err(e) = state
            .db
            .call(move |conn| db::set_timeshift_format(conn, &u, &a, f))
            .await
        {
            tracing::warn!(error = %e, "couldn't remember the catch-up format");
        }
    }
    Ok(started)
}

fn owned_stream(
    state: &AppState,
    user: &CurrentUser,
    sid: &str,
) -> ApiResult<std::sync::Arc<crate::streams::Stream>> {
    state
        .streams
        .get(sid)
        .filter(|s| s.user_id == user.user.id)
        .ok_or_else(|| ApiError::not_found("This stream has ended."))
}

/// `GET /api/streams/{sid}/{file}` (owner only, `no-store`).
pub async fn file(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((sid, file)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !valid_file(&file) {
        return Err(ApiError::not_found("No such file."));
    }
    let stream = owned_stream(&state, &user, &sid)?;
    stream.note_request(&file);
    let path = stream.dir.join(&file);
    let mut bytes = tokio::fs::read(&path).await.ok();
    if bytes.is_none() && file == "index.m3u8" {
        // The first playlist may still be on its way.
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            bytes = tokio::fs::read(&path).await.ok();
            if bytes.is_some() {
                break;
            }
        }
    }
    let bytes = bytes.ok_or_else(|| ApiError::not_found("No such file."))?;
    let mime = if file.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else {
        "video/mp4"
    };
    let mut res = Response::new(HttpBody::from(bytes));
    *res.status_mut() = StatusCode::OK;
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// `DELETE /api/streams/{sid}`.
pub async fn stop(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(sid): Path<String>,
) -> ApiResult<Json<Value>> {
    let stream = owned_stream(&state, &user, &sid)?;
    state.streams.stop(&stream.id).await;
    Ok(empty())
}

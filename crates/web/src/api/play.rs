// SPDX-License-Identifier: GPL-3.0-or-later
//! Playback: `POST /api/play`, the HLS files, `DELETE /api/streams/{sid}`.

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
use crate::error::{ApiError, ApiResult};
use crate::provider::{Creds, Provider};
use crate::state::AppState;
use crate::streams::{StartSpec, valid_file};

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
            let local = login.time_zone().to_local(start_utc);
            Ok(Target {
                url: urls.timeshift_at(
                    sid,
                    local,
                    minutes,
                    TimeshiftFormat::Standard,
                    LiveContainer::Ts,
                ),
                user_agent: None,
                live: false,
                duration: Some(f64::from(minutes) * 60.0),
            })
        }
        ("episode" | "catchup", false) => Err(unsupported()),
        _ => Err(ApiError::bad_request(
            "The kind must be live, movie, episode or catchup.",
        )),
    }
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
    let start = body
        .start
        .filter(|s| s.is_finite() && *s > 0.0 && *s < 7.0 * 24.0 * 3600.0)
        .unwrap_or(0.0);
    let spec = StartSpec {
        user_id: user.user.id.clone(),
        session: user.session,
        account_id: p.row.id.clone(),
        provider_key,
        provider_limit,
        url: target.url,
        user_agent: target.user_agent,
        live: target.live,
        duration_hint: target.duration,
        start: if target.live { 0.0 } else { start },
        hevc: body.hevc,
    };
    let started = state.streams.start(&state.net, spec).await?;
    Ok(Json(json!({
        "stream": started.id,
        "url": format!("/api/streams/{}/index.m3u8", started.id),
        "live": started.live,
        "duration": started.duration,
        "start": started.start,
    })))
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

// SPDX-License-Identifier: GPL-3.0-or-later
//! Favourites, resume points and Continue watching (per provider account).
//!
//! Each row keeps a sealed *snapshot* of what the item looked like (name, artwork
//! address, …) so these lists show without downloading the provider's full film and
//! series lists; the live lists win when they are already loaded.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::catalog::{channel_json, movie_json, show_json};
use super::{Body, empty, img, provider};
use crate::auth::CurrentUser;
use crate::db::{self, Progress};
use crate::error::{ApiError, ApiResult};
use crate::provider::{Channel, Movie, Provider, Show};
use crate::state::AppState;
use crate::text;

/// Favourites per account.
const MAX_FAVOURITES: i64 = 5000;

/// What a favourite or resume point remembers about its item.
#[derive(Serialize, Deserialize, Default, Clone)]
struct Snapshot {
    #[serde(default)]
    name: String,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    archive_days: Option<u32>,
    #[serde(default)]
    year: Option<i32>,
    #[serde(default)]
    rating: Option<f64>,
    #[serde(default)]
    date: Option<i64>,
}

fn seal(state: &AppState, account: &str, kind: &str, item: &str, s: &Snapshot) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(s).ok()?;
    state.keys.seal_snapshot(account, kind, item, &json).ok()
}

fn open(
    state: &AppState,
    account: &str,
    kind: &str,
    item: &str,
    sealed: Option<&[u8]>,
) -> Option<Snapshot> {
    let plain = state
        .keys
        .open_snapshot(account, kind, item, sealed?)
        .ok()?;
    serde_json::from_slice(&plain).ok()
}

fn valid_item(item: &str) -> bool {
    !item.is_empty()
        && item.len() <= 128
        && !item.contains("://")
        && !item.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn channel_snapshot(c: &Channel) -> Snapshot {
    Snapshot {
        name: c.name.clone(),
        image: c.logo.clone(),
        number: c.number,
        categories: c.categories.clone(),
        archive_days: c.archive_days,
        ..Snapshot::default()
    }
}

fn movie_snapshot(m: &Movie) -> Snapshot {
    Snapshot {
        name: m.name.clone(),
        image: m.poster.clone(),
        year: m.year,
        rating: m.rating,
        date: m.added,
        ..Snapshot::default()
    }
}

fn show_snapshot(s: &Show) -> Snapshot {
    Snapshot {
        name: s.name.clone(),
        image: s.poster.clone(),
        year: s.year,
        rating: s.rating,
        date: s.updated,
        ..Snapshot::default()
    }
}

/// A film's snapshot: a cached list, else `get_vod_info`.
async fn find_movie(p: &Provider, item: &str) -> ApiResult<Option<Snapshot>> {
    if let Some(m) = p.peek_movie(item) {
        return Ok(Some(movie_snapshot(&m)));
    }
    if !p.is_xtream() {
        return Ok(p
            .movies(None)
            .await?
            .iter()
            .find(|m| m.id == item)
            .map(movie_snapshot));
    }
    let Ok(id) = item.parse::<u64>() else {
        return Ok(None);
    };
    let info = p.vod_info(id).await?;
    let Some(name) = text::opt_line(info.info.name.as_deref(), text::MAX_NAME) else {
        return Ok(None);
    };
    Ok(Some(Snapshot {
        year: text::year(info.info.release_date.as_deref(), &name),
        name,
        image: info.info.movie_image.clone(),
        rating: text::rating10(info.info.rating, None),
        ..Snapshot::default()
    }))
}

/// A series' snapshot: a cached list, else `get_series_info`.
async fn find_show(p: &Provider, item: &str) -> ApiResult<Option<Snapshot>> {
    if let Some(s) = p.peek_show(item) {
        return Ok(Some(show_snapshot(&s)));
    }
    let Ok(id) = item.parse::<u64>() else {
        return Ok(None);
    };
    if !p.is_xtream() {
        return Ok(None);
    }
    let info = p.series_info(id).await?;
    let Some(name) = text::opt_line(info.info.name.as_deref(), text::MAX_NAME) else {
        return Ok(None);
    };
    Ok(Some(Snapshot {
        year: text::year(info.info.release_date.as_deref(), &name),
        name,
        image: info.info.cover.clone(),
        rating: text::rating10(info.info.rating, info.info.rating_5based),
        date: info.info.last_modified.map(|t| t.timestamp()),
        ..Snapshot::default()
    }))
}

/// `GET …/favourites` → `{channels, movies, series}`.
pub async fn favourites(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let a = p.row.id.clone();
    let favs = state.db.call(move |c| db::list_favourites(c, &a)).await?;
    let live = if favs.iter().any(|f| f.kind == "channel") {
        p.live().await.ok()
    } else {
        None
    };
    let (mut channels, mut movies, mut series) = (Vec::new(), Vec::new(), Vec::new());
    for f in &favs {
        let snap = || open(&state, &p.row.id, &f.kind, &f.item, f.snapshot.as_deref());
        match f.kind.as_str() {
            "channel" => {
                if let Some(c) = live.as_ref().and_then(|l| l.get(&f.item)) {
                    channels.push(channel_json(&state, c));
                } else if let Some(s) = snap() {
                    channels.push(json!({
                        "key": f.item, "name": s.name, "number": s.number,
                        "logo": img(&state, s.image.as_deref()), "categories": s.categories,
                        "archive_days": s.archive_days,
                    }));
                }
            }
            "movie" => {
                if let Some(m) = p.peek_movie(&f.item) {
                    movies.push(movie_json(&state, &m));
                } else if let Some(s) = snap() {
                    movies.push(json!({
                        "id": f.item, "name": s.name, "poster": img(&state, s.image.as_deref()),
                        "year": s.year, "rating": s.rating, "added": s.date,
                    }));
                }
            }
            "series" => {
                if let Some(sh) = p.peek_show(&f.item) {
                    series.push(show_json(&state, &sh));
                } else if let Some(s) = snap() {
                    series.push(json!({
                        "id": f.item, "name": s.name, "poster": img(&state, s.image.as_deref()),
                        "year": s.year, "rating": s.rating, "updated": s.date,
                    }));
                }
            }
            _ => {}
        }
    }
    Ok(Json(
        json!({"channels": channels, "movies": movies, "series": series}),
    ))
}

fn kind_of(kind: &str) -> ApiResult<&'static str> {
    match kind {
        "channel" => Ok("channel"),
        "movie" => Ok("movie"),
        "series" => Ok("series"),
        _ => Err(ApiError::bad_request(
            "The kind must be channel, movie or series.",
        )),
    }
}

/// `PUT …/favourites/{kind}/{item}`.
pub async fn add_favourite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, kind, item)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let kind = kind_of(&kind)?;
    if !valid_item(&item) {
        return Err(ApiError::bad_request("That item id isn't valid."));
    }
    let p = provider(&state, &user.user.id, &id).await?;
    let snapshot = match kind {
        "channel" => p.live().await?.get(&item).map(channel_snapshot),
        "movie" => find_movie(&p, &item).await?,
        _ => find_show(&p, &item).await?,
    }
    .ok_or_else(|| ApiError::not_found("This provider has no such item."))?;
    let sealed = seal(&state, &p.row.id, kind, &item, &snapshot);
    let a = p.row.id.clone();
    let added = state
        .db
        .call(move |c| {
            if db::favourite_count(c, &a)? >= MAX_FAVOURITES {
                return Ok(false);
            }
            db::put_favourite(c, &a, kind, &item, sealed.as_deref())?;
            Ok(true)
        })
        .await?;
    if !added {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "too_many_favourites",
            "You have too many favourites for this provider. Remove some first.",
        ));
    }
    Ok(empty())
}

/// `DELETE …/favourites/{kind}/{item}`.
pub async fn remove_favourite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, kind, item)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let kind = kind_of(&kind)?;
    let p = provider(&state, &user.user.id, &id).await?;
    let a = p.row.id.clone();
    state
        .db
        .call(move |c| db::delete_favourite(c, &a, kind, &item))
        .await?;
    Ok(empty())
}

/// `GET …/continue` → newest first, at most 20, unfinished only.
pub async fn continue_watching(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let a = p.row.id.clone();
    let rows = state
        .db
        .call(move |c| db::continue_watching(c, &a, 20))
        .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let s = open(&state, &p.row.id, &r.kind, &r.item, r.snapshot.as_deref())
                .unwrap_or_default();
            json!({
                "kind": r.kind,
                "id": r.item,
                "series_id": r.series_id,
                "name": s.name,
                "poster": img(&state, s.image.as_deref()),
                "position": r.position,
                "duration": r.duration,
                "updated": r.updated_at,
            })
        })
        .collect();
    Ok(Json(Value::Array(out)))
}

#[derive(Deserialize)]
pub struct ProgressBody {
    kind: String,
    id: String,
    #[serde(default)]
    series_id: Option<String>,
    position: f64,
    duration: f64,
}

/// An episode's snapshot: "Series · S1 E2 · Title" with the series' poster.
async fn find_episode(p: &Provider, series_id: &str, episode: &str) -> ApiResult<Option<Snapshot>> {
    let (Ok(sid), Ok(eid)) = (series_id.parse::<u64>(), episode.parse::<u64>()) else {
        return Ok(None);
    };
    if !p.is_xtream() {
        return Ok(None);
    }
    let info = p.series_info(sid).await?;
    let show = p.peek_show(series_id);
    let series_name = text::opt_line(info.info.name.as_deref(), text::MAX_NAME)
        .or_else(|| show.as_ref().map(|s| s.name.clone()))
        .unwrap_or_default();
    let Some((season, ep)) = info
        .episodes
        .iter()
        .find_map(|(season, eps)| eps.iter().find(|e| e.id == eid).map(|e| (*season, e)))
    else {
        return Ok(None);
    };
    let title = text::line(&ep.title, text::MAX_NAME);
    let mut name = series_name;
    let tag = match ep.episode_num {
        Some(n) => format!("S{season} E{n}"),
        None => format!("S{season}"),
    };
    for part in [tag, title] {
        if !part.is_empty() && !name.contains(&part) {
            if !name.is_empty() {
                name.push_str(" · ");
            }
            name.push_str(&part);
        }
    }
    let image = info
        .info
        .cover
        .clone()
        .or_else(|| show.and_then(|s| s.poster))
        .or_else(|| ep.info.movie_image.clone());
    Ok(Some(Snapshot {
        name,
        image,
        ..Snapshot::default()
    }))
}

/// `PUT …/progress` (≥ 95 % counts as finished and leaves Continue watching).
pub async fn save_progress(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Body(body): Body<ProgressBody>,
) -> ApiResult<Json<Value>> {
    let kind: &'static str = match body.kind.as_str() {
        "movie" => "movie",
        "episode" => "episode",
        _ => return Err(ApiError::bad_request("The kind must be movie or episode.")),
    };
    if !valid_item(&body.id) || body.series_id.as_deref().is_some_and(|s| !valid_item(s)) {
        return Err(ApiError::bad_request("That item id isn't valid."));
    }
    let sane = |v: f64| v.is_finite() && (0.0..=7.0 * 24.0 * 3600.0).contains(&v);
    if !sane(body.position) || !sane(body.duration) || body.duration <= 0.0 {
        return Err(ApiError::bad_request(
            "The position and duration must be seconds.",
        ));
    }
    if kind == "episode" && body.series_id.is_none() {
        return Err(ApiError::bad_request("An episode needs its series_id."));
    }
    let p = provider(&state, &user.user.id, &id).await?;
    let (a, k, i) = (p.row.id.clone(), kind, body.id.clone());
    let existing = state
        .db
        .call(move |c| db::get_progress(c, &a, k, &i))
        .await?;
    // The snapshot is looked up once (when the item is first saved).
    let snapshot = if existing.as_ref().is_some_and(|e| e.snapshot.is_some()) {
        None
    } else {
        let found = match kind {
            "movie" => find_movie(&p, &body.id).await.ok().flatten(),
            _ => find_episode(&p, body.series_id.as_deref().unwrap_or_default(), &body.id)
                .await
                .ok()
                .flatten(),
        };
        found.and_then(|s| seal(&state, &p.row.id, kind, &body.id, &s))
    };
    let position = body.position.min(body.duration);
    let row = Progress {
        kind: kind.to_owned(),
        item: body.id,
        series_id: body.series_id,
        position,
        duration: body.duration,
        finished: position >= 0.95 * body.duration,
        snapshot,
        updated_at: db::now(),
    };
    let a = p.row.id.clone();
    state
        .db
        .call(move |c| db::put_progress(c, &a, &row))
        .await?;
    Ok(empty())
}

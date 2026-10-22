// SPDX-License-Identifier: GPL-3.0-or-later
//! Catalogue endpoints (`/api/accounts/{id}/…`): live TV, EPG, films, series, search.
//! Every string is provider text cleaned by [`crate::text`]; every image address is an
//! `/api/img/<token>` of this server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use opensmarters_core::catalog::SearchQuery;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Q, img, provider};
use crate::auth::CurrentUser;
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::provider::{Channel, Movie, Programme, Provider, Show, matches, now_next};
use crate::state::AppState;
use crate::text;

/// The API's `Channel`.
pub fn channel_json(state: &AppState, c: &Channel) -> Value {
    json!({
        "key": c.key,
        "name": c.name,
        "number": c.number,
        "logo": img(state, c.logo.as_deref()),
        "categories": c.categories,
        "archive_days": c.archive_days,
    })
}

/// The API's `Movie`.
pub fn movie_json(state: &AppState, m: &Movie) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "poster": img(state, m.poster.as_deref()),
        "year": m.year,
        "rating": m.rating,
        "added": m.added,
    })
}

/// The API's `Series`.
pub fn show_json(state: &AppState, s: &Show) -> Value {
    json!({
        "id": s.id,
        "name": s.name,
        "poster": img(state, s.poster.as_deref()),
        "year": s.year,
        "rating": s.rating,
        "updated": s.updated,
    })
}

#[derive(Deserialize)]
pub struct CategoryQuery {
    #[serde(default)]
    category: Option<String>,
}

fn category_filter(q: &CategoryQuery) -> Option<&str> {
    q.category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty() && *c != "all")
}

/// `GET …/live/categories`.
pub async fn live_categories(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let live = p.live().await?;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for c in &live.channels {
        for cat in &c.categories {
            *counts.entry(cat.as_str()).or_default() += 1;
        }
    }
    Ok(Json(Value::Array(
        live.categories
            .iter()
            .map(|c| json!({"id": c.id, "name": c.name, "count": counts.get(c.id.as_str()).copied().unwrap_or(0)}))
            .collect(),
    )))
}

async fn favourite_ids(
    state: &AppState,
    account_id: &str,
    kind: &'static str,
) -> ApiResult<Vec<String>> {
    let a = account_id.to_owned();
    let favs = state.db.call(move |c| db::list_favourites(c, &a)).await?;
    Ok(favs
        .into_iter()
        .filter(|f| f.kind == kind)
        .map(|f| f.item)
        .collect())
}

/// `GET …/live/channels?category=<id>|all|favourites`.
pub async fn live_channels(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Q(q): Q<CategoryQuery>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let live = p.live().await?;
    let out: Vec<Value> = match category_filter(&q) {
        None => live
            .channels
            .iter()
            .map(|c| channel_json(&state, c))
            .collect(),
        Some("favourites") => favourite_ids(&state, &p.row.id, "channel")
            .await?
            .iter()
            .filter_map(|k| live.get(k))
            .map(|c| channel_json(&state, c))
            .collect(),
        Some(cat) => live
            .channels
            .iter()
            .filter(|c| c.categories.iter().any(|x| x == cat))
            .map(|c| channel_json(&state, c))
            .collect(),
    };
    Ok(Json(Value::Array(out)))
}

#[derive(Deserialize)]
pub struct KeysQuery {
    #[serde(default)]
    keys: String,
}

/// Rows of an Xtream channel: the short EPG, falling back to nothing on errors.
async fn xtream_rows(p: &Provider, stream_id: u64) -> Option<Arc<Vec<Programme>>> {
    p.short_epg(stream_id).await.ok()
}

/// `GET …/epg/now?keys=k1,k2,…` (≤ 200) → `{key: [now, next]}`. Keys whose guide
/// couldn't be loaded within the time limit are left out (ask again later); keys
/// without a guide get `[]`.
pub async fn epg_now(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Q(q): Q<KeysQuery>,
) -> ApiResult<Json<Value>> {
    let keys: Vec<&str> = q
        .keys
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();
    if keys.len() > 200 {
        return Err(ApiError::bad_request(
            "Ask for at most 200 channels at a time.",
        ));
    }
    let p = provider(&state, &user.user.id, &id).await?;
    let live = p.live().await?;
    let now = db::now();
    let mut out = serde_json::Map::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    if p.is_xtream() {
        let wanted: Vec<(&str, u64)> = keys
            .iter()
            .filter_map(|k| live.get(k).and_then(|c| c.stream_id).map(|s| (*k, s)))
            .collect();
        let results = futures_util::future::join_all(wanted.iter().map(|(k, sid)| {
            let p = &p;
            async move {
                (
                    *k,
                    tokio::time::timeout_at(deadline, xtream_rows(p, *sid)).await,
                )
            }
        }))
        .await;
        for (k, r) in results {
            // A key whose request failed or timed out is left out (ask again later).
            if let Ok(Some(rows)) = r {
                let rows = now_next(&rows, now);
                out.insert(
                    k.to_owned(),
                    serde_json::to_value(rows).unwrap_or(Value::Null),
                );
            }
        }
    } else if let Ok(Ok(guide)) = tokio::time::timeout_at(deadline, p.guide()).await {
        for k in keys {
            if let Some(c) = live.get(k) {
                let rows = c
                    .epg_id
                    .as_deref()
                    .map(|e| now_next(guide.programmes(e), now))
                    .unwrap_or_default();
                out.insert(
                    k.to_owned(),
                    serde_json::to_value(rows).unwrap_or(Value::Null),
                );
            }
        }
    }
    Ok(Json(Value::Object(out)))
}

#[derive(Deserialize)]
pub struct RangeQuery {
    #[serde(default)]
    from: Option<i64>,
    #[serde(default)]
    to: Option<i64>,
}

/// `GET …/epg/channel/{key}?from=<ts>&to=<ts>` (≤ 48 h; default: the next 12 hours).
pub async fn epg_channel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, key)): Path<(String, String)>,
    Q(q): Q<RangeQuery>,
) -> ApiResult<Json<Value>> {
    let now = db::now();
    let from = q.from.unwrap_or(now - 1800);
    let to = q.to.unwrap_or(from + 12 * 3600);
    if to <= from || to - from > 48 * 3600 {
        return Err(ApiError::bad_request(
            "The time range must be at most 48 hours.",
        ));
    }
    let p = provider(&state, &user.user.id, &id).await?;
    let live = p.live().await?;
    let channel = live
        .get(&key)
        .ok_or_else(|| ApiError::not_found("No such channel."))?;
    let rows: Vec<Programme> = if let Some(sid) = channel.stream_id {
        let mut rows = p
            .schedule(sid)
            .await
            .map(|r| r.to_vec())
            .unwrap_or_default();
        if rows.is_empty() {
            rows = p
                .short_epg(sid)
                .await
                .map(|r| r.to_vec())
                .unwrap_or_default();
        }
        rows
    } else {
        match (&channel.epg_id, p.guide().await) {
            (Some(e), Ok(guide)) => guide.programmes(e).to_vec(),
            _ => Vec::new(),
        }
    };
    let rows: Vec<&Programme> = rows
        .iter()
        .filter(|r| r.end > from && r.start < to)
        .collect();
    Ok(Json(
        serde_json::to_value(rows).unwrap_or(Value::Array(Vec::new())),
    ))
}

/// `GET …/movies/categories`.
pub async fn movie_categories(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let cats = p.movie_categories().await?;
    Ok(Json(Value::Array(
        cats.iter()
            .map(|c| json!({"id": c.id, "name": c.name}))
            .collect(),
    )))
}

/// `GET …/series/categories`.
pub async fn series_categories(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let cats = p.series_categories().await?;
    Ok(Json(Value::Array(
        cats.iter()
            .map(|c| json!({"id": c.id, "name": c.name}))
            .collect(),
    )))
}

/// `GET …/movies?category=<id>|all`.
pub async fn movies(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Q(q): Q<CategoryQuery>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let list = p.movies(category_filter(&q)).await?;
    Ok(Json(Value::Array(
        list.iter().map(|m| movie_json(&state, m)).collect(),
    )))
}

/// `GET …/series?category=<id>|all`.
pub async fn series(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Q(q): Q<CategoryQuery>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let list = p.series(category_filter(&q)).await?;
    Ok(Json(Value::Array(
        list.iter().map(|s| show_json(&state, s)).collect(),
    )))
}

async fn progress_json(
    state: &AppState,
    account_id: &str,
    kind: &'static str,
    item: &str,
) -> ApiResult<Value> {
    let (a, i) = (account_id.to_owned(), item.to_owned());
    let p = state
        .db
        .call(move |c| db::get_progress(c, &a, kind, &i))
        .await?;
    Ok(p.map_or(
        Value::Null,
        |p| json!({"position": p.position, "duration": p.duration}),
    ))
}

fn first_line(v: Option<&str>) -> Option<String> {
    text::opt_line(v, text::MAX_FIELD)
}

/// `GET …/movies/{id}`.
pub async fn movie(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, item)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let progress = progress_json(&state, &p.row.id, "movie", &item).await?;
    if !p.is_xtream() {
        let list = p.movies(None).await?;
        let m = list
            .iter()
            .find(|m| m.id == item)
            .ok_or_else(|| ApiError::not_found("No such film."))?;
        return Ok(Json(json!({
            "id": m.id, "name": m.name, "poster": img(&state, m.poster.as_deref()),
            "backdrop": null, "year": m.year, "rating": m.rating, "duration_secs": null,
            "genre": null, "plot": null, "cast": null, "director": null, "progress": progress,
        })));
    }
    let vod_id: u64 = item
        .parse()
        .map_err(|_| ApiError::not_found("No such film."))?;
    let info = p.vod_info(vod_id).await?;
    let listed = p.peek_movie(&item);
    let d = &info.info;
    let name = d
        .name
        .as_deref()
        .map(|n| text::line(n, text::MAX_NAME))
        .filter(|n| !n.is_empty())
        .or_else(|| listed.as_ref().map(|m| m.name.clone()))
        .ok_or_else(|| ApiError::not_found("No such film."))?;
    let poster = d
        .movie_image
        .clone()
        .or_else(|| listed.as_ref().and_then(|m| m.poster.clone()));
    let year =
        text::year(d.release_date.as_deref(), &name).or(listed.as_ref().and_then(|m| m.year));
    Ok(Json(json!({
        "id": item,
        "name": name,
        "poster": img(&state, poster.as_deref()),
        "backdrop": img(&state, d.backdrop_path.first().map(String::as_str)),
        "year": year,
        "rating": text::rating10(d.rating, None).or(listed.as_ref().and_then(|m| m.rating)),
        "duration_secs": d.duration_secs,
        "genre": first_line(d.genre.as_deref()),
        "plot": text::opt_paragraph(d.plot.as_deref(), text::MAX_PARAGRAPH),
        "cast": first_line(d.cast.as_deref()),
        "director": first_line(d.director.as_deref()),
        "progress": progress,
    })))
}

/// `GET …/series/{id}`.
pub async fn show(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, item)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    if !p.is_xtream() {
        return Err(ApiError::not_found("No such series."));
    }
    let series_id: u64 = item
        .parse()
        .map_err(|_| ApiError::not_found("No such series."))?;
    let info = p.series_info(series_id).await?;
    let listed = p.peek_show(&item);
    let d = &info.info;
    let name = d
        .name
        .as_deref()
        .map(|n| text::line(n, text::MAX_NAME))
        .filter(|n| !n.is_empty())
        .or_else(|| listed.as_ref().map(|s| s.name.clone()))
        .ok_or_else(|| ApiError::not_found("No such series."))?;
    let (a, s) = (p.row.id.clone(), item.clone());
    let progress: HashMap<String, (f64, f64)> = state
        .db
        .call(move |c| db::series_progress(c, &a, &s))
        .await?
        .into_iter()
        .map(|p| (p.item, (p.position, p.duration)))
        .collect();
    let season_names: HashMap<u32, String> = info
        .seasons
        .iter()
        .filter_map(|s| {
            Some((
                s.season_number?,
                text::opt_line(s.name.as_deref(), text::MAX_NAME)?,
            ))
        })
        .collect();
    let seasons: Vec<Value> = info
        .episodes
        .iter()
        .map(|(number, episodes)| {
            json!({
                "number": number,
                "name": season_names.get(number).cloned().unwrap_or_else(|| format!("Season {number}")),
                "episodes": episodes.iter().map(|e| {
                    let id = e.id.to_string();
                    json!({
                        "id": id,
                        "number": e.episode_num,
                        "title": text::line(&e.title, text::MAX_NAME),
                        "plot": text::opt_paragraph(e.info.plot.as_deref(), text::MAX_PARAGRAPH),
                        "duration_secs": e.info.duration_secs,
                        "still": img(&state, e.info.movie_image.as_deref()),
                        "progress": progress.get(&id).map(|(pos, dur)| json!({"position": pos, "duration": dur})),
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    let poster = d
        .cover
        .clone()
        .or_else(|| listed.as_ref().and_then(|s| s.poster.clone()));
    Ok(Json(json!({
        "id": item,
        "name": name,
        "poster": img(&state, poster.as_deref()),
        "backdrop": img(&state, d.backdrop_path.first().map(String::as_str)),
        "year": text::year(d.release_date.as_deref(), &name).or(listed.as_ref().and_then(|s| s.year)),
        "rating": text::rating10(d.rating, d.rating_5based).or(listed.as_ref().and_then(|s| s.rating)),
        "genre": first_line(d.genre.as_deref()),
        "plot": text::opt_paragraph(d.plot.as_deref(), text::MAX_PARAGRAPH),
        "cast": first_line(d.cast.as_deref()),
        "seasons": seasons,
    })))
}

#[derive(Deserialize)]
pub struct SearchQ {
    #[serde(default)]
    q: String,
}

/// `GET …/search?q=<text>` (≥ 2 characters) → at most 50 of each.
pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Q(q): Q<SearchQ>,
) -> ApiResult<Json<Value>> {
    let text = q.q.trim();
    if text.chars().count() < 2 || text.len() > 200 {
        return Err(ApiError::bad_request(
            "Type at least 2 characters to search.",
        ));
    }
    let query = SearchQuery::new(text);
    let p = provider(&state, &user.user.id, &id).await?;
    let live = p.live().await?;
    let channels: Vec<Value> = live
        .channels
        .iter()
        .filter(|c| matches(&query, &c.search))
        .take(50)
        .map(|c| channel_json(&state, c))
        .collect();
    let movies: Vec<Value> = match p.movies(None).await {
        Ok(list) => list
            .iter()
            .filter(|m| matches(&query, &m.search))
            .take(50)
            .map(|m| movie_json(&state, m))
            .collect(),
        Err(_) => Vec::new(),
    };
    let series: Vec<Value> = match p.series(None).await {
        Ok(list) => list
            .iter()
            .filter(|s| matches(&query, &s.search))
            .take(50)
            .map(|s| show_json(&state, s))
            .collect(),
        Err(_) => Vec::new(),
    };
    Ok(Json(
        json!({"channels": channels, "movies": movies, "series": series}),
    ))
}

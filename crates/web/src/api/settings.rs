// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user settings, the about box and the image proxy route.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Body;
use crate::auth::CurrentUser;
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// A person's settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// `24h` or `12h`.
    pub time_format: String,
    /// `ts` or `m3u8`.
    pub live_format: String,
    /// Play the next episode automatically.
    pub autoplay_next: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            time_format: "24h".into(),
            live_format: "ts".into(),
            autoplay_next: true,
        }
    }
}

/// Partial update.
#[derive(Debug, Deserialize)]
pub struct SettingsPatch {
    #[serde(default)]
    time_format: Option<String>,
    #[serde(default)]
    live_format: Option<String>,
    #[serde(default)]
    autoplay_next: Option<bool>,
}

/// A user's settings (defaults filled in).
///
/// # Errors
///
/// Database errors.
pub async fn load(state: &AppState, user_id: &str) -> ApiResult<Settings> {
    let id = user_id.to_owned();
    let raw = state.db.call(move |c| db::get_settings(c, &id)).await?;
    Ok(raw
        .and_then(|r| serde_json::from_str::<Settings>(&r).ok())
        .unwrap_or_default())
}

/// `GET /api/settings`.
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<Settings>> {
    Ok(Json(load(&state, &user.user.id).await?))
}

/// `PUT /api/settings` (any subset of the fields) → the full settings.
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    Body(patch): Body<SettingsPatch>,
) -> ApiResult<Json<Settings>> {
    let mut s = load(&state, &user.user.id).await?;
    if let Some(t) = patch.time_format {
        if !matches!(t.as_str(), "24h" | "12h") {
            return Err(ApiError::bad_request("time_format must be 24h or 12h."));
        }
        s.time_format = t;
    }
    if let Some(f) = patch.live_format {
        if !matches!(f.as_str(), "ts" | "m3u8") {
            return Err(ApiError::bad_request("live_format must be ts or m3u8."));
        }
        s.live_format = f;
    }
    if let Some(a) = patch.autoplay_next {
        s.autoplay_next = a;
    }
    let json = serde_json::to_string(&s).map_err(|_| ApiError::internal("settings json"))?;
    let id = user.user.id.clone();
    state
        .db
        .call(move |c| db::put_settings(c, &id, &json))
        .await?;
    Ok(Json(s))
}

/// `GET /api/about`.
pub async fn about(State(state): State<AppState>, _user: CurrentUser) -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "source_url": env!("CARGO_PKG_REPOSITORY"),
        "transcode_video": state.config.transcode_video.as_str(),
        "streams_per_user": state.config.streams_per_user,
        "max_streams": state.config.max_streams,
    }))
}

/// `GET /api/img/{token}`.
pub async fn image(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(token): Path<String>,
) -> Response {
    state.images.serve(&state, &token).await
}

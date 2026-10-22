// SPDX-License-Identifier: GPL-3.0-or-later
//! Administrator endpoints: people and invitations.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use opensmarters_core::url::Url;
use serde_json::{Value, json};

use super::empty;
use crate::auth::{AdminUser, Peer};
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Invitations are valid for 72 hours.
pub const INVITE_SECS: i64 = 72 * 3600;

/// `GET /api/admin/users`.
pub async fn users(State(state): State<AppState>, _admin: AdminUser) -> ApiResult<Json<Value>> {
    let users = state.db.call(|c| db::list_users(c)).await?;
    Ok(Json(Value::Array(
        users
            .iter()
            .map(|u| {
                json!({
                    "id": u.id,
                    "name": u.name,
                    "is_admin": u.is_admin,
                    "disabled": u.disabled,
                    "created_at": u.created_at,
                    "last_seen_at": u.last_seen_at,
                })
            })
            .collect(),
    )))
}

fn origin_of(url: &Url) -> Option<String> {
    matches!(url.scheme(), "http" | "https")
        .then(|| url.origin().ascii_serialization())
        .filter(|o| o != "null")
}

/// The instance's public origin for links: `OPENSMARTERS_PUBLIC_URL`, else the
/// request's `Origin`, else the forwarded or direct `Host`.
fn base_url(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if let Some(u) = &state.config.public_url {
        let mut s = u.as_str().trim_end_matches('/').to_owned();
        if s.is_empty() {
            s = origin_of(u).unwrap_or_default();
        }
        return s;
    }
    let header_str = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if let Some(origin) = header_str("origin")
        .and_then(|o| Url::parse(o).ok())
        .and_then(|u| origin_of(&u))
    {
        return origin;
    }
    let trusted = peer.is_some_and(|p| state.config.trusted_proxy.trusts(p.ip()));
    let host = trusted
        .then(|| header_str("x-forwarded-host"))
        .flatten()
        .or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()))
        .unwrap_or("localhost");
    let local =
        host.starts_with("localhost") || host.starts_with("127.") || host.starts_with("[::1]");
    let scheme = trusted
        .then(|| header_str("x-forwarded-proto"))
        .flatten()
        .filter(|p| matches!(*p, "http" | "https"))
        .unwrap_or(if local { "http" } else { "https" });
    let candidate = format!("{scheme}://{host}");
    Url::parse(&candidate)
        .ok()
        .and_then(|u| origin_of(&u))
        .unwrap_or_else(|| "http://localhost".to_owned())
}

/// `POST /api/admin/invites` → `{url, expires_at}` (one use, 72 h). The token is in the
/// link's fragment (`/invite#<token>`), so it never reaches a server log.
pub async fn invite(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
    Peer(peer): Peer,
) -> ApiResult<Json<Value>> {
    let token = crate::crypto::random_token().map_err(|_| ApiError::internal("random"))?;
    let hash = state.keys.token_hash("invite", &token);
    let now = db::now();
    let expires_at = now + INVITE_SECS;
    let by = admin.user.id.clone();
    state
        .db
        .call(move |c| db::insert_invite(c, &hash, &by, now, expires_at))
        .await?;
    let base = base_url(&state, &headers, peer);
    Ok(Json(
        json!({"url": format!("{base}/invite#{token}"), "expires_at": expires_at}),
    ))
}

fn not_self(admin: &AdminUser, id: &str, what: &str) -> ApiResult<()> {
    if admin.0.user.id == id {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "cannot_change_self",
            format!("You can't {what} your own account."),
        ));
    }
    Ok(())
}

/// `POST /api/admin/users/{id}/disable`: also ends their sessions and streams.
pub async fn disable(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    not_self(&admin, &id, "disable")?;
    let uid = id.clone();
    let found = state
        .db
        .call(move |c| {
            let found = db::set_disabled(c, &uid, true)?;
            db::delete_user_sessions(c, &uid)?;
            Ok(found)
        })
        .await?;
    if !found {
        return Err(ApiError::not_found("No such person."));
    }
    state.streams.stop_user(&id).await;
    Ok(empty())
}

/// `POST /api/admin/users/{id}/enable`.
pub async fn enable(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let found = state
        .db
        .call(move |c| db::set_disabled(c, &id, false))
        .await?;
    if !found {
        return Err(ApiError::not_found("No such person."));
    }
    Ok(empty())
}

/// `DELETE /api/admin/users/{id}`: their accounts, favourites, progress and sessions go
/// too.
pub async fn remove(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    not_self(&admin, &id, "remove")?;
    state.streams.stop_user(&id).await;
    let uid = id.clone();
    let accounts = state
        .db
        .call(move |c| {
            let accounts = db::list_accounts(c, &uid)?;
            let found = db::delete_user(c, &uid)?;
            Ok(found.then_some(accounts))
        })
        .await?
        .ok_or_else(|| ApiError::not_found("No such person."))?;
    for a in accounts {
        state.providers.drop_account(&a.id);
    }
    Ok(empty())
}

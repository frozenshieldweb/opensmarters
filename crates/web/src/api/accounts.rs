// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider accounts (each person has their own). Credentials are checked against the
//! provider before they are saved, sealed at rest and never returned.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use opensmarters_core::ServerUrl;
use opensmarters_core::url::Url;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{Body, empty, provider};
use crate::auth::CurrentUser;
use crate::db::{self, AccountRow};
use crate::error::{ApiError, ApiResult};
use crate::provider::{self as prov, Creds, LoginFailure};
use crate::state::AppState;
use crate::text;

/// Accounts per person.
const MAX_ACCOUNTS: i64 = 20;

/// The API's `Account`.
pub fn account_json(a: &AccountRow) -> Value {
    json!({
        "id": a.id,
        "name": a.name,
        "kind": a.kind,
        "host": a.host,
        "status": a.status,
        "expires_at": a.expires_at,
        "max_connections": a.max_connections,
        "active_connections": a.active_connections,
        "insecure": a.insecure.unwrap_or(false),
    })
}

/// `GET /api/accounts`.
pub async fn list(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Json<Value>> {
    let id = user.user.id.clone();
    let mut rows = state.db.call(move |c| db::list_accounts(c, &id)).await?;
    // Accounts saved before the flag existed: work it out from their addresses (and the
    // panel's last login answer, if this server has it) and remember it.
    for row in rows.iter_mut().filter(|r| r.insecure.is_none()) {
        let p = prov::Provider::new(&state, row.clone());
        let Ok(creds) = p.creds() else { continue };
        let login = p.cache().peek::<opensmarters_core::LoginInfo>("login");
        let insecure = prov::insecure_of(&creds, login.as_deref());
        row.insecure = Some(insecure);
        let (u, a) = (row.user_id.clone(), row.id.clone());
        state
            .db
            .call(move |c| db::set_account_insecure(c, &u, &a, insecure))
            .await?;
    }
    Ok(Json(Value::Array(rows.iter().map(account_json).collect())))
}

#[allow(clippy::option_option)]
fn double<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

#[derive(Deserialize, Default)]
pub struct AccountBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    m3u_url: Option<String>,
    /// Absent: unchanged; `null` or `""`: none.
    #[serde(default, deserialize_with = "double")]
    #[allow(clippy::option_option)]
    epg_url: Option<Option<String>>,
}

fn invalid(code: &'static str, message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn clean_name(name: Option<&str>, fallback: &str) -> ApiResult<String> {
    let n = text::line(name.unwrap_or_default(), 64);
    if n.is_empty() {
        let f = text::line(fallback, 64);
        return Ok(if f.is_empty() {
            "Provider".to_owned()
        } else {
            f
        });
    }
    Ok(n)
}

fn parse_web_url(state: &AppState, raw: &str, what: &str) -> ApiResult<Url> {
    let url = Url::parse(raw.trim())
        .ok()
        .filter(|u| {
            matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
        })
        .ok_or_else(|| {
            invalid(
                "invalid_request",
                &format!("The {what} must be an http:// or https:// address."),
            )
        })?;
    if raw.len() > 4096 {
        return Err(invalid(
            "invalid_request",
            &format!("The {what} is too long."),
        ));
    }
    state.net.check(&url).map_err(|e| match e {
        crate::net::NetError::Blocked(r) => invalid("blocked_address", r.message()),
        _ => invalid("invalid_request", &format!("The {what} isn't valid.")),
    })?;
    Ok(url)
}

/// The account a body describes, checked against the provider. `current` fills in the
/// fields a PATCH leaves out.
async fn check(
    state: &AppState,
    body: &AccountBody,
    kind: &str,
    current: Option<&Creds>,
) -> ApiResult<(Creds, AccountFacts)> {
    match kind {
        "xtream" => {
            let (cur_server, cur_user, cur_pass) = match current {
                Some(Creds::Xtream {
                    server,
                    username,
                    password,
                }) => (
                    Some(server.to_string()),
                    Some(username.clone()),
                    Some(password.clone()),
                ),
                _ => (None, None, None),
            };
            let server_text = body
                .server
                .clone()
                .or(cur_server)
                .ok_or_else(|| invalid("invalid_request", "Enter the server address."))?;
            let server = ServerUrl::parse(&server_text).map_err(|e| {
                invalid(
                    "invalid_server",
                    &format!("That server address isn't valid: {}.", reason(&e)),
                )
            })?;
            state.net.check(server.as_url()).map_err(|e| match e {
                crate::net::NetError::Blocked(r) => invalid("blocked_address", r.message()),
                _ => invalid("invalid_server", "That server address isn't valid."),
            })?;
            let username = body
                .username
                .clone()
                .or(cur_user)
                .map(|u| u.trim().to_owned())
                .filter(|u| !u.is_empty() && u.len() <= 256 && !u.chars().any(char::is_control))
                .ok_or_else(|| invalid("invalid_request", "Enter the user name."))?;
            let password = match &body.password {
                Some(p) => SecretString::from(p.clone()),
                None => {
                    cur_pass.ok_or_else(|| invalid("invalid_request", "Enter the password."))?
                }
            };
            let pw_len = password.expose_secret().len();
            if pw_len == 0 || pw_len > 1024 {
                return Err(invalid("invalid_request", "Enter the password."));
            }
            let info = prov::fetch_login(state, &server, &username, &password)
                .await
                .map_err(|f| prov::login_error(&f))?;
            if let Err(why) = info.check(Utc::now()) {
                return Err(prov::login_error(&LoginFailure::Auth(why)));
            }
            let mut facts = AccountFacts {
                host: server.host_str().to_owned(),
                server: Some(server.to_string()),
                username: Some(username.clone()),
                status: prov::status_of(&info).to_owned(),
                expires_at: info.user_info.exp_date.map(|d| d.timestamp()),
                max_connections: info.user_info.max_connections.map(i64::from),
                active_connections: info.user_info.active_cons.map(i64::from),
                playlist: None,
                insecure: false,
            };
            let creds = Creds::Xtream {
                server,
                username,
                password,
            };
            facts.insecure = prov::insecure_of(&creds, Some(&info));
            Ok((creds, facts))
        }
        "m3u" => {
            let (cur_url, cur_epg) = match current {
                Some(Creds::M3u { url, epg }) => (Some(url.clone()), epg.clone()),
                _ => (None, None),
            };
            let url = match &body.m3u_url {
                Some(u) => SecretString::from(u.trim().to_owned()),
                None => cur_url
                    .ok_or_else(|| invalid("invalid_request", "Enter the playlist address."))?,
            };
            let parsed = parse_web_url(state, url.expose_secret(), "playlist address")?;
            let epg = match &body.epg_url {
                None => cur_epg,
                Some(None) => None,
                Some(Some(e)) if e.trim().is_empty() => None,
                Some(Some(e)) => {
                    parse_web_url(state, e, "guide (EPG) address")?;
                    Some(SecretString::from(e.trim().to_owned()))
                }
            };
            let playlist = prov::fetch_playlist(state, &url).await?;
            let creds = Creds::M3u { url, epg };
            let facts = AccountFacts {
                host: parsed.host_str().unwrap_or_default().to_owned(),
                server: None,
                username: None,
                status: "active".to_owned(),
                expires_at: None,
                max_connections: None,
                active_connections: None,
                playlist: Some(playlist),
                insecure: prov::insecure_of(&creds, None),
            };
            Ok((creds, facts))
        }
        _ => Err(invalid(
            "invalid_request",
            "The provider kind must be \"xtream\" or \"m3u\".",
        )),
    }
}

fn reason(e: &opensmarters_core::Error) -> String {
    match e {
        opensmarters_core::Error::InvalidUrl { reason } => reason.clone(),
        _ => "it isn't a web address".to_owned(),
    }
}

/// What the provider check learnt.
struct AccountFacts {
    host: String,
    server: Option<String>,
    username: Option<String>,
    status: String,
    expires_at: Option<i64>,
    max_connections: Option<i64>,
    active_connections: Option<i64>,
    playlist: Option<opensmarters_core::m3u::Playlist>,
    /// An address is plain `http` ([`prov::insecure_of`]).
    insecure: bool,
}

#[allow(clippy::too_many_arguments)]
fn row_from(
    state: &AppState,
    id: &str,
    user_id: &str,
    name: String,
    kind: &str,
    creds: &Creds,
    facts: &AccountFacts,
    created_at: i64,
) -> ApiResult<AccountRow> {
    Ok(AccountRow {
        id: id.to_owned(),
        user_id: user_id.to_owned(),
        name,
        kind: kind.to_owned(),
        host: facts.host.clone(),
        server: facts.server.clone(),
        username: facts.username.clone(),
        secret: prov::seal(state, user_id, id, creds)?,
        status: facts.status.clone(),
        expires_at: facts.expires_at,
        max_connections: facts.max_connections,
        active_connections: facts.active_connections,
        created_at,
        insecure: Some(facts.insecure),
        // New provider details: the catch-up format is found again.
        timeshift_format: None,
    })
}

/// `POST /api/accounts`.
pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Body(body): Body<AccountBody>,
) -> ApiResult<Json<Value>> {
    let uid = user.user.id.clone();
    let count = state.db.call(move |c| db::account_count(c, &uid)).await?;
    if count >= MAX_ACCOUNTS {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "too_many_accounts",
            "You've reached the limit of 20 providers. Remove one first.",
        ));
    }
    let kind = body.kind.clone().unwrap_or_default();
    let (creds, mut facts) = check(&state, &body, &kind, None).await?;
    let id = crate::crypto::random_id().map_err(|_| ApiError::internal("random"))?;
    let name = clean_name(body.name.as_deref(), &facts.host)?;
    let row = row_from(
        &state,
        &id,
        &user.user.id,
        name,
        &kind,
        &creds,
        &facts,
        db::now(),
    )?;
    let r = row.clone();
    state.db.call(move |c| db::upsert_account(c, &r)).await?;
    if let Some(playlist) = facts.playlist.take() {
        prov::Provider::new(&state, row.clone())
            .prime_m3u(playlist)
            .await;
    }
    tracing::info!(account = %id, kind = %kind, "provider added");
    Ok(Json(account_json(&row)))
}

/// `PATCH /api/accounts/{id}`.
pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Body(body): Body<AccountBody>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    let mut row = p.row.clone();
    let kind = body.kind.clone().unwrap_or_else(|| row.kind.clone());
    let provider_fields = body.server.is_some()
        || body.username.is_some()
        || body.password.is_some()
        || body.m3u_url.is_some()
        || body.epg_url.is_some()
        || kind != row.kind;
    if provider_fields {
        // A kind change starts from nothing; else unchanged fields keep their values
        // (a key change makes the old secret unreadable: then everything is required).
        let current = if kind == row.kind {
            p.creds().ok()
        } else {
            None
        };
        let (creds, mut facts) = check(&state, &body, &kind, current.as_ref()).await?;
        let name = match &body.name {
            Some(n) => clean_name(Some(n), &facts.host)?,
            None => row.name.clone(),
        };
        row = row_from(
            &state,
            &row.id,
            &row.user_id,
            name,
            &kind,
            &creds,
            &facts,
            row.created_at,
        )?;
        state.streams.stop_account(&row.id).await;
        state.providers.drop_account(&row.id);
        let r = row.clone();
        state.db.call(move |c| db::upsert_account(c, &r)).await?;
        if let Some(playlist) = facts.playlist.take() {
            prov::Provider::new(&state, row.clone())
                .prime_m3u(playlist)
                .await;
        }
        return Ok(Json(account_json(&row)));
    } else if let Some(n) = &body.name {
        row.name = clean_name(Some(n), &row.host)?;
    }
    let r = row.clone();
    state.db.call(move |c| db::upsert_account(c, &r)).await?;
    Ok(Json(account_json(&row)))
}

/// `DELETE /api/accounts/{id}`.
pub async fn remove(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    state.streams.stop_account(&p.row.id).await;
    let (uid, aid) = (user.user.id.clone(), p.row.id.clone());
    state
        .db
        .call(move |c| db::delete_account(c, &uid, &aid))
        .await?;
    state.providers.drop_account(&p.row.id);
    tracing::info!(account = %p.row.id, "provider removed");
    Ok(empty())
}

/// `POST /api/accounts/{id}/refresh`: drops the cached lists and checks the account
/// again (status, expiry, connections).
pub async fn refresh(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = provider(&state, &user.user.id, &id).await?;
    state.providers.drop_account(&p.row.id);
    let mut row = p.row.clone();
    let creds = p.creds()?;
    match &creds {
        Creds::Xtream {
            server,
            username,
            password,
        } => {
            let info = prov::fetch_login(&state, server, username, password)
                .await
                .map_err(|f| match f {
                    LoginFailure::Net(e) => ApiError::provider(&e),
                    other => prov::login_error(&other),
                })?;
            row.status = prov::status_of(&info).to_owned();
            row.expires_at = info.user_info.exp_date.map(|d| d.timestamp());
            row.max_connections = info.user_info.max_connections.map(i64::from);
            row.active_connections = info.user_info.active_cons.map(i64::from);
            row.insecure = Some(prov::insecure_of(&creds, Some(&info)));
        }
        Creds::M3u { .. } => {
            let fresh = crate::provider::Provider::new(&state, row.clone());
            fresh.m3u().await?;
            row.status = "active".to_owned();
            row.insecure = Some(prov::insecure_of(&creds, None));
        }
    }
    let r = row.clone();
    state.db.call(move |c| db::upsert_account(c, &r)).await?;
    Ok(Json(account_json(&row)))
}

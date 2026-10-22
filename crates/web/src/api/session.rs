// SPDX-License-Identifier: GPL-3.0-or-later
//! Status, first-run setup, sign-in / sign-out, password change, invite acceptance.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Body, empty};
use crate::auth::{self, ClientIp, CurrentUser, Limiter};
use crate::db::{self, User};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// `{id, name, is_admin}`.
pub fn user_json(u: &User) -> Value {
    json!({"id": u.id, "name": u.name, "is_admin": u.is_admin})
}

fn with_session(body: Value, token: &str) -> Response {
    let mut res = Json(body).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, auth::session_cookie(token));
    res
}

/// `GET /api/status`.
pub async fn status(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let user = auth::session_of(&state, &headers).await?;
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "setup_required": state.setup_open(),
        "user": user.map(|u| user_json(&u.user)),
    })))
}

#[derive(Deserialize)]
pub struct SetupBody {
    setup_code: String,
    name: String,
    password: String,
}

/// `POST /api/setup`.
pub async fn setup(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Body(body): Body<SetupBody>,
) -> ApiResult<Response> {
    if !state.setup_open() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_set_up",
            "This server is already set up. Sign in instead.",
        ));
    }
    // Per address only: a global lock would let anyone block the owner's setup (the
    // code has about 60 bits, far beyond guessing).
    let keys = vec![format!("setup-ip:{ip}")];
    state.limiter.check(&keys).map_err(Limiter::rejection)?;
    if !state.setup_code_matches(&body.setup_code) {
        state.limiter.fail(&keys);
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_setup_code",
            "That setup code isn't right. It's printed in the server's log.",
        ));
    }
    let name = auth::validate_name(&body.name)?;
    auth::validate_password(&body.password)?;
    let password = SecretString::from(body.password);
    let hash = state.passwords.hash(&password).await?;
    let user = User {
        id: crate::crypto::random_id().map_err(|_| ApiError::internal("random"))?,
        name,
        is_admin: true,
        disabled: false,
        created_at: db::now(),
        last_seen_at: None,
    };
    let u = user.clone();
    let created = state
        .db
        .call(move |c| {
            // Only while no user exists (single use, also against a race).
            let tx = c.transaction()?;
            if db::user_count(&tx)? > 0 {
                return Ok(false);
            }
            let ok = db::insert_user(&tx, &u, &hash)?;
            tx.commit()?;
            Ok(ok)
        })
        .await?;
    if !created {
        state.close_setup();
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_set_up",
            "This server is already set up. Sign in instead.",
        ));
    }
    state.close_setup();
    state.limiter.succeed(&keys);
    tracing::info!(user = %user.name, "administrator created; setup closed");
    let token = auth::create_session(&state, &user.id).await?;
    Ok(with_session(json!({"user": user_json(&user)}), &token))
}

#[derive(Deserialize)]
pub struct LoginBody {
    name: String,
    password: String,
}

/// `POST /api/login`.
pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Body(body): Body<LoginBody>,
) -> ApiResult<Response> {
    let name_key = db::name_key(body.name.trim());
    let keys = vec![format!("login-ip:{ip}"), format!("login-name:{name_key}")];
    state.limiter.check(&keys).map_err(Limiter::rejection)?;
    let bad = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_credentials",
            "That name and password don't match an account here.",
        )
    };
    let name = body.name.trim().to_owned();
    let found = if name.len() <= 64 && body.password.len() <= auth::MAX_PASSWORD_BYTES {
        state.db.call(move |c| db::user_by_name(c, &name)).await?
    } else {
        None
    };
    let password = SecretString::from(body.password);
    let (user, phc) = match found {
        Some((u, h)) => (Some(u), Some(h)),
        None => (None, None),
    };
    let ok = state.passwords.verify(&password, phc).await;
    let Some(user) = user.filter(|u| ok && !u.disabled) else {
        state.limiter.fail(&keys);
        return Err(bad());
    };
    state.limiter.succeed(&keys);
    // Rotate: a new token, and the session this browser had (if any) ends.
    if let Some(old) = auth::read_cookie(&headers) {
        let hash = state.keys.token_hash("session", &old);
        state.db.call(move |c| db::delete_session(c, &hash)).await?;
    }
    let token = auth::create_session(&state, &user.id).await?;
    Ok(with_session(json!({"user": user_json(&user)}), &token))
}

/// `POST /api/logout`.
pub async fn logout(State(state): State<AppState>, user: CurrentUser) -> ApiResult<Response> {
    let hash = user.session;
    state.db.call(move |c| db::delete_session(c, &hash)).await?;
    // Streams of this browser end with it.
    state.streams.stop_session(&user.session).await;
    let mut res = empty().into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, auth::clear_cookie());
    Ok(res)
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

/// `POST /api/password`.
pub async fn change_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Body(body): Body<PasswordBody>,
) -> ApiResult<Response> {
    let keys = vec![format!("password-user:{}", user.user.id)];
    state.limiter.check(&keys).map_err(Limiter::rejection)?;
    auth::validate_password(&body.new)?;
    let id = user.user.id.clone();
    let phc = state
        .db
        .call(move |c| db::user_by_id(c, &id))
        .await?
        .map(|(_, h)| h);
    if !state
        .passwords
        .verify(&SecretString::from(body.current), phc)
        .await
    {
        state.limiter.fail(&keys);
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong_password",
            "Your current password isn't right.",
        ));
    }
    state.limiter.succeed(&keys);
    let hash = state.passwords.hash(&SecretString::from(body.new)).await?;
    let id = user.user.id.clone();
    // Every session of the user ends; this browser gets a fresh one.
    state
        .db
        .call(move |c| {
            db::set_password_hash(c, &id, &hash)?;
            db::delete_user_sessions(c, &id)
        })
        .await?;
    let token = auth::create_session(&state, &user.user.id).await?;
    Ok(with_session(json!({}), &token))
}

#[derive(Deserialize)]
pub struct AcceptBody {
    token: String,
    name: String,
    password: String,
}

/// `POST /api/invites/accept`.
pub async fn accept_invite(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Body(body): Body<AcceptBody>,
) -> ApiResult<Response> {
    let keys = vec![format!("invite-ip:{ip}")];
    state.limiter.check(&keys).map_err(Limiter::rejection)?;
    let name = auth::validate_name(&body.name)?;
    auth::validate_password(&body.password)?;
    let invalid = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_invite",
            "This invitation link isn't valid any more. Ask for a new one.",
        )
    };
    let token = body.token.trim().to_owned();
    if token.is_empty() || token.len() > 128 {
        state.limiter.fail(&keys);
        return Err(invalid());
    }
    let hash = state
        .passwords
        .hash(&SecretString::from(body.password))
        .await?;
    let token_hash = state.keys.token_hash("invite", &token);
    let user = User {
        id: crate::crypto::random_id().map_err(|_| ApiError::internal("random"))?,
        name,
        is_admin: false,
        disabled: false,
        created_at: db::now(),
        last_seen_at: None,
    };
    let u = user.clone();
    let now = db::now();
    let outcome = state
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if !db::use_invite(&tx, &token_hash, now)? {
                return Ok(Err("invite"));
            }
            if !db::insert_user(&tx, &u, &hash)? {
                // Roll back: the invite stays usable with another name.
                return Ok(Err("name"));
            }
            tx.commit()?;
            Ok(Ok(()))
        })
        .await?;
    match outcome {
        Err("invite") => {
            state.limiter.fail(&keys);
            Err(invalid())
        }
        Err(_) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "name_taken",
            "That name is already used. Choose another one.",
        )),
        Ok(()) => {
            tracing::info!(user = %user.name, "invitation accepted");
            let token = auth::create_session(&state, &user.id).await?;
            Ok(with_session(json!({"user": user_json(&user)}), &token))
        }
    }
}

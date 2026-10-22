// SPDX-License-Identifier: GPL-3.0-or-later
//! The JSON API under `/api` (contract: `docs/WEB_PROMPT.md` §4).

mod accounts;
mod admin;
mod catalog;
mod library;
mod play;
mod session;
mod settings;

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use serde::de::DeserializeOwned;

use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::provider::Provider;
use crate::state::AppState;

/// The CSRF header every non-GET request must carry.
pub const CSRF_HEADER: &str = "x-opensmarters";

/// A JSON body; bad JSON is a 400 `invalid_request` in the API's error shape.
#[derive(Debug)]
pub struct Body<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(JsonRejection::MissingJsonContentType(_)) => Err(ApiError::bad_request(
                "Send the request as JSON (Content-Type: application/json).",
            )),
            Err(JsonRejection::BytesRejection(_)) => Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_request",
                "The request is too large.",
            )),
            Err(_) => Err(ApiError::bad_request("The request isn't valid.")),
        }
    }
}

/// Query parameters; bad ones are a 400 `invalid_request`.
#[derive(Debug)]
pub struct Q<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for Q<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| Q(v))
            .map_err(|_: QueryRejection| {
                ApiError::bad_request("The request's parameters aren't valid.")
            })
    }
}

/// `{}`.
pub fn empty() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

/// The provider account `id` of `user_id`, or 404.
///
/// # Errors
///
/// 404 `not_found` (also for another user's account).
pub async fn provider(state: &AppState, user_id: &str, id: &str) -> ApiResult<Provider> {
    let (u, a) = (user_id.to_owned(), id.to_owned());
    let row = state
        .db
        .call(move |c| db::get_account(c, &u, &a))
        .await?
        .ok_or_else(|| ApiError::not_found("No such provider."))?;
    Ok(Provider::new(state, row))
}

/// `/api/img/<token>` for a provider image address.
pub fn img(state: &AppState, url: Option<&str>) -> Option<String> {
    url.and_then(|u| state.keys.image_token(u))
        .map(|t| format!("/api/img/{t}"))
}

/// CSRF guard and API response headers.
pub async fn guard(req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe
        && req
            .headers()
            .get(CSRF_HEADER)
            .is_none_or(|v| v.as_bytes() != b"1")
    {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "csrf",
            "This request was refused because a security header was missing. Reload the page and try again.",
        )
        .into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    h.entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(
            "default-src 'none'; frame-ancestors 'none'",
        ));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
}

async fn not_found() -> ApiError {
    ApiError::not_found("There's nothing at this address.")
}

/// The `/api` router.
pub fn router() -> Router<AppState> {
    let account = Router::new()
        .route(
            "/",
            axum::routing::patch(accounts::update).delete(accounts::remove),
        )
        .route("/refresh", post(accounts::refresh))
        .route("/live/categories", get(catalog::live_categories))
        .route("/live/channels", get(catalog::live_channels))
        .route("/epg/now", get(catalog::epg_now))
        .route("/epg/channel/{key}", get(catalog::epg_channel))
        .route("/movies/categories", get(catalog::movie_categories))
        .route("/movies", get(catalog::movies))
        .route("/movies/{item}", get(catalog::movie))
        .route("/series/categories", get(catalog::series_categories))
        .route("/series", get(catalog::series))
        .route("/series/{item}", get(catalog::show))
        .route("/search", get(catalog::search))
        .route("/favourites", get(library::favourites))
        .route(
            "/favourites/{kind}/{item}",
            put(library::add_favourite).delete(library::remove_favourite),
        )
        .route("/continue", get(library::continue_watching))
        .route("/progress", put(library::save_progress));
    Router::new()
        .route("/status", get(session::status))
        .route("/setup", post(session::setup))
        .route("/login", post(session::login))
        .route("/logout", post(session::logout))
        .route("/password", post(session::change_password))
        .route("/invites/accept", post(session::accept_invite))
        .route("/admin/users", get(admin::users))
        .route("/admin/invites", post(admin::invite))
        .route("/admin/users/{id}", delete(admin::remove))
        .route("/admin/users/{id}/disable", post(admin::disable))
        .route("/admin/users/{id}/enable", post(admin::enable))
        .route("/accounts", get(accounts::list).post(accounts::create))
        .nest("/accounts/{id}", account)
        .route("/play", post(play::play))
        .route("/streams/{sid}", delete(play::stop))
        .route("/streams/{sid}/{file}", get(play::file))
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route("/about", get(settings::about))
        .route("/img/{token}", get(settings::image))
        .fallback(not_found)
}

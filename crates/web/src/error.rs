// SPDX-License-Identifier: GPL-3.0-or-later
//! API errors: `{"error": {"code": "<snake_case>", "message": "<plain English>"}}` with a
//! fitting HTTP status. Messages are written for people; they never contain URLs,
//! credentials or internal details (those go to the redacted log).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::net::NetError;

/// An error answer.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Plain-English message for the user.
    pub message: String,
    /// `Retry-After` seconds (429 / 503).
    pub retry_after: Option<u64>,
}

/// Handler result.
pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    /// A new error.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    /// 400 `invalid_request`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    /// 401 `not_signed_in`.
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "not_signed_in",
            "Please sign in again.",
        )
    }

    /// 403 `forbidden`.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// 404 `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// 500 `internal` (details are logged, not shown).
    pub fn internal(what: &str) -> Self {
        tracing::error!(what, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Something went wrong on the server. Try again in a moment.",
        )
    }

    /// Adds `Retry-After`.
    #[must_use]
    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    /// A provider request failed while browsing (502).
    pub fn provider(e: &NetError) -> Self {
        match e {
            NetError::Blocked(r) => {
                Self::new(StatusCode::BAD_GATEWAY, "blocked_address", r.message())
            }
            NetError::Timeout => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_unreachable",
                "Your provider took too long to answer. Try again in a moment.",
            ),
            NetError::Http(401 | 403) => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_error",
                "Your provider refused the request. Check this provider's details in Settings.",
            ),
            NetError::Http(404) => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_error",
                "Your provider doesn't have this any more.",
            ),
            NetError::Http(_) => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_error",
                "Your provider had a problem answering. Try again in a moment.",
            ),
            NetError::TooLarge(_) => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_error",
                "Your provider sent more data than this server accepts.",
            ),
            NetError::InsecureRedirect => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_error",
                "Your provider tried to switch from a secure to an unencrypted connection, which isn't allowed.",
            ),
            NetError::InvalidUrl | NetError::Network(_) => Self::new(
                StatusCode::BAD_GATEWAY,
                "provider_unreachable",
                "Your provider can't be reached right now. Try again in a moment.",
            ),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({"error": {"code": self.code, "message": self.message}}));
        let mut res = (self.status, body).into_response();
        res.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(secs) = self.retry_after
            && let Ok(v) = HeaderValue::from_str(&secs.to_string())
        {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
        res
    }
}

impl From<crate::db::DbError> for ApiError {
    fn from(e: crate::db::DbError) -> Self {
        tracing::error!(error = %e, "database error");
        ApiError::internal("database")
    }
}

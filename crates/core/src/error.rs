// SPDX-License-Identifier: GPL-3.0-or-later
//! Error types for the Xtream client and the JSON parsers.

use std::fmt;

use chrono::{DateTime, Utc};

use crate::model::AccountStatus;

/// Errors returned by the Xtream client, the HTTP layer and the JSON parsers.
///
/// **No variant ever contains credentials.** Network messages are stripped of their URL
/// and passed through the client's [`crate::Redactor`]; parse messages never quote the
/// response body; [`Error::InvalidUrl`] carries a reason, not the URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The server URL (or another URL) could not be used. `reason` explains why; the
    /// offending URL itself is deliberately not included.
    #[error("invalid URL: {reason}")]
    InvalidUrl {
        /// Human-readable reason.
        reason: String,
    },
    /// A configuration value is unusable (e.g. a User-Agent with control characters).
    #[error("invalid configuration: {reason}")]
    InvalidConfig {
        /// Human-readable reason.
        reason: String,
    },
    /// Connection, DNS, TLS or transfer failure.
    #[error("network error: {message}")]
    Network {
        /// Redacted description of the underlying error chain.
        message: String,
    },
    /// Connect, read or overall request timeout.
    #[error("the request timed out")]
    Timeout,
    /// The server answered with a non-success HTTP status. A `403` from a panel that
    /// whitelists player User-Agents can sometimes be fixed with
    /// [`crate::IPTV_SMARTERS_UA`].
    #[error("the server returned HTTP status {status}")]
    Http {
        /// HTTP status code.
        status: u16,
    },
    /// The response body (after decompression) is larger than the configured cap.
    #[error("the response is larger than the {limit}-byte limit")]
    TooLarge {
        /// The cap that was exceeded, in bytes.
        limit: u64,
    },
    /// The response is not the JSON we expected (HTML error page, truncated body, …).
    #[error("could not understand the server response: {message}")]
    Parse {
        /// Description of the problem. Never contains the response body.
        message: String,
    },
    /// An `https` request was redirected to a plain `http` address and was stopped, so
    /// the credentials in the URL never travel unencrypted (see
    /// [`crate::HttpConfig::allow_https_downgrade`]).
    #[error(
        "the server redirected a secure (https) request to an unencrypted (http) address, \
         so it was stopped to protect your password"
    )]
    InsecureRedirect,
    /// Login was refused, or a list endpoint answered with an auth-failure object.
    #[error("authentication failed: {status}")]
    Auth {
        /// Why authentication failed.
        status: AuthFailure,
    },
    /// A resumed download got a different part of the file than it asked for (its
    /// `Content-Range` doesn't start where the file stopped), so nothing was appended
    /// ([`crate::download`]).
    #[error("the server sent a different part of the file than was asked for")]
    RangeMismatch,
    /// The server has nothing from the requested position on (HTTP 416). `total` is the
    /// file's size if the server said: equal to the position, the file is complete.
    #[error("the server has nothing after the requested position")]
    RangeNotSatisfiable {
        /// Size of the whole file, if known.
        total: Option<u64>,
    },
}

impl Error {
    pub(crate) fn parse(message: impl Into<String>) -> Self {
        Error::Parse {
            message: message.into(),
        }
    }

    pub(crate) fn invalid_url(reason: impl Into<String>) -> Self {
        Error::InvalidUrl {
            reason: reason.into(),
        }
    }
}

/// Why an Xtream login (or an authenticated call) was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthFailure {
    /// `user_info.auth` was falsy (or the panel returned an empty answer): wrong
    /// username or password.
    InvalidCredentials,
    /// Credentials are valid but the account status is not `Active`
    /// (`Expired`, `Banned`, `Disabled`, …).
    Status(AccountStatus),
    /// The status is `Active`, but `exp_date` is in the past according to the server
    /// clock (`server_info.timestamp_now`, or the local clock when absent).
    Expired {
        /// The account's expiry date.
        exp_date: DateTime<Utc>,
    },
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthFailure::InvalidCredentials => f.write_str("invalid username or password"),
            AuthFailure::Status(status) => write!(f, "account status is {status}"),
            AuthFailure::Expired { exp_date } => {
                write!(f, "account expired on {}", exp_date.format("%Y-%m-%d"))
            }
        }
    }
}

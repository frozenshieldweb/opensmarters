// SPDX-License-Identifier: GPL-3.0-or-later
//! The app's error type, with plain-language messages for the UI. No variant ever
//! contains a credential or a URL (the library errors it wraps guarantee the same).

use opensmarters_core::{AccountStatus, AuthFailure};

/// An error shown to the user or logged.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AppError {
    /// Provider / network / parse error from the core library.
    #[error(transparent)]
    Provider(#[from] opensmarters_core::Error),
    /// Local database or keychain problem.
    #[error("storage error: {0}")]
    Store(String),
    /// The playlist could not be read.
    #[error("playlist error: {0}")]
    Playlist(String),
    /// The player (libmpv) is unavailable or refused the request.
    #[error("player error: {0}")]
    Player(String),
    /// Input validation or another user-facing problem (already plain language).
    #[error("{0}")]
    Message(String),
    /// A user-facing problem with its own headline ("Channel not found").
    #[error("{title}: {message}")]
    Titled {
        /// The headline.
        title: &'static str,
        /// The explanation (plain language).
        message: String,
    },
    /// The task was superseded or cancelled.
    #[error("cancelled")]
    Cancelled,
    /// Parental control: the item is in a locked category (or its categories could not
    /// be checked yet). Ask for the PIN, then `Session::unlock_category` (or
    /// `Session::unlock_item` when no category is known) and retry; when
    /// [`LockedItem::category`] is `None`, first try `Session::verify_item`.
    #[error("locked by parental control")]
    Locked(LockedItem),
    /// A background job failed unexpectedly (it panicked or its worker went away). The
    /// details are in the log.
    #[error("internal error")]
    Internal,
}

/// What [`AppError::Locked`] refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedItem {
    /// Live, movies or series (the section whose locks apply).
    pub kind: opensmarters_store::ContentKind,
    /// The item: Xtream stream id / series id as a string, or a live channel key.
    pub item: String,
    /// The locked category to unlock with `Session::unlock_category`. `None` when the
    /// item's categories are not known yet: `Session::verify_item` looks them up (one
    /// cached request), then retry; if it still can't tell, unlock the item itself with
    /// `Session::unlock_item` after the PIN.
    pub category: Option<opensmarters_core::CategoryId>,
    /// The category's display name, when known (for the PIN prompt's subtitle).
    pub category_name: Option<String>,
}

impl LockedItem {
    /// True when the categories of the item could not be checked yet.
    pub fn is_unverified(&self) -> bool {
        self.category.is_none()
    }
}

impl From<opensmarters_store::StoreError> for AppError {
    fn from(e: opensmarters_store::StoreError) -> Self {
        AppError::Store(e.to_string())
    }
}

impl From<opensmarters_core::m3u::M3uError> for AppError {
    fn from(e: opensmarters_core::m3u::M3uError) -> Self {
        AppError::Playlist(e.to_string())
    }
}

impl From<opensmarters_player::Error> for AppError {
    fn from(e: opensmarters_player::Error) -> Self {
        AppError::Player(e.to_string())
    }
}

impl From<opensmarters_player::ValidationError> for AppError {
    fn from(e: opensmarters_player::ValidationError) -> Self {
        AppError::Player(format!("the stream address was rejected ({e})"))
    }
}

impl AppError {
    /// A plain message.
    pub fn msg(s: impl Into<String>) -> Self {
        AppError::Message(s.into())
    }

    /// A plain message with its own headline.
    pub fn titled(title: &'static str, message: impl Into<String>) -> Self {
        AppError::Titled {
            title,
            message: message.into(),
        }
    }

    /// True for connection problems (show the offline state, retry later).
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            AppError::Provider(
                opensmarters_core::Error::Network { .. } | opensmarters_core::Error::Timeout
            )
        )
    }

    /// The parental lock that refused a request, if that is what happened.
    pub fn locked(&self) -> Option<&LockedItem> {
        match self {
            AppError::Locked(l) => Some(l),
            _ => None,
        }
    }

    /// True if the provider refused the credentials or the account state.
    pub fn is_auth(&self) -> bool {
        matches!(
            self,
            AppError::Provider(opensmarters_core::Error::Auth { .. })
        )
    }

    /// A short headline ("Can't reach your provider").
    pub fn title(&self) -> &'static str {
        use opensmarters_core::Error as E;
        match self {
            AppError::Provider(E::Network { .. } | E::Timeout) => "Can't reach your provider",
            AppError::Provider(E::Auth { status }) => match status {
                AuthFailure::InvalidCredentials => "Wrong username or password",
                _ => "Account not active",
            },
            AppError::Provider(E::Http { .. }) => "The provider refused the request",
            AppError::Provider(_) | AppError::Playlist(_) => "Something went wrong",
            AppError::Store(_) => "Storage problem",
            AppError::Player(_) => "Playback problem",
            AppError::Message(_) => "Check the details",
            AppError::Titled { title, .. } => title,
            AppError::Cancelled => "Cancelled",
            AppError::Locked(_) => "Locked by parental control",
            AppError::Internal => "Something went wrong",
        }
    }

    /// A user-facing explanation in plain language.
    pub fn user_message(&self) -> String {
        use opensmarters_core::Error as E;
        match self {
            AppError::Provider(E::Timeout) => {
                "Your provider didn't answer in time. Check your connection and try again.".into()
            }
            AppError::Provider(E::Network { .. }) => {
                "Check your internet connection and the server address, then try again.".into()
            }
            AppError::Provider(E::Http { status: 403 }) => {
                "The provider refused the request (HTTP 403). Some providers only accept known \
                 players: try the \u{201c}IPTV Smarters compatible\u{201d} User-Agent in the \
                 account's advanced settings."
                    .into()
            }
            AppError::Provider(E::Http { status: 404 }) => {
                "The provider doesn't have this (HTTP 404). Check the server address.".into()
            }
            AppError::Provider(E::Http { status }) => {
                format!("The provider answered with an error (HTTP {status}). Try again later.")
            }
            AppError::Provider(E::Auth { status }) => match status {
                AuthFailure::InvalidCredentials => {
                    "The provider didn't accept this username and password.".into()
                }
                AuthFailure::Status(s) => status_message(s),
                AuthFailure::Expired { exp_date } => format!(
                    "This subscription expired on {}. Contact your provider to renew it.",
                    exp_date.format("%-d %B %Y")
                ),
                _ => "The provider refused this account.".into(),
            },
            AppError::Provider(E::TooLarge { .. }) => {
                "The provider's answer is larger than the safety limit, so it was not loaded."
                    .into()
            }
            AppError::Provider(E::Parse { .. }) => {
                "The provider sent an answer we couldn't read. Check the server address (it \
                 should be the panel's address, not a web page)."
                    .into()
            }
            AppError::Provider(E::InvalidUrl { reason }) => {
                format!("The address isn't valid: {reason}.")
            }
            AppError::Provider(other) => other.to_string(),
            AppError::Playlist(e) => format!("The playlist couldn't be read: {e}."),
            AppError::Store(e) => format!("Local storage problem: {e}."),
            AppError::Player(e) => e.clone(),
            AppError::Message(m) => m.clone(),
            AppError::Titled { message, .. } => message.clone(),
            AppError::Cancelled => "Cancelled.".into(),
            AppError::Locked(l) if l.is_unverified() => {
                "Checking parental control for this title. Try again in a moment, or enter \
                 the PIN."
                    .into()
            }
            AppError::Locked(l) => match &l.category_name {
                Some(name) => format!(
                    "\u{201c}{}\u{201d} is locked. Enter the PIN to watch it.",
                    crate::ui::text::cap(name, 60)
                ),
                None => "This is in a locked category. Enter the PIN to watch it.".into(),
            },
            AppError::Internal => {
                "Something went wrong on our side. Try again; details are in the log.".into()
            }
        }
    }
}

/// Explains a non-active account status.
pub fn status_message(s: &AccountStatus) -> String {
    match s {
        AccountStatus::Expired => {
            "This subscription has expired. Contact your provider to renew it.".into()
        }
        AccountStatus::Banned => "This account has been banned by the provider.".into(),
        AccountStatus::Disabled => "This account has been disabled by the provider.".into(),
        AccountStatus::Active => "This account is active.".into(),
        other => format!(
            "The provider reports the account status \u{201c}{}\u{201d}.",
            crate::ui::text::cap(other.as_str(), 40)
        ),
    }
}

/// Result alias.
pub type AppResult<T> = Result<T, AppError>;

// SPDX-License-Identifier: GPL-3.0-or-later
//! Error types.

use std::fmt;
use std::path::PathBuf;

/// libmpv could not be loaded.
///
/// The `Display` text is suitable for showing to the user as-is; it never
/// contains a media URL.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// No candidate file existed.
    #[error(
        "{} not found (looked next to the executable{}). See docs/LIBMPV.md for how to install it.",
        crate::ffi::LIBMPV_FILE_NAME,
        if cfg!(debug_assertions) { ", in vendor/libmpv and on PATH" } else { " and on PATH" }
    )]
    NotFound {
        /// Every absolute path that was checked, in order.
        tried: Vec<PathBuf>,
    },
    /// A file was found but the OS refused to load it (wrong architecture,
    /// missing dependency such as `vulkan-1.dll`, corrupt file…).
    #[error("failed to load {path:?}: {message}. See docs/LIBMPV.md.")]
    Open {
        /// The file that failed to load.
        path: PathBuf,
        /// OS loader message.
        message: String,
    },
    /// The library lacks a function we need (too old or not libmpv).
    #[error("libmpv is missing symbol {symbol}: {message}")]
    MissingSymbol {
        /// C symbol name.
        symbol: String,
        /// Loader message.
        message: String,
    },
    /// Client API major version mismatch.
    #[error("libmpv client API {found} is not supported (need {required})")]
    IncompatibleVersion {
        /// Version reported by the library.
        found: String,
        /// Version this crate supports.
        required: String,
    },
}

/// A libmpv error code plus its description from `mpv_error_string`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpvError {
    /// Raw negative `mpv_error` code.
    pub code: i32,
    /// Human-readable text from libmpv (never contains URLs).
    pub message: String,
}

impl fmt::Display for MpvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for MpvError {}

/// Errors returned by [`crate::Player`] and friends.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// libmpv could not be loaded.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// `mpv_create` returned NULL (out of memory or similar).
    #[error("mpv_create failed")]
    Create,
    /// A hardened option could not be applied, so the player refuses to start.
    #[error("failed to set mandatory mpv option `{name}`: {source}")]
    MandatoryOption {
        /// Option name (never a URL).
        name: String,
        /// Underlying mpv error.
        source: MpvError,
    },
    /// `mpv_initialize` failed.
    #[error("mpv_initialize failed: {0}")]
    Initialize(MpvError),
    /// A libmpv call failed.
    #[error("mpv call `{op}` failed: {source}")]
    Mpv {
        /// Short operation name (never a URL).
        op: &'static str,
        /// Underlying mpv error.
        source: MpvError,
    },
    /// The URL's scheme is valid but not enabled in this player's
    /// [`crate::UrlPolicy`] (its ffmpeg protocol whitelist would block it).
    #[error("this player was not configured to allow `{0}` URLs")]
    SchemeNotEnabled(String),
    /// A local-file [`crate::PlayableUrl`] was passed to a player built
    /// without [`crate::PlayerConfig::allow_local_files`].
    #[error("local files are disabled for this player")]
    LocalFilesDisabled,
    /// Invalid argument (value out of range, bad track id…).
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
    /// The render context could not be created.
    #[error("render context: {0}")]
    Render(String),
}

/// Why a URL, header value or other untrusted input was rejected.
///
/// Messages never echo the rejected input (it may contain credentials).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// Empty input.
    #[error("empty")]
    Empty,
    /// Longer than the allowed maximum (bytes).
    #[error("too long (max {max} bytes)")]
    TooLong {
        /// Maximum allowed length in bytes.
        max: usize,
    },
    /// Contains a control character (including CR, LF, TAB, NUL, DEL).
    #[error("contains a control character")]
    ControlChar,
    /// Contains whitespace.
    #[error("contains whitespace")]
    Whitespace,
    /// Contains a backslash (parsers disagree on its meaning in URLs).
    #[error("contains a backslash")]
    Backslash,
    /// Not a parseable absolute URL.
    #[error("not a valid absolute URL")]
    Malformed,
    /// Scheme not on the allowlist.
    #[error("scheme `{0}` is not allowed")]
    SchemeNotAllowed(String),
    /// URL has no host (or an empty one).
    #[error("missing host")]
    MissingHost,
    /// Local path is not absolute or does not name a file.
    #[error("not an absolute path to an existing file")]
    BadLocalPath,
}

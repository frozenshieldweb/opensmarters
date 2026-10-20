//! Input validation and sanitising for strings that end up in the database.
//!
//! Two flavours:
//! - *validate* — user/app-controlled identifiers and settings: reject bad input with
//!   [`StoreError::Invalid`] so bugs surface.
//! - *sanitise* — provider-supplied display strings (hostile input): never fail, strip
//!   control characters and cap the length so a hostile panel cannot bloat the database.

use crate::error::{Result, StoreError};

/// Max length (chars) of profile display names.
pub(crate) const MAX_PROFILE_NAME_CHARS: usize = 128;
/// Max length (bytes) of URLs and paths we keep.
pub(crate) const MAX_URL_BYTES: usize = 2048;
/// Max length (bytes) of provider item ids (stream ids, series ids, category ids, …).
pub(crate) const MAX_ITEM_ID_BYTES: usize = 256;
/// Max length (chars) of provider display names / titles in snapshots.
pub(crate) const MAX_TITLE_CHARS: usize = 512;
/// Max length (bytes) of short provider tokens such as a container extension.
pub(crate) const MAX_TOKEN_BYTES: usize = 16;

pub(crate) fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Trims and validates a user-entered name.
pub(crate) fn validate_name(field: &'static str, s: &str, max_chars: usize) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(StoreError::invalid(field, "must not be empty"));
    }
    if has_control(t) {
        return Err(StoreError::invalid(
            field,
            "must not contain control characters",
        ));
    }
    if t.chars().count() > max_chars {
        return Err(StoreError::invalid(field, "is too long"));
    }
    Ok(t.to_owned())
}

/// Validates a provider item id used as a key (favourites, history, locks).
///
/// Ids are keys, so they are rejected rather than truncated (truncation would merge
/// distinct items). URLs are rejected outright: M3U stream URLs routinely embed
/// credentials and must never be used as ids.
pub(crate) fn validate_item_id<'a>(field: &'static str, s: &'a str) -> Result<&'a str> {
    if s.is_empty() {
        return Err(StoreError::invalid(field, "must not be empty"));
    }
    if s.len() > MAX_ITEM_ID_BYTES {
        return Err(StoreError::invalid(field, "is too long"));
    }
    if has_control(s) {
        return Err(StoreError::invalid(
            field,
            "must not contain control characters",
        ));
    }
    if s.contains("://") {
        return Err(StoreError::invalid(
            field,
            "must not be a URL (URLs can embed credentials)",
        ));
    }
    Ok(s)
}

/// Sanitises a provider display string: control characters become spaces, surrounding
/// whitespace is trimmed and the result is capped at `max_chars`.
pub(crate) fn sanitize_display(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars * 4));
    for c in s.trim().chars().take(max_chars) {
        out.push(if c.is_control() { ' ' } else { c });
    }
    out.trim_end().to_owned()
}

/// Sanitises an optional provider string that only makes sense whole (URLs, ids):
/// dropped (None) when empty, too long or containing control characters.
pub(crate) fn sanitize_opt(s: Option<&str>, max_bytes: usize) -> Option<String> {
    let t = s?.trim();
    if t.is_empty() || t.len() > max_bytes || has_control(t) {
        None
    } else {
        Some(t.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_strings_are_capped_and_cleaned() {
        assert_eq!(sanitize_display("  a\u{0}b\nc  ", 10), "a b c");
        assert_eq!(sanitize_display("ééééé", 3), "ééé");
        assert_eq!(sanitize_display("", 3), "");
    }

    #[test]
    fn item_ids() {
        assert!(validate_item_id("id", "12345").is_ok());
        assert!(validate_item_id("id", "").is_err());
        assert!(validate_item_id("id", "http://h/live/u/p/1.ts").is_err());
        assert!(validate_item_id("id", "a\nb").is_err());
        assert!(validate_item_id("id", &"x".repeat(MAX_ITEM_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn optional_values() {
        assert_eq!(sanitize_opt(Some("  "), 10), None);
        assert_eq!(sanitize_opt(Some(" x "), 10).as_deref(), Some("x"));
        assert_eq!(sanitize_opt(Some("0123456789a"), 10), None);
        assert_eq!(sanitize_opt(None, 10), None);
    }

    #[test]
    fn names() {
        assert_eq!(
            validate_name("n", "  Home  ", 10).ok().as_deref(),
            Some("Home")
        );
        assert!(validate_name("n", "   ", 10).is_err());
        assert!(validate_name("n", "a\tb", 10).is_err());
        assert!(validate_name("n", "elevenchars", 10).is_err());
    }
}

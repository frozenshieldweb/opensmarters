//! `GET /__updates/releases/latest`: a GitHub-shaped "latest release" answer, so the app's
//! update check can be exercised (and screenshotted) without asking the real GitHub API.
//!
//! Query parameters (all optional):
//!
//! | Parameter | Effect |
//! |---|---|
//! | `tag=v0.3.0` | the `tag_name` (≤ 128 bytes, sent as given, even if invalid; default: this crate's version with the minor bumped, e.g. `v0.3.0` for 0.2.0) |
//! | `status=429` | answer with that HTTP status and a GitHub-style error object instead |
//! | `draft=true`, `prerelease=true` | set those flags |
//! | `big=1` | pad the body to about 1 MiB (more than the app's 256 KiB cap) |
//! | `delay_ms=5000` | wait that long first (≤ 30 s), to see the "checking" state |
//!
//! The answer also carries an `html_url` and a `body` with markup that a client must
//! **never** use or render.

use std::time::Duration;

use axum::extract::Query;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::server::Params;

/// Longest `tag` accepted (the app itself refuses anything over 64).
const MAX_TAG: usize = 128;

/// The default tag: this crate's version with the minor bumped (`0.2.0` → `v0.3.0`).
pub(crate) fn default_tag() -> String {
    let v = env!("CARGO_PKG_VERSION");
    let mut parts = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    format!("v{major}.{}.0", minor.saturating_add(1))
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true" | "yes"))
}

pub(crate) async fn latest(Query(pairs): Query<Vec<(String, String)>>) -> Response {
    let p = Params::from_pairs(pairs);
    if let Some(ms) = p.opt("delay_ms").and_then(|v| v.parse::<u64>().ok()) {
        tokio::time::sleep(Duration::from_millis(ms.min(30_000))).await;
    }
    if let Some(status) = p
        .opt("status")
        .and_then(|s| s.parse::<u16>().ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .filter(|s| !s.is_success())
    {
        let message = match status.as_u16() {
            403 | 429 => "API rate limit exceeded for 127.0.0.1. (fake-panel)",
            404 => "Not Found",
            _ => "Server Error",
        };
        let body = json!({
            "message": message,
            "documentation_url": "https://docs.github.com/rest",
            "status": status.as_u16().to_string(),
        });
        return (
            status,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            body.to_string(),
        )
            .into_response();
    }
    let tag = match p.opt("tag") {
        Some(t) if t.len() <= MAX_TAG => t.to_owned(),
        Some(_) => return (StatusCode::BAD_REQUEST, "tag too long").into_response(),
        None => default_tag(),
    };
    let notes = if truthy(p.opt("big")) {
        "x".repeat(1024 * 1024)
    } else {
        "<script>alert('release notes are never rendered')</script> **Bold** [link](https://example.invalid/)".to_owned()
    };
    let body = json!({
        "url": "https://api.github.com/repos/example/fake/releases/1",
        "html_url": "https://example.invalid/never-open-this",
        "id": 1,
        "tag_name": tag,
        "target_commitish": "main",
        "name": format!("opensmarters {tag}"),
        "draft": truthy(p.opt("draft")),
        "prerelease": truthy(p.opt("prerelease")),
        "created_at": "2026-10-20T12:00:00Z",
        "published_at": "2026-10-20T12:00:00Z",
        "assets": [{
            "name": "opensmarters-setup.exe",
            "browser_download_url": "https://example.invalid/never-download-this.exe",
        }],
        "body": notes,
    });
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tag_bumps_the_minor() {
        let t = default_tag();
        assert!(t.starts_with('v'), "{t}");
        assert!(t.ends_with(".0"), "{t}");
        assert_ne!(t, format!("v{}", env!("CARGO_PKG_VERSION")));
    }
}

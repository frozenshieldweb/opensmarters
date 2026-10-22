// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser UI (`web/dist`, built by `npm run build` in `web/`), served from the
//! binary.
//!
//! - **Release builds embed `web/dist` at compile time** (`rust-embed`); the Docker
//!   image builds the UI first, then the server. **Debug builds read `web/dist` from
//!   disk** at request time, so a rebuilt UI shows up without rebuilding the server.
//!   Without `web/dist` the server still builds and serves a short built-in page that
//!   says the UI isn't built (tests and `cargo build` work either way).
//! - SPA fallback: a path without a file extension that isn't under `/api` gets
//!   `index.html` (client-side routes); a missing file with an extension (`.js`,
//!   `.css`, images…) is a 404.
//! - Caching: Vite's content-hashed `assets/*` are cached for a year (`immutable`);
//!   `index.html` is `no-store`; anything else (manifest, service worker, icons) is
//!   revalidated (`no-cache`, with an `ETag`).
//! - Security headers on every page: the CSP from WEB_PROMPT §5 (no inline script or
//!   style), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
//!   `X-Frame-Options: DENY`, `Cross-Origin-Opener-Policy: same-origin`, and a
//!   `Permissions-Policy` that turns off sensors and devices.

use std::borrow::Cow;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
#[allow_missing = true]
struct Dist;

/// The SPA's Content Security Policy.
pub const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; \
connect-src 'self'; style-src 'self'; script-src 'self'; font-src 'self'; manifest-src 'self'; \
worker-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";

const PLACEHOLDER_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>opensmarters web</title>
<link rel="stylesheet" href="/__opensmarters/placeholder.css">
</head>
<body>
<main>
<h1>opensmarters web</h1>
<p>The server is running, but its browser interface wasn't built into this binary.</p>
<p>Build the interface with <code>npm ci &amp;&amp; npm run build</code> in <code>web/</code>, then rebuild the server (release builds embed <code>web/dist</code>; debug builds read it from disk). The Docker image does both.</p>
<p>The API is available under <code>/api</code>, for example <a href="/api/status">/api/status</a>.</p>
</main>
</body>
</html>
"#;

const PLACEHOLDER_CSS: &str = "body{margin:0;min-height:100vh;display:grid;place-items:center;\
background:#0f0d14;color:#ece6f5;font:16px/1.6 system-ui,sans-serif}\
main{max-width:36rem;padding:2rem}h1{font-weight:600;letter-spacing:.01em}\
code{background:#221d2b;padding:.1rem .35rem;border-radius:.3rem}a{color:#ffb38a}";

/// Adds the page security headers.
pub fn page_headers(h: &mut HeaderMap) {
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), usb=(), payment=()"),
    );
}

/// Whether a real UI is available (embedded or, in debug builds, on disk).
pub fn ui_built() -> bool {
    Dist::get("index.html").is_some()
}

fn respond(
    status: StatusCode,
    body: Cow<'static, [u8]>,
    mime: &str,
    cache: &'static str,
    etag: Option<String>,
) -> Response {
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Some(tag) = etag.and_then(|t| HeaderValue::from_str(&t).ok()) {
        h.insert(header::ETAG, tag);
    }
    page_headers(h);
    res
}

fn index() -> Response {
    match Dist::get("index.html") {
        Some(f) => respond(
            StatusCode::OK,
            f.data,
            "text/html; charset=utf-8",
            "no-store",
            None,
        ),
        None => respond(
            StatusCode::OK,
            Cow::Borrowed(PLACEHOLDER_HTML.as_bytes()),
            "text/html; charset=utf-8",
            "no-store",
            None,
        ),
    }
}

fn not_found() -> Response {
    respond(
        StatusCode::NOT_FOUND,
        Cow::Borrowed(b"Not found\n"),
        "text/plain; charset=utf-8",
        "no-store",
        None,
    )
}

/// Serves a non-API path.
pub async fn serve(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed\n").into_response();
    }
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() || path == "index.html" {
        return index();
    }
    if path == "__opensmarters/placeholder.css" {
        return respond(
            StatusCode::OK,
            Cow::Borrowed(PLACEHOLDER_CSS.as_bytes()),
            "text/css; charset=utf-8",
            "no-cache",
            None,
        );
    }
    if path
        .split('/')
        .any(|seg| seg == ".." || seg.starts_with('.'))
        || path.contains('\\')
    {
        return not_found();
    }
    if let Some(file) = Dist::get(path) {
        let etag = format!(
            "\"{}\"",
            crate::crypto::hex(&file.metadata.sha256_hash()[..16])
        );
        if headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim() == etag))
        {
            let mut res = respond(
                StatusCode::NOT_MODIFIED,
                Cow::Borrowed(b""),
                "text/plain",
                "no-cache",
                Some(etag),
            );
            res.headers_mut().remove(header::CONTENT_TYPE);
            return res;
        }
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        let mime = file.metadata.mimetype().to_owned();
        let mime = if mime.starts_with("text/")
            || mime == "application/javascript"
            || mime == "application/json"
            || mime.ends_with("+json")
        {
            format!("{mime}; charset=utf-8")
        } else {
            mime
        };
        return respond(StatusCode::OK, file.data, &mime, cache, Some(etag));
    }
    let last = path.rsplit('/').next().unwrap_or(path);
    if last.contains('.') && !last.ends_with(".html") {
        return not_found();
    }
    index()
}

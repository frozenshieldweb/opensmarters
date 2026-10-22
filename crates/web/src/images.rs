// SPDX-License-Identifier: GPL-3.0-or-later
//! The image proxy: `GET /api/img/<token>`.
//!
//! Every image address in an API answer is replaced by a token that only this server
//! can make ([`crate::crypto::Keys::image_token`]: authenticated encryption of the
//! provider's address), so the proxy is **not an open proxy** and the browser never sees
//! a provider address. A request:
//!
//! 1. needs a session, and a token this server issued (else 404);
//! 2. is served from the disk cache (`<data>/cache/images`, 7 days, 256 MiB, oldest
//!    files evicted first) or fetched through the SSRF-safe client (≤ 5 MB, 20 s, ≤ 3
//!    redirects);
//! 3. must be an image: a `Content-Type` of `image/*` (or a generic binary type) **and**
//!    PNG / JPEG / GIF / WebP magic bytes (no SVG, no HTML); the header of the image is
//!    decoded to check its size (≤ 8192 px per side, ≤ 40 megapixels);
//! 4. is answered with the sniffed type, `X-Content-Type-Options: nosniff` and
//!    `Content-Security-Policy: default-src 'none'; sandbox`.
//!
//! Failures are remembered for 5 minutes so a broken logo isn't fetched again for every
//! row; at most 8 downloads run at once.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use opensmarters_core::url::Url;

use crate::error::ApiError;
use crate::net::{NetError, Purpose};
use crate::state::AppState;

const MAX_SIDE: u32 = 8192;
const MAX_PIXELS: u64 = 40_000_000;
const CACHE_CAP: u64 = 256 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const FAIL_TTL: Duration = Duration::from_secs(300);

/// The image proxy's state.
#[derive(Debug)]
pub struct ImageProxy {
    dir: PathBuf,
    bytes: AtomicU64,
    permits: tokio::sync::Semaphore,
    flights: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    failures: Mutex<HashMap<String, Instant>>,
}

/// A recognised image format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    Webp,
}

impl Format {
    fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Gif => "image/gif",
            Format::Webp => "image/webp",
        }
    }
}

/// The format from the first bytes.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else {
        None
    }
}

/// Checks the declared type, the magic bytes and the decoded size.
///
/// # Errors
///
/// A short reason (logged, not shown).
pub fn validate(bytes: &[u8], content_type: Option<&str>) -> Result<Format, &'static str> {
    if let Some(ct) = content_type {
        let ct = ct.trim().to_ascii_lowercase();
        let generic =
            ct.starts_with("application/octet-stream") || ct.starts_with("binary/octet-stream");
        if !(ct.starts_with("image/") || generic) || ct.starts_with("image/svg") {
            return Err("not an image content type");
        }
    }
    let format = sniff(bytes).ok_or("unknown image format")?;
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "unreadable image")?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|_| "unreadable image header")?;
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS
    {
        return Err("image dimensions out of bounds");
    }
    Ok(format)
}

fn image_response(bytes: Vec<u8>, format: Format) -> Response {
    let mut res = Response::new(Body::from(bytes));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.mime()),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    res
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .filter(std::fs::Metadata::is_file)
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Deletes expired files, then the oldest until the folder is under 80 % of the cap.
fn trim(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(modified).unwrap_or_default() > CACHE_TTL {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        files.push((modified, meta.len(), e.path()));
    }
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, path) in &files {
        if total <= CACHE_CAP * 8 / 10 {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(*len);
        }
    }
    total
}

impl ImageProxy {
    /// Uses (and creates) `dir` as the disk cache.
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let bytes = trim(&dir).max(dir_size(&dir).min(CACHE_CAP));
        ImageProxy {
            dir,
            bytes: AtomicU64::new(bytes),
            permits: tokio::sync::Semaphore::new(8),
            flights: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    fn failed_recently(&self, name: &str) -> bool {
        let mut f = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        if f.len() > 10_000 {
            f.retain(|_, at| at.elapsed() < FAIL_TTL);
        }
        f.get(name).is_some_and(|at| at.elapsed() < FAIL_TTL)
    }

    fn remember_failure(&self, name: &str) {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), Instant::now());
    }

    async fn read_cached(&self, path: &Path) -> Option<(Vec<u8>, Format)> {
        let meta = tokio::fs::metadata(path).await.ok()?;
        let age = SystemTime::now()
            .duration_since(meta.modified().ok()?)
            .unwrap_or_default();
        if age > CACHE_TTL {
            return None;
        }
        let bytes = tokio::fs::read(path).await.ok()?;
        let format = sniff(&bytes)?;
        Some((bytes, format))
    }

    async fn write_cached(&self, path: &Path, bytes: &[u8]) {
        let tmp = path.with_extension("part");
        if tokio::fs::write(&tmp, bytes).await.is_ok()
            && tokio::fs::rename(&tmp, path).await.is_ok()
        {
            let total =
                self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed) + bytes.len() as u64;
            if total > CACHE_CAP {
                let dir = self.dir.clone();
                if let Ok(after) = tokio::task::spawn_blocking(move || trim(&dir)).await {
                    self.bytes.store(after, Ordering::Relaxed);
                }
            }
        } else {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }

    /// Answers `GET /api/img/<token>` (the session was checked by the caller).
    pub async fn serve(&self, state: &AppState, token: &str) -> Response {
        let Some(address) = state.keys.image_address(token) else {
            return ApiError::not_found("No such image.").into_response();
        };
        let name = state.keys.image_cache_name(&address);
        let path = self.dir.join(&name);
        if let Some((bytes, format)) = self.read_cached(&path).await {
            return image_response(bytes, format);
        }
        if self.failed_recently(&name) {
            return unavailable();
        }
        let flight = {
            let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            if flights.len() > 1000 {
                flights.retain(|_, f| Arc::strong_count(f) > 1);
            }
            Arc::clone(flights.entry(name.clone()).or_default())
        };
        let _guard = flight.lock().await;
        if let Some((bytes, format)) = self.read_cached(&path).await {
            return image_response(bytes, format);
        }
        let Ok(_permit) = self.permits.acquire().await else {
            return unavailable();
        };
        let Ok(url) = Url::parse(&address) else {
            self.remember_failure(&name);
            return unavailable();
        };
        match state.net.get(&url, Purpose::Image).await {
            Ok((bytes, content_type)) => match validate(&bytes, content_type.as_deref()) {
                Ok(format) => {
                    self.write_cached(&path, &bytes).await;
                    image_response(bytes, format)
                }
                Err(why) => {
                    tracing::debug!(why, "image refused");
                    self.remember_failure(&name);
                    unavailable()
                }
            },
            Err(e) => {
                if !matches!(e, NetError::Timeout) {
                    self.remember_failure(&name);
                }
                tracing::debug!(error = %e, "image download failed");
                unavailable()
            }
        }
    }
}

fn unavailable() -> Response {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "image_unavailable",
        "This image isn't available.",
    )
    .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::new(w, h);
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn validation() {
        let ok = png(4, 3);
        assert_eq!(validate(&ok, Some("image/png")), Ok(Format::Png));
        assert_eq!(validate(&ok, None), Ok(Format::Png));
        assert_eq!(
            validate(&ok, Some("application/octet-stream")),
            Ok(Format::Png)
        );
        assert!(validate(&ok, Some("text/html")).is_err());
        assert!(
            validate(
                b"<svg xmlns='http://www.w3.org/2000/svg'/>",
                Some("image/svg+xml")
            )
            .is_err()
        );
        assert!(validate(b"<html>", Some("image/png")).is_err());
        assert!(validate(&png(MAX_SIDE + 1, 1), Some("image/png")).is_err());
        // A PNG header claiming 100000 x 100000 pixels.
        let mut huge = ok.clone();
        huge[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(validate(&huge, Some("image/png")).is_err());
    }
}

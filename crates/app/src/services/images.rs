// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider images (logos, posters, backdrops): fetched with the core's capped HTTP
//! client (10 MB, timeouts, http/https only, no cookies), decoded with dimension and
//! allocation limits, downscaled to a size bucket, uploaded as egui textures, and cached
//! in memory (LRU by bytes) and on disk (`<data>/images`, PNG, size-capped).
//!
//! Non-blocking: [`ImageService::get`] returns [`ImageSlot::Loading`] and starts a fetch;
//! the UI is woken when the texture is ready. Draw a placeholder until then (the card
//! widgets do).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use lru::LruCache;
use opensmarters_core::net::HttpClient;
use opensmarters_core::{HttpConfig, Redactor};
use tokio::runtime::Handle;

/// Maximum image download size.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum decoded dimension (either side) accepted from a provider.
pub const MAX_IMAGE_DIMENSION: u32 = 8192;
/// Memory budget for decoded textures.
pub const MEMORY_BUDGET_BYTES: usize = 192 * 1024 * 1024;
/// Disk cache cap (trimmed to 80% at startup when exceeded).
pub const DISK_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// Concurrent downloads.
pub const MAX_CONCURRENT: usize = 6;
/// Failed images are retried after this long.
pub const RETRY_FAILED_AFTER: Duration = Duration::from_secs(300);

/// Target size class for decoding (longest side in pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageSize {
    /// Channel logos, avatars (≤ 256 px).
    Logo,
    /// Poster cards (≤ 512 px).
    Poster,
    /// Backdrop cards (≤ 768 px).
    Card,
    /// Full-bleed backdrops (≤ 1920 px).
    Backdrop,
}

impl ImageSize {
    fn max_side(self) -> u32 {
        match self {
            ImageSize::Logo => 256,
            ImageSize::Poster => 512,
            ImageSize::Card => 768,
            ImageSize::Backdrop => 1920,
        }
    }
}

/// The state of one image.
#[derive(Clone)]
pub enum ImageSlot {
    /// No URL, or the URL is not usable (draw the placeholder).
    None,
    /// Downloading/decoding (draw the placeholder or a skeleton).
    Loading,
    /// Ready to draw.
    Ready(TextureHandle),
    /// Failed (draw the placeholder).
    Failed,
}

impl std::fmt::Debug for ImageSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageSlot::None => f.write_str("None"),
            ImageSlot::Loading => f.write_str("Loading"),
            ImageSlot::Ready(t) => write!(f, "Ready({:?})", t.size()),
            ImageSlot::Failed => f.write_str("Failed"),
        }
    }
}

impl ImageSlot {
    /// The texture, if ready.
    pub fn texture(&self) -> Option<&TextureHandle> {
        match self {
            ImageSlot::Ready(t) => Some(t),
            _ => None,
        }
    }
}

type Key = (String, ImageSize);

enum Entry {
    Ready(TextureHandle, usize),
    Failed(Instant),
}

struct Inner {
    rt: Handle,
    ctx: egui::Context,
    http: Option<HttpClient>,
    disk: Option<PathBuf>,
    mem: Mutex<LruCache<Key, Entry>>,
    mem_bytes: Mutex<usize>,
    inflight: Mutex<HashSet<Key>>,
    permits: Arc<tokio::sync::Semaphore>,
}

/// The image service (cheap to clone).
#[derive(Clone)]
pub struct ImageService {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ImageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageService")
            .field("disk", &self.inner.disk)
            .finish_non_exhaustive()
    }
}

impl ImageService {
    /// Creates the service. `disk` is the cache directory (`None` = memory only).
    pub fn new(rt: Handle, ctx: egui::Context, redactor: Redactor, disk: Option<PathBuf>) -> Self {
        let config = HttpConfig {
            max_response_bytes: MAX_IMAGE_BYTES,
            request_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(15),
            ..HttpConfig::default()
        };
        let http = match HttpClient::new(&config, redactor) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(error = %e, "image downloads disabled");
                None
            }
        };
        if let Some(dir) = &disk {
            let dir = dir.clone();
            rt.spawn_blocking(move || trim_disk_cache(&dir, DISK_BUDGET_BYTES));
        }
        Self {
            inner: Arc::new(Inner {
                rt,
                ctx,
                http,
                disk,
                mem: Mutex::new(LruCache::unbounded()),
                mem_bytes: Mutex::new(0),
                inflight: Mutex::new(HashSet::new()),
                permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT)),
            }),
        }
    }

    /// The image at `url` in size class `size`. Starts a fetch when needed.
    pub fn get(&self, url: Option<&str>, size: ImageSize) -> ImageSlot {
        let Some(url) = url.map(str::trim).filter(|u| is_fetchable(u)) else {
            return ImageSlot::None;
        };
        let key: Key = (url.to_owned(), size);
        {
            let mut mem = self
                .inner
                .mem
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match mem.get(&key) {
                Some(Entry::Ready(t, _)) => return ImageSlot::Ready(t.clone()),
                Some(Entry::Failed(at)) if at.elapsed() < RETRY_FAILED_AFTER => {
                    return ImageSlot::Failed;
                }
                _ => {}
            }
        }
        {
            let mut inflight = self
                .inner
                .inflight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !inflight.insert(key.clone()) {
                return ImageSlot::Loading;
            }
        }
        let inner = Arc::clone(&self.inner);
        self.inner.rt.spawn(async move {
            let permits = Arc::clone(&inner.permits);
            let Ok(_permit) = permits.acquire_owned().await else {
                return;
            };
            let result = load(&inner, &key).await;
            let entry = match result {
                Ok(image) => {
                    let bytes = image.width() * image.height() * 4;
                    let tex = inner.ctx.load_texture(
                        format!("img:{:016x}", super::stable_hash(&[&key.0])),
                        image,
                        TextureOptions {
                            mipmap_mode: Some(egui::TextureFilter::Linear),
                            ..TextureOptions::LINEAR
                        },
                    );
                    Entry::Ready(tex, bytes)
                }
                Err(reason) => {
                    tracing::debug!(%reason, "image unavailable");
                    Entry::Failed(Instant::now())
                }
            };
            {
                let mut mem = inner.mem.lock().unwrap_or_else(PoisonError::into_inner);
                let mut total = inner
                    .mem_bytes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if let Entry::Ready(_, b) = &entry {
                    *total += b;
                }
                if let Some(Entry::Ready(_, old)) = mem.put(key.clone(), entry) {
                    *total = total.saturating_sub(old);
                }
                while *total > MEMORY_BUDGET_BYTES {
                    match mem.pop_lru() {
                        Some((_, Entry::Ready(_, b))) => *total = total.saturating_sub(b),
                        Some(_) => {}
                        None => break,
                    }
                }
            }
            inner
                .inflight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
            inner.ctx.request_repaint();
        });
        ImageSlot::Loading
    }

    /// Drops all textures (e.g. when switching profiles).
    pub fn clear_memory(&self) {
        self.inner
            .mem
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        *self
            .inner
            .mem_bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = 0;
    }

    /// Bytes of decoded textures held in memory.
    pub fn memory_bytes(&self) -> usize {
        *self
            .inner
            .mem_bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The disk cache directory.
    pub fn disk_dir(&self) -> Option<&Path> {
        self.inner.disk.as_deref()
    }
}

/// http/https only, bounded length, no whitespace/control characters.
pub fn is_fetchable(url: &str) -> bool {
    if url.is_empty()
        || url.len() > 2048
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn disk_path(dir: &Path, key: &Key) -> PathBuf {
    let size = match key.1 {
        ImageSize::Logo => "l",
        ImageSize::Poster => "p",
        ImageSize::Card => "c",
        ImageSize::Backdrop => "b",
    };
    let a = super::stable_hash(&[&key.0]);
    let b = super::stable_hash(&["opensmarters-img", &key.0]);
    dir.join(format!("{a:016x}{b:016x}-{size}.png"))
}

async fn load(inner: &Inner, key: &Key) -> Result<ColorImage, String> {
    // Disk cache first.
    if let Some(dir) = &inner.disk {
        let path = disk_path(dir, key);
        let from_disk = tokio::task::spawn_blocking(move || -> Option<ColorImage> {
            let bytes = std::fs::read(&path).ok()?;
            decode(&bytes, ImageSize::Backdrop).ok()
        })
        .await
        .ok()
        .flatten();
        if let Some(img) = from_disk {
            return Ok(img);
        }
    }
    let http = inner.http.as_ref().ok_or("downloads disabled")?;
    let url = opensmarters_core::url::Url::parse(&key.0).map_err(|_| "bad url".to_owned())?;
    let bytes = http.get_bytes(&url).await.map_err(|e| e.to_string())?;
    let size = key.1;
    let disk = inner.disk.as_ref().map(|d| disk_path(d, key));
    tokio::task::spawn_blocking(move || {
        let img = decode(&bytes, size)?;
        if let Some(path) = disk {
            save_png(&path, &img);
        }
        Ok(img)
    })
    .await
    .map_err(|_| "decode task failed".to_owned())?
}

/// Decodes with limits and downscales so the longest side is at most the size class.
fn decode(bytes: &[u8], size: ImageSize) -> Result<ColorImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?;
    let max = size.max_side();
    let img = if img.width() > max || img.height() > max {
        img.resize(max, max, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let dims = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(dims, rgba.as_raw()))
}

fn save_png(path: &Path, img: &ColorImage) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut raw = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        raw.extend_from_slice(&p.to_srgba_unmultiplied());
    }
    let (Ok(w), Ok(h)) = (u32::try_from(img.size[0]), u32::try_from(img.size[1])) else {
        return;
    };
    if let Some(buf) = image::RgbaImage::from_raw(w, h, raw) {
        let tmp = path.with_extension("tmp");
        if buf.save_with_format(&tmp, image::ImageFormat::Png).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Deletes the oldest files until the directory is under 80% of `budget`.
fn trim_disk_cache(dir: &Path, budget: u64) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| {
                (
                    m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    m.len(),
                    e.path(),
                )
            })
        })
        .collect();
    let total: u64 = files.iter().map(|f| f.1).sum();
    if total <= budget {
        return;
    }
    files.sort_by_key(|f| f.0);
    let mut remaining = total;
    for (_, len, path) in files {
        if remaining <= budget / 10 * 8 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining = remaining.saturating_sub(len);
        }
    }
}

/// Deletes the whole disk cache (Settings → Storage).
pub fn clear_disk_cache(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_are_fetched() {
        assert!(is_fetchable("https://img.example/a.png"));
        assert!(is_fetchable("HTTP://img.example/a.png"));
        assert!(!is_fetchable("file:///C:/x.png"));
        assert!(!is_fetchable("data:image/png;base64,AAA"));
        assert!(!is_fetchable("http://a b"));
        assert!(!is_fetchable(""));
    }

    #[test]
    fn decode_downscales_and_rejects_garbage() {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(1200, 600, image::Rgba([10, 20, 30, 255]));
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap_or_else(|e| panic!("{e}"));
        let out = decode(&buf, ImageSize::Poster).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.size, [512, 256]);
        assert!(decode(b"<html>not an image</html>", ImageSize::Logo).is_err());
    }
}

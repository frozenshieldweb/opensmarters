// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider images (logos, posters, backdrops): fetched with the core's capped HTTP
//! client (10 MB, timeouts, http/https only, no cookies), decoded with dimension and
//! allocation limits, downscaled to a size bucket, uploaded as egui textures, and cached
//! in memory (LRU by bytes) and on disk (`<data>/images`, PNG, size-capped).
//!
//! Non-blocking: [`ImageService::get`] returns [`ImageSlot::Loading`] and queues a fetch;
//! the UI is woken when the texture is ready. Draw a placeholder until then (the card
//! widgets do). Call `get` every frame for what is on screen:
//!
//! - **Bounded work**: downloads go through one queue
//!   ([`crate::services::fetch_queue`]) with [`MAX_CONCURRENT`] workers, the most
//!   recently drawn images first; an image that has not been drawn for [`WANTED_FOR`]
//!   is dropped from the queue before it is downloaded (paging through a 35 000-film
//!   grid never queues thousands of downloads). [`ImageService::cancel_pending`] drops
//!   the queue (profile switch).
//! - **Decode limits** (SECURITY.md SR-10): at most [`MAX_IMAGE_DIMENSION`] px per side,
//!   [`MAX_DECODE_BYTES`] of decoder memory, [`MAX_DECODES`] decodes at a time.
//! - **Failures** are remembered in a bounded LRU ([`MAX_FAILED`]) and retried after
//!   [`RETRY_FAILED_AFTER`].
//! - **Disk cache** is trimmed at startup and again after every [`TRIM_EVERY_BYTES`]
//!   written (to 80% of [`DISK_BUDGET_BYTES`]).
//! - **Private artwork** (SR-13): URLs registered with [`ImageService::mark_private`]
//!   (artwork of locked categories, registered by the session) are kept in memory only;
//!   their disk files are deleted. [`ImageService::clear_image_disk_cache`] wipes the
//!   disk cache (the session calls it when a parental PIN is set).
//! - **Redirects**: an `https` image may be redirected to `http` only when its URL
//!   carries no registered credential (artwork CDNs do this; API URLs never may, SR-07).

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use lru::LruCache;
use opensmarters_core::net::HttpClient;
use opensmarters_core::{HttpConfig, Redactor};
use tokio::runtime::Handle;

use super::fetch_queue::{FetchQueue, QueueStats};

/// Maximum image download size.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum decoded dimension (either side) accepted from a provider.
pub const MAX_IMAGE_DIMENSION: u32 = 4096;
/// Maximum decoder allocation per image.
pub const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
/// Decodes running at the same time.
pub const MAX_DECODES: usize = 2;
/// Memory budget for decoded textures.
pub const MEMORY_BUDGET_BYTES: usize = 192 * 1024 * 1024;
/// Disk cache cap (trimmed to 80% when exceeded).
pub const DISK_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// The disk cache is checked against its cap after this many bytes were written.
pub const TRIM_EVERY_BYTES: u64 = 16 * 1024 * 1024;
/// Concurrent downloads.
pub const MAX_CONCURRENT: usize = 6;
/// A queued image not drawn for this long is dropped before it is downloaded.
pub const WANTED_FOR: Duration = Duration::from_secs(2);
/// Queued downloads kept at most.
pub const MAX_QUEUED: usize = 512;
/// Failed images are retried after this long.
pub const RETRY_FAILED_AFTER: Duration = Duration::from_secs(300);
/// Failed images remembered at most.
pub const MAX_FAILED: usize = 2048;
/// Private (memory-only) image URLs remembered at most; beyond that, every image is
/// treated as private.
pub const MAX_PRIVATE: usize = 262_144;

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

    const ALL: [ImageSize; 4] = [
        ImageSize::Logo,
        ImageSize::Poster,
        ImageSize::Card,
        ImageSize::Backdrop,
    ];
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

struct Inner {
    rt: Handle,
    ctx: egui::Context,
    redactor: Redactor,
    /// Refuses https → http redirects (URLs that might carry credentials).
    http: Option<HttpClient>,
    /// Allows https → http redirects (credential-free artwork URLs only).
    http_lenient: Option<HttpClient>,
    disk: Option<PathBuf>,
    mem: Mutex<LruCache<Key, (TextureHandle, usize)>>,
    mem_bytes: Mutex<usize>,
    failed: Mutex<LruCache<Key, Instant>>,
    queue: FetchQueue<Key, ()>,
    decodes: Arc<tokio::sync::Semaphore>,
    private: Mutex<(HashSet<u64>, bool)>,
    written_since_trim: AtomicU64,
    trimming: Arc<AtomicBool>,
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

fn url_hash(url: &str) -> u64 {
    super::stable_hash(&["opensmarters-private-img", url])
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
        let client = |c: &HttpConfig| match HttpClient::new(c, redactor.clone()) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(error = %e, "image downloads disabled");
                None
            }
        };
        let http = client(&config);
        let http_lenient = client(&HttpConfig {
            allow_https_downgrade: true,
            ..config
        });
        let trimming = Arc::new(AtomicBool::new(false));
        if let Some(dir) = &disk {
            spawn_trim(&rt, dir.clone(), Arc::clone(&trimming));
        }
        Self {
            inner: Arc::new(Inner {
                rt,
                ctx,
                redactor,
                http,
                http_lenient,
                disk,
                mem: Mutex::new(LruCache::unbounded()),
                mem_bytes: Mutex::new(0),
                failed: Mutex::new(LruCache::new(
                    NonZeroUsize::new(MAX_FAILED).unwrap_or(NonZeroUsize::MIN),
                )),
                queue: FetchQueue::new(MAX_CONCURRENT, WANTED_FOR, MAX_QUEUED),
                decodes: Arc::new(tokio::sync::Semaphore::new(MAX_DECODES)),
                private: Mutex::new((HashSet::new(), false)),
                written_since_trim: AtomicU64::new(0),
                trimming,
            }),
        }
    }

    /// The image at `url` in size class `size`. Queues a fetch when needed; call it every
    /// frame while the image is on screen (that keeps its request wanted).
    pub fn get(&self, url: Option<&str>, size: ImageSize) -> ImageSlot {
        let Some(url) = url.map(str::trim).filter(|u| is_fetchable(u)) else {
            return ImageSlot::None;
        };
        let key: Key = (url.to_owned(), size);
        if let Some((t, _)) = self
            .inner
            .mem
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return ImageSlot::Ready(t.clone());
        }
        {
            let mut failed = self
                .inner
                .failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match failed.get(&key) {
                Some(at) if at.elapsed() < RETRY_FAILED_AFTER => return ImageSlot::Failed,
                Some(_) => {
                    failed.pop(&key);
                }
                None => {}
            }
        }
        if self.inner.queue.contains(&key) {
            self.inner.queue.touch(&key);
            return ImageSlot::Loading;
        }
        let r = self.inner.queue.request(key, (), |_, ()| {});
        if r.spawn_worker {
            let inner = Arc::clone(&self.inner);
            self.inner.rt.spawn(async move { worker(&inner).await });
        }
        ImageSlot::Loading
    }

    /// Drops every queued (not yet started) download, e.g. when the profile changes.
    pub fn cancel_pending(&self) {
        let _ = self.inner.queue.clear_pending();
    }

    /// Queue counters (pending, running, dropped, peak concurrency) for logs and tests.
    pub fn queue_stats(&self) -> QueueStats {
        self.inner.queue.stats()
    }

    /// Keeps these artwork URLs out of the disk cache (memory only) and deletes files
    /// already cached for them. Used for items of locked categories.
    pub fn mark_private<'a>(&self, urls: impl IntoIterator<Item = &'a str>) {
        let mut fresh = Vec::new();
        {
            let mut p = self
                .inner
                .private
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for u in urls {
                let u = u.trim();
                if u.is_empty() {
                    continue;
                }
                if p.0.len() >= MAX_PRIVATE {
                    p.1 = true;
                    break;
                }
                if p.0.insert(url_hash(u)) {
                    fresh.push(u.to_owned());
                }
            }
        }
        if fresh.is_empty() {
            return;
        }
        if let Some(dir) = self.inner.disk.clone() {
            self.inner.rt.spawn_blocking(move || {
                for u in &fresh {
                    for size in ImageSize::ALL {
                        let _ = std::fs::remove_file(disk_path(&dir, &(u.clone(), size)));
                    }
                }
            });
        }
    }

    /// True if `url` must not be written to (or read from) the disk cache.
    pub fn is_private(&self, url: &str) -> bool {
        let p = self
            .inner
            .private
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        p.1 || p.0.contains(&url_hash(url.trim()))
    }

    /// Deletes the whole disk image cache in the background (memory textures stay).
    /// Called when a parental PIN is set, so artwork cached before cannot be browsed in
    /// Explorer.
    pub fn clear_image_disk_cache(&self) {
        if let Some(dir) = self.inner.disk.clone() {
            self.inner.rt.spawn_blocking(move || {
                if let Err(e) = clear_disk_cache(&dir) {
                    tracing::warn!(error = %e, "couldn't clear the image cache");
                }
            });
        }
    }

    /// Drops all textures (e.g. when switching profiles).
    pub fn clear_memory(&self) {
        self.inner
            .mem
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.inner
            .failed
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

/// One download worker: runs queued images until the queue is empty.
async fn worker(inner: &Arc<Inner>) {
    let mut worker = inner.queue.worker();
    let mut dropped = Vec::new();
    while let Some((key, ())) = worker.next(&mut dropped) {
        dropped.clear();
        match load(inner, &key).await {
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
                let mut mem = inner.mem.lock().unwrap_or_else(PoisonError::into_inner);
                let mut total = inner
                    .mem_bytes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                *total += bytes;
                if let Some((_, old)) = mem.put(key.clone(), (tex, bytes)) {
                    *total = total.saturating_sub(old);
                }
                while *total > MEMORY_BUDGET_BYTES {
                    match mem.pop_lru() {
                        Some((_, (_, b))) => *total = total.saturating_sub(b),
                        None => break,
                    }
                }
            }
            Err(reason) => {
                tracing::debug!(%reason, "image unavailable");
                inner
                    .failed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .put(key.clone(), Instant::now());
            }
        }
        inner.ctx.request_repaint();
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

fn is_private(inner: &Inner, url: &str) -> bool {
    let p = inner.private.lock().unwrap_or_else(PoisonError::into_inner);
    p.1 || p.0.contains(&url_hash(url.trim()))
}

async fn decode_limited(
    inner: &Inner,
    bytes: Vec<u8>,
    size: ImageSize,
) -> Result<ColorImage, String> {
    let _permit = Arc::clone(&inner.decodes)
        .acquire_owned()
        .await
        .map_err(|_| "shutting down".to_owned())?;
    tokio::task::spawn_blocking(move || decode(&bytes, size))
        .await
        .map_err(|_| "decode task failed".to_owned())?
}

async fn load(inner: &Arc<Inner>, key: &Key) -> Result<ColorImage, String> {
    let private = is_private(inner, &key.0);
    let disk = inner
        .disk
        .as_ref()
        .filter(|_| !private)
        .map(|d| disk_path(d, key));
    // Disk cache first.
    if let Some(path) = disk.clone() {
        let cached = tokio::task::spawn_blocking(move || std::fs::read(path).ok())
            .await
            .ok()
            .flatten();
        if let Some(bytes) = cached
            && let Ok(img) = decode_limited(inner, bytes, ImageSize::Backdrop).await
        {
            return Ok(img);
        }
    }
    // Credential-free URLs may follow an https → http redirect (artwork CDNs);
    // anything that carries a registered secret may not (SR-07).
    let http = if inner.redactor.redact(&key.0) == key.0 {
        inner.http_lenient.as_ref().or(inner.http.as_ref())
    } else {
        inner.http.as_ref()
    }
    .ok_or("downloads disabled")?;
    let url = opensmarters_core::url::Url::parse(&key.0).map_err(|_| "bad url".to_owned())?;
    let bytes = http.get_bytes(&url).await.map_err(|e| e.to_string())?;
    let img = decode_limited(inner, bytes, key.1).await?;
    // Registered as private while it downloaded: still memory only.
    if let Some(path) = disk.filter(|_| !is_private(inner, &key.0)) {
        let img2 = img.clone();
        let inner2 = Arc::clone(inner);
        tokio::task::spawn_blocking(move || {
            let written = save_png(&path, &img2);
            if written > 0 {
                let total = inner2
                    .written_since_trim
                    .fetch_add(written, Ordering::Relaxed)
                    + written;
                if total >= TRIM_EVERY_BYTES
                    && let Some(dir) = path.parent()
                {
                    inner2.written_since_trim.store(0, Ordering::Relaxed);
                    spawn_trim(&inner2.rt, dir.to_path_buf(), Arc::clone(&inner2.trimming));
                }
            }
        });
    }
    Ok(img)
}

/// Decodes with limits and downscales so the longest side is at most the size class.
fn decode(bytes: &[u8], size: ImageSize) -> Result<ColorImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?;
    let max = size.max_side();
    // `thumbnail` averages areas in integer arithmetic: no float intermediate buffers
    // as large as the source.
    let img = if img.width() > max || img.height() > max {
        img.thumbnail(max, max)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let dims = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(dims, rgba.as_raw()))
}

/// Writes the PNG; returns the bytes written (0 on failure).
fn save_png(path: &Path, img: &ColorImage) -> u64 {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut raw = Vec::with_capacity(img.pixels.len() * 4);
    for p in &img.pixels {
        raw.extend_from_slice(&p.to_srgba_unmultiplied());
    }
    let (Ok(w), Ok(h)) = (u32::try_from(img.size[0]), u32::try_from(img.size[1])) else {
        return 0;
    };
    if let Some(buf) = image::RgbaImage::from_raw(w, h, raw) {
        let tmp = path.with_extension("tmp");
        if buf.save_with_format(&tmp, image::ImageFormat::Png).is_ok()
            && std::fs::rename(&tmp, path).is_ok()
        {
            return std::fs::metadata(path).map_or(0, |m| m.len());
        }
        let _ = std::fs::remove_file(&tmp);
    }
    0
}

fn spawn_trim(rt: &Handle, dir: PathBuf, trimming: Arc<AtomicBool>) {
    if trimming.swap(true, Ordering::AcqRel) {
        return;
    }
    rt.spawn_blocking(move || {
        trim_disk_cache(&dir, DISK_BUDGET_BYTES);
        trimming.store(false, Ordering::Release);
    });
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
#[allow(clippy::unwrap_used)]
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

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap_or_else(|e| panic!("{e}"));
        buf
    }

    #[test]
    fn decode_downscales_and_rejects_garbage() {
        let out = decode(&png(1200, 600), ImageSize::Poster).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.size, [512, 256]);
        assert!(decode(b"<html>not an image</html>", ImageSize::Logo).is_err());
    }

    #[test]
    fn decode_refuses_oversized_images() {
        // Wider than the 4096 px limit (cheap to encode: one solid colour).
        assert!(decode(&png(4097, 8), ImageSize::Logo).is_err());
        assert!(decode(&png(4096, 8), ImageSize::Logo).is_ok());
        // A 4096 × 4096 RGBA16 image needs 128 MiB: over the 64 MiB decoder budget.
        let mut buf = Vec::new();
        image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::new(4096, 4096)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        assert!(decode(&buf, ImageSize::Logo).is_err());
    }

    fn service(rt: &tokio::runtime::Runtime, disk: Option<PathBuf>) -> ImageService {
        ImageService::new(
            rt.handle().clone(),
            egui::Context::default(),
            Redactor::new(),
            disk,
        )
    }

    #[test]
    fn queue_is_bounded_and_drops_images_no_longer_drawn() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let svc = service(&rt, None);
        // A port nothing listens on: downloads fail fast, but only 6 run at once.
        for i in 0..400 {
            let url = format!("http://127.0.0.1:9/{i}.png");
            assert!(matches!(
                svc.get(Some(&url), ImageSize::Poster),
                ImageSlot::Loading
            ));
        }
        let s = svc.queue_stats();
        assert!(s.workers <= MAX_CONCURRENT && s.running <= MAX_CONCURRENT);
        assert!(s.pending <= MAX_QUEUED);
        // Nothing is drawn any more: after the wanted window the queue empties without
        // downloading the rest.
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let s = svc.queue_stats();
            if s.pending == 0 && s.running == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = svc.queue_stats();
        assert_eq!((s.pending, s.running), (0, 0));
        assert!(s.peak_running <= MAX_CONCURRENT);
        assert!(
            s.started < 400,
            "most queued images were dropped, not downloaded ({s:?})"
        );
        // Failures are remembered (no refetch every frame) …
        let failed = (0..400)
            .filter(|i| {
                matches!(
                    svc.get(
                        Some(&format!("http://127.0.0.1:9/{i}.png")),
                        ImageSize::Poster
                    ),
                    ImageSlot::Failed
                )
            })
            .count();
        assert_eq!(failed as u64, s.started);
        svc.cancel_pending();
        assert_eq!(svc.queue_stats().pending, 0);
    }

    #[test]
    fn private_urls_stay_off_disk_and_lose_their_files() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let dir = std::env::temp_dir().join(format!("os-img-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let url = "http://img.example.invalid/adult.png";
        let file = disk_path(&dir, &(url.to_owned(), ImageSize::Poster));
        std::fs::write(&file, png(4, 4)).unwrap();
        let svc = service(&rt, Some(dir.clone()));
        assert!(!svc.is_private(url));
        svc.mark_private([url]);
        assert!(svc.is_private(url));
        assert!(svc.is_private(&format!("  {url} ")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while file.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !file.exists(),
            "a cached file of a private image is deleted"
        );
        std::fs::write(dir.join("other.png"), png(2, 2)).unwrap();
        svc.clear_image_disk_cache();
        let deadline = Instant::now() + Duration::from_secs(5);
        while dir.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!dir.exists());
    }

    #[test]
    fn trim_keeps_the_newest_files_under_budget() {
        let dir = std::env::temp_dir().join(format!("os-trim-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..10 {
            std::fs::write(dir.join(format!("{i}.png")), vec![0u8; 1000]).unwrap();
        }
        trim_disk_cache(&dir, 5000);
        let left = std::fs::read_dir(&dir).unwrap().count();
        assert!(
            left <= 4,
            "trimmed to 80% of the budget ({left} files left)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

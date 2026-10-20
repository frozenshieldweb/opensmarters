//! Generated artwork: gradient tiles with geometric shapes and blocky initials.
//!
//! Logos are square (256×256), posters/covers 2:3 (300×450), backdrops 16:9 (640×360),
//! episode stills 16:9 (400×225). Initials use a built-in 5×7 bitmap font, so no font or
//! text-rendering dependency is needed. Rendered PNGs are cached in memory.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

use crate::catalogue::Icon;
use crate::rng::{Rng, hash_str, mix3};
use crate::server::AppState;

/// Which artwork shape to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ImageKind {
    Logo,
    Poster,
    Cover,
    Backdrop,
    Episode,
}

impl ImageKind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "logo" => Self::Logo,
            "poster" => Self::Poster,
            "cover" => Self::Cover,
            "backdrop" => Self::Backdrop,
            "episode" => Self::Episode,
            _ => return None,
        })
    }

    pub(crate) fn size(self) -> (u32, u32) {
        match self {
            Self::Logo => (256, 256),
            Self::Poster | Self::Cover => (300, 450),
            Self::Backdrop => (640, 360),
            Self::Episode => (400, 225),
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::Poster => "poster",
            Self::Cover => "cover",
            Self::Backdrop => "backdrop",
            Self::Episode => "episode",
        }
    }
}

const PALETTE: &[[u8; 3]] = &[
    [0x1f, 0x3b, 0x73],
    [0x2e, 0x86, 0xab],
    [0xe8, 0x4a, 0x5f],
    [0xff, 0x84, 0x7c],
    [0xf6, 0xae, 0x2d],
    [0x99, 0xb8, 0x98],
    [0x2a, 0x36, 0x3b],
    [0x6c, 0x5b, 0x7b],
    [0x33, 0x65, 0x81],
    [0x86, 0xbb, 0xd8],
    [0x0b, 0x6e, 0x4f],
    [0x9b, 0x2c, 0x5d],
    [0x40, 0x2e, 0x7a],
    [0xd9, 0x4e, 0x1f],
    [0x14, 0x8f, 0x77],
    [0x5d, 0x2e, 0x8c],
];

const WHITE: [u8; 3] = [0xff, 0xff, 0xff];
const INK: [u8; 3] = [0x12, 0x14, 0x1c];

/// A raw RGB8 canvas.
struct Canvas {
    w: i32,
    h: i32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w: w as i32,
            h: h as i32,
            px: vec![0; (w * h * 3) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let i = ((y * self.w + x) * 3) as usize;
        for (dst, src) in self.px[i..i + 3].iter_mut().zip(c) {
            let d = f32::from(*dst);
            *dst = (d + (f32::from(src) - d) * a).round() as u8;
        }
    }

    fn gradient(&mut self, c1: [u8; 3], c2: [u8; 3], angle: f32) {
        let (s, c) = angle.sin_cos();
        let (cx, cy) = (self.w as f32 / 2.0, self.h as f32 / 2.0);
        let extent = (cx * c.abs() + cy * s.abs()).max(1.0);
        for y in 0..self.h {
            for x in 0..self.w {
                let p = ((x as f32 - cx) * c + (y as f32 - cy) * s) / extent;
                let t = (p * 0.5 + 0.5).clamp(0.0, 1.0);
                let i = ((y * self.w + x) * 3) as usize;
                for ((dst, a), b) in self.px[i..i + 3].iter_mut().zip(c1).zip(c2) {
                    let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
                    *dst = v.round() as u8;
                }
            }
        }
    }

    fn bbox(&self, cx: f32, cy: f32, r: f32) -> (i32, i32, i32, i32) {
        (
            ((cx - r - 1.0).floor() as i32).max(0),
            ((cy - r - 1.0).floor() as i32).max(0),
            ((cx + r + 1.0).ceil() as i32).min(self.w - 1),
            ((cy + r + 1.0).ceil() as i32).min(self.h - 1),
        )
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3], a: f32) {
        let (x0, y0, x1, y1) = self.bbox(cx, cy, r);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                self.blend(x, y, c, a * cov);
            }
        }
    }

    fn ring(&mut self, cx: f32, cy: f32, r: f32, thick: f32, c: [u8; 3], a: f32) {
        let (x0, y0, x1, y1) = self.bbox(cx, cy, r + thick);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let cov = (thick / 2.0 + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
                self.blend(x, y, c, a * cov);
            }
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3], a: f32) {
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.w) {
                self.blend(x, y, c, a);
            }
        }
    }

    fn stripes(&mut self, period: f32, width: f32, c: [u8; 3], a: f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let p = (x + y) as f32 % period;
                if p < width {
                    self.blend(x, y, c, a);
                }
            }
        }
    }

    fn diamond(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3], a: f32) {
        let (x0, y0, x1, y1) = self.bbox(cx, cy, r);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = (x as f32 - cx).abs() + (y as f32 - cy).abs();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                self.blend(x, y, c, a * cov);
            }
        }
    }

    /// Right-pointing "play" triangle centred on (cx, cy).
    fn play(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 3], a: f32) {
        let (x0, y0, x1, y1) = self.bbox(cx, cy, r);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f32 - (cx - r * 0.5);
                let dy = (y as f32 - cy).abs();
                if dx >= 0.0 && dx <= r * 1.3 && dy <= (r * 1.3 - dx) * 0.6 {
                    self.blend(x, y, c, a);
                }
            }
        }
    }

    /// Blocky text centred on (cx, cy), `scale` pixels per font cell.
    fn text(&mut self, s: &str, cx: i32, cy: i32, scale: i32, c: [u8; 3], shadow: [u8; 3]) {
        let glyphs: Vec<[u8; 7]> = s.chars().filter_map(glyph).collect();
        if glyphs.is_empty() {
            return;
        }
        let n = glyphs.len() as i32;
        let width = n * 5 * scale + (n - 1) * scale;
        let height = 7 * scale;
        let left = cx - width / 2;
        let top = cy - height / 2;
        let inset = if scale >= 7 { scale / 7 } else { 0 };
        let off = (scale / 4).max(1);
        for (color, alpha, dx) in [(shadow, 0.45, off), (c, 1.0, 0)] {
            for (gi, rows) in glyphs.iter().enumerate() {
                let gx = left + gi as i32 * 6 * scale;
                for (ry, row) in rows.iter().enumerate() {
                    for col in 0..5 {
                        if (row >> (4 - col)) & 1 == 1 {
                            let x = gx + col * scale + dx;
                            let y = top + ry as i32 * scale + dx;
                            self.rect(
                                x + inset,
                                y + inset,
                                x + scale - inset,
                                y + scale - inset,
                                color,
                                alpha,
                            );
                        }
                    }
                }
            }
        }
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(self.px.len() / 4);
        PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
            .write_image(
                &self.px,
                self.w as u32,
                self.h as u32,
                ExtendedColorType::Rgb8,
            )
            .map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// 5×7 bitmap glyphs for A–Z and 0–9 (each row's low 5 bits, MSB = leftmost).
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        _ => return None,
    })
}

/// Up to two initials from the first ASCII alphanumeric of each word.
fn initials(label: &str) -> String {
    label
        .split_whitespace()
        .filter_map(|w| w.chars().find(char::is_ascii_alphanumeric))
        .take(2)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn luminance(c: [u8; 3]) -> f32 {
    0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])
}

/// Render one PNG. Deterministic in (`kind`, `seed`, `label`).
pub(crate) fn render(kind: ImageKind, seed: u64, label: &str) -> Result<Vec<u8>, String> {
    let mut rng = Rng::new(mix3(seed, hash_str(kind.tag()), hash_str(label)));
    let (w, h) = kind.size();
    let mut cv = Canvas::new(w, h);
    let (wf, hf) = (w as f32, h as f32);

    let i1 = rng.below(PALETTE.len() as u64) as usize;
    let i2 = (i1 + 1 + rng.below(PALETTE.len() as u64 - 1) as usize) % PALETTE.len();
    let i3 = rng.below(PALETTE.len() as u64) as usize;
    let (c1, c2, c3) = (PALETTE[i1], PALETTE[i2], PALETTE[i3]);
    let mid = [
        ((u16::from(c1[0]) + u16::from(c2[0])) / 2) as u8,
        ((u16::from(c1[1]) + u16::from(c2[1])) / 2) as u8,
        ((u16::from(c1[2]) + u16::from(c2[2])) / 2) as u8,
    ];
    let (fg, shadow) = if luminance(mid) > 165.0 {
        (INK, WHITE)
    } else {
        (WHITE, INK)
    };
    let angle = rng.below(360) as f32 * std::f32::consts::PI / 180.0;
    let text = initials(label);

    match kind {
        ImageKind::Logo => {
            cv.gradient(c1, c2, angle);
            for _ in 0..3 {
                let r = rng.range(30, 110) as f32;
                cv.disc(
                    rng.below(w as u64) as f32,
                    rng.below(h as u64) as f32,
                    r,
                    WHITE,
                    0.08 + rng.below(10) as f32 / 100.0,
                );
            }
            cv.ring(wf / 2.0, hf / 2.0, 100.0, 7.0, fg, 0.55);
            if text.is_empty() {
                cv.diamond(wf / 2.0, hf / 2.0, 62.0, fg, 0.9);
                cv.disc(wf / 2.0, hf / 2.0, 20.0, c3, 0.9);
            } else {
                let scale = if text.chars().count() > 1 { 13 } else { 17 };
                cv.text(&text, w as i32 / 2, h as i32 / 2, scale, fg, shadow);
            }
        }
        ImageKind::Poster | ImageKind::Cover => {
            cv.gradient(c1, c2, std::f32::consts::FRAC_PI_2 + (angle.sin() * 0.4));
            cv.stripes(28.0, 6.0, WHITE, 0.05);
            let sun_r = rng.range(60, 95) as f32;
            let sun_x = rng.range(80, 220) as f32;
            let sun_y = rng.range(110, 180) as f32;
            if kind == ImageKind::Poster {
                cv.disc(sun_x, sun_y, sun_r, c3, 0.55);
                cv.ring(sun_x, sun_y, sun_r + 14.0, 3.0, WHITE, 0.35);
            } else {
                cv.diamond(sun_x, sun_y, sun_r, c3, 0.5);
                cv.ring(sun_x, sun_y, sun_r * 0.6, 4.0, WHITE, 0.4);
            }
            for _ in 0..4 {
                cv.disc(
                    rng.below(w as u64) as f32,
                    rng.range(200, 330) as f32,
                    rng.range(10, 40) as f32,
                    WHITE,
                    0.1,
                );
            }
            cv.rect(0, 330, w as i32, h as i32, INK, 0.4);
            cv.rect(20, 338, w as i32 - 20, 341, WHITE, 0.5);
            if text.is_empty() {
                cv.diamond(wf / 2.0, 395.0, 30.0, WHITE, 0.9);
            } else {
                cv.text(&text, w as i32 / 2, 395, 8, WHITE, INK);
            }
        }
        ImageKind::Backdrop | ImageKind::Episode => {
            cv.gradient(c1, c2, angle.cos() * 0.5);
            let n = if kind == ImageKind::Backdrop { 7 } else { 5 };
            for _ in 0..n {
                cv.disc(
                    rng.below(w as u64) as f32,
                    rng.below(h as u64) as f32,
                    rng.range(h as u64 / 12, h as u64 / 3) as f32,
                    if rng.chance(70) { WHITE } else { c3 },
                    0.08 + rng.below(14) as f32 / 100.0,
                );
            }
            cv.stripes(40.0, 10.0, WHITE, 0.04);
            if kind == ImageKind::Episode {
                cv.disc(wf / 2.0, hf / 2.0, 38.0, INK, 0.45);
                cv.play(wf / 2.0 + 4.0, hf / 2.0, 22.0, WHITE, 0.95);
            }
            if !text.is_empty() {
                let scale = if kind == ImageKind::Backdrop { 6 } else { 4 };
                let tw = text.chars().count() as i32 * 6 * scale;
                cv.text(
                    &text,
                    24 + tw / 2,
                    h as i32 - 12 - 4 * scale,
                    scale,
                    fg,
                    shadow,
                );
            }
        }
    }
    cv.encode()
}

/// In-memory cache of rendered PNGs, bounded so arbitrary ids can't grow it forever.
#[derive(Debug, Default)]
pub(crate) struct ImageCache {
    map: Mutex<HashMap<String, Bytes>>,
}

const CACHE_CAP: usize = 4096;

impl ImageCache {
    fn get(&self, key: &str) -> Option<Bytes> {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn put(&self, key: String, png: Bytes) {
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.len() >= CACHE_CAP {
            map.clear();
        }
        map.insert(key, png);
    }
}

/// `GET /images/{kind}/{file}`.
pub(crate) async fn handler(
    State(st): State<AppState>,
    Path((kind, file)): Path<(String, String)>,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "no such image").into_response();
    let Some(kind) = ImageKind::parse(&kind) else {
        return not_found();
    };
    let Some(stem) = file.strip_suffix(".png") else {
        return not_found();
    };
    let cat = &st.catalogue;
    let label = match kind {
        ImageKind::Logo => stem
            .parse()
            .ok()
            .and_then(|id| cat.live_channel(id))
            .filter(|c| c.icon == Icon::Generated)
            .map(|c| c.name.clone()),
        ImageKind::Poster => stem
            .parse()
            .ok()
            .and_then(|id| cat.movie(id))
            .map(|m| m.title.clone()),
        ImageKind::Cover => stem
            .parse()
            .ok()
            .and_then(|id| cat.series_by_id(id))
            .map(|s| s.name.clone()),
        ImageKind::Episode => stem
            .parse()
            .ok()
            .and_then(|id| cat.episode(id))
            .map(|(s, _)| s.name.clone()),
        ImageKind::Backdrop => {
            if let Some(id) = stem.strip_prefix("movie-") {
                id.parse()
                    .ok()
                    .and_then(|id| cat.movie(id))
                    .filter(|m| m.has_backdrop)
                    .map(|m| m.title.clone())
            } else if let Some(id) = stem.strip_prefix("series-") {
                id.parse()
                    .ok()
                    .and_then(|id| cat.series_by_id(id))
                    .map(|s| s.name.clone())
            } else {
                None
            }
        }
    };
    let Some(label) = label else {
        return not_found();
    };
    let key = format!("{}/{stem}", kind.tag());
    let png = if let Some(hit) = st.images.get(&key) {
        hit
    } else {
        let seed = crate::rng::mix(st.seed, hash_str(stem));
        let rendered = tokio::task::spawn_blocking(move || render(kind, seed, &label)).await;
        match rendered {
            Ok(Ok(bytes)) => {
                let bytes = Bytes::from(bytes);
                st.images.put(key, bytes.clone());
                bytes
            }
            _ => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "image rendering failed")
                    .into_response();
            }
        }
    };
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        png,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_cases() {
        assert_eq!(initials("Northwind News"), "NN");
        assert_eq!(initials("🎬 Reel One"), "RO");
        assert_eq!(initials("さくらテレビ"), "");
        assert_eq!(initials("Κύμα TV"), "T");
    }

    #[test]
    fn renders_png() {
        for kind in [
            ImageKind::Logo,
            ImageKind::Poster,
            ImageKind::Cover,
            ImageKind::Backdrop,
            ImageKind::Episode,
        ] {
            let png = render(kind, 1, "Cobalt Sports").unwrap_or_default();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{kind:?}");
            assert_eq!(png, render(kind, 1, "Cobalt Sports").unwrap_or_default());
        }
    }
}

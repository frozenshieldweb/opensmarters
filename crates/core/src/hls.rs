// SPDX-License-Identifier: GPL-3.0-or-later
//! A small, defensive HLS playlist parser (RFC 8216), for **recording** channels that are
//! only offered as HLS. Playback never uses it (mpv/FFmpeg read HLS themselves).
//!
//! The playlist is hostile input:
//!
//! - **Bounded:** the body ([`HlsLimits::max_bytes`]), each line, the number of variants
//!   and of segments are capped. A playlist with more segments keeps the **newest** ones
//!   (a live window is what matters).
//! - **Panic-free and lenient:** unknown tags are ignored; a bad attribute, number or URI
//!   skips that item, never the whole playlist. Fuzzed (`fuzz/fuzz_targets/hls.rs`).
//! - **Web addresses only:** every URI is resolved against the playlist's own address
//!   and must be `http` or `https` with a host; anything else (`file:`, `data:`, a path
//!   that escapes to another scheme…) is dropped and counted in [`Media::skipped`].
//! - **Unencrypted only:** an `#EXT-X-KEY` (or `#EXT-X-SESSION-KEY`) whose `METHOD` isn't
//!   `NONE` fails with [`HlsError::Encrypted`]: recording encrypted streams is not
//!   supported (there is no DRM support either).
//! - Byte-range segments (`#EXT-X-BYTERANGE`, a ranged `#EXT-X-MAP`) are reported as
//!   [`HlsError::Unsupported`].
//!
//! ```
//! use opensmarters_core::hls::{self, HlsLimits, Playlist};
//! use opensmarters_core::url::Url;
//!
//! let base = Url::parse("http://cdn.example.com/live/42/index.m3u8").unwrap();
//! let body = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:7\n\
//!              #EXTINF:4.0,\nseg-7.ts\n#EXTINF:4.0,\nseg-8.ts\n";
//! let Ok(Playlist::Media(m)) = hls::parse(body, &base, &HlsLimits::default()) else {
//!     panic!("a media playlist");
//! };
//! assert_eq!(m.segments[1].sequence, 8);
//! assert_eq!(m.segments[1].uri.as_str(), "http://cdn.example.com/live/42/seg-8.ts");
//! ```

use std::collections::VecDeque;

use url::Url;

/// Limits applied while parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsLimits {
    /// Largest playlist accepted. Default 4 MiB.
    pub max_bytes: usize,
    /// Longer lines are skipped. Default 16 KiB.
    pub max_line_len: usize,
    /// Most segments kept (the newest). Default 20 000.
    pub max_segments: usize,
    /// Most variants kept. Default 256.
    pub max_variants: usize,
    /// Longest URI accepted (after resolving). Default 8 KiB.
    pub max_uri_len: usize,
}

impl Default for HlsLimits {
    fn default() -> Self {
        HlsLimits {
            max_bytes: 4 * 1024 * 1024,
            max_line_len: 16 * 1024,
            max_segments: 20_000,
            max_variants: 256,
            max_uri_len: 8 * 1024,
        }
    }
}

/// Why a playlist was not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HlsError {
    /// It doesn't start with `#EXTM3U`.
    #[error("not an HLS playlist")]
    NotAPlaylist,
    /// Larger than [`HlsLimits::max_bytes`].
    #[error("the playlist is larger than the {limit}-byte limit")]
    TooLarge {
        /// The cap.
        limit: usize,
    },
    /// The segments are encrypted (`METHOD` other than `NONE`).
    #[error("the stream is encrypted ({method}), which recording doesn't support")]
    Encrypted {
        /// The `METHOD` value (capped, printable ASCII only).
        method: String,
    },
    /// A feature recording doesn't support (byte ranges).
    #[error("the stream uses {what}, which recording doesn't support")]
    Unsupported {
        /// What it is.
        what: &'static str,
    },
    /// A master playlist without one usable variant.
    #[error("the playlist offers no stream that can be recorded")]
    NoVariants,
}

/// One stream of a master playlist (`#EXT-X-STREAM-INF`).
#[derive(Clone, PartialEq, Eq)]
pub struct Variant {
    /// Media playlist address (absolute, http/https). **Secret** (may carry credentials).
    pub uri: Url,
    /// `BANDWIDTH` (bits per second).
    pub bandwidth: Option<u64>,
    /// `RESOLUTION` (width, height).
    pub resolution: Option<(u32, u32)>,
    /// The variant names an `AUDIO` group whose rendition has its own URI: its audio is a
    /// separate stream, which recording can't merge.
    pub separate_audio: bool,
}

impl std::fmt::Debug for Variant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Variant")
            .field("uri", &"<redacted>")
            .field("bandwidth", &self.bandwidth)
            .field("resolution", &self.resolution)
            .field("separate_audio", &self.separate_audio)
            .finish()
    }
}

/// A master playlist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Master {
    /// Usable variants, in playlist order.
    pub variants: Vec<Variant>,
    /// Variant URIs dropped (not http/https, too long, unparsable).
    pub skipped: usize,
}

impl Master {
    /// The variant to record: the highest `BANDWIDTH` (then resolution) among those with
    /// their audio inside the stream. `None` if every variant keeps audio separately.
    pub fn best(&self) -> Option<&Variant> {
        self.variants
            .iter()
            .filter(|v| !v.separate_audio)
            .max_by_key(|v| {
                (
                    v.bandwidth.unwrap_or(0),
                    v.resolution.map_or(0, |(w, h)| u64::from(w) * u64::from(h)),
                )
            })
    }
}

/// One media segment.
#[derive(Clone, PartialEq)]
pub struct Segment {
    /// Address (absolute, http/https). **Secret** (may carry credentials).
    pub uri: Url,
    /// Media sequence number.
    pub sequence: u64,
    /// `#EXTINF` duration in seconds (0 when missing or invalid).
    pub duration: f64,
    /// Preceded by `#EXT-X-DISCONTINUITY`.
    pub discontinuity: bool,
    /// The initialisation section in effect (`#EXT-X-MAP`, fragmented MP4).
    pub init: Option<Url>,
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("uri", &"<redacted>")
            .field("sequence", &self.sequence)
            .field("duration", &self.duration)
            .field("discontinuity", &self.discontinuity)
            .field("init", &self.init.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// A media playlist.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Media {
    /// `#EXT-X-TARGETDURATION` (seconds).
    pub target_duration: Option<f64>,
    /// `#EXT-X-MEDIA-SEQUENCE` (sequence number of the first segment listed).
    pub media_sequence: u64,
    /// Segments, oldest first (at most [`HlsLimits::max_segments`], the newest).
    pub segments: Vec<Segment>,
    /// `#EXT-X-ENDLIST`: no more segments will be added.
    pub ended: bool,
    /// Segment URIs dropped (not http/https, too long, unparsable).
    pub skipped: usize,
}

impl Media {
    /// How long to wait before reloading a live playlist: half the target duration
    /// (RFC 8216 §6.3.4 allows reloading after one target duration; half catches up
    /// faster), at least 1 s and at most 10 s.
    pub fn reload_after(&self) -> std::time::Duration {
        let target = self
            .target_duration
            .or_else(|| self.segments.last().map(|s| s.duration))
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(6.0);
        std::time::Duration::from_secs_f64((target / 2.0).clamp(1.0, 10.0))
    }
}

/// A parsed playlist.
#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    /// Variants (pick one with [`Master::best`] and load it).
    Master(Master),
    /// Segments.
    Media(Media),
}

/// True if `head` (the first bytes of a body) starts like an HLS/M3U playlist: optional
/// UTF-8 BOM and whitespace, then `#EXTM3U`.
pub fn looks_like_playlist(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    head[start..].starts_with(b"#EXTM3U")
}

/// Splits an attribute list (`KEY=VALUE,KEY="quoted, value"`) into pairs. Keys are
/// upper-cased; quotes are removed. Malformed pieces are skipped.
fn attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() && out.len() < 64 {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq]
            .trim()
            .trim_start_matches(',')
            .trim()
            .to_ascii_uppercase();
        let after = &rest[eq + 1..];
        let (value, next) = if let Some(q) = after.strip_prefix('"') {
            match q.find('"') {
                Some(end) => (&q[..end], q[end + 1..].trim_start_matches([',', ' '])),
                None => (q, ""),
            }
        } else {
            match after.find(',') {
                Some(c) => (&after[..c], &after[c + 1..]),
                None => (after, ""),
            }
        };
        if !key.is_empty() && key.len() <= 64 {
            out.push((key, value.trim().to_owned()));
        }
        rest = next;
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Resolves `reference` against `base` and keeps it only if it is a web address.
fn web_uri(base: &Url, reference: &str, max_len: usize) -> Option<Url> {
    let reference = reference.trim();
    if reference.is_empty()
        || reference.len() > max_len
        || reference.chars().any(|c| c.is_control() || c == '\\')
    {
        return None;
    }
    let url = base.join(reference).ok()?;
    let web =
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| !h.is_empty());
    (web && url.as_str().len() <= max_len).then_some(url)
}

fn printable(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic())
        .take(max)
        .collect()
}

/// `#EXT-X-STREAM-INF` attributes waiting for their URI line: bandwidth, resolution and
/// `AUDIO` group.
type PendingVariant = (Option<u64>, Option<(u32, u32)>, Option<String>);

/// Parses a playlist fetched from `base` (its final address after redirects).
///
/// # Errors
///
/// [`HlsError::TooLarge`], [`HlsError::NotAPlaylist`], [`HlsError::Encrypted`],
/// [`HlsError::Unsupported`], [`HlsError::NoVariants`] (a master playlist with no usable
/// variant).
pub fn parse(body: &[u8], base: &Url, limits: &HlsLimits) -> Result<Playlist, HlsError> {
    if body.len() > limits.max_bytes {
        return Err(HlsError::TooLarge {
            limit: limits.max_bytes,
        });
    }
    if !looks_like_playlist(body) {
        return Err(HlsError::NotAPlaylist);
    }
    let text = String::from_utf8_lossy(body);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    // Variants with their `AUDIO` group (resolved at the end: groups may be declared later).
    let mut variants: Vec<(Variant, Option<String>)> = Vec::new();
    let mut variant_skipped = 0usize;
    let mut separate_audio_groups: Vec<String> = Vec::new();
    let mut pending_variant: Option<PendingVariant> = None;
    let mut is_master = false;

    let mut media = Media::default();
    let mut segments: VecDeque<Segment> = VecDeque::new();
    let mut pending_duration: Option<f64> = None;
    let mut pending_discontinuity = false;
    let mut init: Option<Url> = None;
    let mut index: u64 = 0;

    for raw in text.split('\n') {
        if raw.len() > limits.max_line_len {
            continue;
        }
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(tag) = line.strip_prefix('#') {
            let (name, value) = tag.split_once(':').unwrap_or((tag, ""));
            match name.trim().to_ascii_uppercase().as_str() {
                "EXT-X-STREAM-INF" => {
                    is_master = true;
                    let a = attributes(value);
                    let bandwidth = attr(&a, "BANDWIDTH").and_then(|b| b.parse::<u64>().ok());
                    let resolution = attr(&a, "RESOLUTION").and_then(|r| {
                        let (w, h) = r.split_once(['x', 'X'])?;
                        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
                    });
                    let audio = attr(&a, "AUDIO").map(str::to_owned);
                    pending_variant = Some((bandwidth, resolution, audio));
                }
                "EXT-X-MEDIA" => {
                    let a = attributes(value);
                    let is_audio =
                        attr(&a, "TYPE").is_some_and(|t| t.eq_ignore_ascii_case("AUDIO"));
                    if is_audio
                        && attr(&a, "URI").is_some_and(|u| !u.trim().is_empty())
                        && let Some(group) = attr(&a, "GROUP-ID")
                        && separate_audio_groups.len() < 64
                    {
                        separate_audio_groups.push(group.to_owned());
                    }
                }
                "EXT-X-KEY" | "EXT-X-SESSION-KEY" => {
                    let a = attributes(value);
                    let method = attr(&a, "METHOD").unwrap_or("NONE");
                    if !method.eq_ignore_ascii_case("NONE") {
                        return Err(HlsError::Encrypted {
                            method: printable(method, 32),
                        });
                    }
                }
                "EXT-X-BYTERANGE" => {
                    return Err(HlsError::Unsupported {
                        what: "byte ranges",
                    });
                }
                "EXT-X-MAP" => {
                    let a = attributes(value);
                    if attr(&a, "BYTERANGE").is_some() {
                        return Err(HlsError::Unsupported {
                            what: "byte ranges",
                        });
                    }
                    init = attr(&a, "URI").and_then(|u| web_uri(base, u, limits.max_uri_len));
                }
                "EXT-X-TARGETDURATION" => {
                    media.target_duration = value
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|d| d.is_finite() && *d > 0.0 && *d <= 86_400.0);
                }
                // Only before the first segment (RFC 8216 §4.3.3.2): a second one can't
                // renumber segments already listed.
                "EXT-X-MEDIA-SEQUENCE" if index == 0 => {
                    if let Ok(n) = value.trim().parse::<u64>() {
                        media.media_sequence = n;
                    }
                }
                "EXTINF" => {
                    let d = value.split(',').next().unwrap_or("").trim();
                    pending_duration = Some(
                        d.parse::<f64>()
                            .ok()
                            .filter(|d| d.is_finite())
                            .map_or(0.0, |d| d.clamp(0.0, 86_400.0)),
                    );
                }
                "EXT-X-DISCONTINUITY" => pending_discontinuity = true,
                "EXT-X-ENDLIST" => media.ended = true,
                _ => {}
            }
            continue;
        }
        // A URI line.
        if let Some((bandwidth, resolution, audio)) = pending_variant.take() {
            match web_uri(base, line, limits.max_uri_len) {
                Some(uri) if variants.len() < limits.max_variants => variants.push((
                    Variant {
                        uri,
                        bandwidth,
                        resolution,
                        separate_audio: false,
                    },
                    audio,
                )),
                Some(_) => {}
                None => variant_skipped += 1,
            }
            continue;
        }
        if is_master {
            continue;
        }
        let sequence = media.media_sequence.saturating_add(index);
        index = index.saturating_add(1);
        let duration = pending_duration.take().unwrap_or(0.0);
        let discontinuity = std::mem::take(&mut pending_discontinuity);
        match web_uri(base, line, limits.max_uri_len) {
            Some(uri) => {
                if segments.len() >= limits.max_segments.max(1) {
                    segments.pop_front();
                }
                segments.push_back(Segment {
                    uri,
                    sequence,
                    duration,
                    discontinuity,
                    init: init.clone(),
                });
            }
            None => media.skipped += 1,
        }
    }

    if is_master {
        let master = Master {
            variants: variants
                .into_iter()
                .map(|(mut v, audio)| {
                    v.separate_audio = audio.is_some_and(|g| separate_audio_groups.contains(&g));
                    v
                })
                .collect(),
            skipped: variant_skipped,
        };
        if master.variants.is_empty() {
            return Err(HlsError::NoVariants);
        }
        return Ok(Playlist::Master(master));
    }
    media.segments = segments.into();
    Ok(Playlist::Media(media))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("http://cdn.example.com/live/user/pass/42/index.m3u8").unwrap()
    }

    fn media(body: &str) -> Media {
        match parse(body.as_bytes(), &base(), &HlsLimits::default()) {
            Ok(Playlist::Media(m)) => m,
            other => panic!("expected a media playlist, got {other:?}"),
        }
    }

    fn master(body: &str) -> Master {
        match parse(body.as_bytes(), &base(), &HlsLimits::default()) {
            Ok(Playlist::Master(m)) => m,
            other => panic!("expected a master playlist, got {other:?}"),
        }
    }

    #[test]
    fn media_playlist_with_sequence_and_relative_uris() {
        let m = media(
            "\u{feff}#EXTM3U\r\n#EXT-X-VERSION:3\r\n#EXT-X-TARGETDURATION:6\r\n\
             #EXT-X-MEDIA-SEQUENCE:1200\r\n#EXTINF:6.006,\r\nseg-1200.ts\r\n\
             #EXT-X-DISCONTINUITY\r\n#EXTINF:5.5,title, with comma\r\n/abs/seg-1201.ts?t=1\r\n\
             #EXTINF:6,\r\nhttps://other.example.net/seg-1202.ts\r\n",
        );
        assert_eq!(m.target_duration, Some(6.0));
        assert_eq!(m.media_sequence, 1200);
        assert!(!m.ended);
        assert_eq!(m.segments.len(), 3);
        assert_eq!(
            m.segments[0].uri.as_str(),
            "http://cdn.example.com/live/user/pass/42/seg-1200.ts"
        );
        assert_eq!(
            m.segments[1].uri.as_str(),
            "http://cdn.example.com/abs/seg-1201.ts?t=1"
        );
        assert_eq!(m.segments[2].sequence, 1202);
        assert!(m.segments[1].discontinuity && !m.segments[2].discontinuity);
        assert!((m.segments[0].duration - 6.006).abs() < 1e-9);
        assert_eq!(m.reload_after(), std::time::Duration::from_secs(3));
    }

    #[test]
    fn ended_vod_playlist() {
        let m =
            media("#EXTM3U\n#EXTINF:4.0,\nseg-000.ts\n#EXTINF:4.0,\nseg-001.ts\n#EXT-X-ENDLIST\n");
        assert!(m.ended);
        assert_eq!(m.media_sequence, 0);
        assert_eq!(
            m.segments.iter().map(|s| s.sequence).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn master_picks_the_best_variant_with_muxed_audio() {
        let m = master(
            "#EXTM3U\n\
             #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",URI=\"audio/en.m3u8\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360\nlow/index.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,AUDIO=\"aud\"\nhd/index.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"\nmid/index.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=9000000\nfile:///etc/passwd\n",
        );
        assert_eq!(m.variants.len(), 3);
        assert_eq!(m.skipped, 1);
        let best = m.best().unwrap();
        assert_eq!(best.bandwidth, Some(2_500_000));
        assert_eq!(best.resolution, Some((1280, 720)));
        assert!(best.uri.as_str().ends_with("/42/mid/index.m3u8"));
        // Only separate-audio variants: nothing to record.
        let only = master(
            "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=a,URI=\"a.m3u8\"\n\
             #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=a\nv.m3u8\n",
        );
        assert!(only.best().is_none());
    }

    #[test]
    fn encrypted_and_byte_ranges_are_refused() {
        let enc =
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://k.example/key\"\n#EXTINF:4,\na.ts\n";
        assert_eq!(
            parse(enc.as_bytes(), &base(), &HlsLimits::default()),
            Err(HlsError::Encrypted {
                method: "AES-128".into()
            })
        );
        let none = "#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\na.ts\n";
        assert_eq!(media(none).segments.len(), 1);
        let session = "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n";
        assert!(matches!(
            parse(session.as_bytes(), &base(), &HlsLimits::default()),
            Err(HlsError::Encrypted { .. })
        ));
        let br = "#EXTM3U\n#EXTINF:4,\n#EXT-X-BYTERANGE:100@0\na.ts\n";
        assert!(matches!(
            parse(br.as_bytes(), &base(), &HlsLimits::default()),
            Err(HlsError::Unsupported { .. })
        ));
    }

    #[test]
    fn hostile_uris_are_dropped() {
        let m = media(
            "#EXTM3U\n#EXTINF:1,\nfile:///C:/Windows/win.ini\n#EXTINF:1,\ndata:video/mp2t;base64,AAAA\n\
             #EXTINF:1,\nedl://x\n#EXTINF:1,\n\\\\server\\share\\a.ts\n#EXTINF:1,\nhttp://\n\
             #EXTINF:1,\nok.ts\n",
        );
        assert_eq!(m.skipped, 5);
        assert_eq!(m.segments.len(), 1);
        // Sequence numbers still count the dropped entries.
        assert_eq!(m.segments[0].sequence, 5);
        // A file: base can't smuggle local files either.
        let local = Url::parse("file:///tmp/list.m3u8").unwrap();
        match parse(
            b"#EXTM3U\n#EXTINF:1,\na.ts\n",
            &local,
            &HlsLimits::default(),
        ) {
            Ok(Playlist::Media(m)) => assert!(m.segments.is_empty() && m.skipped == 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn init_sections_follow_the_map_tag() {
        let m = media(
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nseg1.m4s\n\
             #EXT-X-MAP:URI=\"init2.mp4\"\n#EXTINF:2,\nseg2.m4s\n",
        );
        assert!(
            m.segments[0]
                .init
                .as_ref()
                .unwrap()
                .as_str()
                .ends_with("/init.mp4")
        );
        assert!(
            m.segments[1]
                .init
                .as_ref()
                .unwrap()
                .as_str()
                .ends_with("/init2.mp4")
        );
    }

    #[test]
    fn not_a_playlist_and_limits() {
        for body in [
            "",
            "<html><body>Error</body></html>",
            "\x47\x40\x00\x10",
            "EXTM3U",
        ] {
            assert_eq!(
                parse(body.as_bytes(), &base(), &HlsLimits::default()),
                Err(HlsError::NotAPlaylist),
                "{body:?}"
            );
        }
        assert!(looks_like_playlist(b"  \n#EXTM3U\n"));
        assert!(!looks_like_playlist(b"#EXTM3"));
        let limits = HlsLimits {
            max_bytes: 10,
            ..HlsLimits::default()
        };
        assert!(matches!(
            parse(b"#EXTM3U\n#EXTINF:1,\na.ts\n", &base(), &limits),
            Err(HlsError::TooLarge { .. })
        ));
        // Too many segments: the newest are kept.
        let mut body = String::from("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:10\n");
        for i in 0..50 {
            body.push_str(&format!("#EXTINF:1,\ns{i}.ts\n"));
        }
        let limits = HlsLimits {
            max_segments: 5,
            ..HlsLimits::default()
        };
        let Ok(Playlist::Media(m)) = parse(body.as_bytes(), &base(), &limits) else {
            panic!("media");
        };
        assert_eq!(m.segments.len(), 5);
        assert_eq!(m.segments[0].sequence, 55);
        assert_eq!(m.segments[4].sequence, 59);
        // A master with no usable variant.
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nftp://x/y\n",
                &base(),
                &HlsLimits::default()
            ),
            Err(HlsError::NoVariants)
        );
    }

    #[test]
    fn odd_numbers_do_not_panic() {
        let m = media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:NaN\n#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n\
             #EXTINF:-5,\na.ts\n#EXTINF:1e400,\nb.ts\n#EXTINF:inf,\nc.ts\n",
        );
        assert_eq!(m.target_duration, None);
        assert_eq!(m.segments[0].duration, 0.0);
        assert_eq!(m.segments[2].sequence, u64::MAX);
        let m = media(
            "#EXTM3U
#EXT-X-MEDIA-SEQUENCE:10
#EXTINF:1,
a.ts
#EXT-X-MEDIA-SEQUENCE:2
#EXTINF:1,
b.ts
",
        );
        assert_eq!(
            m.segments[1].sequence, 11,
            "a late MEDIA-SEQUENCE is ignored"
        );
        let a = attributes("A=1,B=\"x, y\",,=bad,C");
        assert_eq!(
            a,
            vec![("A".into(), "1".into()), ("B".into(), "x, y".into())]
        );
    }
}

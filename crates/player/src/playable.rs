// SPDX-License-Identifier: GPL-3.0-or-later
//! Validated inputs that are allowed to reach mpv: [`PlayableUrl`],
//! [`UserAgent`] and [`Referrer`].
//!
//! Every value that reaches mpv from provider data goes through one of these
//! types. None of them implements `Display`, and their `Debug` output is
//! redacted, so they cannot be logged by accident.

use std::fmt;
use std::path::Path;

use crate::error::ValidationError;

/// Maximum accepted URL length in bytes (before and after normalisation).
pub const MAX_URL_LEN: usize = 8 * 1024;
/// Maximum accepted HTTP header value length in bytes.
pub const MAX_HEADER_LEN: usize = 1024;

/// Which non-HTTP streaming schemes a player accepts.
///
/// The default ([`UrlPolicy::HTTP_ONLY`]) accepts `http` and `https` only.
/// The other schemes are off unless the user turns them on in settings. The
/// same policy also determines the ffmpeg `protocol_whitelist` the player
/// installs, so a [`crate::Player`] refuses URLs its policy does not allow
/// even if the [`PlayableUrl`] was built with a more permissive one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct UrlPolicy {
    /// Accept `rtmp://` and `rtmps://`.
    pub allow_rtmp: bool,
    /// Accept `rtsp://` and `rtsps://`.
    pub allow_rtsp: bool,
    /// Accept `udp://` and `rtp://` (multicast IPTV).
    pub allow_udp: bool,
}

impl UrlPolicy {
    /// `http` and `https` only.
    pub const HTTP_ONLY: Self = Self {
        allow_rtmp: false,
        allow_rtsp: false,
        allow_udp: false,
    };

    /// Everything this crate can be configured to accept.
    pub const ALL_STREAMING: Self = Self {
        allow_rtmp: true,
        allow_rtsp: true,
        allow_udp: true,
    };

    /// Whether `scheme` (lower-case, without `:`) is accepted.
    pub fn allows_scheme(&self, scheme: &str) -> bool {
        match scheme {
            "http" | "https" => true,
            "rtmp" | "rtmps" => self.allow_rtmp,
            "rtsp" | "rtsps" => self.allow_rtsp,
            "udp" | "rtp" => self.allow_udp,
            _ => false,
        }
    }

    /// The ffmpeg protocols needed for this policy, for `protocol_whitelist`.
    ///
    /// `tcp`/`tls` are the transports under `http`/`https`; `crypto` is
    /// AES-128 HLS; `data` is inline `data:` URIs some HLS playlists use for
    /// keys; `httpproxy` is ffmpeg's CONNECT proxy for https.
    pub(crate) fn ffmpeg_protocols(&self, local_files: bool) -> String {
        let mut p = vec!["http", "https", "tls", "tcp", "crypto", "data", "httpproxy"];
        if self.allow_rtmp {
            p.extend(["rtmp", "rtmps", "rtmpt", "rtmpts", "rtmpe", "rtmpte"]);
        }
        if self.allow_rtsp {
            p.extend(["rtsp", "rtsps", "rtp", "srtp", "udp"]);
        }
        if self.allow_udp {
            p.extend(["udp", "rtp"]);
        }
        if local_files {
            p.push("file");
        }
        let mut seen = Vec::new();
        for x in p {
            if !seen.contains(&x) {
                seen.push(x);
            }
        }
        seen.join(",")
    }
}

/// A media URL that is safe to hand to mpv. **The only way to load media.**
///
/// Construction rejects:
/// * empty input and input longer than [`MAX_URL_LEN`] bytes;
/// * control characters (CR, LF, TAB, NUL, DEL, C1 controls), any Unicode
///   whitespace, and backslashes;
/// * anything that is not an absolute URL with a non-empty host;
/// * every scheme except `http`/`https` (plus `rtmp`/`rtsp`/`udp` when the
///   [`UrlPolicy`] allows them). That rules out mpv and ffmpeg
///   pseudo-protocols such as `file:`, `edl://`, `lavfi://`, `av://`,
///   `memory://`, `fd://`, `appending://`, `concat:`, `subfile:` and
///   `crypto+http://`.
///
/// The URL is re-serialised by the WHATWG parser, and mpv receives that
/// normalised form, so what mpv sees is exactly what was checked. The
/// fragment (`#…`) is dropped because it means nothing to a stream.
///
/// Local files can be played only through [`PlayableUrl::from_owned_media`] (files the
/// app saved itself, inside its recordings folder, checked by content) or
/// [`PlayableUrl::from_local_file`], which is meant for development and tests and only
/// works on a player built with [`crate::PlayerConfig::allow_local_files`].
///
/// `Debug` is redacted, and there is deliberately no `Display`: stream URLs
/// carry credentials.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PlayableUrl {
    url: String,
    scheme: String,
    local: bool,
    owned: bool,
}

impl PlayableUrl {
    /// Parses an `http`/`https` URL.
    ///
    /// # Errors
    /// Returns a [`ValidationError`] that never echoes the input.
    pub fn parse(input: &str) -> Result<Self, ValidationError> {
        Self::parse_with_policy(input, UrlPolicy::HTTP_ONLY)
    }

    /// Parses a URL, also accepting the extra schemes `policy` enables.
    ///
    /// # Errors
    /// Returns a [`ValidationError`] that never echoes the input.
    pub fn parse_with_policy(input: &str, policy: UrlPolicy) -> Result<Self, ValidationError> {
        check_raw(input, MAX_URL_LEN)?;
        // Check the scheme on the raw text first, so the parser's leniency
        // never matters for the allowlist decision.
        let raw_scheme = raw_scheme(input).ok_or(ValidationError::Malformed)?;
        if !policy.allows_scheme(&raw_scheme) {
            return Err(ValidationError::SchemeNotAllowed(raw_scheme));
        }
        let mut url = url::Url::parse(input).map_err(|_| ValidationError::Malformed)?;
        let scheme = url.scheme().to_owned();
        if scheme != raw_scheme || !policy.allows_scheme(&scheme) {
            return Err(ValidationError::SchemeNotAllowed(scheme));
        }
        if url.cannot_be_a_base() {
            return Err(ValidationError::Malformed);
        }
        match url.host_str() {
            Some(h) if !h.is_empty() => {}
            _ => return Err(ValidationError::MissingHost),
        }
        url.set_fragment(None);
        let out = String::from(url);
        // Normalisation can only add percent-escapes; re-check everything.
        check_raw(&out, MAX_URL_LEN)?;
        if !out.is_ascii() {
            return Err(ValidationError::Malformed);
        }
        Ok(Self {
            url: out,
            scheme,
            local: false,
            owned: false,
        })
    }

    /// A local file, for **development and tests only**.
    ///
    /// The path must be absolute and name an existing regular file. The
    /// result is a `file://` URL and is only accepted by a player built with
    /// [`crate::PlayerConfig::allow_local_files`]. Do not use this for
    /// anything that came from a provider.
    ///
    /// # Errors
    /// [`ValidationError::BadLocalPath`] if the path is relative or not a file.
    pub fn from_local_file(path: &Path) -> Result<Self, ValidationError> {
        if !path.is_absolute() || !path.is_file() {
            return Err(ValidationError::BadLocalPath);
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| ValidationError::BadLocalPath)?;
        let canonical = strip_verbatim_prefix(&canonical);
        let url =
            url::Url::from_file_path(&canonical).map_err(|()| ValidationError::BadLocalPath)?;
        let out = String::from(url);
        check_raw(&out, MAX_URL_LEN)?;
        Ok(Self {
            url: out,
            scheme: "file".to_owned(),
            local: true,
            owned: false,
        })
    }

    /// A media file the app saved itself (a download, a recording or a clip), for
    /// playback on any player (no [`crate::PlayerConfig::allow_local_files`] needed).
    ///
    /// Checks, after resolving symbolic links: `root` is an absolute folder, `file` is a
    /// regular file **inside** it, and the file's first bytes are a known audio/video
    /// container ([`sniff_container`]): text playlists (M3U, PLS, EDL…) and unknown
    /// data are refused, so a "film" that is really a playlist cannot make mpv open other
    /// files or URLs.
    ///
    /// # Errors
    /// [`ValidationError::BadLocalPath`] (not absolute, missing, not a file),
    /// [`ValidationError::OutsideRoot`] or [`ValidationError::NotMedia`].
    pub fn from_owned_media(root: &Path, file: &Path) -> Result<Self, ValidationError> {
        if !root.is_absolute() || !file.is_absolute() {
            return Err(ValidationError::BadLocalPath);
        }
        let root = std::fs::canonicalize(root).map_err(|_| ValidationError::BadLocalPath)?;
        let canonical = std::fs::canonicalize(file).map_err(|_| ValidationError::BadLocalPath)?;
        if !root.is_dir() || !canonical.is_file() {
            return Err(ValidationError::BadLocalPath);
        }
        if !canonical.starts_with(&root) || canonical == root {
            return Err(ValidationError::OutsideRoot);
        }
        let mut head = [0u8; SNIFF_BYTES];
        let n = read_head(&canonical, &mut head).map_err(|_| ValidationError::BadLocalPath)?;
        if sniff_container(&head[..n]).is_none() {
            return Err(ValidationError::NotMedia);
        }
        let canonical = strip_verbatim_prefix(&canonical);
        let url =
            url::Url::from_file_path(&canonical).map_err(|()| ValidationError::BadLocalPath)?;
        let out = String::from(url);
        check_raw(&out, MAX_URL_LEN)?;
        Ok(Self {
            url: out,
            scheme: "file".to_owned(),
            local: true,
            owned: true,
        })
    }

    /// The normalised URL. Treat it as a secret: never log it unredacted.
    pub fn as_str(&self) -> &str {
        &self.url
    }

    /// Lower-case scheme (`http`, `https`, `rtmp`, …, or `file` for local
    /// files).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Whether this is a local file ([`PlayableUrl::from_local_file`] or
    /// [`PlayableUrl::from_owned_media`]).
    pub fn is_local_file(&self) -> bool {
        self.local
    }

    /// Whether this was built with [`PlayableUrl::from_owned_media`].
    pub fn is_owned_media(&self) -> bool {
        self.owned
    }
}

/// How many leading bytes [`sniff_container`] looks at (callers should pass at least
/// this many when the file is that long).
pub const SNIFF_BYTES: usize = 4096;

/// An audio/video container recognised by [`sniff_container`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Container {
    /// MPEG transport stream (188-byte packets, or 192-byte M2TS packets).
    MpegTs,
    /// Matroska or WebM.
    Matroska,
    /// ISO base media (MP4, MOV, fragmented MP4).
    Mp4,
    /// AVI.
    Avi,
    /// Flash video.
    Flv,
    /// MPEG program stream.
    MpegPs,
    /// ASF / WMV.
    Asf,
}

impl Container {
    /// The usual file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Container::MpegTs => "ts",
            Container::Matroska => "mkv",
            Container::Mp4 => "mp4",
            Container::Avi => "avi",
            Container::Flv => "flv",
            Container::MpegPs => "mpg",
            Container::Asf => "wmv",
        }
    }
}

/// Recognises a media container from the first bytes of a file (pass up to
/// [`SNIFF_BYTES`]). Returns `None` for anything else, including text playlists (M3U,
/// PLS, XSPF, mpv EDL), HTML error pages and empty input. Downloaders should check the
/// first bytes they receive with this before keeping a file.
pub fn sniff_container(head: &[u8]) -> Option<Container> {
    let at = |i: usize, sig: &[u8]| head.get(i..i + sig.len()) == Some(sig);
    if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Container::Matroska);
    }
    if ["ftyp", "styp", "moov"].iter().any(|b| at(4, b.as_bytes())) {
        return Some(Container::Mp4);
    }
    if at(0, b"RIFF") && at(8, b"AVI ") {
        return Some(Container::Avi);
    }
    if at(0, b"FLV") && head.get(3) == Some(&1) {
        return Some(Container::Flv);
    }
    if at(0, &[0x00, 0x00, 0x01, 0xBA]) {
        return Some(Container::MpegPs);
    }
    if at(0, &[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        return Some(Container::Asf);
    }
    // Transport streams: a sync byte every packet. Ask for three in a row when the input
    // is long enough, two otherwise (short clips).
    let ts = |offset: usize, stride: usize| {
        let syncs = (0..3)
            .map(|k| offset + k * stride)
            .take_while(|i| *i < head.len())
            .collect::<Vec<_>>();
        syncs.len() >= 2 && syncs.iter().all(|i| head[*i] == 0x47)
    };
    if ts(0, 188) || ts(4, 192) {
        return Some(Container::MpegTs);
    }
    None
}

fn read_head(path: &Path, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// A new file the player may **write**: the output of [`crate::Player::dump_cache`]
/// (a clip of the buffer) or [`crate::Player::set_stream_record`] (a recording of what is
/// playing).
///
/// Checks: `root` is an existing absolute folder; `file_name` is a bare name (no
/// separators, no `..`, no `:`, no control characters, no leading dot) of at most 180
/// characters ending in `.ts`, `.mkv` or `.mp4`; the file does not exist yet (outputs
/// never overwrite anything). The resulting path is absolute and plain, so neither mpv
/// nor ffmpeg can read it as a protocol URL.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaOutput {
    path: std::path::PathBuf,
}

impl MediaOutput {
    /// Validates `root` + `file_name`.
    ///
    /// # Errors
    /// [`ValidationError::BadLocalPath`] (root not an absolute folder),
    /// [`ValidationError::BadFileName`] or [`ValidationError::AlreadyExists`].
    pub fn new(root: &Path, file_name: &str) -> Result<Self, ValidationError> {
        if !root.is_absolute() {
            return Err(ValidationError::BadLocalPath);
        }
        let root = std::fs::canonicalize(root).map_err(|_| ValidationError::BadLocalPath)?;
        if !root.is_dir() {
            return Err(ValidationError::BadLocalPath);
        }
        let count = file_name.chars().count();
        let ext_ok = Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["ts", "mkv", "mp4"].contains(&e.to_ascii_lowercase().as_str()));
        if count == 0
            || count > 180
            || !ext_ok
            || file_name.starts_with(['.', ' '])
            || file_name.ends_with(['.', ' '])
            || file_name.contains("..")
            || file_name
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*%".contains(c))
        {
            return Err(ValidationError::BadFileName);
        }
        let stem = file_name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if device {
            return Err(ValidationError::BadFileName);
        }
        let path = strip_verbatim_prefix(&root).join(file_name);
        if path.exists() {
            return Err(ValidationError::AlreadyExists);
        }
        let s = path.to_string_lossy();
        check_raw_path(&s)?;
        Ok(Self { path })
    }

    /// The absolute path to write.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Debug for MediaOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MediaOutput").field(&self.path).finish()
    }
}

/// Paths handed to mpv for writing: no control characters, not too long.
fn check_raw_path(s: &str) -> Result<(), ValidationError> {
    if s.len() > MAX_URL_LEN {
        return Err(ValidationError::TooLong { max: MAX_URL_LEN });
    }
    if s.chars().any(char::is_control) {
        return Err(ValidationError::ControlChar);
    }
    Ok(())
}

impl fmt::Debug for PlayableUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlayableUrl({}://<redacted>)", self.scheme)
    }
}

/// An HTTP `User-Agent` value for mpv's `user-agent` option.
///
/// Printable ASCII (`0x20..=0x7E`) only, 1 to [`MAX_HEADER_LEN`] bytes, so
/// it cannot inject header lines.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UserAgent(String);

impl UserAgent {
    /// Validates a User-Agent string.
    ///
    /// # Errors
    /// Empty, too long, control characters, or non-ASCII.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        check_header_value(value)?;
        Ok(Self(value.to_owned()))
    }

    /// The value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UserAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // User agents are not secret; show them.
        f.debug_tuple("UserAgent").field(&self.0).finish()
    }
}

/// An HTTP `Referer` value for mpv's `referrer` option: an `http`/`https`
/// URL that passes the same checks as [`PlayableUrl`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Referrer(String);

impl Referrer {
    /// Validates a referrer URL.
    ///
    /// # Errors
    /// Same rules as [`PlayableUrl::parse`], plus the header-value rules.
    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        let url = PlayableUrl::parse(value)?;
        check_header_value(url.as_str())?;
        Ok(Self(url.url))
    }

    /// The normalised value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Referrer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Referrer(<redacted>)")
    }
}

fn check_raw(input: &str, max: usize) -> Result<(), ValidationError> {
    if input.is_empty() {
        return Err(ValidationError::Empty);
    }
    if input.len() > max {
        return Err(ValidationError::TooLong { max });
    }
    for c in input.chars() {
        if c.is_control() {
            return Err(ValidationError::ControlChar);
        }
        if c.is_whitespace() || is_invisible_format_char(c) {
            return Err(ValidationError::Whitespace);
        }
        if c == '\\' {
            return Err(ValidationError::Backslash);
        }
    }
    Ok(())
}

/// Zero-width and bidi-control characters that are not `is_whitespace` but
/// render as nothing; they have no business in a URL.
fn is_invisible_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

fn check_header_value(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty);
    }
    if value.len() > MAX_HEADER_LEN {
        return Err(ValidationError::TooLong {
            max: MAX_HEADER_LEN,
        });
    }
    for b in value.bytes() {
        if b.is_ascii_control() || !b.is_ascii() {
            return Err(ValidationError::ControlChar);
        }
    }
    Ok(())
}

/// RFC 3986 scheme of the raw input, lower-cased, if it has one.
fn raw_scheme(input: &str) -> Option<String> {
    let (scheme, _) = input.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// `std::fs::canonicalize` returns `\\?\C:\…` on Windows, which
/// `Url::from_file_path` turns into an odd URL; strip the verbatim prefix for
/// ordinary drive paths.
fn strip_verbatim_prefix(p: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str().to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && rest.as_bytes().get(1) == Some(&b':')
        {
            return std::path::PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> PlayableUrl {
        match PlayableUrl::parse(s) {
            Ok(u) => u,
            Err(e) => panic!("expected {s:?} to be accepted, got {e:?}"),
        }
    }

    fn err(s: &str) -> ValidationError {
        match PlayableUrl::parse(s) {
            Ok(u) => panic!("expected {s:?} to be rejected, got {:?}", u.as_str()),
            Err(e) => e,
        }
    }

    #[test]
    fn accepts_ordinary_stream_urls() {
        let cases = [
            (
                "http://example.com/live/u/p/1.ts",
                "http://example.com/live/u/p/1.ts",
            ),
            (
                "https://example.com/live/u/p/1.m3u8",
                "https://example.com/live/u/p/1.m3u8",
            ),
            (
                "http://example.com:8080/movie/u/p/42.mkv",
                "http://example.com:8080/movie/u/p/42.mkv",
            ),
            (
                "http://192.0.2.10:25461/live/a/b/3.ts",
                "http://192.0.2.10:25461/live/a/b/3.ts",
            ),
            ("http://[2001:db8::1]:80/x.ts", "http://[2001:db8::1]/x.ts"),
            ("HTTP://Example.COM/A.ts", "http://example.com/A.ts"),
            (
                "http://example.com/get?token=a%20b&x=1",
                "http://example.com/get?token=a%20b&x=1",
            ),
            (
                "http://user:pass@example.com/x.ts",
                "http://user:pass@example.com/x.ts",
            ),
            ("https://example.com/x.ts#frag", "https://example.com/x.ts"),
            ("http://example.com", "http://example.com/"),
            (
                "http://127.0.0.1:8765/test.m3u8",
                "http://127.0.0.1:8765/test.m3u8",
            ),
        ];
        for (input, expected) in cases {
            let u = ok(input);
            assert_eq!(u.as_str(), expected, "input {input:?}");
            assert!(!u.is_local_file());
        }
        assert_eq!(ok("HTTPS://example.com/").scheme(), "https");
    }

    #[test]
    fn normalises_unicode_host_and_path_to_ascii() {
        let u = ok("http://bücher.example/фильм.ts");
        assert!(u.as_str().is_ascii());
        assert!(u.as_str().starts_with("http://xn--bcher-kva.example/"));
    }

    #[test]
    fn rejects_dangerous_mpv_and_ffmpeg_schemes() {
        let cases = [
            "file:///C:/Windows/win.ini",
            "FILE:///C:/Windows/win.ini",
            "file://localhost/etc/passwd",
            "edl://%10%/C:/x.mkv",
            "lavfi://sine",
            "lavfi:[sine]",
            "av://dshow:video=x",
            "avdevice://dshow:video=x",
            "memory://hello",
            "fd://0",
            "fdclose://3",
            "appending://C:/x.ts",
            "hex://41414141",
            "null://",
            "slice://0-10@http://example.com/x.ts",
            "mf://C:/*.jpg",
            "dvb://1",
            "cdda://",
            "bd://",
            "dvd://",
            "smb://host/share/x.ts",
            "ytdl://example",
            "lavf://http://example.com/x.ts",
            "ffmpeg://http://example.com/x.ts",
            "concat:http://example.com/a.ts|http://example.com/b.ts",
            "subfile,,start,0,end,0,,:http://example.com/x",
            "crypto+http://example.com/x.ts",
            "hls+http://example.com/x.m3u8",
            "cache:http://example.com/x.ts",
            "async:http://example.com/x.ts",
            "data:text/plain,hello",
            "javascript:alert(1)",
            "ftp://example.com/x.ts",
            "gopher://example.com/",
            "ws://example.com/",
            "mpv://x",
            "rtmp://example.com/live/x",
            "rtmps://example.com/live/x",
            "rtsp://example.com/x",
            "udp://@239.0.0.1:1234",
            "rtp://239.0.0.1:1234",
            "srt://example.com:9000",
            "tcp://example.com:80",
            "tls://example.com:443",
            "pipe:0",
            "zmq:tcp://x",
            "httpproxy://example.com",
        ];
        for c in cases {
            let e = err(c);
            assert!(
                matches!(
                    e,
                    ValidationError::SchemeNotAllowed(_) | ValidationError::Malformed
                ),
                "{c:?} -> {e:?}"
            );
        }
    }

    #[test]
    fn rejects_non_urls_and_paths() {
        for c in [
            "/etc/passwd",
            "./x.ts",
            "x.ts",
            "example.com/x.ts",
            "//example.com/x.ts",
            ":foo",
            "1http://example.com",
            "ht tp://example.com",
            "C:/Windows/win.ini",
        ] {
            let e = err(c);
            assert!(
                matches!(
                    e,
                    ValidationError::Malformed
                        | ValidationError::SchemeNotAllowed(_)
                        | ValidationError::Whitespace
                ),
                "{c:?} -> {e:?}"
            );
        }
        assert_eq!(err(r"C:\Windows\win.ini"), ValidationError::Backslash);
        assert_eq!(
            err(r"http://evil.example\@good.example/"),
            ValidationError::Backslash
        );
    }

    #[test]
    fn rejects_missing_or_empty_host() {
        assert!(matches!(
            err("http://"),
            ValidationError::Malformed | ValidationError::MissingHost
        ));
        assert!(matches!(
            err("https:///"),
            ValidationError::Malformed | ValidationError::MissingHost
        ));
        let p = UrlPolicy::ALL_STREAMING;
        assert!(matches!(
            PlayableUrl::parse_with_policy("rtmp:///live", p),
            Err(ValidationError::MissingHost | ValidationError::Malformed)
        ));
        assert!(matches!(
            PlayableUrl::parse_with_policy("rtsp:x", p),
            Err(ValidationError::MissingHost | ValidationError::Malformed)
        ));
    }

    #[test]
    fn rejects_control_characters_and_whitespace() {
        let bad = [
            (
                "http://example.com/a\r\nX-Injected: 1",
                ValidationError::ControlChar,
            ),
            ("http://example.com/a\n", ValidationError::ControlChar),
            ("http://example.com/a\t", ValidationError::ControlChar),
            ("http://example.com/\0.ts", ValidationError::ControlChar),
            ("http://example.com/\u{7f}", ValidationError::ControlChar),
            ("http://example.com/\u{85}", ValidationError::ControlChar),
            (
                "http://example.com/\u{1b}[31m",
                ValidationError::ControlChar,
            ),
            ("http://exa mple.com/", ValidationError::Whitespace),
            (" http://example.com/", ValidationError::Whitespace),
            ("http://example.com/ ", ValidationError::Whitespace),
            ("http://example.com/\u{a0}", ValidationError::Whitespace),
            ("http://example.com/\u{2028}", ValidationError::Whitespace),
            ("http://example.com/\u{3000}", ValidationError::Whitespace),
            ("http://example.com/\u{200b}", ValidationError::Whitespace),
            ("http://example.com/\u{202e}", ValidationError::Whitespace),
            ("http://example.com/\u{feff}", ValidationError::Whitespace),
        ];
        for (input, expected) in bad {
            assert_eq!(err(input), expected, "{input:?}");
        }
        assert_eq!(err(""), ValidationError::Empty);
        assert_eq!(err("   "), ValidationError::Whitespace);
    }

    #[test]
    fn enforces_length_limit() {
        let base = "http://example.com/";
        let exact = format!("{base}{}", "a".repeat(MAX_URL_LEN - base.len()));
        assert_eq!(exact.len(), MAX_URL_LEN);
        assert_eq!(ok(&exact).as_str().len(), MAX_URL_LEN);
        let over = format!("{exact}a");
        assert_eq!(err(&over), ValidationError::TooLong { max: MAX_URL_LEN });
        // Normalisation growth past the limit is caught too: each `|` stays,
        // but each `"` becomes `%22` (3 bytes).
        let grows = format!("{base}{}", "\"".repeat(MAX_URL_LEN - base.len()));
        assert_eq!(err(&grows), ValidationError::TooLong { max: MAX_URL_LEN });
    }

    #[test]
    fn policy_enables_extra_schemes() {
        let p = UrlPolicy {
            allow_rtmp: true,
            ..UrlPolicy::HTTP_ONLY
        };
        assert!(PlayableUrl::parse_with_policy("rtmp://example.com/live/x", p).is_ok());
        assert!(PlayableUrl::parse_with_policy("rtsp://example.com/x", p).is_err());
        let all = UrlPolicy::ALL_STREAMING;
        for u in [
            "rtmp://example.com/live/x",
            "rtmps://example.com/live/x",
            "rtsp://example.com:554/x",
            "rtsps://example.com/x",
            "udp://@239.0.0.1:1234",
            "udp://239.0.0.1:1234",
            "rtp://239.0.0.1:5004",
        ] {
            let v = PlayableUrl::parse_with_policy(u, all);
            assert!(v.is_ok(), "{u:?} -> {v:?}");
        }
        // Even the permissive policy never allows file/lavfi/etc.
        for u in ["file:///C:/x", "lavfi://sine", "edl://x", "concat:a|b"] {
            assert!(PlayableUrl::parse_with_policy(u, all).is_err(), "{u}");
        }
    }

    #[test]
    fn ffmpeg_protocol_whitelist_follows_policy() {
        assert_eq!(
            UrlPolicy::HTTP_ONLY.ffmpeg_protocols(false),
            "http,https,tls,tcp,crypto,data,httpproxy"
        );
        let all = UrlPolicy::ALL_STREAMING.ffmpeg_protocols(true);
        for p in ["rtmp", "rtsp", "udp", "rtp", "file"] {
            assert!(all.split(',').any(|x| x == p), "{p} in {all}");
        }
        assert_eq!(all.split(',').filter(|x| *x == "udp").count(), 1);
        assert!(
            !UrlPolicy::HTTP_ONLY
                .ffmpeg_protocols(false)
                .contains("file")
        );
    }

    #[test]
    fn debug_output_is_redacted() {
        let u = ok("http://user:secretpass@example.com/live/user/secretpass/1.ts");
        let d = format!("{u:?}");
        assert!(!d.contains("secret"), "{d}");
        assert!(!d.contains("example.com"), "{d}");
        assert_eq!(d, "PlayableUrl(http://<redacted>)");
        let r = Referrer::parse("http://example.com/secret").map(|r| format!("{r:?}"));
        assert_eq!(r.as_deref(), Ok("Referrer(<redacted>)"));
    }

    #[test]
    fn local_files_need_the_explicit_constructor() {
        let rel = PlayableUrl::from_local_file(Path::new("Cargo.toml"));
        assert_eq!(rel, Err(ValidationError::BadLocalPath));
        let missing = std::env::temp_dir().join("opensmarters-definitely-missing-file.ts");
        assert_eq!(
            PlayableUrl::from_local_file(&missing),
            Err(ValidationError::BadLocalPath)
        );
        let dir = std::env::temp_dir();
        assert_eq!(
            PlayableUrl::from_local_file(&dir),
            Err(ValidationError::BadLocalPath)
        );
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let u = PlayableUrl::from_local_file(&manifest).map_err(|e| e.to_string());
        let u = u.as_ref().map(|u| {
            (
                u.scheme(),
                u.is_local_file(),
                u.as_str().starts_with("file:///"),
            )
        });
        assert_eq!(u, Ok(("file", true, true)));
    }

    #[test]
    fn user_agent_validation() {
        assert!(UserAgent::new("opensmarters/0.1.0").is_ok());
        assert!(UserAgent::new("IPTVSmartersPro").is_ok());
        assert!(UserAgent::new("Mozilla/5.0 (Windows NT 10.0; Win64; x64)").is_ok());
        assert_eq!(UserAgent::new(""), Err(ValidationError::Empty));
        for bad in [
            "a\r\nX-Evil: 1",
            "a\nb",
            "a\0",
            "tab\there",
            "caf\u{e9}",
            "\u{7f}",
        ] {
            assert_eq!(
                UserAgent::new(bad),
                Err(ValidationError::ControlChar),
                "{bad:?}"
            );
        }
        let long = "a".repeat(MAX_HEADER_LEN + 1);
        assert_eq!(
            UserAgent::new(&long),
            Err(ValidationError::TooLong {
                max: MAX_HEADER_LEN
            })
        );
        assert!(UserAgent::new(&"a".repeat(MAX_HEADER_LEN)).is_ok());
    }

    #[test]
    fn referrer_validation() {
        assert_eq!(
            Referrer::parse("http://example.com/x").map(|r| r.as_str().to_owned()),
            Ok("http://example.com/x".to_owned())
        );
        assert!(Referrer::parse("http://example.com/\r\nX: y").is_err());
        assert!(Referrer::parse("file:///C:/x").is_err());
        assert!(Referrer::parse("not a url").is_err());
        let long = format!("http://example.com/{}", "a".repeat(MAX_HEADER_LEN));
        assert!(matches!(
            Referrer::parse(&long),
            Err(ValidationError::TooLong { .. })
        ));
    }

    #[test]
    fn raw_scheme_parsing() {
        assert_eq!(raw_scheme("HTTP://x").as_deref(), Some("http"));
        assert_eq!(
            raw_scheme("crypto+http://x").as_deref(),
            Some("crypto+http")
        );
        assert_eq!(raw_scheme("subfile,,start:x"), None);
        assert_eq!(raw_scheme("://x"), None);
        assert_eq!(raw_scheme("noscheme"), None);
    }

    /// A unique scratch folder under the system temp dir (removed on drop).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "opensmarters-player-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
            Self(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ts_bytes(packets: usize) -> Vec<u8> {
        let mut v = vec![0u8; 188 * packets];
        for k in 0..packets {
            v[k * 188] = 0x47;
        }
        v
    }

    #[test]
    fn sniffs_containers_and_refuses_text() {
        assert_eq!(sniff_container(&ts_bytes(3)), Some(Container::MpegTs));
        let mut m2ts = vec![0u8; 192 * 3];
        for k in 0..3 {
            m2ts[k * 192 + 4] = 0x47;
        }
        assert_eq!(sniff_container(&m2ts), Some(Container::MpegTs));
        assert_eq!(
            sniff_container(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0]),
            Some(Container::Matroska)
        );
        assert_eq!(sniff_container(b"\0\0\0\x20ftypisom"), Some(Container::Mp4));
        assert_eq!(
            sniff_container(b"RIFF\0\0\0\0AVI LIST"),
            Some(Container::Avi)
        );
        assert_eq!(sniff_container(b"FLV\x01\x05"), Some(Container::Flv));
        assert_eq!(
            sniff_container(&[0, 0, 1, 0xBA, 0x44]),
            Some(Container::MpegPs)
        );
        for text in [
            &b"#EXTM3U\n#EXTINF:-1,x\nfile:///C:/Windows/win.ini\n"[..],
            b"[playlist]\nFile1=http://x/\n",
            b"# mpv EDL v0\nfile:///C:/x.mkv\n",
            b"<?xml version=\"1.0\"?><playlist/>",
            b"<html><body>403 Forbidden</body></html>",
            b"G",
            b"",
        ] {
            assert_eq!(
                sniff_container(text),
                None,
                "{:?}",
                String::from_utf8_lossy(text)
            );
        }
        // A single 0x47 followed by garbage is not a transport stream.
        let mut fake = ts_bytes(3);
        fake[188] = b'#';
        assert_eq!(sniff_container(&fake), None);
    }

    #[test]
    fn owned_media_must_be_inside_the_root_and_real_media() {
        let root = Scratch::new("owned");
        let other = Scratch::new("other");
        let good = root.0.join("Film.ts");
        std::fs::write(&good, ts_bytes(4)).unwrap_or_else(|e| panic!("{e}"));
        let u = PlayableUrl::from_owned_media(&root.0, &good).unwrap_or_else(|e| panic!("{e}"));
        assert!(u.is_local_file() && u.is_owned_media());
        assert!(u.as_str().starts_with("file:///"));

        let playlist = root.0.join("Not a film.mkv");
        std::fs::write(&playlist, b"#EXTM3U\nhttp://example.com/x.ts\n")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            PlayableUrl::from_owned_media(&root.0, &playlist),
            Err(ValidationError::NotMedia)
        );

        let outside = other.0.join("Film.ts");
        std::fs::write(&outside, ts_bytes(4)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            PlayableUrl::from_owned_media(&root.0, &outside),
            Err(ValidationError::OutsideRoot)
        );
        let sneaky = root
            .0
            .join("..")
            .join(other.0.file_name().unwrap_or_default())
            .join("Film.ts");
        assert_eq!(
            PlayableUrl::from_owned_media(&root.0, &sneaky),
            Err(ValidationError::OutsideRoot)
        );
        assert_eq!(
            PlayableUrl::from_owned_media(&root.0, &root.0),
            Err(ValidationError::BadLocalPath)
        );
        assert_eq!(
            PlayableUrl::from_owned_media(Path::new("relative"), &good),
            Err(ValidationError::BadLocalPath)
        );
        assert_eq!(
            PlayableUrl::from_owned_media(&root.0, &root.0.join("missing.ts")),
            Err(ValidationError::BadLocalPath)
        );
    }

    #[test]
    fn media_outputs_are_new_safe_names() {
        let root = Scratch::new("out");
        let o = MediaOutput::new(&root.0, "News 20-00 clip.ts").unwrap_or_else(|e| panic!("{e}"));
        assert!(o.path().is_absolute());
        assert!(o.path().ends_with("News 20-00 clip.ts"));
        assert!(MediaOutput::new(&root.0, "Clip.MKV").is_ok());
        for bad in [
            "",
            "clip",
            "clip.m3u8",
            "clip.txt",
            ".clip.ts",
            "clip.ts ",
            "../clip.ts",
            "a/b.ts",
            r"a\b.ts",
            "C:clip.ts",
            "x%03d.ts",
            "a\nb.ts",
            "CON.ts",
            "lpt1.mkv",
        ] {
            assert_eq!(
                MediaOutput::new(&root.0, bad),
                Err(ValidationError::BadFileName),
                "{bad:?}"
            );
        }
        std::fs::write(root.0.join("taken.ts"), b"x").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            MediaOutput::new(&root.0, "taken.ts"),
            Err(ValidationError::AlreadyExists)
        );
        assert_eq!(
            MediaOutput::new(Path::new("relative"), "clip.ts"),
            Err(ValidationError::BadLocalPath)
        );
    }
}

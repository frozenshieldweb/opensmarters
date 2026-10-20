// SPDX-License-Identifier: GPL-3.0-or-later
//! Credential redaction for log lines.
//!
//! Xtream credentials travel in URLs: in the query string of API calls
//! (`player_api.php?username=…&password=…`) and in the path of stream URLs
//! (`/live/{user}/{pass}/{id}.ts`). mpv and ffmpeg echo those URLs in their logs. Every
//! log line must therefore go through a [`Redactor`] before it is written anywhere.
//!
//! Two layers:
//!
//! 1. **Structural** ([`redact_url`]), works on any text, no knowledge needed:
//!    - query values of `username`, `password`, `user`, `pass`, `pwd`, `passwd`,
//!      `token` and `auth` (case-insensitive) → `***`;
//!    - the two path segments after `/live/`, `/movie/`, `/series/`, `/timeshift/`
//!      when at least three segments follow (`/live/***/***/1.ts`);
//!    - URL userinfo: `scheme://user:pass@host` → `scheme://***@host`.
//! 2. **Known secrets** ([`Redactor`]): exact occurrences of registered secrets
//!    (passwords, usernames, whole M3U URLs), including their common percent-encoded
//!    and form-encoded spellings, are replaced by `***`. Secrets shorter than
//!    [`MIN_SECRET_LEN`] are not scrubbed from free text (it would mangle every line);
//!    they are still covered by the structural layer inside URLs.
//!
//! [`RedactingWriter`] wraps any `io::Write` (e.g. stderr or a log file) so it can be
//! plugged into `tracing_subscriber::fmt().with_writer(…)`.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, PoisonError, RwLock};

use percent_encoding::{AsciiSet, CONTROLS, NON_ALPHANUMERIC, utf8_percent_encode};

/// Replacement text for redacted values.
pub const REDACTED: &str = "***";

/// Secrets shorter than this (in bytes) are only redacted structurally.
pub const MIN_SECRET_LEN: usize = 3;

const SENSITIVE_PARAMS: [&str; 8] = [
    "username", "password", "user", "pass", "pwd", "passwd", "token", "auth",
];

const CREDENTIAL_PATHS: [&str; 4] = ["/live/", "/movie/", "/series/", "/timeshift/"];

/// The `url` crate's path-segment encode set.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'?')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

/// The `url` crate's query encode set (special schemes).
const QUERY: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'\'');

/// Ends a URL token in free text.
fn is_url_delim(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'>' | b'`')
}

/// Ends a query value.
fn is_value_delim(b: u8) -> bool {
    is_url_delim(b) || matches!(b, b'&' | b'#')
}

/// Structurally redacts credentials in URLs found anywhere in `text`. See the
/// [module docs](self).
pub fn redact_url(text: &str) -> String {
    let step1 = redact_userinfo(text);
    let step2 = redact_query_params(&step1);
    redact_credential_paths(&step2)
}

fn redact_userinfo(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(rel) = text[search..].find("://") {
        let auth_start = search + rel + 3;
        let auth_end = bytes[auth_start..]
            .iter()
            .position(|&b| is_url_delim(b) || matches!(b, b'/' | b'?' | b'#'))
            .map_or(bytes.len(), |p| auth_start + p);
        if let Some(at) = text[auth_start..auth_end].rfind('@') {
            out.push_str(&text[last..auth_start]);
            out.push_str(REDACTED);
            last = auth_start + at;
        }
        search = auth_end.max(auth_start);
    }
    out.push_str(&text[last..]);
    out
}

fn redact_query_params(text: &str) -> String {
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'?' | b'&' | b';') {
            let name_start = i + 1;
            let matched = SENSITIVE_PARAMS.iter().find(|p| {
                let end = name_start + p.len();
                lower_bytes.get(name_start..end) == Some(p.as_bytes())
                    && bytes.get(end) == Some(&b'=')
            });
            if let Some(param) = matched {
                let value_start = name_start + param.len() + 1;
                let value_end = bytes[value_start..]
                    .iter()
                    .position(|&b| is_value_delim(b))
                    .map_or(bytes.len(), |p| value_start + p);
                if value_end > value_start {
                    out.push_str(&text[last..value_start]);
                    out.push_str(REDACTED);
                    last = value_end;
                }
                i = value_end.max(value_start);
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

fn redact_credential_paths(text: &str) -> String {
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut search = 0;
    loop {
        let next = CREDENTIAL_PATHS
            .iter()
            .filter_map(|m| lower[search..].find(m).map(|p| (search + p, m.len())))
            .min_by_key(|(pos, _)| *pos);
        let Some((pos, len)) = next else { break };
        let seg_start = pos + len;
        let token_end = bytes[seg_start..]
            .iter()
            .position(|&b| is_url_delim(b) || matches!(b, b'?' | b'#'))
            .map_or(bytes.len(), |p| seg_start + p);
        let rest = &text[seg_start..token_end];
        let mut segs = rest.splitn(3, '/');
        match (segs.next(), segs.next(), segs.next()) {
            (Some(user), Some(pass), Some(tail))
                if !user.is_empty() && !pass.is_empty() && !tail.is_empty() =>
            {
                out.push_str(&text[last..seg_start]);
                out.push_str(REDACTED);
                out.push('/');
                out.push_str(REDACTED);
                out.push('/');
                last = seg_start + user.len() + 1 + pass.len() + 1;
                search = last;
            }
            _ => search = seg_start,
        }
    }
    out.push_str(&text[last..]);
    out
}

/// The spellings under which a secret may appear in a log line.
fn variants(secret: &str) -> Vec<String> {
    let mut v = vec![
        secret.to_owned(),
        utf8_percent_encode(secret, NON_ALPHANUMERIC).to_string(),
        utf8_percent_encode(secret, PATH_SEGMENT).to_string(),
        utf8_percent_encode(secret, QUERY).to_string(),
        url::form_urlencoded::byte_serialize(secret.as_bytes()).collect(),
    ];
    // Lower-case hex escapes.
    let lower_hex: Vec<String> = v.iter().map(|s| lower_percent_hex(s)).collect();
    v.extend(lower_hex);
    v.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    v.dedup();
    v
}

fn lower_percent_hex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '%' {
            for _ in 0..2 {
                if let Some(h) = chars.next() {
                    out.push(h.to_ascii_lowercase());
                }
            }
        }
    }
    out
}

#[derive(Default)]
struct Inner {
    /// (secret, its spellings longest first)
    secrets: Vec<(String, Vec<String>)>,
}

/// Scrubs known secrets and URL credentials from text. Cheap to clone: clones share the
/// same secret set, so the app can register a new account's password once and every
/// clone (tracing writer, mpv log forwarder, Xtream client) sees it.
///
/// `Debug` shows only the number of secrets.
#[derive(Clone, Default)]
pub struct Redactor {
    inner: Arc<RwLock<Inner>>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("secrets", &self.secret_count())
            .finish()
    }
}

impl Redactor {
    /// An empty redactor (structural redaction only).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a secret (password, username, M3U URL, …). Empty strings are ignored;
    /// secrets shorter than [`MIN_SECRET_LEN`] are accepted but only redacted
    /// structurally.
    pub fn add_secret(&self, secret: &str) {
        if secret.is_empty() {
            return;
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if inner.secrets.iter().any(|(s, _)| s == secret) {
            return;
        }
        inner.secrets.push((secret.to_owned(), variants(secret)));
        // Longest first, so a secret containing another is replaced whole.
        inner
            .secrets
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    /// Unregisters a secret (e.g. when an account is deleted).
    pub fn remove_secret(&self, secret: &str) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        inner.secrets.retain(|(s, _)| s != secret);
    }

    /// Number of registered secrets.
    pub fn secret_count(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .secrets
            .len()
    }

    /// Redacts `text`: known secrets first (so a whole M3U URL disappears), then the
    /// structural URL rules.
    pub fn redact(&self, text: &str) -> String {
        let scrubbed = {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            let mut current = text.to_owned();
            for (secret, spellings) in &inner.secrets {
                if secret.len() < MIN_SECRET_LEN {
                    continue;
                }
                for spelling in spellings {
                    if current.contains(spelling.as_str()) {
                        current = current.replace(spelling.as_str(), REDACTED);
                    }
                }
            }
            current
        };
        redact_url(&scrubbed)
    }
}

/// Maximum bytes buffered without a newline before a [`RedactingWriter`] flushes anyway.
const MAX_PENDING: usize = 64 * 1024;

/// An `io::Write` adapter that redacts each line before forwarding it.
///
/// Bytes are buffered until a newline (or [`Write::flush`], or drop), so a secret split
/// across two `write` calls is still caught.
///
/// ```
/// use std::io::Write;
/// use opensmarters_core::redact::{RedactingWriter, Redactor};
///
/// let redactor = Redactor::new();
/// redactor.add_secret("hunter22");
/// let mut sink = Vec::new();
/// {
///     let mut w = RedactingWriter::new(&mut sink, redactor.clone());
///     writeln!(w, "GET http://h/live/u/hunter22/1.ts?token=abc").unwrap();
/// }
/// assert_eq!(String::from_utf8(sink).unwrap(), "GET http://h/live/***/***/1.ts?token=***\n");
/// ```
///
/// With `tracing_subscriber` (in the app):
/// `fmt().with_writer(move || RedactingWriter::new(std::io::stderr(), redactor.clone()))`.
pub struct RedactingWriter<W: Write> {
    inner: W,
    redactor: Redactor,
    pending: Vec<u8>,
}

impl<W: Write> fmt::Debug for RedactingWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedactingWriter")
            .field("pending_bytes", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl<W: Write> RedactingWriter<W> {
    /// Wraps `inner`.
    pub fn new(inner: W, redactor: Redactor) -> Self {
        RedactingWriter {
            inner,
            redactor,
            pending: Vec::new(),
        }
    }

    fn emit(&mut self, upto: usize) -> io::Result<()> {
        let chunk: Vec<u8> = self.pending.drain(..upto).collect();
        let text = String::from_utf8_lossy(&chunk);
        self.inner.write_all(self.redactor.redact(&text).as_bytes())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(nl) = self.pending.iter().position(|&b| b == b'\n') {
            self.emit(nl + 1)?;
        }
        if self.pending.len() > MAX_PENDING {
            self.emit(self.pending.len())?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            self.emit(self.pending.len())?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        // Best effort: a failing log sink must not panic.
        let _ = self.flush();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn query_params() {
        assert_eq!(
            redact_url(
                "GET http://panel.example.com/player_api.php?username=alice&password=s3cret&action=get_live_streams"
            ),
            "GET http://panel.example.com/player_api.php?username=***&password=***&action=get_live_streams"
        );
        assert_eq!(
            redact_url("http://h/get.php?type=m3u&PassWord=x%26y#frag"),
            "http://h/get.php?type=m3u&PassWord=***#frag"
        );
        assert_eq!(redact_url("a?token=abc def"), "a?token=*** def");
        assert_eq!(redact_url("x?username=&b=1"), "x?username=&b=1");
        assert_eq!(redact_url("x?usernames=1"), "x?usernames=1");
        assert_eq!(
            redact_url("x?a=1&amp;password=zz"),
            "x?a=1&amp;password=***"
        );
        assert_eq!(
            redact_url("\"http://h/x?pass=p\""),
            "\"http://h/x?pass=***\""
        );
    }

    #[test]
    fn credential_paths() {
        assert_eq!(
            redact_url("Opening http://stream.example.net:8080/live/alice/s3cret/101.ts done"),
            "Opening http://stream.example.net:8080/live/***/***/101.ts done"
        );
        assert_eq!(
            redact_url("http://h/movie/u/p/201.mkv http://h/series/u/p/1001.mp4"),
            "http://h/movie/***/***/201.mkv http://h/series/***/***/1001.mp4"
        );
        assert_eq!(
            redact_url("http://h/timeshift/u/p/60/2024-01-01:07-00/101.ts"),
            "http://h/timeshift/***/***/60/2024-01-01:07-00/101.ts"
        );
        assert_eq!(
            redact_url("http://h/LIVE/u/p/1.m3u8"),
            "http://h/LIVE/***/***/1.m3u8"
        );
        // Percent-encoded segments stay one segment.
        assert_eq!(
            redact_url("http://h/live/a%2Fb/c%23d/1.ts"),
            "http://h/live/***/***/1.ts"
        );
        // Not enough segments: not a stream URL.
        assert_eq!(
            redact_url("http://h/live/index.html"),
            "http://h/live/index.html"
        );
        assert_eq!(redact_url("see /live/ docs"), "see /live/ docs");
    }

    #[test]
    fn userinfo() {
        assert_eq!(
            redact_url("http://alice:s3cret@panel.example.com:8080/x"),
            "http://***@panel.example.com:8080/x"
        );
        assert_eq!(
            redact_url("rtsp://cam@192.0.2.1/stream"),
            "rtsp://***@192.0.2.1/stream"
        );
        assert_eq!(redact_url("mailto:a@b.c"), "mailto:a@b.c");
        assert_eq!(redact_url("http://h/path@x"), "http://h/path@x");
    }

    #[test]
    fn all_layers_together() {
        let line = "[ffmpeg] http://u:p@h/live/u/p/1.ts?token=t then https://h/get.php?username=u&password=p";
        let out = redact_url(line);
        assert_eq!(
            out,
            "[ffmpeg] http://***@h/live/***/***/1.ts?token=*** then https://h/get.php?username=***&password=***"
        );
    }

    #[test]
    fn redactor_scrubs_known_secrets_and_encodings() {
        let r = Redactor::new();
        r.add_secret("p@ss w/rd#1");
        assert_eq!(r.secret_count(), 1);
        for spelling in [
            "p@ss w/rd#1",
            "p%40ss%20w%2Frd%231",
            "p%40ss%20w%2frd%231",
            "p@ss%20w%2Frd%231",
            "p%40ss+w%2Frd%231",
        ] {
            let out = r.redact(&format!("error near {spelling} in body"));
            assert_eq!(out, "error near *** in body", "{spelling}");
        }
        // Free text (not in a URL) is scrubbed too.
        assert_eq!(r.redact("password is p@ss w/rd#1!"), "password is ***!");
    }

    #[test]
    fn redactor_handles_whole_urls_and_overlaps() {
        let r = Redactor::new();
        let m3u = "https://lists.example.org/private/abcdef123456.m3u";
        r.add_secret(m3u);
        r.add_secret("abc");
        r.add_secret("abcdef");
        assert_eq!(r.redact(&format!("loading {m3u} ...")), "loading *** ...");
        assert_eq!(r.redact("abcdef and abc"), "*** and ***");
        // Clones share state.
        let clone = r.clone();
        clone.add_secret("zzz9");
        assert_eq!(r.redact("zzz9"), "***");
        r.remove_secret("zzz9");
        assert_eq!(clone.redact("zzz9"), "zzz9");
        // Debug never prints secrets.
        assert!(!format!("{r:?}").contains("abc"));
    }

    #[test]
    fn short_secrets_only_structural() {
        let r = Redactor::new();
        r.add_secret("ab");
        r.add_secret("");
        assert_eq!(r.redact("a cab ab"), "a cab ab");
        assert_eq!(
            r.redact("http://h/live/ab/ab/1.ts"),
            "http://h/live/***/***/1.ts"
        );
    }

    #[test]
    fn unicode_is_safe() {
        let r = Redactor::new();
        r.add_secret("mötley€");
        assert_eq!(r.redact("x mötley€ y"), "x *** y");
        assert_eq!(r.redact("x m%C3%B6tley%E2%82%AC y"), "x *** y");
        // Non-ASCII around delimiters must not break slicing.
        assert_eq!(redact_url("é?password=ü&é"), "é?password=***&é");
        assert_eq!(redact_url("€://é@ü"), "€://***@ü");
        assert_eq!(redact_url("/live/é/ü/€"), "/live/***/***/€");
    }

    #[test]
    fn writer_redacts_per_line() {
        let r = Redactor::new();
        r.add_secret("hunter22");
        let mut sink = Vec::new();
        {
            let mut w = RedactingWriter::new(&mut sink, r);
            w.write_all(b"first hunt").unwrap();
            w.write_all(b"er22 line\nsecond http://h/live/u/p/1.ts")
                .unwrap();
            // Dropped without explicit flush: the remainder is still written, redacted.
        }
        let text = String::from_utf8(sink).unwrap();
        assert_eq!(text, "first *** line\nsecond http://h/live/***/***/1.ts");
    }
}

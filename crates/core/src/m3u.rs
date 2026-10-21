// SPDX-License-Identifier: GPL-3.0-or-later
//! Line-based M3U / M3U8 (IPTV "extended M3U") playlist parser.
//!
//! Unlike the original client (which split the whole text on `#EXTINF` and took the
//! URL from the last `http://`), this parser works **line by line**:
//!
//! - Input: UTF-8 (BOM stripped, invalid sequences replaced) or UTF-16 with BOM;
//!   `\r\n`, `\n` and lone `\r` line endings; leading/trailing whitespace ignored.
//! - `#EXTM3U [attrs]`: header attributes; `url-tvg` / `x-tvg-url` / `tvg-url` give
//!   XMLTV URLs (comma-separated lists allowed) → [`Playlist::tvg_urls`].
//! - `#EXTINF:<duration> [attrs],<title>`: a real attribute tokenizer — quoted values
//!   (`"…"` or `'…'`) may contain commas, spaces, `=` and other key names; unquoted
//!   values end at whitespace or a comma; keys are case-insensitive (lower-cased); for
//!   duplicates the first non-empty value wins (all are kept in
//!   [`M3uEntry::attributes`]); the title is everything after the first comma outside
//!   quotes.
//! - `#EXTGRP:<group>`: group when `group-title` is absent.
//! - `#EXTVLCOPT:http-user-agent=…` / `http-referrer=…` (or `http-referer`).
//! - `#KODIPROP`, `#EXT-X-…`, other `#` lines and blank lines are ignored.
//! - The URL is the **next non-comment line**. Directives seen since the previous URL
//!   apply to it; nothing is ever inherited from a previous entry. An `#EXTINF` without
//!   a URL is dropped and counted in [`Playlist::skipped`].
//! - Any URI scheme is accepted here; **the player must validate schemes**. A line
//!   that is not preceded by `#EXTINF` is only taken as an entry if it looks like a URI
//!   (`scheme:…`), so an HTML error page is rejected with [`M3uError::NotAPlaylist`].
//! - Entries are classified ([`EntryKind`]) by URL: `/movie/` → movie, `/series/` →
//!   series, VOD file extensions (`.mp4`, `.mkv`, `.avi`, …) → movie, else live.
//! - Limits ([`M3uLimits`]): total size, line length and entry count, each with its own
//!   error; attributes per line and XMLTV URLs are capped (extra ones are ignored), so
//!   neither memory nor parse time can be blown up by a hostile playlist.
//!
//! [`detect_xtream`] recognises `get.php?username=…&password=…&type=m3u…` playlist URLs
//! so the app can offer an Xtream login instead.

use std::borrow::Cow;
use std::fmt;

use secrecy::SecretString;
use url::Url;

use crate::de::parse_u64_str;
use crate::model::{Category, CategoryId, ServerUrl};
use crate::redact::redact_url;

/// Size limits for [`parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct M3uLimits {
    /// Maximum input size in bytes. Default 256 MiB.
    pub max_total_bytes: usize,
    /// Maximum length of one line in bytes. Default 256 KiB.
    pub max_line_len: usize,
    /// Maximum number of entries. Default 1 000 000.
    pub max_entries: usize,
    /// Attributes kept per `#EXTINF` / `#EXTM3U` line; further ones are ignored (the
    /// title is still found). Bounds memory: each kept attribute costs ~100 bytes, so
    /// without a cap a playlist of `a=b a=b …` lines grows ~20× in memory. Default 64.
    pub max_attributes_per_line: usize,
    /// XMLTV URLs kept from `#EXTM3U` headers ([`Playlist::tvg_urls`]); further ones
    /// are ignored. Default 16.
    pub max_tvg_urls: usize,
}

impl Default for M3uLimits {
    fn default() -> Self {
        M3uLimits {
            max_total_bytes: 256 * 1024 * 1024,
            max_line_len: 256 * 1024,
            max_entries: 1_000_000,
            max_attributes_per_line: 64,
            max_tvg_urls: 16,
        }
    }
}

/// `#EXTM3U` attributes kept in [`Playlist::header_attributes`] across all header lines.
const MAX_HEADER_ATTRIBUTES: usize = 256;

/// Why a playlist could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum M3uError {
    /// The input exceeds [`M3uLimits::max_total_bytes`].
    #[error("the playlist is larger than {limit} bytes")]
    TooLarge {
        /// The limit.
        limit: usize,
    },
    /// A line exceeds [`M3uLimits::max_line_len`].
    #[error("line {line} of the playlist is longer than {limit} bytes")]
    LineTooLong {
        /// 1-based line number.
        line: usize,
        /// The limit.
        limit: usize,
    },
    /// More than [`M3uLimits::max_entries`] entries.
    #[error("the playlist has more than {limit} entries")]
    TooManyEntries {
        /// The limit.
        limit: usize,
    },
    /// No `#EXTM3U`, no `#EXTINF` and no URI lines: not a playlist (e.g. an HTML page).
    #[error("this is not an M3U playlist")]
    NotAPlaylist,
}

/// What an entry is, judged from its URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EntryKind {
    /// A live channel (default).
    #[default]
    Live,
    /// A movie (`/movie/` path or a VOD file extension).
    Movie,
    /// A series episode (`/series/` path).
    Series,
}

/// File extensions that mark an entry as VOD.
pub const VOD_EXTENSIONS: [&str; 12] = [
    "mp4", "mkv", "avi", "mov", "wmv", "m4v", "webm", "mpg", "mpeg", "divx", "ogv", "3gp",
];

/// Classifies a URL: `/movie/` → [`EntryKind::Movie`], `/series/` →
/// [`EntryKind::Series`], a [`VOD_EXTENSIONS`] file → [`EntryKind::Movie`], else
/// [`EntryKind::Live`]. Case-insensitive; query and fragment are ignored.
pub fn classify_url(url: &str) -> EntryKind {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let lower = path.to_ascii_lowercase();
    if lower.contains("/movie/") {
        return EntryKind::Movie;
    }
    if lower.contains("/series/") {
        return EntryKind::Series;
    }
    let file = lower.rsplit('/').next().unwrap_or_default();
    match file.rsplit_once('.') {
        Some((_, ext)) if VOD_EXTENSIONS.contains(&ext) => EntryKind::Movie,
        _ => EntryKind::Live,
    }
}

/// One playlist entry.
///
/// `Debug` redacts credentials in URL fields.
#[derive(Clone, PartialEq, Default)]
pub struct M3uEntry {
    /// Title (after the comma), else `tvg-name`, else the URL's file name.
    pub title: String,
    /// Stream URL, verbatim (any scheme; validate before playback).
    pub url: String,
    /// `#EXTINF` duration in seconds; `None` for `-1`/`0`/invalid.
    pub duration: Option<f64>,
    /// Live / movie / series.
    pub kind: EntryKind,
    /// `tvg-id` (XMLTV channel id).
    pub tvg_id: Option<String>,
    /// `tvg-name`.
    pub tvg_name: Option<String>,
    /// `tvg-logo`.
    pub tvg_logo: Option<String>,
    /// `tvg-chno` (channel number).
    pub tvg_chno: Option<u32>,
    /// `group-title`, else `#EXTGRP`.
    pub group_title: Option<String>,
    /// `catchup` (`default`, `append`, `shift`, `flussonic`, `xc`, …).
    pub catchup: Option<String>,
    /// `catchup-days`.
    pub catchup_days: Option<u32>,
    /// `catchup-source` template.
    pub catchup_source: Option<String>,
    /// `tvg-shift` in hours.
    pub tvg_shift: Option<f32>,
    /// `#EXTVLCOPT:http-user-agent` (or a `http-user-agent` / `user-agent` attribute).
    pub user_agent: Option<String>,
    /// `#EXTVLCOPT:http-referrer` (or a `http-referrer` / `referrer` attribute).
    pub referrer: Option<String>,
    /// All `#EXTINF` attributes, keys lower-cased, in order (duplicates kept).
    pub attributes: Vec<(String, String)>,
    /// 1-based line number of the URL line.
    pub line: usize,
}

impl M3uEntry {
    /// First non-empty value of an attribute (key is matched case-insensitively).
    pub fn attribute(&self, key: &str) -> Option<&str> {
        let key = key.to_lowercase();
        self.attributes
            .iter()
            .find(|(k, v)| *k == key && !v.trim().is_empty())
            .map(|(_, v)| v.trim())
    }

    /// The category id for this entry (its group).
    pub fn category_id(&self) -> Option<CategoryId> {
        self.group_title.as_deref().map(CategoryId::from)
    }
}

impl fmt::Debug for M3uEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redact = |s: &Option<String>| s.as_deref().map(redact_url);
        f.debug_struct("M3uEntry")
            .field("title", &self.title)
            .field("url", &redact_url(&self.url))
            .field("kind", &self.kind)
            .field("tvg_id", &self.tvg_id)
            .field("tvg_name", &self.tvg_name)
            .field("tvg_logo", &redact(&self.tvg_logo))
            .field("tvg_chno", &self.tvg_chno)
            .field("group_title", &self.group_title)
            .field("catchup", &self.catchup)
            .field("catchup_days", &self.catchup_days)
            .field("catchup_source", &redact(&self.catchup_source))
            .field("tvg_shift", &self.tvg_shift)
            .field("line", &self.line)
            .finish_non_exhaustive()
    }
}

/// A parsed playlist.
#[derive(Clone, PartialEq, Default)]
pub struct Playlist {
    /// `#EXTM3U` attributes, keys lower-cased.
    pub header_attributes: Vec<(String, String)>,
    /// XMLTV URLs from `url-tvg` / `x-tvg-url` / `tvg-url`, deduplicated, in order.
    pub tvg_urls: Vec<String>,
    /// Entries in file order.
    pub entries: Vec<M3uEntry>,
    /// Entries dropped: `#EXTINF` without URL, non-URI lines outside entries.
    pub skipped: usize,
}

impl fmt::Debug for Playlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tvg: Vec<String> = self.tvg_urls.iter().map(|u| redact_url(u)).collect();
        f.debug_struct("Playlist")
            .field("tvg_urls", &tvg)
            .field("entries", &self.entries)
            .field("skipped", &self.skipped)
            .finish_non_exhaustive()
    }
}

impl Playlist {
    /// Categories: the distinct groups in order of first appearance (id = name = group).
    pub fn categories(&self) -> Vec<Category> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for g in self.entries.iter().filter_map(|e| e.group_title.as_deref()) {
            if seen.insert(g) {
                out.push(Category {
                    id: CategoryId::from(g),
                    name: g.to_owned(),
                    parent_id: None,
                });
            }
        }
        out
    }

    /// Entries of a given kind.
    pub fn entries_of_kind(&self, kind: EntryKind) -> impl Iterator<Item = &M3uEntry> {
        self.entries.iter().filter(move |e| e.kind == kind)
    }
}

/// Parses playlist bytes (see the [module docs](self)).
///
/// # Errors
///
/// A [`M3uError`] when a limit is exceeded or the input is not a playlist.
pub fn parse(input: &[u8], limits: &M3uLimits) -> Result<Playlist, M3uError> {
    if input.len() > limits.max_total_bytes {
        return Err(M3uError::TooLarge {
            limit: limits.max_total_bytes,
        });
    }
    parse_text(&decode_input(input), limits)
}

/// Parses an already-decoded playlist.
///
/// # Errors
///
/// As [`parse`].
pub fn parse_str(input: &str, limits: &M3uLimits) -> Result<Playlist, M3uError> {
    if input.len() > limits.max_total_bytes {
        return Err(M3uError::TooLarge {
            limit: limits.max_total_bytes,
        });
    }
    parse_text(input.strip_prefix('\u{FEFF}').unwrap_or(input), limits)
}

fn decode_input(input: &[u8]) -> Cow<'_, str> {
    if let Some(rest) = input.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest);
    }
    if let Some(rest) = input.strip_prefix(b"\xFF\xFE") {
        return Cow::Owned(decode_utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = input.strip_prefix(b"\xFE\xFF") {
        return Cow::Owned(decode_utf16(rest, u16::from_be_bytes));
    }
    String::from_utf8_lossy(input)
}

fn decode_utf16(bytes: &[u8], to_u16: fn([u8; 2]) -> u16) -> String {
    let units = bytes.as_chunks::<2>().0.iter().copied().map(to_u16);
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Splits on `\r\n`, `\n` or `\r`.
struct Lines<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            return None;
        }
        match self.rest.find(['\r', '\n']) {
            Some(i) => {
                let line = &self.rest[..i];
                let skip = if self.rest[i..].starts_with("\r\n") {
                    2
                } else {
                    1
                };
                self.rest = &self.rest[i + skip..];
                Some(line)
            }
            None => {
                let line = self.rest;
                self.rest = "";
                Some(line)
            }
        }
    }
}

/// Case-insensitive directive match: `name` followed by `:`, whitespace or end.
fn strip_directive<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let head = line.get(..name.len())?;
    if !head.eq_ignore_ascii_case(name) {
        return None;
    }
    let rest = &line[name.len()..];
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c == ':' || c.is_whitespace() => Some(rest),
        Some(_) => None,
    }
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Tokenizes `key=value` attributes. With `title_comma`, an unquoted comma ends the
/// attributes and the rest is returned as the title.
/// [`tokenize_capped`] without a cap (tests).
#[cfg(test)]
fn tokenize(s: &str, title_comma: bool) -> (Vec<(String, String)>, Option<String>) {
    tokenize_capped(s, title_comma, usize::MAX)
}

/// Like `tokenize`, keeping at most `max_attrs` attributes (the rest of the line is
/// still scanned, so the title after the comma is found).
fn tokenize_capped(
    s: &str,
    title_comma: bool,
    max_attrs: usize,
) -> (Vec<(String, String)>, Option<String>) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut attrs = Vec::new();
    let mut i = 0;
    let is_stop = |b: u8| is_ws(b) || (title_comma && b == b',');
    loop {
        while i < n && is_ws(bytes[i]) {
            i += 1;
        }
        if i >= n {
            return (attrs, None);
        }
        if title_comma && bytes[i] == b',' {
            return (attrs, Some(s[i + 1..].trim().to_owned()));
        }
        let key_start = i;
        while i < n && !is_stop(bytes[i]) && bytes[i] != b'=' {
            i += 1;
        }
        let key = &s[key_start..i];
        let mut j = i;
        while j < n && is_ws(bytes[j]) {
            j += 1;
        }
        if j < n && bytes[j] == b'=' {
            i = j + 1;
            while i < n && is_ws(bytes[i]) {
                i += 1;
            }
            let value = if i < n && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = char::from(bytes[i]);
                let start = i + 1;
                if let Some(rel) = s[start..].find(quote) {
                    i = start + rel + 1;
                    &s[start..start + rel]
                } else {
                    // Unterminated quote: the value runs to the next comma.
                    let end = if title_comma {
                        s[start..].find(',').map_or(n, |r| start + r)
                    } else {
                        n
                    };
                    i = end;
                    &s[start..end]
                }
            } else {
                let start = i;
                while i < n && !is_stop(bytes[i]) {
                    i += 1;
                }
                &s[start..i]
            };
            if !key.is_empty() && attrs.len() < max_attrs {
                attrs.push((key.to_lowercase(), value.to_owned()));
            }
        } else {
            if !key.is_empty() && attrs.len() < max_attrs {
                attrs.push((key.to_lowercase(), String::new()));
            }
            if i == key_start {
                i += 1; // defensive: always make progress
            }
        }
    }
}

#[derive(Default)]
struct ExtInf {
    duration: Option<f64>,
    attrs: Vec<(String, String)>,
    title: String,
}

fn parse_extinf(rest: &str, max_attrs: usize) -> ExtInf {
    let s = rest.trim_start();
    let dur_len = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.')))
        .unwrap_or(s.len());
    let duration = s[..dur_len]
        .parse::<f64>()
        .ok()
        .filter(|d| d.is_finite() && *d > 0.0);
    let (attrs, title) = tokenize_capped(&s[dur_len..], true, max_attrs);
    ExtInf {
        duration,
        attrs,
        title: title.unwrap_or_default(),
    }
}

#[derive(Default)]
struct Pending {
    extinf: Option<ExtInf>,
    group: Option<String>,
    user_agent: Option<String>,
    referrer: Option<String>,
}

fn first_attr(attrs: &[(String, String)], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(k, v)| k == key && !v.trim().is_empty())
        .map(|(_, v)| v.trim().to_owned())
}

/// A display name for an entry without a title: the URL's last path segment.
fn title_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && !s.contains(':'))
        .map_or_else(|| redact_url(url), str::to_owned)
}

impl Pending {
    fn into_entry(self, url: &str, line: usize) -> M3uEntry {
        let ExtInf {
            duration,
            attrs,
            title,
        } = self.extinf.unwrap_or_default();
        let get = |k: &str| first_attr(&attrs, k);
        let tvg_name = get("tvg-name");
        let title = if !title.is_empty() {
            title
        } else if let Some(name) = &tvg_name {
            name.clone()
        } else {
            title_from_url(url)
        };
        let parse_u32 = |s: String| parse_u64_str(&s).and_then(|v| u32::try_from(v).ok());
        M3uEntry {
            title,
            url: url.to_owned(),
            duration,
            kind: classify_url(url),
            tvg_id: get("tvg-id"),
            tvg_name,
            tvg_logo: get("tvg-logo"),
            tvg_chno: get("tvg-chno").and_then(parse_u32),
            group_title: get("group-title").or(self.group),
            catchup: get("catchup").or_else(|| get("catchup-type")),
            catchup_days: get("catchup-days").and_then(parse_u32),
            catchup_source: get("catchup-source"),
            tvg_shift: get("tvg-shift")
                .and_then(|s| s.replace(',', ".").parse::<f32>().ok())
                .filter(|v| v.is_finite()),
            user_agent: self
                .user_agent
                .or_else(|| get("http-user-agent"))
                .or_else(|| get("user-agent")),
            referrer: self
                .referrer
                .or_else(|| get("http-referrer"))
                .or_else(|| get("http-referer"))
                .or_else(|| get("referrer")),
            attributes: attrs,
            line,
        }
    }
}

/// `scheme:` with a scheme of at least two characters (so `C:\…` is not a URI).
fn looks_like_uri(line: &str) -> bool {
    let Some((scheme, _)) = line.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn parse_text(text: &str, limits: &M3uLimits) -> Result<Playlist, M3uError> {
    let mut playlist = Playlist::default();
    let mut saw_marker = false;
    let mut pending = Pending::default();

    for (idx, raw) in (Lines { rest: text }).enumerate() {
        let line_no = idx + 1;
        if raw.len() > limits.max_line_len {
            return Err(M3uError::LineTooLong {
                line: line_no,
                limit: limits.max_line_len,
            });
        }
        let line = raw.trim_start_matches('\u{FEFF}').trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            if let Some(rest) = strip_directive(line, "#EXTM3U") {
                saw_marker = true;
                let (attrs, _) = tokenize_capped(
                    rest.strip_prefix(':').unwrap_or(rest),
                    false,
                    limits.max_attributes_per_line,
                );
                for (k, v) in &attrs {
                    if matches!(k.as_str(), "url-tvg" | "x-tvg-url" | "tvg-url") {
                        for u in v.split(',').map(str::trim).filter(|u| !u.is_empty()) {
                            // Capped first: the duplicate check is linear in the list.
                            if playlist.tvg_urls.len() >= limits.max_tvg_urls {
                                break;
                            }
                            if !playlist.tvg_urls.iter().any(|x| x == u) {
                                playlist.tvg_urls.push(u.to_owned());
                            }
                        }
                    }
                }
                let room = MAX_HEADER_ATTRIBUTES.saturating_sub(playlist.header_attributes.len());
                playlist
                    .header_attributes
                    .extend(attrs.into_iter().take(room));
            } else if let Some(rest) = strip_directive(line, "#EXTINF") {
                saw_marker = true;
                if pending.extinf.is_some() {
                    playlist.skipped += 1;
                }
                pending.extinf = Some(parse_extinf(
                    rest.strip_prefix(':').unwrap_or(rest),
                    limits.max_attributes_per_line,
                ));
            } else if let Some(rest) = strip_directive(line, "#EXTGRP") {
                let group = rest.strip_prefix(':').unwrap_or(rest).trim();
                if !group.is_empty() {
                    pending.group = Some(group.to_owned());
                }
            } else if let Some(rest) = strip_directive(line, "#EXTVLCOPT") {
                let opt = rest.strip_prefix(':').unwrap_or(rest).trim();
                if let Some((k, v)) = opt.split_once('=') {
                    let v = v.trim();
                    if !v.is_empty() {
                        match k.trim().to_ascii_lowercase().as_str() {
                            "http-user-agent" => pending.user_agent = Some(v.to_owned()),
                            "http-referrer" | "http-referer" => {
                                pending.referrer = Some(v.to_owned());
                            }
                            _ => {}
                        }
                    }
                }
            }
            // #KODIPROP, #EXT-X-*, other comments: ignored.
            continue;
        }
        if pending.extinf.is_none() && !looks_like_uri(line) {
            playlist.skipped += 1;
            continue;
        }
        if playlist.entries.len() >= limits.max_entries {
            return Err(M3uError::TooManyEntries {
                limit: limits.max_entries,
            });
        }
        let entry = std::mem::take(&mut pending).into_entry(line, line_no);
        playlist.entries.push(entry);
    }
    if pending.extinf.is_some() {
        playlist.skipped += 1;
    }
    if !saw_marker && playlist.entries.is_empty() {
        return Err(M3uError::NotAPlaylist);
    }
    Ok(playlist)
}

/// Xtream credentials found in a `get.php` playlist URL. `Debug` hides the password.
#[derive(Clone)]
pub struct XtreamM3uLink {
    /// The panel URL (`get.php` and the query removed).
    pub server: ServerUrl,
    /// `username` query value.
    pub username: String,
    /// `password` query value.
    pub password: SecretString,
}

impl fmt::Debug for XtreamM3uLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XtreamM3uLink")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

/// Recognises an Xtream `get.php?username=…&password=…[&type=m3u…]` playlist URL.
/// Returns `None` for anything else (other paths, missing credentials, a `type` that is
/// not `m3u*`, non-HTTP schemes, URLs with userinfo).
pub fn detect_xtream(url: &str) -> Option<XtreamM3uLink> {
    let t = url.trim();
    let lower = t.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        t.to_owned()
    } else if t.contains("://") {
        return None;
    } else {
        format!("http://{t}")
    };
    let parsed = Url::parse(&with_scheme).ok()?;
    let last = parsed.path_segments()?.rfind(|s| !s.is_empty())?;
    if !last.eq_ignore_ascii_case("get.php") {
        return None;
    }
    let (mut username, mut password, mut kind) = (None, None, None);
    for (k, v) in parsed.query_pairs() {
        let slot = match k.to_ascii_lowercase().as_str() {
            "username" => &mut username,
            "password" => &mut password,
            "type" => &mut kind,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(v.into_owned());
        }
    }
    let username = username.filter(|u| !u.is_empty())?;
    let password = password.filter(|p| !p.is_empty())?;
    if kind.is_some_and(|k| !k.to_ascii_lowercase().starts_with("m3u")) {
        return None;
    }
    let server = ServerUrl::parse(&with_scheme).ok()?;
    Some(XtreamM3uLink {
        server,
        username,
        password: SecretString::from(password),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn p(s: &str) -> Playlist {
        parse(s.as_bytes(), &M3uLimits::default()).unwrap()
    }

    #[test]
    fn tokenizer_handles_quotes_commas_and_key_names() {
        let (attrs, title) = tokenize(
            r#" tvg-id="a,b" tvg-name="group-title=x" Group-Title='News, World' tvg-logo=http://img.example.org/l.png tvg-ID="dup",My, Title "quoted""#,
            true,
        );
        assert_eq!(
            attrs,
            vec![
                ("tvg-id".into(), "a,b".into()),
                ("tvg-name".into(), "group-title=x".into()),
                ("group-title".into(), "News, World".into()),
                ("tvg-logo".into(), "http://img.example.org/l.png".into()),
                ("tvg-id".into(), "dup".into()),
            ]
        );
        assert_eq!(title.as_deref(), Some(r#"My, Title "quoted""#));
    }

    #[test]
    fn tokenizer_edge_cases() {
        assert_eq!(tokenize("", true), (vec![], None));
        assert_eq!(tokenize(",", true), (vec![], Some(String::new())));
        let (a, t) = tokenize(r#"tvg-name="unterminated,Title"#, true);
        assert_eq!(a, vec![("tvg-name".into(), "unterminated".into())]);
        assert_eq!(t.as_deref(), Some("Title"));
        let (a, t) = tokenize(r#"key = "v" flag =,T"#, true);
        assert_eq!(
            a,
            vec![("key".into(), "v".into()), ("flag".into(), String::new())]
        );
        assert_eq!(t.as_deref(), Some("T"));
        let (a, _) = tokenize("=x ==y lone", true);
        assert_eq!(a, vec![("lone".into(), String::new())]);
        let (a, t) = tokenize(r#"tvg-id="x"tvg-name="y",T"#, true);
        assert_eq!(a.len(), 2);
        assert_eq!(t.as_deref(), Some("T"));
    }

    #[test]
    fn basic_playlist() {
        let pl = p(
            "#EXTM3U\n#EXTINF:-1 tvg-id=\"news.synthetic\" tvg-name=\"News\" tvg-logo=\"https://img.example.org/n.png\" group-title=\"News\",Synthetic News\nhttp://stream.example.net/live/u/p/1.ts\n",
        );
        assert_eq!(pl.entries.len(), 1);
        let e = &pl.entries[0];
        assert_eq!(e.title, "Synthetic News");
        assert_eq!(e.url, "http://stream.example.net/live/u/p/1.ts");
        assert_eq!(e.tvg_id.as_deref(), Some("news.synthetic"));
        assert_eq!(e.tvg_logo.as_deref(), Some("https://img.example.org/n.png"));
        assert_eq!(e.group_title.as_deref(), Some("News"));
        assert_eq!(e.kind, EntryKind::Live);
        assert_eq!(e.duration, None);
        assert_eq!(e.line, 3);
        assert_eq!(pl.skipped, 0);
    }

    #[test]
    fn never_inherits_url_and_counts_dangling_extinf() {
        let pl = p(
            "#EXTM3U\n#EXTINF:-1,First\nhttp://a.example/1.ts\n#EXTINF:-1,No URL\n#EXTINF:-1,Second\nrtmp://b.example/live/2\n#EXTINF:-1,Last without URL\n",
        );
        let titles: Vec<_> = pl
            .entries
            .iter()
            .map(|e| (e.title.as_str(), e.url.as_str()))
            .collect();
        assert_eq!(
            titles,
            vec![
                ("First", "http://a.example/1.ts"),
                ("Second", "rtmp://b.example/live/2")
            ]
        );
        assert_eq!(pl.skipped, 2);
    }

    #[test]
    fn directives_apply_to_next_entry_only() {
        let pl = p(
            "#EXTM3U\n#EXTGRP:Group A\n#EXTVLCOPT:http-user-agent=Agent/1.0\n#EXTVLCOPT:http-referrer=https://ref.example/\n#KODIPROP:inputstream=inputstream.adaptive\n#EXTINF:-1,One\nhttp://a.example/1.ts\n#EXTINF:-1,Two\nhttp://a.example/2.ts\n#EXTINF:-1 group-title=\"Attr\",Three\n#EXTGRP:Ignored\nhttp://a.example/3.ts\n",
        );
        let e = &pl.entries;
        assert_eq!(e[0].group_title.as_deref(), Some("Group A"));
        assert_eq!(e[0].user_agent.as_deref(), Some("Agent/1.0"));
        assert_eq!(e[0].referrer.as_deref(), Some("https://ref.example/"));
        assert_eq!(e[1].group_title, None);
        assert_eq!(e[1].user_agent, None);
        assert_eq!(e[2].group_title.as_deref(), Some("Attr")); // attribute wins over EXTGRP
    }

    #[test]
    fn header_tvg_urls() {
        let pl = p(
            "#EXTM3U url-tvg=\"http://epg.example.org/a.xml,http://epg.example.org/b.xml.gz\" x-tvg-url=http://epg.example.org/a.xml tvg-shift=1\n#EXTINF:0,X\nhttp://a.example/x.ts",
        );
        assert_eq!(
            pl.tvg_urls,
            vec![
                "http://epg.example.org/a.xml",
                "http://epg.example.org/b.xml.gz"
            ]
        );
        assert!(
            pl.header_attributes
                .iter()
                .any(|(k, v)| k == "tvg-shift" && v == "1")
        );
    }

    #[test]
    fn line_endings_and_bom() {
        for sep in ["\r\n", "\n", "\r"] {
            let text = [
                "\u{FEFF}#EXTM3U",
                "#EXTINF:-1,Name",
                "http://a.example/1.ts",
                "",
            ]
            .join(sep);
            let pl = parse(text.as_bytes(), &M3uLimits::default()).unwrap();
            assert_eq!(pl.entries.len(), 1, "{sep:?}");
            assert_eq!(pl.entries[0].title, "Name");
            assert_eq!(pl.entries[0].url, "http://a.example/1.ts");
        }
        // UTF-16 LE with BOM.
        let mut bytes = vec![0xFF, 0xFE];
        for u in "#EXTM3U\n#EXTINF:-1,Ünïcödé\nhttp://a.example/1.ts\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let pl = parse(&bytes, &M3uLimits::default()).unwrap();
        assert_eq!(pl.entries[0].title, "Ünïcödé");
    }

    #[test]
    fn typed_attributes() {
        let pl = p(r#"#EXTM3U
#EXTINF:-1 tvg-chno="12" catchup="default" catchup-days="7" catchup-source="?utc={utc}&lutc={lutc}" tvg-shift="-1.5" timeshift="3",Twelve
http://a.example/12.ts
#EXTINF:120.5 tvg-chno="abc" catchup-days="" tvg-shift="x",Bad values
http://a.example/13.ts
"#);
        let a = &pl.entries[0];
        assert_eq!(a.tvg_chno, Some(12));
        assert_eq!(a.catchup.as_deref(), Some("default"));
        assert_eq!(a.catchup_days, Some(7));
        assert_eq!(a.catchup_source.as_deref(), Some("?utc={utc}&lutc={lutc}"));
        assert_eq!(a.tvg_shift, Some(-1.5));
        assert_eq!(a.attribute("TimeShift"), Some("3"));
        let b = &pl.entries[1];
        assert_eq!(b.duration, Some(120.5));
        assert_eq!(b.tvg_chno, None);
        assert_eq!(b.catchup_days, None);
        assert_eq!(b.tvg_shift, None);
    }

    #[test]
    fn title_fallbacks() {
        let pl = p(
            "#EXTM3U\n#EXTINF:-1 tvg-name=\"From tvg-name\",\nhttp://a.example/1.ts\n#EXTINF:-1\nhttp://a.example/path/file.ts?token=x\nhttp://a.example/bare.m3u8\n",
        );
        assert_eq!(pl.entries[0].title, "From tvg-name");
        assert_eq!(pl.entries[1].title, "file.ts");
        assert_eq!(pl.entries[2].title, "bare.m3u8");
    }

    #[test]
    fn classification() {
        assert_eq!(classify_url("http://h/live/u/p/1.ts"), EntryKind::Live);
        assert_eq!(classify_url("http://h/u/p/1"), EntryKind::Live);
        assert_eq!(classify_url("http://h/movie/u/p/2.mkv"), EntryKind::Movie);
        assert_eq!(classify_url("http://h/MOVIE/u/p/2"), EntryKind::Movie);
        assert_eq!(classify_url("http://h/series/u/p/3.mp4"), EntryKind::Series);
        assert_eq!(
            classify_url("http://h/vod/film.MP4?x=/series/"),
            EntryKind::Movie
        );
        assert_eq!(classify_url("http://h/a.avi#t=1"), EntryKind::Movie);
        assert_eq!(classify_url("http://h/stream.m3u8"), EntryKind::Live);
        assert_eq!(classify_url("udp://@239.0.0.1:1234"), EntryKind::Live);
    }

    #[test]
    fn any_scheme_accepted_validation_is_the_players_job() {
        let pl = p(
            "#EXTM3U\n#EXTINF:-1,A\nfile:///etc/passwd\n#EXTINF:-1,B\nudp://@239.0.0.1:1234\n#EXTINF:-1,C\nrelative/path.ts\n",
        );
        assert_eq!(pl.entries.len(), 3);
        assert_eq!(pl.entries[2].url, "relative/path.ts");
    }

    #[test]
    fn rejects_non_playlists() {
        let html = "<!DOCTYPE html>\n<html><body>Error</body></html>\n";
        assert_eq!(
            parse(html.as_bytes(), &M3uLimits::default()),
            Err(M3uError::NotAPlaylist)
        );
        assert_eq!(
            parse(b"", &M3uLimits::default()),
            Err(M3uError::NotAPlaylist)
        );
        assert_eq!(
            parse(b"{\"user_info\":{}}", &M3uLimits::default()),
            Err(M3uError::NotAPlaylist)
        );
        // A bare URL list is a valid (simple) playlist.
        let pl = p("http://a.example/1.ts\njunk line\nhttps://a.example/2.m3u8\n");
        assert_eq!(pl.entries.len(), 2);
        assert_eq!(pl.skipped, 1);
        // Header alone: empty but valid.
        assert!(p("#EXTM3U\n").entries.is_empty());
    }

    #[test]
    fn attribute_and_tvg_url_caps() {
        let mut line = String::from("#EXTINF:-1");
        for i in 0..200 {
            line.push_str(&format!(" k{i}=v{i}"));
        }
        line.push_str(
            " tvg-name=\"late\",The Title
http://h/1.ts
",
        );
        let pl = p(&line);
        assert_eq!(pl.entries[0].attributes.len(), 64);
        assert_eq!(pl.entries[0].title, "The Title");

        let mut header = String::from("#EXTM3U url-tvg=\"");
        for i in 0..50 {
            header.push_str(&format!("http://e{i}.example/x.xml,"));
        }
        header.push_str(
            "\"
#EXTM3U x-tvg-url=\"http://late.example/y.xml\"
",
        );
        header.push_str(
            "#EXTINF:-1,x
http://h/1
",
        );
        let pl = p(&header);
        assert_eq!(pl.tvg_urls.len(), 16);
        assert_eq!(pl.tvg_urls[0], "http://e0.example/x.xml");
    }

    /// A hostile header with ~40 000 distinct XMLTV URLs per line used to take seconds
    /// per line (quadratic duplicate check). Now the list is capped first.
    #[test]
    fn many_tvg_urls_parse_quickly() {
        let mut s = String::new();
        let mut n = 0u64;
        for _ in 0..8 {
            s.push_str("#EXTM3U url-tvg=\"");
            let start = s.len();
            while s.len() - start < 250 * 1024 {
                s.push_str(&format!("u{n},"));
                n += 1;
            }
            s.push_str(
                "\"
",
            );
        }
        s.push_str(
            "#EXTINF:-1,x
http://h/1
",
        );
        let t = std::time::Instant::now();
        let pl = p(&s);
        assert_eq!(pl.tvg_urls.len(), 16);
        assert!(pl.header_attributes.len() <= MAX_HEADER_ATTRIBUTES);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn limits() {
        let limits = M3uLimits {
            max_total_bytes: 10,
            ..M3uLimits::default()
        };
        assert_eq!(
            parse(b"#EXTM3U\n#EXTINF:-1,x\n", &limits),
            Err(M3uError::TooLarge { limit: 10 })
        );
        let limits = M3uLimits {
            max_line_len: 20,
            ..M3uLimits::default()
        };
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXTINF:-1,this line is way too long\nhttp://a/1",
                &limits
            ),
            Err(M3uError::LineTooLong { line: 2, limit: 20 })
        );
        let limits = M3uLimits {
            max_entries: 1,
            ..M3uLimits::default()
        };
        assert_eq!(
            parse(b"#EXTM3U\nhttp://a/1\nhttp://a/2\n", &limits),
            Err(M3uError::TooManyEntries { limit: 1 })
        );
    }

    #[test]
    fn categories_in_order() {
        let pl = p(
            "#EXTM3U\n#EXTINF:-1 group-title=\"B\",1\nhttp://a/1\n#EXTINF:-1 group-title=\"A\",2\nhttp://a/2\n#EXTINF:-1 group-title=\"B\",3\nhttp://a/3\n#EXTINF:-1,4\nhttp://a/4\n",
        );
        let names: Vec<_> = pl.categories().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["B", "A"]);
        assert_eq!(pl.entries[0].category_id(), Some(CategoryId::from("B")));
    }

    #[test]
    fn debug_redacts_urls() {
        let pl = p(
            "#EXTM3U url-tvg=\"http://h/xmltv.php?username=u&password=secretpw\"\n#EXTINF:-1,A\nhttp://h/live/user/secretpw/1.ts\n",
        );
        let dbg = format!("{pl:?}");
        assert!(!dbg.contains("secretpw"), "{dbg}");
    }

    #[test]
    fn detects_xtream_links() {
        let link = detect_xtream("http://panel.example.com:8080/get.php?username=demo&password=p%40ss&type=m3u_plus&output=ts").unwrap();
        assert_eq!(link.server.to_string(), "http://panel.example.com:8080");
        assert_eq!(link.username, "demo");
        assert_eq!(link.password.expose_secret(), "p@ss");
        assert!(!format!("{link:?}").contains("p@ss"));

        let link = detect_xtream(" panel.example.com/sub/GET.PHP?password=x&username=y ").unwrap();
        assert_eq!(link.server.to_string(), "http://panel.example.com/sub");
        assert_eq!(link.username, "y");
        assert!(
            detect_xtream("https://panel.example.com/get.php?username=a&password=b&type=m3u")
                .is_some()
        );

        for no in [
            "http://panel.example.com/get.php?username=a",
            "http://panel.example.com/get.php?username=&password=b",
            "http://panel.example.com/get.php?username=a&password=b&type=enigma22_script",
            "http://panel.example.com/playlist.m3u?username=a&password=b",
            "ftp://panel.example.com/get.php?username=a&password=b",
            "http://u:p@panel.example.com/get.php?username=a&password=b",
            "not a url at all",
            "",
        ] {
            assert!(detect_xtream(no).is_none(), "{no}");
        }
    }

    #[test]
    fn fixture_playlist() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/m3u/quirks.m3u"
        ))
        .unwrap();
        let pl = parse(&bytes, &M3uLimits::default()).unwrap();
        let summary: Vec<(String, EntryKind)> = pl
            .entries
            .iter()
            .map(|e| (e.title.clone(), e.kind))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("Synthetic News HD".to_owned(), EntryKind::Live),
                ("Logo http, stream https".to_owned(), EntryKind::Live),
                ("RTMP channel".to_owned(), EntryKind::Live),
                ("UDP multicast".to_owned(), EntryKind::Live),
                ("Synthetic Movie (2020)".to_owned(), EntryKind::Movie),
                ("Synthetic Series S01E01".to_owned(), EntryKind::Series),
                ("Local file movie".to_owned(), EntryKind::Movie),
                ("Weird \"title\", with commas".to_owned(), EntryKind::Live),
            ]
        );
        let logo = &pl.entries[1];
        assert_eq!(
            logo.tvg_logo.as_deref(),
            Some("http://img.example.org/logo.png")
        );
        assert_eq!(
            logo.url,
            "https://secure.example.net/live/demo_user/demo_pass/2.m3u8"
        );
        assert_eq!(pl.entries[2].url, "rtmp://rtmp.example.net/live/3");
        assert_eq!(pl.entries[3].url, "udp://@239.1.1.1:5000");
        assert!(pl.entries.iter().all(|e| !e.title.ends_with('\r')));
        assert_eq!(pl.skipped, 1);
        assert_eq!(pl.tvg_urls, vec!["http://epg.example.org/guide.xml.gz"]);
    }
}

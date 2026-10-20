// SPDX-License-Identifier: GPL-3.0-or-later
//! Streaming XMLTV parser.
//!
//! - **Streaming** (`quick-xml` pull parser): memory is proportional to the *output*,
//!   not to the document; no recursion, so nesting depth cannot overflow the stack.
//! - **gzip** input is detected by its magic bytes and decompressed on the fly
//!   (multi-member aware). The **decompressed** size is capped
//!   ([`XmltvLimits::max_decompressed_bytes`]), which defeats gzip bombs.
//! - **Entities:** only the five XML entities and numeric character references are
//!   resolved. DTDs are never processed, so there is no external-entity access and no
//!   entity expansion ("billion laughs"). Unknown entities are kept as literal text.
//! - **Limits:** channel and programme counts (errors), longest text node
//!   ([`XmltvLimits::max_token_bytes`], error — bounds what one parser event can
//!   buffer), text length kept per field (truncated).
//! - **Leniency:** end-tag names are not checked; a programme without a parseable
//!   `start` or without `channel` is skipped (counted); a syntax error or truncated file
//!   after some data keeps what was parsed and records a warning.
//! - **Times:** `YYYYMMDDhhmmss ±hhmm` (seconds optional, offset optional = UTC, also
//!   `±hh:mm`, `Z`, `UTC`, `GMT`).
//!
//! Output ([`Epg`]) is indexed by channel id, programmes sorted by start. Look-ups by
//! channel id fall back to a case-insensitive match (`tvg-id` / `epg_channel_id` casing
//! often differs from the XMLTV file).

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Read};

use chrono::{DateTime, NaiveDate, Utc};
use flate2::read::MultiGzDecoder;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;

use crate::de::truncate_at_char_boundary;

/// Limits for [`parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmltvLimits {
    /// Maximum bytes read after decompression. Default 1 GiB.
    pub max_decompressed_bytes: u64,
    /// Maximum number of channels. Default 100 000.
    pub max_channels: usize,
    /// Maximum number of programmes. Default 5 000 000.
    pub max_programmes: usize,
    /// Maximum length of one text field, in bytes (longer text is truncated).
    /// Default 8 KiB.
    pub max_text_len: usize,
    /// Maximum values kept per repeated field (titles, display names, …). Default 16.
    pub max_values_per_field: usize,
    /// Maximum bytes between two `<` (i.e. the longest text node). The parser buffers a
    /// whole text event, so this bounds the memory one event can take. Default 16 MiB.
    pub max_token_bytes: u64,
}

impl Default for XmltvLimits {
    fn default() -> Self {
        XmltvLimits {
            max_decompressed_bytes: 1024 * 1024 * 1024,
            max_channels: 100_000,
            max_programmes: 5_000_000,
            max_text_len: 8 * 1024,
            max_values_per_field: 16,
            max_token_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Why an XMLTV document could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum XmltvError {
    /// The decompressed input exceeds [`XmltvLimits::max_decompressed_bytes`].
    #[error("the EPG is larger than {limit} bytes once decompressed")]
    TooLarge {
        /// The limit.
        limit: u64,
    },
    /// A text node is longer than [`XmltvLimits::max_token_bytes`].
    #[error("the EPG contains a text node longer than {limit} bytes")]
    TokenTooLong {
        /// The limit.
        limit: u64,
    },
    /// More channels than [`XmltvLimits::max_channels`].
    #[error("the EPG has more than {limit} channels")]
    TooManyChannels {
        /// The limit.
        limit: usize,
    },
    /// More programmes than [`XmltvLimits::max_programmes`].
    #[error("the EPG has more than {limit} programmes")]
    TooManyProgrammes {
        /// The limit.
        limit: usize,
    },
    /// The input could not be read or decompressed, before any data was parsed.
    #[error("could not read the EPG: {message}")]
    Io {
        /// Description.
        message: String,
    },
    /// The document is not XML, before any data was parsed.
    #[error("invalid XML at byte {position}: {message}")]
    Xml {
        /// Byte offset (decompressed).
        position: u64,
        /// Description.
        message: String,
    },
    /// Well-formed input without a `<tv>` root, channels or programmes.
    #[error("this is not an XMLTV document")]
    NotXmltv,
}

/// A text with an optional `lang` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangText {
    /// `lang` attribute.
    pub lang: Option<String>,
    /// Text (entities resolved, trimmed).
    pub text: String,
}

/// A `<channel>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmltvChannel {
    /// `id` attribute.
    pub id: String,
    /// `<display-name>`s.
    pub display_names: Vec<LangText>,
    /// `<icon src>`.
    pub icon: Option<String>,
    /// `<url>`s.
    pub urls: Vec<String>,
}

impl XmltvChannel {
    /// First display name, else the id.
    pub fn name(&self) -> &str {
        self.display_names
            .first()
            .map_or(self.id.as_str(), |t| t.text.as_str())
    }
}

/// A `<programme>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmltvProgramme {
    /// `channel` attribute.
    pub channel: String,
    /// `start`.
    pub start: DateTime<Utc>,
    /// `stop`, when present and valid.
    pub stop: Option<DateTime<Utc>>,
    /// `<title>`s.
    pub titles: Vec<LangText>,
    /// `<sub-title>`s.
    pub sub_titles: Vec<LangText>,
    /// `<desc>`s.
    pub descriptions: Vec<LangText>,
    /// `<category>`s.
    pub categories: Vec<LangText>,
    /// `<icon src>`.
    pub icon: Option<String>,
    /// `<episode-num system=…>` values: `(system, value)`.
    pub episode_nums: Vec<(Option<String>, String)>,
    /// `catchup-id` attribute (some providers).
    pub catchup_id: Option<String>,
}

fn pick<'a>(values: &'a [LangText], lang: Option<&str>) -> Option<&'a str> {
    lang.and_then(|l| {
        values
            .iter()
            .find(|t| t.lang.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(l)))
    })
    .or_else(|| values.first())
    .map(|t| t.text.as_str())
}

impl XmltvProgramme {
    /// Title in `lang` if available, else the first title, else `""`.
    pub fn title(&self, lang: Option<&str>) -> &str {
        pick(&self.titles, lang).unwrap_or_default()
    }

    /// Description in `lang` if available, else the first one.
    pub fn description(&self, lang: Option<&str>) -> Option<&str> {
        pick(&self.descriptions, lang)
    }

    /// `true` when `start <= now < stop` (a missing stop counts as not live).
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        self.start <= now && self.stop.is_some_and(|s| now < s)
    }
}

/// A parsed XMLTV guide.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Epg {
    /// Channels by id.
    pub channels: BTreeMap<String, XmltvChannel>,
    /// Programmes by channel id, sorted by start.
    pub programmes: BTreeMap<String, Vec<XmltvProgramme>>,
    /// Programmes skipped (no channel, no/invalid start).
    pub skipped_programmes: usize,
    /// Non-fatal problems (e.g. truncated file); the data before them is kept.
    pub warnings: Vec<String>,
    lower_index: HashMap<String, String>,
}

impl Epg {
    fn resolve_id<'a>(&'a self, id: &'a str, map_has: impl Fn(&str) -> bool) -> Option<&'a str> {
        if map_has(id) {
            return Some(id);
        }
        self.lower_index.get(&id.to_lowercase()).map(String::as_str)
    }

    /// Programmes for a channel id (exact, else case-insensitive), sorted by start.
    pub fn programmes_for(&self, channel_id: &str) -> &[XmltvProgramme] {
        self.resolve_id(channel_id, |k| self.programmes.contains_key(k))
            .and_then(|k| self.programmes.get(k))
            .map_or(&[], Vec::as_slice)
    }

    /// A channel by id (exact, else case-insensitive).
    pub fn channel(&self, channel_id: &str) -> Option<&XmltvChannel> {
        self.resolve_id(channel_id, |k| self.channels.contains_key(k))
            .and_then(|k| self.channels.get(k))
    }

    /// The programme on air at `now` and the one after it.
    pub fn now_and_next(
        &self,
        channel_id: &str,
        now: DateTime<Utc>,
    ) -> (Option<&XmltvProgramme>, Option<&XmltvProgramme>) {
        let progs = self.programmes_for(channel_id);
        let idx = progs.partition_point(|p| p.start <= now);
        let current = idx
            .checked_sub(1)
            .and_then(|i| progs.get(i))
            .filter(|p| p.stop.is_none_or(|s| now < s));
        (current, progs.get(idx))
    }

    /// Total number of programmes.
    pub fn programme_count(&self) -> usize {
        self.programmes.values().map(Vec::len).sum()
    }
}

/// Parses an XMLTV time: `YYYYMMDDhhmm[ss] [±hhmm|±hh:mm|±hh|Z|UTC|GMT]`.
pub fn parse_xmltv_time(s: &str) -> Option<DateTime<Utc>> {
    let t = s.trim();
    let digits_end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let digits = &t[..digits_end];
    if digits.len() < 12 {
        return None;
    }
    let num = |a: usize, b: usize| digits.get(a..b).and_then(|x| x.parse::<u32>().ok());
    let year = i32::try_from(num(0, 4)?).ok()?;
    let date = NaiveDate::from_ymd_opt(year, num(4, 6)?, num(6, 8)?)?;
    let sec = if digits.len() >= 14 { num(12, 14)? } else { 0 };
    let naive = date.and_hms_opt(num(8, 10)?, num(10, 12)?, sec)?;

    let mut rest = t[digits_end..].trim_start();
    // Fractional seconds (".000") are ignored.
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start();
    }
    let offset_secs: i64 = if rest.is_empty()
        || rest.eq_ignore_ascii_case("z")
        || rest.eq_ignore_ascii_case("utc")
        || rest.eq_ignore_ascii_case("gmt")
    {
        0
    } else {
        let (sign, body) = match rest.as_bytes().first()? {
            b'+' => (1, &rest[1..]),
            b'-' => (-1, &rest[1..]),
            _ => return None,
        };
        let body: String = body.chars().filter(|c| *c != ':').collect();
        if !body.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (h, m) = match body.len() {
            2 => (body.parse::<i64>().ok()?, 0),
            4 => (
                body[..2].parse::<i64>().ok()?,
                body[2..].parse::<i64>().ok()?,
            ),
            _ => return None,
        };
        if h > 14 || m > 59 {
            return None;
        }
        sign * (h * 3600 + m * 60)
    };
    let utc = naive.and_utc() - chrono::Duration::seconds(offset_secs);
    Some(utc)
}

/// A reader that fails once more than `limit` bytes have been read.
/// A reader that fails once more than `remaining` bytes have been read, or when more
/// than `max_run` consecutive bytes contain no `<`.
struct LimitedReader<R> {
    inner: R,
    remaining: u64,
    max_run: u64,
    run: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitExceeded {
    Total,
    Run,
}

impl std::fmt::Display for LimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LimitExceeded::Total => "size limit exceeded",
            LimitExceeded::Run => "text node too long",
        })
    }
}

impl std::error::Error for LimitExceeded {}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            // Probe one byte: EOF exactly at the limit is fine.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::other(LimitExceeded::Total)),
            };
        }
        let max = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let n = self.inner.read(&mut buf[..max])?;
        self.remaining -= n as u64;
        for &b in &buf[..n] {
            if b == b'<' {
                self.run = 0;
            } else {
                self.run += 1;
                if self.run > self.max_run {
                    return Err(io::Error::other(LimitExceeded::Run));
                }
            }
        }
        Ok(n)
    }
}

fn limit_error(e: &quick_xml::Error) -> Option<LimitExceeded> {
    match e {
        quick_xml::Error::Io(io) => io
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<LimitExceeded>())
            .copied(),
        _ => None,
    }
}

/// Parses XMLTV bytes (plain or gzip).
///
/// # Errors
///
/// See [`XmltvError`].
pub fn parse_bytes(input: &[u8], limits: &XmltvLimits) -> Result<Epg, XmltvError> {
    parse(input, limits)
}

/// Parses XMLTV from a reader (plain or gzip, detected from the first bytes).
///
/// # Errors
///
/// See [`XmltvError`].
pub fn parse<R: Read>(mut reader: R, limits: &XmltvLimits) -> Result<Epg, XmltvError> {
    let mut head = [0u8; 2];
    let mut got = 0;
    while got < 2 {
        match reader.read(&mut head[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                return Err(XmltvError::Io {
                    message: e.to_string(),
                });
            }
        }
    }
    let chained = io::Cursor::new(head[..got].to_vec()).chain(reader);
    let source: Box<dyn Read> = if got == 2 && head == [0x1f, 0x8b] {
        Box::new(MultiGzDecoder::new(chained))
    } else {
        Box::new(chained)
    };
    let limited = LimitedReader {
        inner: source,
        remaining: limits.max_decompressed_bytes,
        max_run: limits.max_token_bytes,
        run: 0,
    };
    parse_reader(BufReader::with_capacity(64 * 1024, limited), limits)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    DisplayName,
    ChannelUrl,
    Title,
    SubTitle,
    Desc,
    Category,
    EpisodeNum,
}

struct Capture {
    field: Field,
    depth: usize,
    lang: Option<String>,
    system: Option<String>,
    text: String,
}

struct ProgrammeBuilder {
    channel: String,
    start: Option<DateTime<Utc>>,
    stop: Option<DateTime<Utc>>,
    catchup_id: Option<String>,
    titles: Vec<LangText>,
    sub_titles: Vec<LangText>,
    descriptions: Vec<LangText>,
    categories: Vec<LangText>,
    icon: Option<String>,
    episode_nums: Vec<(Option<String>, String)>,
}

enum Ctx {
    None,
    Channel(XmltvChannel),
    Programme(Box<ProgrammeBuilder>),
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    let mut attrs = e.attributes();
    attrs.with_checks(false);
    for a in attrs.flatten() {
        if a.key.local_name().as_ref() == name {
            let value = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_or_else(|_| a.value.to_string(), |v| v.into_owned());
            let value = value.trim().to_owned();
            return (!value.is_empty()).then_some(value);
        }
    }
    None
}

fn local_name(name: QName<'_>) -> String {
    name.local_name().as_ref().to_ascii_lowercase()
}

struct State<'l> {
    limits: &'l XmltvLimits,
    epg: Epg,
    ctx: Ctx,
    capture: Option<Capture>,
    depth: usize,
    saw_tv: bool,
    channel_count: usize,
    programme_count: usize,
}

impl State<'_> {
    fn push_text(&mut self, s: &str) {
        let max = self.limits.max_text_len;
        if let Some(c) = self.capture.as_mut().filter(|c| c.text.len() < max) {
            c.text.push_str(s);
            truncate_at_char_boundary(&mut c.text, max);
        }
    }

    fn start(&mut self, e: &BytesStart<'_>, empty: bool) -> Result<(), XmltvError> {
        let name = local_name(e.name());
        if !empty {
            self.depth += 1;
        }
        match (&mut self.ctx, name.as_str()) {
            (_, "tv") => self.saw_tv = true,
            (Ctx::None, "channel") => {
                let ch = XmltvChannel {
                    id: attr(e, "id").unwrap_or_default(),
                    ..XmltvChannel::default()
                };
                self.ctx = Ctx::Channel(ch);
                if empty {
                    self.finish_channel()?;
                }
            }
            (Ctx::None, "programme") => {
                let pb = ProgrammeBuilder {
                    channel: attr(e, "channel").unwrap_or_default(),
                    start: attr(e, "start").as_deref().and_then(parse_xmltv_time),
                    stop: attr(e, "stop").as_deref().and_then(parse_xmltv_time),
                    catchup_id: attr(e, "catchup-id"),
                    titles: Vec::new(),
                    sub_titles: Vec::new(),
                    descriptions: Vec::new(),
                    categories: Vec::new(),
                    icon: None,
                    episode_nums: Vec::new(),
                };
                self.ctx = Ctx::Programme(Box::new(pb));
                if empty {
                    self.finish_programme()?;
                }
            }
            (Ctx::Channel(ch), "icon") => {
                if ch.icon.is_none() {
                    ch.icon = attr(e, "src");
                }
            }
            (Ctx::Programme(p), "icon") => {
                if p.icon.is_none() {
                    p.icon = attr(e, "src");
                }
            }
            (ctx, other) if self.capture.is_none() && !empty => {
                let field = match (ctx, other) {
                    (Ctx::Channel(_), "display-name") => Some(Field::DisplayName),
                    (Ctx::Channel(_), "url") => Some(Field::ChannelUrl),
                    (Ctx::Programme(_), "title") => Some(Field::Title),
                    (Ctx::Programme(_), "sub-title") => Some(Field::SubTitle),
                    (Ctx::Programme(_), "desc") => Some(Field::Desc),
                    (Ctx::Programme(_), "category") => Some(Field::Category),
                    (Ctx::Programme(_), "episode-num") => Some(Field::EpisodeNum),
                    _ => None,
                };
                if let Some(field) = field {
                    self.capture = Some(Capture {
                        field,
                        depth: self.depth,
                        lang: attr(e, "lang"),
                        system: attr(e, "system"),
                        text: String::new(),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, name: &str) -> Result<(), XmltvError> {
        if self.capture.as_ref().is_some_and(|c| c.depth == self.depth) {
            self.finish_capture();
        }
        match (&self.ctx, name) {
            (Ctx::Channel(_), "channel") => self.finish_channel()?,
            (Ctx::Programme(_), "programme") => self.finish_programme()?,
            _ => {}
        }
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }

    fn finish_capture(&mut self) {
        let Some(c) = self.capture.take() else { return };
        let text = c.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let max = self.limits.max_values_per_field;
        let lt = LangText { lang: c.lang, text };
        let push = |v: &mut Vec<LangText>, lt: LangText| {
            if v.len() < max {
                v.push(lt);
            }
        };
        match (&mut self.ctx, c.field) {
            (Ctx::Channel(ch), Field::DisplayName) => push(&mut ch.display_names, lt),
            (Ctx::Channel(ch), Field::ChannelUrl) if ch.urls.len() < max => ch.urls.push(lt.text),
            (Ctx::Programme(p), Field::Title) => push(&mut p.titles, lt),
            (Ctx::Programme(p), Field::SubTitle) => push(&mut p.sub_titles, lt),
            (Ctx::Programme(p), Field::Desc) => push(&mut p.descriptions, lt),
            (Ctx::Programme(p), Field::Category) => push(&mut p.categories, lt),
            (Ctx::Programme(p), Field::EpisodeNum) if p.episode_nums.len() < max => {
                p.episode_nums.push((c.system, lt.text));
            }
            _ => {}
        }
    }

    fn finish_channel(&mut self) -> Result<(), XmltvError> {
        self.capture = None;
        if let Ctx::Channel(ch) = std::mem::replace(&mut self.ctx, Ctx::None) {
            if ch.id.is_empty() {
                return Ok(());
            }
            if !self.epg.channels.contains_key(&ch.id) {
                self.channel_count += 1;
                if self.channel_count > self.limits.max_channels {
                    return Err(XmltvError::TooManyChannels {
                        limit: self.limits.max_channels,
                    });
                }
            }
            self.epg.channels.insert(ch.id.clone(), ch);
        }
        Ok(())
    }

    fn finish_programme(&mut self) -> Result<(), XmltvError> {
        self.capture = None;
        if let Ctx::Programme(p) = std::mem::replace(&mut self.ctx, Ctx::None) {
            let p = *p;
            let (Some(start), false) = (p.start, p.channel.is_empty()) else {
                self.epg.skipped_programmes += 1;
                return Ok(());
            };
            self.programme_count += 1;
            if self.programme_count > self.limits.max_programmes {
                return Err(XmltvError::TooManyProgrammes {
                    limit: self.limits.max_programmes,
                });
            }
            let stop = p.stop.filter(|s| *s >= start);
            let prog = XmltvProgramme {
                channel: p.channel.clone(),
                start,
                stop,
                titles: p.titles,
                sub_titles: p.sub_titles,
                descriptions: p.descriptions,
                categories: p.categories,
                icon: p.icon,
                episode_nums: p.episode_nums,
                catchup_id: p.catchup_id,
            };
            self.epg.programmes.entry(p.channel).or_default().push(prog);
        }
        Ok(())
    }

    fn has_data(&self) -> bool {
        !self.epg.channels.is_empty() || self.programme_count > 0
    }
}

fn parse_reader<R: BufRead>(input: R, limits: &XmltvLimits) -> Result<Epg, XmltvError> {
    let mut reader = quick_xml::Reader::from_reader(input);
    {
        let config = reader.config_mut();
        config.check_end_names = false;
        config.allow_unmatched_ends = true;
        config.allow_dangling_amp = true;
        config.check_comments = false;
    }
    let mut state = State {
        limits,
        epg: Epg::default(),
        ctx: Ctx::None,
        capture: None,
        depth: 0,
        saw_tv: false,
        channel_count: 0,
        programme_count: 0,
    };
    let mut buf = Vec::new();
    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => {
                match limit_error(&e) {
                    Some(LimitExceeded::Total) => {
                        return Err(XmltvError::TooLarge {
                            limit: limits.max_decompressed_bytes,
                        });
                    }
                    Some(LimitExceeded::Run) => {
                        return Err(XmltvError::TokenTooLong {
                            limit: limits.max_token_bytes,
                        });
                    }
                    None => {}
                }
                let message = e.to_string();
                if state.has_data() {
                    state.epg.warnings.push(format!(
                        "stopped at byte {}: {message}",
                        reader.error_position()
                    ));
                    break;
                }
                return Err(match e {
                    quick_xml::Error::Io(_) => XmltvError::Io { message },
                    _ => XmltvError::Xml {
                        position: reader.error_position(),
                        message,
                    },
                });
            }
        };
        match event {
            Event::Start(e) => state.start(&e, false)?,
            Event::Empty(e) => state.start(&e, true)?,
            Event::End(e) => {
                let name = local_name(e.name());
                state.end(&name)?;
            }
            Event::Text(t) => {
                let text = t.xml10_content();
                state.push_text(&text);
            }
            Event::CData(t) => {
                let text = t.xml10_content();
                state.push_text(&text);
            }
            Event::GeneralRef(r) => {
                if state.capture.is_some() {
                    let resolved = match r.resolve_char_ref() {
                        Ok(Some(c)) => Some(c.to_string()),
                        Ok(None) => quick_xml::escape::resolve_xml_entity(&r).map(str::to_owned),
                        Err(_) => None,
                    };
                    match resolved {
                        Some(s) => state.push_text(&s),
                        None => state.push_text(&format!("&{};", &*r)),
                    }
                }
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
        buf.clear();
    }
    // A document cut inside an element still yields the element if it is complete
    // enough.
    match state.ctx {
        Ctx::Programme(_) => {
            state.finish_capture();
            state.finish_programme()?;
        }
        Ctx::Channel(_) => {
            state.finish_capture();
            state.finish_channel()?;
        }
        Ctx::None => {}
    }
    if !state.saw_tv && !state.has_data() {
        return Err(XmltvError::NotXmltv);
    }
    let mut epg = state.epg;
    let mut lower_index = HashMap::new();
    for key in epg.channels.keys().chain(epg.programmes.keys()) {
        lower_index
            .entry(key.to_lowercase())
            .or_insert_with(|| key.clone());
    }
    epg.lower_index = lower_index;
    for progs in epg.programmes.values_mut() {
        progs.sort_by_key(|p| p.start);
    }
    Ok(epg)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn fixture() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/xmltv/basic.xml"
        ))
        .unwrap()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn times() {
        assert_eq!(
            parse_xmltv_time("20240101120000 +0100"),
            Some(utc(2024, 1, 1, 11, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000+0100"),
            Some(utc(2024, 1, 1, 11, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000 -0530"),
            Some(utc(2024, 1, 1, 17, 30, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000 +01:00"),
            Some(utc(2024, 1, 1, 11, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000 +02"),
            Some(utc(2024, 1, 1, 10, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000"),
            Some(utc(2024, 1, 1, 12, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("202401011200"),
            Some(utc(2024, 1, 1, 12, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000 Z"),
            Some(utc(2024, 1, 1, 12, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000 GMT"),
            Some(utc(2024, 1, 1, 12, 0, 0))
        );
        assert_eq!(
            parse_xmltv_time("20240101120000.000 +0000"),
            Some(utc(2024, 1, 1, 12, 0, 0))
        );
        assert_eq!(parse_xmltv_time("20240101120000 CET"), None);
        assert_eq!(parse_xmltv_time("20241301120000"), None);
        assert_eq!(parse_xmltv_time("2024010112"), None);
        assert_eq!(parse_xmltv_time("20240101120000 +2500"), None);
        assert_eq!(parse_xmltv_time(""), None);
    }

    #[test]
    fn parses_channels_and_programmes() {
        let epg = parse_bytes(&fixture(), &XmltvLimits::default()).unwrap();
        assert_eq!(epg.channels.len(), 2);
        let news = &epg.channels["news.synthetic"];
        assert_eq!(news.display_names.len(), 2);
        assert_eq!(news.name(), "Synthetic News");
        assert_eq!(news.display_names[1].lang.as_deref(), Some("fr"));
        assert_eq!(
            news.icon.as_deref(),
            Some("https://img.example.org/news.png")
        );
        let progs = epg.programmes_for("news.synthetic");
        assert_eq!(progs.len(), 3);
        // Sorted by start even though the file is not.
        assert_eq!(progs[0].start, utc(2024, 1, 1, 6, 0, 0));
        assert_eq!(progs[0].title(None), "Morning News");
        assert_eq!(progs[0].title(Some("fr")), "Journal du matin");
        assert_eq!(
            progs[0].description(None),
            Some("Headlines & weather <live> ünïcode ✓ ©")
        );
        assert_eq!(progs[0].categories[0].text, "News");
        assert_eq!(
            progs[0].episode_nums,
            vec![(Some("xmltv_ns".into()), "0.4.0/1".into())]
        );
        assert_eq!(progs[1].title(None), "CDATA <Title>");
        assert_eq!(progs[1].stop, None); // stop before start → dropped
        assert_eq!(progs[2].sub_titles[0].text, "Unknown &custom; entity");
        assert_eq!(epg.skipped_programmes, 2);
        // Case-insensitive channel look-up.
        assert_eq!(epg.programmes_for("NEWS.Synthetic").len(), 3);
        assert!(epg.channel("Sports.Synthetic").is_some());
        assert!(epg.programmes_for("nope").is_empty());
        assert!(epg.warnings.is_empty());
        assert_eq!(epg.programme_count(), 4);
    }

    #[test]
    fn now_and_next() {
        let epg = parse_bytes(&fixture(), &XmltvLimits::default()).unwrap();
        let (now, next) = epg.now_and_next("news.synthetic", utc(2024, 1, 1, 6, 30, 0));
        assert_eq!(now.map(|p| p.title(None)), Some("Morning News"));
        assert_eq!(next.map(|p| p.title(None)), Some("CDATA <Title>"));
        let (now, next) = epg.now_and_next("news.synthetic", utc(2023, 1, 1, 0, 0, 0));
        assert!(now.is_none());
        assert!(next.is_some());
        assert!(epg.programmes_for("news.synthetic")[0].is_live_at(utc(2024, 1, 1, 6, 59, 59)));
    }

    #[test]
    fn gzip_input() {
        let epg = parse_bytes(&gzip(&fixture()), &XmltvLimits::default()).unwrap();
        assert_eq!(epg.programme_count(), 4);
        // Multi-member gzip.
        let mut two = gzip(b"<tv><channel id=\"a\"/>");
        two.extend(gzip(b"<channel id=\"b\"/></tv>"));
        let epg = parse_bytes(&two, &XmltvLimits::default()).unwrap();
        assert_eq!(epg.channels.len(), 2);
    }

    #[test]
    fn gzip_bomb_is_capped() {
        // 64 MiB of zeros compresses to ~64 KiB.
        let mut data = b"<tv><!--".to_vec();
        data.resize(64 * 1024 * 1024, b' ');
        let bomb = gzip(&data);
        assert!(bomb.len() < 1024 * 1024);
        let limits = XmltvLimits {
            max_decompressed_bytes: 1024 * 1024,
            ..XmltvLimits::default()
        };
        assert_eq!(
            parse_bytes(&bomb, &limits),
            Err(XmltvError::TooLarge { limit: 1024 * 1024 })
        );
        // Plain input is capped too.
        assert_eq!(
            parse_bytes(&data, &limits),
            Err(XmltvError::TooLarge { limit: 1024 * 1024 })
        );
        // Exactly at the limit is fine.
        let exact = b"<tv></tv>";
        let limits = XmltvLimits {
            max_decompressed_bytes: exact.len() as u64,
            ..XmltvLimits::default()
        };
        assert!(parse_bytes(exact, &limits).is_ok());
    }

    #[test]
    fn huge_text_node_is_rejected_before_buffering() {
        let mut doc = b"<tv><programme channel=\"x\" start=\"20240101000000\"><title>".to_vec();
        doc.resize(doc.len() + 2 * 1024 * 1024, b'a');
        doc.extend_from_slice(b"</title></programme></tv>");
        let limits = XmltvLimits {
            max_token_bytes: 1024 * 1024,
            ..XmltvLimits::default()
        };
        assert_eq!(
            parse_bytes(&doc, &limits),
            Err(XmltvError::TokenTooLong { limit: 1024 * 1024 })
        );
        // The default limit accepts it (and truncates the stored text).
        let epg = parse_bytes(&doc, &XmltvLimits::default()).unwrap();
        assert_eq!(epg.programmes_for("x")[0].title(None).len(), 8 * 1024);
    }

    #[test]
    fn no_entity_expansion() {
        let doc = br#"<?xml version="1.0"?>
<!DOCTYPE tv [
 <!ENTITY a "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa">
 <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
 <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
 <!ENTITY ext SYSTEM "file:///etc/passwd">
]>
<tv><programme channel="x" start="20240101000000"><title>&c; &ext;</title></programme></tv>"#;
        let epg = parse_bytes(doc, &XmltvLimits::default()).unwrap();
        assert_eq!(epg.programmes_for("x")[0].title(None), "&c; &ext;");
    }

    #[test]
    fn count_and_text_limits() {
        let mut doc = String::from("<tv>");
        for i in 0..10 {
            doc.push_str(&format!("<channel id=\"c{i}\"/><programme channel=\"c{i}\" start=\"20240101000000\"><title>{}</title></programme>", "x".repeat(100)));
        }
        doc.push_str("</tv>");
        let limits = XmltvLimits {
            max_channels: 5,
            ..XmltvLimits::default()
        };
        assert_eq!(
            parse_bytes(doc.as_bytes(), &limits),
            Err(XmltvError::TooManyChannels { limit: 5 })
        );
        let limits = XmltvLimits {
            max_programmes: 3,
            ..XmltvLimits::default()
        };
        assert_eq!(
            parse_bytes(doc.as_bytes(), &limits),
            Err(XmltvError::TooManyProgrammes { limit: 3 })
        );
        let limits = XmltvLimits {
            max_text_len: 10,
            ..XmltvLimits::default()
        };
        let epg = parse_bytes(doc.as_bytes(), &limits).unwrap();
        assert_eq!(epg.programmes_for("c0")[0].title(None), "x".repeat(10));
    }

    #[test]
    fn truncated_and_invalid_input() {
        let full = fixture();
        let cut = &full[..full.len() * 2 / 3];
        let epg = parse_bytes(cut, &XmltvLimits::default()).unwrap();
        assert!(epg.channels.len() == 2);
        // Truncated gzip keeps what was decoded.
        let gz = gzip(&full);
        let epg = parse_bytes(&gz[..gz.len() - 20], &XmltvLimits::default());
        assert!(epg.is_ok(), "{epg:?}");

        assert_eq!(
            parse_bytes(b"", &XmltvLimits::default()),
            Err(XmltvError::NotXmltv)
        );
        assert_eq!(
            parse_bytes(b"<html><body>404</body></html>", &XmltvLimits::default()),
            Err(XmltvError::NotXmltv)
        );
        assert!(matches!(
            parse_bytes(b"<tv><<<", &XmltvLimits::default()),
            Err(XmltvError::Xml { .. })
        ));
        assert!(matches!(
            parse_bytes(&[0x1f, 0x8b, 0, 0], &XmltvLimits::default()),
            Err(XmltvError::Io { .. })
        ));
        assert!(
            parse_bytes(b"<tv/>", &XmltvLimits::default())
                .unwrap()
                .channels
                .is_empty()
        );
    }
}

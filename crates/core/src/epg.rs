// SPDX-License-Identifier: GPL-3.0-or-later
//! EPG helpers: lenient base64 text decoding and server time-zone handling.
//!
//! ## Text
//!
//! Xtream EPG `title` / `description` are base64 of UTF-8. [`decode_text`] accepts the
//! standard and URL-safe alphabets, with or without padding, with embedded whitespace
//! (line-wrapped base64), and falls back to the **raw text** when the input is not
//! base64, does not decode to valid UTF-8, or decodes to text containing control
//! characters (so plain-text titles from odd panels survive). Output is capped at
//! [`MAX_EPG_TEXT_LEN`] bytes.
//!
//! ## Time
//!
//! Prefer the epoch `start_timestamp` / `stop_timestamp` fields. Where only server-local
//! strings exist (`"2024-01-01 07:00:00"`), convert them with [`ServerTimeZone`], built
//! from `server_info` ([`ServerTimeZone::from_server_info`]).

use base64::Engine;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::de::truncate_at_char_boundary;
use crate::model::ServerInfo;

/// Maximum length of a decoded EPG title or description, in bytes.
pub const MAX_EPG_TEXT_LEN: usize = 16 * 1024;

const LENIENT: GeneralPurposeConfig = GeneralPurposeConfig::new()
    .with_decode_padding_mode(DecodePaddingMode::Indifferent)
    .with_decode_allow_trailing_bits(true);
const STANDARD_LENIENT: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, LENIENT);
const URL_SAFE_LENIENT: GeneralPurpose = GeneralPurpose::new(&alphabet::URL_SAFE, LENIENT);

/// Decodes an EPG text field: base64 → UTF-8, else the raw text (trimmed). Never fails.
pub fn decode_text(s: &str) -> String {
    let mut out = decode_base64_utf8(s).unwrap_or_else(|| s.trim().to_owned());
    truncate_at_char_boundary(&mut out, MAX_EPG_TEXT_LEN);
    out
}

/// Strict-ish base64 → UTF-8 text. `None` when `s` is empty, is not base64 (standard or
/// URL-safe, padding optional, whitespace ignored), is not valid UTF-8 once decoded, or
/// decodes to control characters other than `\n`, `\r`, `\t`.
pub fn decode_base64_utf8(s: &str) -> Option<String> {
    let compact: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    if !compact
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_' | b'='))
    {
        return None;
    }
    let bytes = if compact.contains(['-', '_']) {
        URL_SAFE_LENIENT.decode(compact.as_bytes()).ok()?
    } else {
        STANDARD_LENIENT.decode(compact.as_bytes()).ok()?
    };
    let text = String::from_utf8(bytes).ok()?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return None;
    }
    Some(text.trim().to_owned())
}

/// Parses a server-local date-time string: `YYYY-MM-DD HH:MM:SS`, `YYYY-MM-DD HH:MM`,
/// or the same with a `T` separator.
pub fn parse_server_datetime(s: &str) -> Option<NaiveDateTime> {
    let t = s.trim();
    if t.len() > 32 {
        return None;
    }
    const FORMATS: [&str; 4] = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ];
    FORMATS
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(t, f).ok())
}

/// The time zone in which a panel expresses its local date-time strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerTimeZone {
    /// A named IANA zone (`server_info.timezone`), DST-aware.
    Iana(Tz),
    /// A fixed offset, derived from `time_now` − `timestamp_now`.
    Fixed(FixedOffset),
    /// Unknown: local strings are taken as UTC.
    #[default]
    Utc,
}

impl ServerTimeZone {
    /// Parses an IANA zone name (`Europe/Paris`). `None` when unknown or empty.
    pub fn from_name(name: &str) -> Option<Self> {
        let t = name.trim();
        if t.is_empty() || t.len() > 64 {
            return None;
        }
        t.parse::<Tz>().ok().map(ServerTimeZone::Iana)
    }

    /// Best available zone for a panel: the IANA `timezone` when recognised, else a fixed
    /// offset derived from `time_now` (server wall clock) and `timestamp_now` (epoch),
    /// rounded to 15 minutes, else UTC.
    pub fn from_server_info(info: &ServerInfo) -> Self {
        if let Some(tz) = info.timezone.as_deref().and_then(Self::from_name) {
            return tz;
        }
        if let (Some(local), Some(now)) = (
            info.time_now.as_deref().and_then(parse_server_datetime),
            info.timestamp_now,
        ) {
            let diff = local.and_utc().timestamp() - now.timestamp();
            let rounded = ((diff as f64) / 900.0).round() as i64 * 900;
            let offset = Some(rounded)
                .filter(|r| r.abs() <= 14 * 3600)
                .and_then(|r| i32::try_from(r).ok())
                .and_then(FixedOffset::east_opt);
            if let Some(offset) = offset {
                return ServerTimeZone::Fixed(offset);
            }
        }
        ServerTimeZone::Utc
    }

    /// Converts a server-local wall-clock time to UTC. Ambiguous times (DST fall-back)
    /// take the earlier instant; non-existent times (DST spring-forward gap) are shifted
    /// forward by an hour, which is what the server's own clock would read.
    pub fn to_utc(&self, local: NaiveDateTime) -> DateTime<Utc> {
        match self {
            ServerTimeZone::Utc => local.and_utc(),
            ServerTimeZone::Fixed(offset) => resolve(offset, local),
            ServerTimeZone::Iana(tz) => resolve(tz, local),
        }
    }

    /// Converts a UTC instant to the server's wall-clock time.
    pub fn to_local(&self, utc: DateTime<Utc>) -> NaiveDateTime {
        match self {
            ServerTimeZone::Utc => utc.naive_utc(),
            ServerTimeZone::Fixed(offset) => utc.with_timezone(offset).naive_local(),
            ServerTimeZone::Iana(tz) => utc.with_timezone(tz).naive_local(),
        }
    }
}

fn resolve<Z: TimeZone>(tz: &Z, local: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        LocalResult::None => {
            let shifted = local + chrono::Duration::hours(1);
            match tz.from_local_datetime(&shifted) {
                LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
                LocalResult::None => local.and_utc(),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn naive(s: &str) -> NaiveDateTime {
        parse_server_datetime(s).unwrap()
    }

    #[test]
    fn base64_variants() {
        // "Morning News"
        assert_eq!(decode_text("TW9ybmluZyBOZXdz"), "Morning News");
        // Unicode, standard alphabet with padding: "Météo ✓"
        let std = base64::engine::general_purpose::STANDARD.encode("Météo ✓");
        assert_eq!(decode_text(&std), "Météo ✓");
        // Without padding.
        assert_eq!(decode_text(std.trim_end_matches('=')), "Météo ✓");
        // URL-safe alphabet.
        let bytes = [0xEF_u8, 0xBF, 0xBE, 0x3F, 0x3E]; // "\u{FFFE}?>" encodes with - and _
        let url = base64::engine::general_purpose::URL_SAFE.encode(bytes);
        assert!(url.contains(['-', '_']));
        assert_eq!(decode_text(&url), "\u{FFFE}?>");
        // Whitespace / line wrapping.
        assert_eq!(decode_text(" TW9ybmlu\nZyBOZXdz \r\n"), "Morning News");
    }

    #[test]
    fn falls_back_to_raw_text() {
        assert_eq!(decode_text("Plain title"), "Plain title"); // spaces → not base64
        assert_eq!(decode_text("News"), "News"); // valid base64, invalid UTF-8
        assert_eq!(decode_text("Film"), "Film"); // decodes to a control char
        assert_eq!(decode_text("Café"), "Café"); // non-ASCII
        assert_eq!(decode_text("abc=def"), "abc=def"); // padding in the middle
        assert_eq!(decode_text(""), "");
        assert_eq!(decode_text("   "), "");
        assert_eq!(decode_base64_utf8("not base64!"), None);
    }

    #[test]
    fn caps_text_length() {
        let long = "é".repeat(MAX_EPG_TEXT_LEN);
        let out = decode_text(&long);
        assert!(out.len() <= MAX_EPG_TEXT_LEN);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn server_datetime_formats() {
        let expected = NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(7, 30, 0)
            .unwrap();
        assert_eq!(parse_server_datetime("2024-01-01 07:30:00"), Some(expected));
        assert_eq!(parse_server_datetime(" 2024-01-01 07:30 "), Some(expected));
        assert_eq!(parse_server_datetime("2024-01-01T07:30:00"), Some(expected));
        assert_eq!(parse_server_datetime("2024-13-01 07:30:00"), None);
        assert_eq!(parse_server_datetime("yesterday"), None);
    }

    #[test]
    fn iana_zone_conversion_with_dst() {
        let tz = ServerTimeZone::from_name("Europe/Paris").unwrap();
        // Winter: UTC+1.
        assert_eq!(
            tz.to_utc(naive("2024-01-01 07:00:00")).timestamp(),
            1_704_088_800
        );
        // Summer: UTC+2.
        assert_eq!(
            tz.to_utc(naive("2024-07-01 12:00:00")),
            Utc.with_ymd_and_hms(2024, 7, 1, 10, 0, 0).unwrap()
        );
        // Spring-forward gap (02:30 does not exist on 2024-03-31): no panic, sane answer.
        assert_eq!(
            tz.to_utc(naive("2024-03-31 02:30:00")),
            Utc.with_ymd_and_hms(2024, 3, 31, 1, 30, 0).unwrap()
        );
        // Fall-back ambiguity (02:30 happens twice on 2024-10-27): earlier instant.
        assert_eq!(
            tz.to_utc(naive("2024-10-27 02:30:00")),
            Utc.with_ymd_and_hms(2024, 10, 27, 0, 30, 0).unwrap()
        );
        let utc = Utc.with_ymd_and_hms(2024, 1, 1, 6, 0, 0).unwrap();
        assert_eq!(tz.to_local(utc), naive("2024-01-01 07:00:00"));
        assert_eq!(ServerTimeZone::from_name("Mars/Olympus"), None);
        assert_eq!(ServerTimeZone::from_name(""), None);
    }

    #[test]
    fn zone_from_server_info() {
        let mut info = ServerInfo {
            timezone: Some("America/New_York".into()),
            ..ServerInfo::default()
        };
        assert!(matches!(
            ServerTimeZone::from_server_info(&info),
            ServerTimeZone::Iana(_)
        ));

        // Unknown name → offset from the two clocks (server is UTC+5:30, a few seconds skew).
        info.timezone = Some("Not/AZone".into());
        info.timestamp_now = DateTime::from_timestamp(1_704_067_200, 0);
        info.time_now = Some("2024-01-01 05:30:07".into());
        let tz = ServerTimeZone::from_server_info(&info);
        assert_eq!(
            tz,
            ServerTimeZone::Fixed(FixedOffset::east_opt(5 * 3600 + 1800).unwrap())
        );
        assert_eq!(
            tz.to_utc(naive("2024-01-01 05:30:00")).timestamp(),
            1_704_067_200
        );

        // Nothing usable → UTC.
        info.time_now = None;
        assert_eq!(ServerTimeZone::from_server_info(&info), ServerTimeZone::Utc);
        info.time_now = Some("garbage".into());
        assert_eq!(ServerTimeZone::from_server_info(&info), ServerTimeZone::Utc);
        // Absurd offsets are ignored.
        info.time_now = Some("2024-01-03 00:00:00".into());
        assert_eq!(ServerTimeZone::from_server_info(&info), ServerTimeZone::Utc);
    }
}

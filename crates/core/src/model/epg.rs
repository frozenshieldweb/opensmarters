// SPDX-License-Identifier: GPL-3.0-or-later
//! `get_short_epg` / `get_simple_data_table` listings and catch-up programmes.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::de::{self, Listing};
use crate::epg::{ServerTimeZone, decode_text, parse_server_datetime};
use crate::error::Error;

/// One EPG entry, decoded.
///
/// - `title` / `description` are base64-decoded (raw-text fallback, see
///   [`crate::epg::decode_text`]).
/// - `start` / `stop` come from `start_timestamp` / `stop_timestamp` (epoch seconds);
///   when those are missing, from the server-local `start` / `end` strings converted
///   with the server time zone.
/// - `local_start` / `local_end` keep the raw server-local strings verbatim, because the
///   timeshift URL needs the server's wall-clock start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpgListing {
    /// `id`.
    pub id: Option<String>,
    /// `epg_id`.
    pub epg_id: Option<String>,
    /// `channel_id` (XMLTV id).
    pub channel_id: Option<String>,
    /// `lang`.
    pub lang: Option<String>,
    /// Decoded title.
    pub title: String,
    /// Decoded description.
    pub description: String,
    /// Start instant.
    pub start: DateTime<Utc>,
    /// Stop instant.
    pub stop: DateTime<Utc>,
    /// Raw server-local `start` (`YYYY-MM-DD HH:MM:SS`), if sent.
    pub local_start: Option<String>,
    /// Raw server-local `end` / `stop`, if sent.
    pub local_end: Option<String>,
    /// `now_playing` flag (as sent; not trusted for "now" logic, use the times).
    pub now_playing: bool,
    /// `has_archive`: catch-up is available for this programme.
    pub has_archive: bool,
}

#[derive(Deserialize)]
struct RawEpgListing {
    #[serde(default, deserialize_with = "de::opt_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    epg_id: Option<String>,
    #[serde(default, deserialize_with = "de::string")]
    title: String,
    #[serde(default, deserialize_with = "de::opt_string")]
    lang: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    start: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    end: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    stop: Option<String>,
    #[serde(default, deserialize_with = "de::string")]
    description: String,
    #[serde(default, deserialize_with = "de::opt_string")]
    channel_id: Option<String>,
    #[serde(default, deserialize_with = "de::opt_i64")]
    start_timestamp: Option<i64>,
    #[serde(default, deserialize_with = "de::opt_i64")]
    stop_timestamp: Option<i64>,
    #[serde(default, deserialize_with = "de::opt_i64")]
    end_timestamp: Option<i64>,
    #[serde(default, deserialize_with = "de::bool")]
    now_playing: bool,
    #[serde(default, deserialize_with = "de::bool")]
    has_archive: bool,
}

impl EpgListing {
    fn from_raw(raw: RawEpgListing, tz: &ServerTimeZone) -> Result<Self, String> {
        let local_end = raw.end.or(raw.stop);
        let from_local = |s: Option<&str>| s.and_then(parse_server_datetime).map(|n| tz.to_utc(n));
        let start = raw
            .start_timestamp
            .and_then(de::timestamp)
            .or_else(|| from_local(raw.start.as_deref()))
            .ok_or("missing or invalid start time")?;
        let stop = raw
            .stop_timestamp
            .or(raw.end_timestamp)
            .and_then(de::timestamp)
            .or_else(|| from_local(local_end.as_deref()))
            .ok_or("missing or invalid stop time")?;
        if stop < start {
            return Err("stop time is before start time".into());
        }
        Ok(EpgListing {
            id: raw.id,
            epg_id: raw.epg_id,
            channel_id: raw.channel_id,
            lang: raw.lang,
            title: decode_text(&raw.title),
            description: decode_text(&raw.description),
            start,
            stop,
            local_start: raw.start,
            local_end,
            now_playing: raw.now_playing,
            has_archive: raw.has_archive,
        })
    }

    /// `true` when `start <= now < stop`.
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        self.start <= now && now < self.stop
    }

    /// The server wall-clock start: the raw `local_start` string when parseable, else
    /// `start` converted with `tz`.
    pub fn server_local_start(&self, tz: &ServerTimeZone) -> NaiveDateTime {
        self.local_start
            .as_deref()
            .and_then(parse_server_datetime)
            .unwrap_or_else(|| tz.to_local(self.start))
    }

    /// The catch-up view of this programme (does not check `has_archive`).
    pub fn to_archive_programme(&self, tz: &ServerTimeZone) -> ArchiveProgramme {
        ArchiveProgramme {
            title: self.title.clone(),
            description: self.description.clone(),
            start: self.start,
            stop: self.stop,
            start_local: self.server_local_start(tz),
        }
    }
}

/// A programme that can be replayed through the timeshift URL
/// ([`crate::urls::StreamUrls::timeshift`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveProgramme {
    /// Decoded title.
    pub title: String,
    /// Decoded description.
    pub description: String,
    /// Start instant (for display, converted to the user's zone by the UI).
    pub start: DateTime<Utc>,
    /// Stop instant.
    pub stop: DateTime<Utc>,
    /// Server wall-clock start: what goes into the timeshift URL.
    pub start_local: NaiveDateTime,
}

impl ArchiveProgramme {
    /// Duration in whole minutes, rounded **up**, at least 1. Computed from the instants,
    /// so it is correct across DST changes (the original computed it from local strings
    /// and could be fractional or off by an hour).
    pub fn duration_minutes(&self) -> u32 {
        let secs = (self.stop - self.start).num_seconds().max(0);
        let minutes = (secs + 59) / 60;
        u32::try_from(minutes).unwrap_or(u32::MAX).max(1)
    }

    /// The server-local calendar date of the start (catch-up lists group by it).
    pub fn server_date(&self) -> NaiveDate {
        self.start_local.date()
    }
}

/// Programmes offered for catch-up: `has_archive` set and already started at `now`, in
/// input order.
pub fn archive_programmes(
    listings: &[EpgListing],
    tz: &ServerTimeZone,
    now: DateTime<Utc>,
) -> Vec<ArchiveProgramme> {
    listings
        .iter()
        .filter(|l| l.has_archive && l.start <= now)
        .map(|l| l.to_archive_programme(tz))
        .collect()
}

/// Groups programmes by server-local date (the original labels tabs `DD MMM YYYY` and
/// selects the newest date).
pub fn group_by_server_date(
    programmes: Vec<ArchiveProgramme>,
) -> BTreeMap<NaiveDate, Vec<ArchiveProgramme>> {
    let mut out: BTreeMap<NaiveDate, Vec<ArchiveProgramme>> = BTreeMap::new();
    for p in programmes {
        out.entry(p.server_date()).or_default().push(p);
    }
    out
}

/// Parses a `get_short_epg` / `get_simple_data_table` body
/// (`{"epg_listings":[…]}`, or a bare list). Entries without usable times are skipped
/// and reported in the [`Listing`].
///
/// # Errors
///
/// [`Error::Parse`] for non-JSON bodies, [`Error::Auth`] for an auth-failure object.
pub fn parse_epg_listings(body: &[u8], tz: &ServerTimeZone) -> Result<Listing<EpgListing>, Error> {
    let text = de::body_text(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Listing::default());
    }
    let top: &RawValue = serde_json::from_str(trimmed).map_err(|e| de::not_json(&e))?;
    let convert = |raw: &RawValue| {
        serde_json::from_str::<RawEpgListing>(raw.get())
            .map_err(|e| e.to_string())
            .and_then(|r| EpgListing::from_raw(r, tz))
    };
    let s = top.get().trim();
    if s.starts_with('{') {
        let de::OrderedEntries(entries) = serde_json::from_str(s).map_err(|e| de::not_json(&e))?;
        if let Some(err) = de::auth_object_error(&entries) {
            return Err(err);
        }
        if let Some((_, listings)) = entries.iter().find(|(k, _)| k == "epg_listings") {
            return de::list_from_raw(listings, convert);
        }
    }
    de::list_from_raw(top, convert)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::AuthFailure;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/xtream/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    fn paris() -> ServerTimeZone {
        ServerTimeZone::from_name("Europe/Paris").unwrap()
    }

    #[test]
    fn short_epg_decodes_titles_and_times() {
        let l = parse_epg_listings(&fixture("short_epg.json"), &paris()).unwrap();
        assert_eq!(l.skipped, 1, "{:?}", l.errors); // no times at all
        assert_eq!(l.items.len(), 4);
        let a = &l.items[0];
        assert_eq!(a.title, "Morning News");
        assert_eq!(a.description, "Héadlines & weather — ünïcode ✓");
        assert_eq!(a.start.timestamp(), 1_704_088_800);
        assert_eq!(a.stop.timestamp(), 1_704_092_400);
        assert_eq!(a.local_start.as_deref(), Some("2024-01-01 07:00:00"));
        assert!(a.has_archive);
        assert!(!a.now_playing);
        // Plain-text title fallback.
        assert_eq!(l.items[1].title, "Plain Title");
        // No timestamps: local strings converted with the server zone (UTC+1 in January).
        let c = &l.items[2];
        assert_eq!(c.start.timestamp(), 1_704_096_000);
        assert_eq!(c.stop.timestamp(), 1_704_099_600);
        // `stop` key + end_timestamp variants.
        assert_eq!(l.items[3].stop.timestamp(), 1_704_103_200);
        assert_eq!(l.items[3].local_end.as_deref(), Some("2024-01-01 11:00:00"));
    }

    #[test]
    fn epg_envelope_variants() {
        let tz = ServerTimeZone::Utc;
        for body in [
            &b""[..],
            b"[]",
            br#"{"epg_listings":[]}"#,
            br#"{"epg_listings":null}"#,
            br#"{}"#,
        ] {
            assert!(parse_epg_listings(body, &tz).unwrap().is_empty());
        }
        let bare = br#"[{"title":"VA==","start_timestamp":100,"stop_timestamp":200}]"#;
        assert_eq!(parse_epg_listings(bare, &tz).unwrap().items[0].title, "T");
        let keyed = br#"{"epg_listings":{"0":{"title":"VA==","start_timestamp":100,"stop_timestamp":200}}}"#;
        assert_eq!(parse_epg_listings(keyed, &tz).unwrap().len(), 1);
        assert_eq!(
            parse_epg_listings(br#"{"user_info":{"auth":0}}"#, &tz).unwrap_err(),
            Error::Auth {
                status: AuthFailure::InvalidCredentials
            }
        );
        assert!(parse_epg_listings(b"<html>", &tz).is_err());
        let backwards = br#"[{"start_timestamp":200,"stop_timestamp":100}]"#;
        assert_eq!(parse_epg_listings(backwards, &tz).unwrap().skipped, 1);
    }

    #[test]
    fn archive_programmes_and_grouping() {
        let l = parse_epg_listings(&fixture("simple_data_table.json"), &paris()).unwrap();
        assert_eq!(l.items.len(), 4);
        let now = DateTime::from_timestamp(1_704_110_400, 0).unwrap(); // 2024-01-01 12:00Z
        let progs = archive_programmes(&l.items, &paris(), now);
        // has_archive=0 dropped, future programme dropped.
        assert_eq!(progs.len(), 2);
        assert_eq!(progs[0].title, "Late Show");
        assert_eq!(progs[0].start_local.to_string(), "2023-12-31 23:30:00");
        assert_eq!(progs[0].duration_minutes(), 90);
        assert_eq!(progs[1].duration_minutes(), 46); // 45m30s rounds up
        let groups = group_by_server_date(progs);
        let dates: Vec<String> = groups.keys().map(ToString::to_string).collect();
        assert_eq!(dates, vec!["2023-12-31", "2024-01-01"]);
    }

    #[test]
    fn server_local_start_falls_back_to_zone() {
        let l = parse_epg_listings(
            br#"[{"title":"","start_timestamp":1704088800,"stop_timestamp":1704092400}]"#,
            &paris(),
        )
        .unwrap();
        let p = l.items[0].to_archive_programme(&paris());
        assert_eq!(p.start_local.to_string(), "2024-01-01 07:00:00");
    }
}

//! Quirk switches that make the panel misbehave the way real panels do.

use std::fmt;
use std::str::FromStr;

use crate::Error;

/// Combinable quirk switches. `Default` enables only `category-all-literal`.
///
/// Parse from the CLI form with [`Quirks::parse_list`] or [`str::parse`]
/// (`"mixed-types,slow,no-category-all-literal"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools)]
pub struct Quirks {
    /// `mixed-types`: numbers randomly as strings vs ints, ids as strings, some `null`s and
    /// empty strings. Deterministic per item and field.
    pub mixed_types: bool,
    /// `html-content-type`: JSON served as `text/html; charset=UTF-8`.
    pub html_content_type: bool,
    /// `backdrop-string`: `backdrop_path` as a string instead of an array.
    pub backdrop_string: bool,
    /// `episodes-array`: `get_series_info.episodes` as an array of arrays (one per season)
    /// instead of an object keyed by season, for every third series.
    pub episodes_array: bool,
    /// `category-all-literal` (default **on**): a non-numeric `category_id` such as `ALL`
    /// or `FAVOURITE` returns everything, like real panels. When off, it returns `[]`.
    /// An omitted `category_id` always returns everything.
    pub category_all_literal: bool,
    /// `slow`: random 0–3 s latency on API, M3U and XMLTV endpoints.
    pub slow: bool,
    /// `huge`: 50 000 live channels.
    pub huge: bool,
    /// `stream-host`: `server_info.url` is `localhost` while the API is on `127.0.0.1`.
    pub stream_host: bool,
    /// `malformed-item`: one corrupt object in each category/stream list (and one EPG entry
    /// with invalid base64).
    pub malformed_item: bool,
    /// `timeshift-503`: every timeshift request answers `503`, like a real panel whose
    /// archive is down (clients must degrade gracefully).
    pub timeshift_503: bool,
    /// `timeshift-smarters-only`: only the IPTV Smarters start format
    /// (`YYYY-MM-DD:HH:MM:SS`) plays; the standard `YYYY-MM-DD:HH-MM` form gets `503`
    /// (exercises a client's format fallback).
    pub timeshift_smarters_only: bool,
    /// `stall`: live `.ts` streams send the first 40 % of their clip, then keep the
    /// connection open without sending another byte (a frozen stream that never closes,
    /// like a real panel after a zap). Exercises a client's stall detection.
    pub stall: bool,
    /// `slow-media`: film and episode files (and live `.ts` without `live-endless`) are
    /// sent at about 256 KiB/s, so downloads take a while (pause, resume, preemption).
    /// `Range` requests still work.
    pub slow_media: bool,
    /// `live-endless`: a live `.ts` stream never ends: the channel's clip loops at about
    /// its real bitrate, like a real channel (recordings, stall handling). No `Range`.
    pub live_endless: bool,
    /// `media-html`: film, episode and live `.ts` addresses answer `200 OK` with an HTML
    /// error page instead of video (clients must not keep it).
    pub media_html: bool,
    /// `media-playlist`: film and episode addresses answer `200 OK` with a text playlist
    /// pointing at local files (clients must not keep it or follow it).
    pub media_playlist: bool,
    /// `tagged-categories`: most live, film and series categories carry a region tag in
    /// their name (`"UK | News"`, `"FR | Sports"`, `"[CA] Kids"`, `"DE: Kino"`), like many
    /// real panels (to try category management by region). Ids and contents don't change.
    pub tagged_categories: bool,
}

impl Default for Quirks {
    fn default() -> Self {
        Self {
            mixed_types: false,
            html_content_type: false,
            backdrop_string: false,
            episodes_array: false,
            category_all_literal: true,
            slow: false,
            huge: false,
            stream_host: false,
            malformed_item: false,
            timeshift_503: false,
            timeshift_smarters_only: false,
            stall: false,
            slow_media: false,
            live_endless: false,
            media_html: false,
            media_playlist: false,
            tagged_categories: false,
        }
    }
}

impl Quirks {
    /// Every quirk name accepted by [`Quirks::set`] (prefix with `no-` to disable).
    pub const NAMES: &'static [&'static str] = &[
        "mixed-types",
        "html-content-type",
        "backdrop-string",
        "episodes-array",
        "category-all-literal",
        "slow",
        "huge",
        "stream-host",
        "malformed-item",
        "timeshift-503",
        "timeshift-smarters-only",
        "stall",
        "slow-media",
        "live-endless",
        "media-html",
        "media-playlist",
        "tagged-categories",
    ];

    /// No quirks at all (note: also turns `category-all-literal` off).
    pub fn none() -> Self {
        Self {
            category_all_literal: false,
            ..Self::default()
        }
    }

    fn slot(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "mixed-types" => &mut self.mixed_types,
            "html-content-type" => &mut self.html_content_type,
            "backdrop-string" => &mut self.backdrop_string,
            "episodes-array" => &mut self.episodes_array,
            "category-all-literal" => &mut self.category_all_literal,
            "slow" => &mut self.slow,
            "huge" => &mut self.huge,
            "stream-host" => &mut self.stream_host,
            "malformed-item" => &mut self.malformed_item,
            "timeshift-503" => &mut self.timeshift_503,
            "timeshift-smarters-only" => &mut self.timeshift_smarters_only,
            "stall" => &mut self.stall,
            "slow-media" => &mut self.slow_media,
            "live-endless" => &mut self.live_endless,
            "media-html" => &mut self.media_html,
            "media-playlist" => &mut self.media_playlist,
            "tagged-categories" => &mut self.tagged_categories,
            _ => return None,
        })
    }

    /// Enable (`"name"`) or disable (`"no-name"`) one quirk.
    pub fn apply(&mut self, token: &str) -> Result<(), Error> {
        let token = token.trim();
        if token.is_empty() {
            return Ok(());
        }
        let (name, on) = match token.strip_prefix("no-") {
            Some(rest) => (rest, false),
            None => (token, true),
        };
        let slot = self
            .slot(name)
            .ok_or_else(|| Error::UnknownQuirk(token.to_owned()))?;
        *slot = on;
        Ok(())
    }

    /// Set one quirk by name.
    pub fn set(&mut self, name: &str, on: bool) -> Result<(), Error> {
        let slot = self
            .slot(name)
            .ok_or_else(|| Error::UnknownQuirk(name.to_owned()))?;
        *slot = on;
        Ok(())
    }

    /// Start from [`Quirks::default`] and apply each token; tokens may themselves be
    /// comma-separated lists.
    pub fn parse_list<I, S>(items: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut q = Self::default();
        for item in items {
            for token in item.as_ref().split(',') {
                q.apply(token)?;
            }
        }
        Ok(q)
    }

    /// Names of the enabled quirks.
    pub fn enabled(&self) -> Vec<&'static str> {
        let mut copy = *self;
        Self::NAMES
            .iter()
            .copied()
            .filter(|n| copy.slot(n).is_some_and(|v| *v))
            .collect()
    }
}

impl FromStr for Quirks {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_list([s])
    }
}

impl fmt::Display for Quirks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.enabled().join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let q: Quirks = "mixed-types, slow,no-category-all-literal"
            .parse()
            .unwrap_or_default();
        assert!(q.mixed_types && q.slow && !q.category_all_literal);
        assert!(Quirks::parse_list(["bogus"]).is_err());
        assert_eq!(Quirks::default().to_string(), "category-all-literal");
        let q: Quirks = "slow-media,live-endless,media-html,media-playlist"
            .parse()
            .unwrap_or_default();
        assert!(q.slow_media && q.live_endless && q.media_html && q.media_playlist);
    }
}

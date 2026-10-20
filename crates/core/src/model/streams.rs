// SPDX-License-Identifier: GPL-3.0-or-later
//! Category and stream list items.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use super::ser;
use crate::de;

/// A category id. Opaque: usually numeric on the wire (`"12"` or `12`), but compared
/// as a string. For M3U playlists it is the group title.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct CategoryId(String);

impl CategoryId {
    /// Wraps an id.
    pub fn new(id: impl Into<String>) -> Self {
        CategoryId(id.into())
    }

    /// The id as sent to the server.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CategoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CategoryId {
    fn from(s: &str) -> Self {
        CategoryId(s.to_owned())
    }
}

impl From<String> for CategoryId {
    fn from(s: String) -> Self {
        CategoryId(s)
    }
}

impl<'de> Deserialize<'de> for CategoryId {
    /// Lenient but required: a non-empty string or a number.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        de::required_string(d).map(CategoryId)
    }
}

fn opt_category_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<CategoryId>, D::Error> {
    Ok(de::opt_string(d)?.map(CategoryId))
}

fn category_ids<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<CategoryId>, D::Error> {
    Ok(de::string_list(d)?.into_iter().map(CategoryId).collect())
}

/// An entry of `get_live_categories` / `get_vod_categories` / `get_series_categories`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// `category_id` (required).
    #[serde(rename = "category_id")]
    pub id: CategoryId,
    /// `category_name`.
    #[serde(rename = "category_name", default, deserialize_with = "de::string")]
    pub name: String,
    /// `parent_id` (usually `0`).
    #[serde(default, deserialize_with = "de::opt_u64")]
    pub parent_id: Option<u64>,
}

/// An entry of `get_live_streams`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveStream {
    /// Channel number (`num`).
    #[serde(default, deserialize_with = "de::opt_u64")]
    pub num: Option<u64>,
    /// Display name.
    #[serde(default, deserialize_with = "de::string")]
    pub name: String,
    /// Stream id used in the live URL (required).
    #[serde(deserialize_with = "de::required_u64")]
    pub stream_id: u64,
    /// Logo URL (provider-controlled; fetch with size limits).
    #[serde(default, deserialize_with = "de::opt_string")]
    pub stream_icon: Option<String>,
    /// XMLTV / EPG channel id.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub epg_channel_id: Option<String>,
    /// When the stream was added.
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub added: Option<DateTime<Utc>>,
    /// Primary category.
    #[serde(default, deserialize_with = "opt_category_id")]
    pub category_id: Option<CategoryId>,
    /// All categories (newer panels).
    #[serde(default, deserialize_with = "category_ids")]
    pub category_ids: Vec<CategoryId>,
    /// Catch-up available.
    #[serde(default, deserialize_with = "de::bool")]
    pub tv_archive: bool,
    /// Catch-up window in days (informational).
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub tv_archive_duration: Option<u32>,
    /// `custom_sid`.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub custom_sid: Option<String>,
    /// Direct source URL, if the panel exposes one (may contain secrets; treat as such).
    #[serde(default, deserialize_with = "de::opt_string")]
    pub direct_source: Option<String>,
    /// `live`, `created_live`, `radio_streams`, …
    #[serde(default, deserialize_with = "de::opt_string")]
    pub stream_type: Option<String>,
    /// Adult flag (newer panels).
    #[serde(default, deserialize_with = "de::bool")]
    pub is_adult: bool,
}

/// An entry of `get_vod_streams`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VodStream {
    /// `num`.
    #[serde(default, deserialize_with = "de::opt_u64")]
    pub num: Option<u64>,
    /// Display name.
    #[serde(default, deserialize_with = "de::string")]
    pub name: String,
    /// Stream id used in the movie URL and `get_vod_info` (required).
    #[serde(deserialize_with = "de::required_u64")]
    pub stream_id: u64,
    /// Poster URL.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub stream_icon: Option<String>,
    /// Rating out of 10.
    #[serde(default, deserialize_with = "de::opt_f64")]
    pub rating: Option<f64>,
    /// Rating out of 5.
    #[serde(default, deserialize_with = "de::opt_f64")]
    pub rating_5based: Option<f64>,
    /// When it was added.
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub added: Option<DateTime<Utc>>,
    /// Primary category.
    #[serde(default, deserialize_with = "opt_category_id")]
    pub category_id: Option<CategoryId>,
    /// All categories (newer panels).
    #[serde(default, deserialize_with = "category_ids")]
    pub category_ids: Vec<CategoryId>,
    /// File extension for the movie URL (`mp4`, `mkv`, …), without the dot.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub container_extension: Option<String>,
    /// Direct source URL, if any.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub direct_source: Option<String>,
    /// Adult flag (newer panels).
    #[serde(default, deserialize_with = "de::bool")]
    pub is_adult: bool,
}

impl VodStream {
    /// Rating out of 5: `rating_5based`, else `rating / 2`.
    pub fn rating_out_of_5(&self) -> Option<f64> {
        self.rating_5based.or(self.rating.map(|r| r / 2.0))
    }
}

/// An entry of `get_series`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawSeries")]
pub struct Series {
    /// `num`.
    pub num: Option<u64>,
    /// Display name.
    pub name: String,
    /// Series id for `get_series_info` (required).
    pub series_id: u64,
    /// Poster (`cover`).
    pub cover: Option<String>,
    /// Fallback poster (`stream_icon`).
    pub stream_icon: Option<String>,
    /// Rating out of 10.
    pub rating: Option<f64>,
    /// Rating out of 5.
    pub rating_5based: Option<f64>,
    /// Last modification (used for "recently added").
    #[serde(serialize_with = "ser::opt_timestamp")]
    pub last_modified: Option<DateTime<Utc>>,
    /// Backdrop image URLs (string or array on the wire).
    pub backdrop_path: Vec<String>,
    /// Plot.
    pub plot: Option<String>,
    /// Cast.
    pub cast: Option<String>,
    /// Director.
    pub director: Option<String>,
    /// Genre.
    pub genre: Option<String>,
    /// Release date (`releaseDate`, or `release_date` on some panels), verbatim.
    #[serde(rename = "releaseDate")]
    pub release_date: Option<String>,
    /// YouTube trailer id.
    pub youtube_trailer: Option<String>,
    /// Episode run time, verbatim.
    pub episode_run_time: Option<String>,
    /// Primary category.
    pub category_id: Option<CategoryId>,
    /// All categories (newer panels).
    pub category_ids: Vec<CategoryId>,
    /// Adult flag (newer panels).
    pub is_adult: bool,
}

impl Series {
    /// Poster: `cover`, falling back to `stream_icon`.
    pub fn poster(&self) -> Option<&str> {
        self.cover.as_deref().or(self.stream_icon.as_deref())
    }

    /// Rating out of 5: `rating_5based`, else `rating / 2`.
    pub fn rating_out_of_5(&self) -> Option<f64> {
        self.rating_5based.or(self.rating.map(|r| r / 2.0))
    }
}

/// Wire form of [`Series`]; accepts both `releaseDate` and `release_date` without the
/// duplicate-field error that a serde alias would raise when both are present.
#[derive(Deserialize)]
struct RawSeries {
    #[serde(default, deserialize_with = "de::opt_u64")]
    num: Option<u64>,
    #[serde(default, deserialize_with = "de::string")]
    name: String,
    #[serde(deserialize_with = "de::required_u64")]
    series_id: u64,
    #[serde(default, deserialize_with = "de::opt_string")]
    cover: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    stream_icon: Option<String>,
    #[serde(default, deserialize_with = "de::opt_f64")]
    rating: Option<f64>,
    #[serde(default, deserialize_with = "de::opt_f64")]
    rating_5based: Option<f64>,
    #[serde(default, deserialize_with = "de::opt_timestamp")]
    last_modified: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "de::string_list")]
    backdrop_path: Vec<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    plot: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    cast: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    director: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    genre: Option<String>,
    #[serde(rename = "releaseDate", default, deserialize_with = "de::opt_string")]
    release_date_camel: Option<String>,
    #[serde(rename = "release_date", default, deserialize_with = "de::opt_string")]
    release_date_snake: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    youtube_trailer: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    episode_run_time: Option<String>,
    #[serde(default, deserialize_with = "opt_category_id")]
    category_id: Option<CategoryId>,
    #[serde(default, deserialize_with = "category_ids")]
    category_ids: Vec<CategoryId>,
    #[serde(default, deserialize_with = "de::bool")]
    is_adult: bool,
}

impl From<RawSeries> for Series {
    fn from(r: RawSeries) -> Self {
        Series {
            num: r.num,
            name: r.name,
            series_id: r.series_id,
            cover: r.cover,
            stream_icon: r.stream_icon,
            rating: r.rating,
            rating_5based: r.rating_5based,
            last_modified: r.last_modified,
            backdrop_path: r.backdrop_path,
            plot: r.plot,
            cast: r.cast,
            director: r.director,
            genre: r.genre,
            release_date: r.release_date_camel.or(r.release_date_snake),
            youtube_trailer: r.youtube_trailer,
            episode_run_time: r.episode_run_time,
            category_id: r.category_id,
            category_ids: r.category_ids,
            is_adult: r.is_adult,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::de::{Listing, parse_list};

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/xtream/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    #[test]
    fn categories() {
        let l: Listing<Category> = parse_list(&fixture("live_categories.json")).unwrap();
        assert_eq!(l.items.len(), 3);
        assert_eq!(l.items[0].id.as_str(), "1");
        assert_eq!(l.items[0].name, "News");
        assert_eq!(l.items[1].id.as_str(), "2"); // numeric id on the wire
        assert_eq!(l.items[1].parent_id, Some(0));
        assert_eq!(l.items[2].name, "");
        assert_eq!(l.skipped, 2); // missing id, empty id
    }

    #[test]
    fn live_streams_with_quirks() {
        let l: Listing<LiveStream> = parse_list(&fixture("live_streams.json")).unwrap();
        assert_eq!(l.skipped, 2, "{:?}", l.errors);
        assert_eq!(l.items.len(), 4);
        let a = &l.items[0];
        assert_eq!(a.num, Some(1));
        assert_eq!(a.stream_id, 101);
        assert_eq!(a.name, "Synthetic News");
        assert_eq!(a.epg_channel_id.as_deref(), Some("news.synthetic"));
        assert_eq!(a.category_id.as_ref().map(CategoryId::as_str), Some("1"));
        assert!(a.tv_archive);
        assert_eq!(a.tv_archive_duration, Some(7));
        assert_eq!(a.added, DateTime::from_timestamp(1_700_000_000, 0));
        let b = &l.items[1];
        assert_eq!(b.stream_id, 102); // string id
        assert_eq!(b.num, Some(2)); // "2"
        assert!(!b.tv_archive); // "0"
        assert_eq!(b.stream_icon, None); // ""
        assert_eq!(b.epg_channel_id, None); // null
        assert_eq!(b.category_id.as_ref().map(CategoryId::as_str), Some("2")); // number
        let c = &l.items[2];
        assert!(c.tv_archive); // true
        assert_eq!(
            c.category_ids,
            vec![CategoryId::new("1"), CategoryId::new("3")]
        );
        let d = &l.items[3];
        assert_eq!(d.stream_id, 104); // 104.0
        assert_eq!(d.name, "");
        assert!(d.is_adult);
    }

    #[test]
    fn vod_streams_ratings() {
        let l: Listing<VodStream> = parse_list(&fixture("vod_streams.json")).unwrap();
        assert_eq!(l.skipped, 1);
        let r: Vec<_> = l.items.iter().map(|v| v.rating).collect();
        assert_eq!(r, vec![Some(7.5), Some(8.0), None, None, Some(6.2)]);
        assert_eq!(l.items[0].container_extension.as_deref(), Some("mp4"));
        assert_eq!(l.items[0].rating_out_of_5(), Some(3.75));
        assert_eq!(l.items[1].rating_out_of_5(), Some(4.5)); // rating_5based wins
        assert_eq!(l.items[2].rating_out_of_5(), None);
    }

    #[test]
    fn series_with_quirks() {
        let l: Listing<Series> = parse_list(&fixture("series.json")).unwrap();
        assert_eq!(l.skipped, 1);
        let a = &l.items[0];
        assert_eq!(a.series_id, 501);
        assert_eq!(
            a.backdrop_path,
            vec![
                "https://img.example.org/b1.jpg",
                "https://img.example.org/b2.jpg"
            ]
        );
        assert_eq!(a.release_date.as_deref(), Some("2020-05-01"));
        assert_eq!(a.poster(), Some("https://img.example.org/cover501.jpg"));
        let b = &l.items[1];
        assert_eq!(b.backdrop_path, vec!["https://img.example.org/b3.jpg"]); // string form
        assert_eq!(b.release_date.as_deref(), Some("2019")); // snake_case form
        assert_eq!(b.poster(), Some("https://img.example.org/icon502.png")); // fallback
        assert_eq!(b.rating, None); // "N/A"
        let c = &l.items[2];
        assert!(c.backdrop_path.is_empty()); // null
        assert_eq!(c.release_date.as_deref(), Some("2021-01-01")); // both present: camelCase wins
    }

    #[test]
    fn list_models_round_trip() {
        let live: Listing<LiveStream> = parse_list(&fixture("live_streams.json")).unwrap();
        let json = serde_json::to_vec(&live.items).unwrap();
        let back: Listing<LiveStream> = parse_list(&json).unwrap();
        assert_eq!(back.items, live.items);

        let series: Listing<Series> = parse_list(&fixture("series.json")).unwrap();
        let json = serde_json::to_vec(&series.items).unwrap();
        let back: Listing<Series> = parse_list(&json).unwrap();
        assert_eq!(back.items, series.items);

        let vod: Listing<VodStream> = parse_list(&fixture("vod_streams.json")).unwrap();
        let json = serde_json::to_vec(&vod.items).unwrap();
        assert_eq!(parse_list::<VodStream>(&json).unwrap().items, vod.items);

        let cats: Listing<Category> = parse_list(&fixture("live_categories.json")).unwrap();
        let json = serde_json::to_vec(&cats.items).unwrap();
        assert_eq!(parse_list::<Category>(&json).unwrap().items, cats.items);
    }
}

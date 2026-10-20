// SPDX-License-Identifier: GPL-3.0-or-later
//! `get_vod_info` and `get_series_info` responses.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use super::ser;
use super::streams::CategoryId;
use crate::de;

fn opt_category_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<CategoryId>, D::Error> {
    Ok(de::opt_string(d)?.map(CategoryId::new))
}

// ---------------------------------------------------------------------------------------
// VOD
// ---------------------------------------------------------------------------------------

/// `get_vod_info` response. `info` may be `[]` on the wire (PHP's empty object); it then
/// deserialises to the default. Panels answer `[]` for unknown ids, which yields
/// `VodInfo::default()`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VodInfo {
    /// Metadata.
    #[serde(default, deserialize_with = "de::object_or_default")]
    pub info: VodDetails,
    /// Playback data. The original client ignores it and uses the list item's
    /// `container_extension`; it is kept here for completeness.
    #[serde(default, deserialize_with = "de::object_or_none")]
    pub movie_data: Option<MovieData>,
}

/// `get_vod_info` → `info`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "RawVodDetails")]
pub struct VodDetails {
    /// Title (`name`, else `o_name`).
    pub name: Option<String>,
    /// Poster (`movie_image`, else `cover_big`).
    pub movie_image: Option<String>,
    /// Backdrops (string or array on the wire).
    pub backdrop_path: Vec<String>,
    /// Rating out of 10.
    pub rating: Option<f64>,
    /// Director.
    pub director: Option<String>,
    /// Release date (`releasedate`, else `release_date`), verbatim.
    #[serde(rename = "releasedate")]
    pub release_date: Option<String>,
    /// Duration as displayed (`"01:45:00"`).
    pub duration: Option<String>,
    /// Duration in seconds.
    pub duration_secs: Option<u64>,
    /// Cast (`cast`, else `actors`).
    pub cast: Option<String>,
    /// Genre.
    pub genre: Option<String>,
    /// Plot (`plot`, else `description`).
    pub plot: Option<String>,
    /// YouTube trailer id.
    pub youtube_trailer: Option<String>,
    /// TMDB id (for opt-in enrichment).
    pub tmdb_id: Option<String>,
    /// Country.
    pub country: Option<String>,
}

#[derive(Deserialize)]
struct RawVodDetails {
    #[serde(default, deserialize_with = "de::opt_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    o_name: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    movie_image: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    cover_big: Option<String>,
    #[serde(default, deserialize_with = "de::string_list")]
    backdrop_path: Vec<String>,
    #[serde(default, deserialize_with = "de::opt_f64")]
    rating: Option<f64>,
    #[serde(default, deserialize_with = "de::opt_string")]
    director: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    releasedate: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    release_date: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    duration: Option<String>,
    #[serde(default, deserialize_with = "de::opt_u64")]
    duration_secs: Option<u64>,
    #[serde(default, deserialize_with = "de::opt_string")]
    cast: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    actors: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    genre: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    plot: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    description: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    youtube_trailer: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    tmdb_id: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    country: Option<String>,
}

impl From<RawVodDetails> for VodDetails {
    fn from(r: RawVodDetails) -> Self {
        VodDetails {
            name: r.name.or(r.o_name),
            movie_image: r.movie_image.or(r.cover_big),
            backdrop_path: r.backdrop_path,
            rating: r.rating,
            director: r.director,
            release_date: r.releasedate.or(r.release_date),
            duration: r.duration,
            duration_secs: r.duration_secs,
            cast: r.cast.or(r.actors),
            genre: r.genre,
            plot: r.plot.or(r.description),
            youtube_trailer: r.youtube_trailer,
            tmdb_id: r.tmdb_id,
            country: r.country,
        }
    }
}

/// `get_vod_info` → `movie_data`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MovieData {
    /// Stream id.
    #[serde(default, deserialize_with = "de::opt_u64")]
    pub stream_id: Option<u64>,
    /// Name.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub name: Option<String>,
    /// When it was added.
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub added: Option<DateTime<Utc>>,
    /// Category.
    #[serde(default, deserialize_with = "opt_category_id")]
    pub category_id: Option<CategoryId>,
    /// Container extension.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub container_extension: Option<String>,
    /// Direct source URL, if any.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub direct_source: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Series
// ---------------------------------------------------------------------------------------

/// `get_series_info` response.
///
/// `episodes` is normalised to a map *season number → episodes* whatever the wire form:
/// - an object keyed by season (`{"1":[…],"2":[…]}`; non-numeric keys such as
///   `"Season 2"` use their digits, else the episode's own `season`);
/// - a season's value may itself be an object keyed by index instead of an array;
/// - an array of arrays (season = the episodes' `season` field, else position + 1);
/// - a flat array of episodes (season = the episode's `season` field, else 1);
/// - `[]` / `null` → no episodes.
///
/// Malformed episodes (no numeric `id`) are dropped. Within a season, episodes are sorted
/// by `episode_num` (stable; missing numbers last).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SeriesInfo {
    /// Series metadata. The original client uses the *list item* for the detail header.
    #[serde(default, deserialize_with = "de::object_or_default")]
    pub info: SeriesDetails,
    /// Season metadata (ignored by the original client).
    #[serde(default, deserialize_with = "de::lenient_vec")]
    pub seasons: Vec<Season>,
    /// Episodes by season number.
    #[serde(default, deserialize_with = "episodes")]
    pub episodes: BTreeMap<u32, Vec<Episode>>,
}

impl SeriesInfo {
    /// Total number of episodes.
    pub fn episode_count(&self) -> usize {
        self.episodes.values().map(Vec::len).sum()
    }

    /// Finds an episode by id.
    pub fn episode(&self, id: u64) -> Option<&Episode> {
        self.episodes.values().flatten().find(|e| e.id == id)
    }
}

/// `get_series_info` → `info`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "RawSeriesDetails")]
pub struct SeriesDetails {
    /// Name.
    pub name: Option<String>,
    /// Poster.
    pub cover: Option<String>,
    /// Plot.
    pub plot: Option<String>,
    /// Cast.
    pub cast: Option<String>,
    /// Director.
    pub director: Option<String>,
    /// Genre.
    pub genre: Option<String>,
    /// Release date (`releaseDate`, else `release_date`).
    #[serde(rename = "releaseDate")]
    pub release_date: Option<String>,
    /// Last modification.
    #[serde(serialize_with = "ser::opt_timestamp")]
    pub last_modified: Option<DateTime<Utc>>,
    /// Rating out of 10.
    pub rating: Option<f64>,
    /// Rating out of 5.
    pub rating_5based: Option<f64>,
    /// Backdrops.
    pub backdrop_path: Vec<String>,
    /// YouTube trailer id.
    pub youtube_trailer: Option<String>,
    /// Episode run time, verbatim.
    pub episode_run_time: Option<String>,
    /// Category.
    pub category_id: Option<CategoryId>,
}

#[derive(Deserialize)]
struct RawSeriesDetails {
    #[serde(default, deserialize_with = "de::opt_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    cover: Option<String>,
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
    #[serde(default, deserialize_with = "de::opt_timestamp")]
    last_modified: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "de::opt_f64")]
    rating: Option<f64>,
    #[serde(default, deserialize_with = "de::opt_f64")]
    rating_5based: Option<f64>,
    #[serde(default, deserialize_with = "de::string_list")]
    backdrop_path: Vec<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    youtube_trailer: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    episode_run_time: Option<String>,
    #[serde(default, deserialize_with = "opt_category_id")]
    category_id: Option<CategoryId>,
}

impl From<RawSeriesDetails> for SeriesDetails {
    fn from(r: RawSeriesDetails) -> Self {
        SeriesDetails {
            name: r.name,
            cover: r.cover,
            plot: r.plot,
            cast: r.cast,
            director: r.director,
            genre: r.genre,
            release_date: r.release_date_camel.or(r.release_date_snake),
            last_modified: r.last_modified,
            rating: r.rating,
            rating_5based: r.rating_5based,
            backdrop_path: r.backdrop_path,
            youtube_trailer: r.youtube_trailer,
            episode_run_time: r.episode_run_time,
            category_id: r.category_id,
        }
    }
}

/// `get_series_info` → `seasons[]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Season {
    /// Season number.
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub season_number: Option<u32>,
    /// Name.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub name: Option<String>,
    /// Number of episodes.
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub episode_count: Option<u32>,
    /// Cover image.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub cover: Option<String>,
    /// Large cover image.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub cover_big: Option<String>,
    /// Air date, verbatim.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub air_date: Option<String>,
    /// Overview.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub overview: Option<String>,
}

/// One episode of a series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    /// Episode id used in the episode URL (required).
    #[serde(deserialize_with = "de::required_u64")]
    pub id: u64,
    /// Episode number within the season.
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub episode_num: Option<u32>,
    /// Title.
    #[serde(default, deserialize_with = "de::string")]
    pub title: String,
    /// File extension for the episode URL, without the dot.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub container_extension: Option<String>,
    /// Season number as stated by the episode itself.
    #[serde(default, deserialize_with = "de::opt_u32")]
    pub season: Option<u32>,
    /// When it was added.
    #[serde(
        default,
        deserialize_with = "de::opt_timestamp",
        serialize_with = "ser::opt_timestamp"
    )]
    pub added: Option<DateTime<Utc>>,
    /// Direct source URL, if any.
    #[serde(default, deserialize_with = "de::opt_string")]
    pub direct_source: Option<String>,
    /// Metadata (`[]` on the wire when empty).
    #[serde(default, deserialize_with = "de::object_or_default")]
    pub info: EpisodeInfo,
}

/// Episode metadata.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "RawEpisodeInfo")]
pub struct EpisodeInfo {
    /// Still image.
    pub movie_image: Option<String>,
    /// Rating out of 10.
    pub rating: Option<f64>,
    /// Duration as displayed.
    pub duration: Option<String>,
    /// Duration in seconds.
    pub duration_secs: Option<u64>,
    /// Plot.
    pub plot: Option<String>,
    /// Release / air date (`releasedate`, `release_date` or `air_date`), verbatim.
    #[serde(rename = "releasedate")]
    pub release_date: Option<String>,
    /// TMDB id.
    pub tmdb_id: Option<String>,
}

#[derive(Deserialize)]
struct RawEpisodeInfo {
    #[serde(default, deserialize_with = "de::opt_string")]
    movie_image: Option<String>,
    #[serde(default, deserialize_with = "de::opt_f64")]
    rating: Option<f64>,
    #[serde(default, deserialize_with = "de::opt_string")]
    duration: Option<String>,
    #[serde(default, deserialize_with = "de::opt_u64")]
    duration_secs: Option<u64>,
    #[serde(default, deserialize_with = "de::opt_string")]
    plot: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    releasedate: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    release_date: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    air_date: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    tmdb_id: Option<String>,
}

impl From<RawEpisodeInfo> for EpisodeInfo {
    fn from(r: RawEpisodeInfo) -> Self {
        EpisodeInfo {
            movie_image: r.movie_image,
            rating: r.rating,
            duration: r.duration,
            duration_secs: r.duration_secs,
            plot: r.plot,
            release_date: r.releasedate.or(r.release_date).or(r.air_date),
            tmdb_id: r.tmdb_id,
        }
    }
}

/// Season number from an object key: `"2"`, `" 2 "`, `"Season 2"`, `"S02"`.
fn season_from_key(key: &str) -> Option<u32> {
    let t = key.trim();
    if let Ok(n) = t.parse::<u32>() {
        return Some(n);
    }
    let digits: String = t
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn episodes<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<u32, Vec<Episode>>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(normalise_episodes(value))
}

fn parse_episode(value: serde_json::Value) -> Option<Episode> {
    if value.is_object() {
        Episode::deserialize(value).ok()
    } else {
        None
    }
}

fn normalise_episodes(value: serde_json::Value) -> BTreeMap<u32, Vec<Episode>> {
    use serde_json::Value;

    let mut out: BTreeMap<u32, Vec<Episode>> = BTreeMap::new();
    match value {
        Value::Object(map) => {
            for (key, season_value) in map {
                let key_season = season_from_key(&key);
                for ep in de::lenient_items(season_value).filter_map(parse_episode) {
                    let season = key_season.or(ep.season).unwrap_or(0);
                    out.entry(season).or_default().push(ep);
                }
            }
        }
        Value::Array(items) => {
            for (pos, item) in items.into_iter().enumerate() {
                if item.is_array() {
                    let fallback = u32::try_from(pos).unwrap_or(u32::MAX).saturating_add(1);
                    for ep in de::lenient_items(item).filter_map(parse_episode) {
                        let season = ep.season.unwrap_or(fallback);
                        out.entry(season).or_default().push(ep);
                    }
                } else if let Some(ep) = parse_episode(item) {
                    let season = ep.season.unwrap_or(1);
                    out.entry(season).or_default().push(ep);
                }
            }
        }
        _ => {}
    }
    for eps in out.values_mut() {
        eps.sort_by_key(|e| (e.episode_num.is_none(), e.episode_num));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::de::parse_object;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/xtream/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    fn ids(eps: &[Episode]) -> Vec<u64> {
        eps.iter().map(|e| e.id).collect()
    }

    #[test]
    fn vod_info_full() {
        let v: VodInfo = parse_object(&fixture("vod_info.json")).unwrap();
        assert_eq!(v.info.name.as_deref(), Some("Synthetic Movie A"));
        assert_eq!(v.info.backdrop_path, vec!["https://img.example.org/bd.jpg"]);
        assert_eq!(v.info.rating, Some(7.5));
        assert_eq!(v.info.release_date.as_deref(), Some("2020-01-01"));
        assert_eq!(v.info.plot.as_deref(), Some("Plot text.")); // plot preferred over description
        assert_eq!(v.info.cast.as_deref(), Some("Actor A")); // from `actors`
        assert_eq!(v.info.duration_secs, Some(6300));
        assert_eq!(v.info.tmdb_id.as_deref(), Some("12345"));
        let md = v.movie_data.unwrap();
        assert_eq!(md.stream_id, Some(201));
        assert_eq!(md.container_extension.as_deref(), Some("mp4"));
    }

    #[test]
    fn vod_info_php_empty_objects() {
        let v: VodInfo = parse_object(br#"{"info":[],"movie_data":[]}"#).unwrap();
        assert_eq!(v, VodInfo::default());
        let v: VodInfo = parse_object(b"[]").unwrap();
        assert_eq!(v, VodInfo::default());
        let v: VodInfo =
            parse_object(br#"{"info":{"backdrop_path":"x.jpg","rating":"","description":"d"}}"#)
                .unwrap();
        assert_eq!(v.info.backdrop_path, vec!["x.jpg"]);
        assert_eq!(v.info.rating, None);
        assert_eq!(v.info.plot.as_deref(), Some("d"));
    }

    #[test]
    fn series_info_object_keyed_by_season() {
        let s: SeriesInfo = parse_object(&fixture("series_info_object.json")).unwrap();
        assert_eq!(s.info.name.as_deref(), Some("Synthetic Series One"));
        assert_eq!(s.seasons.len(), 2);
        assert_eq!(s.episodes.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(ids(&s.episodes[&1]), vec![1001, 1002]); // sorted by episode_num
        assert_eq!(ids(&s.episodes[&2]), vec![2001]); // season value keyed by index
        let e = &s.episodes[&1][0];
        assert_eq!(e.title, "Episode One");
        assert_eq!(e.container_extension.as_deref(), Some("mkv"));
        assert_eq!(e.info.rating, Some(8.0));
        assert_eq!(e.info.duration.as_deref(), Some("00:45:00"));
        assert_eq!(s.episode_count(), 3);
        assert_eq!(
            s.episode(2001).map(|e| e.title.as_str()),
            Some("Season Two Opener")
        );
    }

    #[test]
    fn series_info_episode_array_forms() {
        let s: SeriesInfo = parse_object(&fixture("series_info_array.json")).unwrap();
        // Array of arrays: season from episode field, else position + 1.
        assert_eq!(
            s.episodes.keys().copied().collect::<Vec<_>>(),
            vec![1, 2, 5]
        );
        assert_eq!(ids(&s.episodes[&1]), vec![11]);
        assert_eq!(ids(&s.episodes[&2]), vec![21]);
        assert_eq!(ids(&s.episodes[&5]), vec![51]);
        assert!(s.seasons.is_empty()); // `seasons: []`
        assert_eq!(s.info, SeriesDetails::default()); // `info: []`

        let flat: SeriesInfo = parse_object(
            br#"{"episodes":[{"id":"7","season":"3","episode_num":2},{"id":8,"episode_num":1},{"title":"no id"}]}"#,
        )
        .unwrap();
        assert_eq!(ids(&flat.episodes[&3]), vec![7]);
        assert_eq!(ids(&flat.episodes[&1]), vec![8]);
        assert_eq!(flat.episode_count(), 2);
    }

    #[test]
    fn series_info_odd_keys_and_empty() {
        let s: SeriesInfo = parse_object(
            br#"{"episodes":{"Season 4":[{"id":1}],"x":[{"id":2,"season":6}],"y":[{"id":3}],"9":"junk"}}"#,
        )
        .unwrap();
        assert_eq!(ids(&s.episodes[&4]), vec![1]);
        assert_eq!(ids(&s.episodes[&6]), vec![2]);
        assert_eq!(ids(&s.episodes[&0]), vec![3]);
        assert!(!s.episodes.contains_key(&9));
        for body in [
            &br#"{"episodes":[]}"#[..],
            br#"{"episodes":null}"#,
            br#"{}"#,
            b"[]",
        ] {
            let s: SeriesInfo = parse_object(body).unwrap();
            assert!(s.episodes.is_empty());
        }
    }

    #[test]
    fn detail_models_round_trip() {
        let s: SeriesInfo = parse_object(&fixture("series_info_object.json")).unwrap();
        let json = serde_json::to_vec(&s).unwrap();
        assert_eq!(parse_object::<SeriesInfo>(&json).unwrap(), s);
        let v: VodInfo = parse_object(&fixture("vod_info.json")).unwrap();
        let json = serde_json::to_vec(&v).unwrap();
        assert_eq!(parse_object::<VodInfo>(&json).unwrap(), v);
    }
}

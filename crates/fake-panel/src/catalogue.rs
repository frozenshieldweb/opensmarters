//! The synthetic, deterministic catalogue: categories, live channels, movies and series.
//!
//! Generated once per server from [`Config::seed`](crate::Config::seed). With the default
//! seed the counts are fixed: 14 live categories (one empty), 250 live channels (50 000
//! with the `huge` quirk), 10 VOD categories, 150 movies, 6 series categories, 30 series.

use std::collections::{HashMap, HashSet};

use crate::names::*;
use crate::rng::{Rng, hash_str, mix};

/// Live-channel stream ids are `LIVE_ID_BASE + num`.
pub const LIVE_ID_BASE: u64 = 1000;
/// Movie stream ids are `MOVIE_ID_BASE + num`.
pub const MOVIE_ID_BASE: u64 = 200_000;
/// Episode ids start after this value.
pub const EPISODE_ID_BASE: u64 = 300_000;
/// Number of live channels with the `huge` quirk.
pub const HUGE_LIVE_COUNT: usize = 50_000;
/// Number of distinct generated live clips that channels map onto.
pub const LIVE_CLIPS: usize = 4;

/// Content theme, used for names and EPG titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Theme {
    News,
    Sports,
    Kids,
    Documentary,
    Music,
    Entertainment,
    Adult,
    Sussex,
    Essex,
    Greek,
    Japanese,
    Emoji,
    Variety,
    Empty,
}

impl Theme {
    pub(crate) fn epg_titles(self) -> &'static [&'static str] {
        match self {
            Self::News => EPG_NEWS,
            Self::Sports => EPG_SPORTS,
            Self::Kids => EPG_KIDS,
            Self::Documentary => EPG_DOC,
            Self::Music => EPG_MUSIC,
            Self::Entertainment => EPG_ENT,
            Self::Adult => EPG_ADULT,
            Self::Sussex | Self::Essex => EPG_LOCAL,
            Self::Greek => EPG_GREEK,
            Self::Japanese => EPG_JAPANESE,
            Self::Emoji => EPG_EMOJI,
            Self::Variety | Self::Empty => EPG_VARIETY,
        }
    }

    pub(crate) fn lang(self) -> &'static str {
        match self {
            Self::Greek => "el",
            Self::Japanese => "ja",
            _ => "en",
        }
    }

    pub(crate) fn genre(self) -> &'static str {
        match self {
            Self::News => "News",
            Self::Sports => "Sports",
            Self::Kids => "Children",
            Self::Documentary => "Documentary",
            Self::Music => "Music",
            Self::Adult => "Adult",
            Self::Sussex | Self::Essex => "Local",
            _ => "Entertainment",
        }
    }
}

/// A category as returned by `get_*_categories`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// Numeric id (serialised as a string, like real panels).
    pub id: u32,
    /// Display name (may contain unicode and emoji).
    pub name: String,
    /// Always 0.
    pub parent_id: u32,
}

/// What a channel's `stream_icon` points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// A PNG generated and served by the panel.
    Generated,
    /// `stream_icon: ""`.
    Empty,
    /// A panel URL that returns 404.
    Broken,
}

/// A live channel.
#[derive(Debug, Clone)]
pub struct LiveChannel {
    /// Channel number (1-based, server order).
    pub num: u32,
    /// Stream id (`LIVE_ID_BASE + num`).
    pub stream_id: u64,
    /// Display name.
    pub name: String,
    /// Live category id.
    pub category_id: u32,
    /// XMLTV / EPG channel id; `None` means no EPG (`epg_channel_id: null`).
    pub epg_channel_id: Option<String>,
    /// Logo behaviour.
    pub icon: Icon,
    /// Catch-up available.
    pub tv_archive: bool,
    /// Catch-up window in days (0 when no archive).
    pub tv_archive_duration: u32,
    /// `added` epoch seconds.
    pub added: i64,
    /// In the adult category.
    pub is_adult: bool,
    /// Index of the generated live clip served for this channel.
    pub clip: usize,
    pub(crate) theme: Theme,
}

/// A movie (`get_vod_streams` / `get_vod_info`).
#[derive(Debug, Clone)]
pub struct Movie {
    /// Position in the list (1-based).
    pub num: u32,
    /// Stream id (`MOVIE_ID_BASE + num`).
    pub stream_id: u64,
    /// List name, usually `"Title (Year)"`.
    pub name: String,
    /// Title without year.
    pub title: String,
    /// Release year.
    pub year: i32,
    /// VOD category id.
    pub category_id: u32,
    /// 0–10, one decimal (0 = unrated).
    pub rating: f64,
    /// `added` epoch seconds.
    pub added: i64,
    /// `mkv`, `mp4` or `avi`.
    pub container_extension: &'static str,
    /// Plot summary.
    pub plot: String,
    /// Cast names.
    pub cast: Vec<String>,
    /// Director name.
    pub director: String,
    /// Genre string.
    pub genre: String,
    /// `YYYY-MM-DD`.
    pub release_date: String,
    /// Runtime.
    pub duration_secs: u32,
    /// Synthetic TMDB id (far outside the real id range).
    pub tmdb_id: u64,
    /// Whether `backdrop_path` has an entry.
    pub has_backdrop: bool,
    /// In the adult category.
    pub is_adult: bool,
}

/// An episode in `get_series_info`.
#[derive(Debug, Clone)]
pub struct Episode {
    /// Episode stream id (`/series/{u}/{p}/{id}.{ext}`).
    pub id: u64,
    /// Season number.
    pub season: u32,
    /// Episode number within the season.
    pub episode_num: u32,
    /// Full title, `"Series - S01E02 - Name"`.
    pub title: String,
    /// `mkv` or `mp4`.
    pub container_extension: &'static str,
    /// Plot.
    pub plot: String,
    /// Runtime.
    pub duration_secs: u32,
    /// 0–10.
    pub rating: f64,
    /// `YYYY-MM-DD`.
    pub release_date: String,
    /// `added` epoch seconds.
    pub added: i64,
}

/// A season with its episodes.
#[derive(Debug, Clone)]
pub struct Season {
    /// Season number (0 = specials).
    pub number: u32,
    /// `YYYY-MM-DD`.
    pub air_date: String,
    /// Episodes in order.
    pub episodes: Vec<Episode>,
}

/// A series (`get_series` / `get_series_info`).
#[derive(Debug, Clone)]
pub struct Series {
    /// Position in the list (1-based).
    pub num: u32,
    /// Series id.
    pub series_id: u64,
    /// Display name.
    pub name: String,
    /// Series category id.
    pub category_id: u32,
    /// Plot.
    pub plot: String,
    /// Cast names.
    pub cast: Vec<String>,
    /// Director.
    pub director: String,
    /// Genre.
    pub genre: String,
    /// `YYYY-MM-DD`.
    pub release_date: String,
    /// 0–10.
    pub rating: f64,
    /// `last_modified` epoch seconds.
    pub last_modified: i64,
    /// Minutes.
    pub episode_run_time: u32,
    /// Whether `backdrop_path` has an entry.
    pub has_backdrop: bool,
    /// Seasons in order (one series has a season 0, one has a gap).
    pub seasons: Vec<Season>,
}

/// The whole synthetic catalogue.
#[derive(Debug, Clone)]
pub struct Catalogue {
    /// Live categories in server order.
    pub live_categories: Vec<Category>,
    /// VOD categories in server order.
    pub vod_categories: Vec<Category>,
    /// Series categories in server order.
    pub series_categories: Vec<Category>,
    /// Live channels in server order.
    pub live: Vec<LiveChannel>,
    /// Movies in server order.
    pub movies: Vec<Movie>,
    /// Series in server order.
    pub series: Vec<Series>,
    episodes: HashMap<u64, (usize, usize, usize)>,
}

struct CatDef {
    id: u32,
    name: &'static str,
    theme: Theme,
    count: usize,
}

const fn cat(id: u32, name: &'static str, theme: Theme, count: usize) -> CatDef {
    CatDef {
        id,
        name,
        theme,
        count,
    }
}

const LIVE_CATS: &[CatDef] = &[
    cat(1, "News", Theme::News, 30),
    cat(2, "Sports", Theme::Sports, 30),
    cat(3, "Kids", Theme::Kids, 18),
    cat(4, "Documentary", Theme::Documentary, 18),
    cat(5, "Music", Theme::Music, 16),
    cat(6, "Entertainment", Theme::Entertainment, 26),
    cat(7, "Adult Night", Theme::Adult, 8),
    cat(8, "Sussex Local", Theme::Sussex, 10),
    cat(9, "Essex FM", Theme::Essex, 10),
    cat(10, "Ελληνικά", Theme::Greek, 14),
    cat(11, "日本語", Theme::Japanese, 14),
    cat(12, "🎬 Cinema ⭐ Premium 🍿", Theme::Emoji, 16),
    cat(13, "Coming Soon", Theme::Empty, 0),
    cat(14, LONG_CATEGORY_NAME, Theme::Variety, 40),
];

struct VodCat {
    id: u32,
    name: &'static str,
    genre: &'static str,
    count: usize,
}

const fn vod(id: u32, name: &'static str, genre: &'static str, count: usize) -> VodCat {
    VodCat {
        id,
        name,
        genre,
        count,
    }
}

const VOD_CATS: &[VodCat] = &[
    vod(101, "Action", "Action, Adventure", 22),
    vod(102, "Comedy", "Comedy", 22),
    vod(103, "Drama", "Drama", 22),
    vod(104, "Science Fiction", "Science Fiction, Mystery", 18),
    vod(105, "Documentary", "Documentary", 14),
    vod(106, "Family", "Family, Animation", 16),
    vod(107, "Thriller", "Thriller, Crime", 16),
    vod(108, "Ελληνικές Ταινίες", "Δράμα", 8),
    vod(109, "18+ After Dark", "Romance", 6),
    vod(110, "UHD Showcase", "Documentary, Nature", 6),
];

const SERIES_CATS: &[VodCat] = &[
    vod(201, "Drama Series", "Drama", 8),
    vod(202, "Comedy Series", "Comedy", 6),
    vod(203, "Crime & Mystery", "Crime, Mystery", 5),
    vod(204, "Kids Series", "Animation, Family", 4),
    vod(205, "Documentary Series", "Documentary", 4),
    vod(206, "アニメ", "Animation", 3),
];

/// 2025-01-01T00:00:00Z: `added` dates are spread over the following ~14 months.
pub(crate) const ADDED_BASE: i64 = 1_735_689_600;

fn added(rng: &mut Rng) -> i64 {
    ADDED_BASE + rng.below(420) as i64 * 86_400 + rng.below(86_400) as i64
}

fn person(rng: &mut Rng) -> String {
    format!("{} {}", rng.pick(FIRST_NAMES), rng.pick(LAST_NAMES))
}

fn people(rng: &mut Rng, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(n);
    while out.len() < n {
        let p = person(rng);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn plot(rng: &mut Rng) -> String {
    format!(
        "{} {} {}. {}",
        rng.pick(PLOT_SUBJECTS),
        rng.pick(PLOT_VERBS),
        rng.pick(PLOT_OBJECTS),
        rng.pick(PLOT_TWISTS)
    )
}

fn date_string(year: i32, rng: &mut Rng) -> String {
    format!("{year:04}-{:02}-{:02}", rng.range(1, 12), rng.range(1, 28))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Lowercase ASCII slug: `"Cobalt Sports 2 HD"` → `"cobalt.sports.2.hd"`.
fn slug(name: &str) -> String {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(".")
}

fn unique(used: &mut HashSet<String>, candidate: String) -> String {
    if used.insert(candidate.clone()) {
        return candidate;
    }
    let mut n = 2;
    loop {
        let alt = format!("{candidate} {n}");
        if used.insert(alt.clone()) {
            return alt;
        }
        n += 1;
    }
}

fn channel_name(theme: Theme, idx: usize, rng: &mut Rng, used: &mut HashSet<String>) -> String {
    let fixed: Option<&[&str]> = match theme {
        Theme::Adult => Some(ADULT_CHANNELS),
        Theme::Sussex => Some(SUSSEX_CHANNELS),
        Theme::Essex => Some(ESSEX_CHANNELS),
        Theme::Greek => Some(GREEK_CHANNELS),
        Theme::Japanese => Some(JAPANESE_CHANNELS),
        Theme::Emoji => Some(EMOJI_CHANNELS),
        Theme::Variety if idx < VARIETY_SPECIALS.len() => Some(&VARIETY_SPECIALS[idx..=idx]),
        _ => None,
    };
    if let Some(list) = fixed {
        let name = list
            .get(idx % list.len())
            .copied()
            .unwrap_or("Unnamed")
            .to_owned();
        return unique(used, name);
    }
    let suffixes = match theme {
        Theme::News => NEWS_SUFFIX,
        Theme::Sports => SPORTS_SUFFIX,
        Theme::Kids => KIDS_SUFFIX,
        Theme::Documentary => DOC_SUFFIX,
        Theme::Music => MUSIC_SUFFIX,
        Theme::Entertainment => ENT_SUFFIX,
        _ => VARIETY_SUFFIX,
    };
    for _ in 0..32 {
        let mut name = format!("{} {}", rng.pick(BRANDS), rng.pick(suffixes));
        let q = rng.below(100);
        if q < 15 {
            name.push_str(" HD");
        } else if q < 21 {
            name.push_str(" FHD");
        } else if q < 24 {
            name.push_str(" 4K");
        }
        if !used.contains(&name) {
            used.insert(name.clone());
            return name;
        }
    }
    unique(used, format!("{} {}", rng.pick(BRANDS), rng.pick(suffixes)))
}

impl Catalogue {
    /// Generate the catalogue for `seed`; `huge` pads live channels to [`HUGE_LIVE_COUNT`].
    pub fn generate(seed: u64, huge: bool) -> Self {
        let mut rng = Rng::new(mix(seed, hash_str("catalogue")));

        let live_categories = LIVE_CATS
            .iter()
            .map(|c| Category {
                id: c.id,
                name: c.name.to_owned(),
                parent_id: 0,
            })
            .collect();
        let vod_categories = VOD_CATS
            .iter()
            .map(|c| Category {
                id: c.id,
                name: c.name.to_owned(),
                parent_id: 0,
            })
            .collect();
        let series_categories = SERIES_CATS
            .iter()
            .map(|c| Category {
                id: c.id,
                name: c.name.to_owned(),
                parent_id: 0,
            })
            .collect();

        let live = Self::gen_live(&mut rng, huge);
        let movies = Self::gen_movies(&mut rng);
        let series = Self::gen_series(&mut rng);

        let mut episodes = HashMap::new();
        for (si, s) in series.iter().enumerate() {
            for (ni, season) in s.seasons.iter().enumerate() {
                for (ei, ep) in season.episodes.iter().enumerate() {
                    episodes.insert(ep.id, (si, ni, ei));
                }
            }
        }

        Self {
            live_categories,
            vod_categories,
            series_categories,
            live,
            movies,
            series,
            episodes,
        }
    }

    fn gen_live(rng: &mut Rng, huge: bool) -> Vec<LiveChannel> {
        let mut used_names = HashSet::new();
        let mut used_epg = HashSet::new();
        let mut out = Vec::with_capacity(if huge { HUGE_LIVE_COUNT } else { 256 });
        for def in LIVE_CATS {
            for idx in 0..def.count {
                let num = out.len() as u32 + 1;
                let stream_id = LIVE_ID_BASE + u64::from(num);
                let name = channel_name(def.theme, idx, rng, &mut used_names);
                let epg_channel_id = if rng.chance(8) {
                    None
                } else {
                    let base = slug(&name);
                    let base = if base.len() < 3 {
                        format!("ch{stream_id}")
                    } else {
                        base
                    };
                    let mut id = format!("{base}.fp");
                    if !used_epg.insert(id.clone()) {
                        id = format!("{base}.{stream_id}.fp");
                        used_epg.insert(id.clone());
                    }
                    Some(id)
                };
                let icon = match rng.below(100) {
                    0..=5 => Icon::Empty,
                    6..=9 => Icon::Broken,
                    _ => Icon::Generated,
                };
                let tv_archive = def.theme != Theme::Adult && rng.chance(30);
                let tv_archive_duration = if tv_archive {
                    *rng.pick(&[1u32, 2, 3, 3, 5, 7])
                } else {
                    0
                };
                out.push(LiveChannel {
                    num,
                    stream_id,
                    name,
                    category_id: def.id,
                    epg_channel_id,
                    icon,
                    tv_archive,
                    tv_archive_duration,
                    added: added(rng),
                    is_adult: def.theme == Theme::Adult,
                    clip: (stream_id % LIVE_CLIPS as u64) as usize,
                    theme: def.theme,
                });
            }
        }
        if huge {
            let fillable: Vec<&CatDef> = LIVE_CATS
                .iter()
                .filter(|c| c.count > 0 && c.theme != Theme::Adult)
                .collect();
            let mut i = 0usize;
            while out.len() < HUGE_LIVE_COUNT {
                let num = out.len() as u32 + 1;
                let stream_id = LIVE_ID_BASE + u64::from(num);
                let def = fillable[i % fillable.len()];
                let brand = BRANDS[i % BRANDS.len()];
                out.push(LiveChannel {
                    num,
                    stream_id,
                    name: format!("{brand} Bulk {num:05}"),
                    category_id: def.id,
                    epg_channel_id: None,
                    icon: Icon::Generated,
                    tv_archive: false,
                    tv_archive_duration: 0,
                    added: ADDED_BASE + i64::from(num) * 37,
                    is_adult: false,
                    clip: (stream_id % LIVE_CLIPS as u64) as usize,
                    theme: def.theme,
                });
                i += 1;
            }
        }
        out
    }

    fn gen_movies(rng: &mut Rng) -> Vec<Movie> {
        let mut used = HashSet::new();
        let mut out = Vec::with_capacity(150);
        let mut special = 0usize;
        for def in VOD_CATS {
            for idx in 0..def.count {
                let num = out.len() as u32 + 1;
                let title = if def.id == 108 {
                    GREEK_MOVIES[idx % GREEK_MOVIES.len()].to_owned()
                } else if idx == 3 && special < MOVIE_SPECIALS.len() {
                    special += 1;
                    MOVIE_SPECIALS[special - 1].to_owned()
                } else {
                    let adj = rng.pick(MOVIE_ADJ);
                    let noun = rng.pick(MOVIE_NOUN);
                    let noun2 = rng.pick(MOVIE_NOUN);
                    match rng.below(5) {
                        0 => format!("The {adj} {noun}"),
                        1 => format!("{noun} of the {adj} {noun2}"),
                        2 => format!("{adj} {noun}"),
                        3 => format!("Beyond the {adj} {noun}"),
                        _ => format!("The {noun} {}", rng.pick(&["II", "III", "Returns"])),
                    }
                };
                let title = unique(&mut used, title);
                let year = 1968 + rng.below(59) as i32;
                let name = if rng.chance(85) {
                    format!("{title} ({year})")
                } else {
                    title.clone()
                };
                let rating = if rng.chance(5) {
                    0.0
                } else {
                    round1(2.0 + rng.below(77) as f64 / 10.0)
                };
                let container_extension = match rng.below(100) {
                    0..=59 => "mkv",
                    60..=94 => "mp4",
                    _ => "avi",
                };
                let cast_n = rng.range(2, 4) as usize;
                out.push(Movie {
                    num,
                    stream_id: MOVIE_ID_BASE + u64::from(num),
                    name,
                    title,
                    year,
                    category_id: def.id,
                    rating,
                    added: added(rng),
                    container_extension,
                    plot: plot(rng),
                    cast: people(rng, cast_n),
                    director: person(rng),
                    genre: def.genre.to_owned(),
                    release_date: date_string(year, rng),
                    duration_secs: rng.range(80, 165) as u32 * 60 + rng.below(60) as u32,
                    tmdb_id: 99_000_000 + u64::from(num),
                    has_backdrop: rng.chance(90),
                    is_adult: def.id == 109,
                });
            }
        }
        out
    }

    fn gen_series(rng: &mut Rng) -> Vec<Series> {
        let mut used = HashSet::new();
        let mut out: Vec<Series> = Vec::with_capacity(30);
        let mut next_episode = EPISODE_ID_BASE + 1;
        for def in SERIES_CATS {
            for idx in 0..def.count {
                let num = out.len() as u32 + 1;
                let name = if def.id == 206 {
                    ANIME_SERIES[idx % ANIME_SERIES.len()].to_owned()
                } else if rng.below(3) == 0 {
                    format!("The {} {}", rng.pick(BRANDS), rng.pick(SERIES_NOUN))
                } else {
                    format!("{} {}", rng.pick(BRANDS), rng.pick(SERIES_NOUN))
                };
                let name = unique(&mut used, name);
                let year = 1995 + rng.below(31) as i32;
                let season_numbers: Vec<u32> = match num {
                    2 => vec![0, 1, 2],
                    5 => vec![1, 2, 4],
                    _ => (1..=rng.range(1, 4) as u32).collect(),
                };
                let default_ext = if rng.chance(70) { "mkv" } else { "mp4" };
                let run_time = *rng.pick(&[22u32, 25, 30, 42, 45, 50, 58]);
                let mut seasons = Vec::with_capacity(season_numbers.len());
                for sn in season_numbers {
                    let n_eps = rng.range(4, 10) as u32;
                    let season_year = year + sn.saturating_sub(1) as i32;
                    let mut episodes = Vec::with_capacity(n_eps as usize);
                    for e in 1..=n_eps {
                        let ep_name = if sn <= 1 && e == 1 {
                            "Pilot"
                        } else {
                            rng.pick(EPISODE_TITLES)
                        };
                        let ext = if rng.chance(10) {
                            if default_ext == "mkv" { "mp4" } else { "mkv" }
                        } else {
                            default_ext
                        };
                        episodes.push(Episode {
                            id: next_episode,
                            season: sn,
                            episode_num: e,
                            title: format!("{name} - S{sn:02}E{e:02} - {ep_name}"),
                            container_extension: ext,
                            plot: plot(rng),
                            duration_secs: run_time * 60 + rng.below(120) as u32,
                            rating: round1(5.0 + rng.below(46) as f64 / 10.0),
                            release_date: date_string(season_year, rng),
                            added: added(rng),
                        });
                        next_episode += 1;
                    }
                    seasons.push(Season {
                        number: sn,
                        air_date: date_string(season_year, rng),
                        episodes,
                    });
                }
                let cast_n = rng.range(3, 5) as usize;
                out.push(Series {
                    num,
                    series_id: u64::from(num),
                    name,
                    category_id: def.id,
                    plot: plot(rng),
                    cast: people(rng, cast_n),
                    director: person(rng),
                    genre: def.genre.to_owned(),
                    release_date: date_string(year, rng),
                    rating: round1(4.0 + rng.below(56) as f64 / 10.0),
                    last_modified: added(rng),
                    episode_run_time: run_time,
                    has_backdrop: rng.chance(85),
                    seasons,
                });
            }
        }
        out
    }

    /// Live channel by stream id.
    pub fn live_channel(&self, stream_id: u64) -> Option<&LiveChannel> {
        let idx = usize::try_from(stream_id.checked_sub(LIVE_ID_BASE + 1)?).ok()?;
        self.live.get(idx).filter(|c| c.stream_id == stream_id)
    }

    /// Movie by stream id.
    pub fn movie(&self, stream_id: u64) -> Option<&Movie> {
        let idx = usize::try_from(stream_id.checked_sub(MOVIE_ID_BASE + 1)?).ok()?;
        self.movies.get(idx).filter(|m| m.stream_id == stream_id)
    }

    /// Series by id.
    pub fn series_by_id(&self, series_id: u64) -> Option<&Series> {
        let idx = usize::try_from(series_id.checked_sub(1)?).ok()?;
        self.series.get(idx).filter(|s| s.series_id == series_id)
    }

    /// Episode by id, with its series.
    pub fn episode(&self, id: u64) -> Option<(&Series, &Episode)> {
        let (si, ni, ei) = *self.episodes.get(&id)?;
        let s = self.series.get(si)?;
        let ep = s.seasons.get(ni)?.episodes.get(ei)?;
        Some((s, ep))
    }

    /// Name of a live category.
    pub fn live_category_name(&self, id: u32) -> &str {
        find_name(&self.live_categories, id)
    }

    /// Name of a VOD category.
    pub fn vod_category_name(&self, id: u32) -> &str {
        find_name(&self.vod_categories, id)
    }

    /// Name of a series category.
    pub fn series_category_name(&self, id: u32) -> &str {
        find_name(&self.series_categories, id)
    }

    /// Every episode, in series/season order.
    pub fn episodes(&self) -> impl Iterator<Item = (&Series, &Episode)> {
        self.series.iter().flat_map(|s| {
            s.seasons
                .iter()
                .flat_map(move |season| season.episodes.iter().map(move |e| (s, e)))
        })
    }
}

fn find_name(cats: &[Category], id: u32) -> &str {
    cats.iter()
        .find(|c| c.id == id)
        .map_or("", |c| c.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_determinism() {
        let a = Catalogue::generate(7, false);
        let b = Catalogue::generate(7, false);
        assert_eq!(a.live.len(), 250);
        assert_eq!(a.movies.len(), 150);
        assert_eq!(a.series.len(), 30);
        assert_eq!(a.live_categories.len(), 14);
        for (x, y) in a.live.iter().zip(&b.live) {
            assert_eq!(x.name, y.name);
            assert_eq!(x.epg_channel_id, y.epg_channel_id);
        }
        let archive = a.live.iter().filter(|c| c.tv_archive).count();
        assert!((40..=110).contains(&archive), "archive count {archive}");
        assert!(a.live_channel(1001).is_some());
        assert!(a.live_channel(1251).is_none());
        assert!(a.movie(200_001).is_some());
        let (_, ep) = a.episode(300_001).unwrap_or_else(|| unreachable!());
        assert_eq!(ep.episode_num, 1);
    }

    #[test]
    fn huge() {
        let c = Catalogue::generate(7, true);
        assert_eq!(c.live.len(), HUGE_LIVE_COUNT);
        assert!(
            c.live_channel(LIVE_ID_BASE + HUGE_LIVE_COUNT as u64)
                .is_some()
        );
    }
}

//! `player_api.php`: login, categories, stream lists, VOD/series info and EPG.

use axum::extract::{Query, State};
use axum::response::Response;
use serde_json::{Map, Value, json};

use crate::catalogue::{ADDED_BASE, Category, Episode, Icon, LiveChannel, Movie, Series};
use crate::epg;
use crate::rng::hash_str;
use crate::server::{AppState, Params, State as PanelState, json_response};
use crate::shape::{Shape, hms};
use crate::time::{TIMEZONE, local_string, now};
use crate::users::{self, AccountKind, TestUser};

fn auth_failed() -> Value {
    json!({ "user_info": { "auth": 0 } })
}

/// `GET /player_api.php?username=&password=[&action=…]`.
pub(crate) async fn player_api(
    State(st): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let p = Params::from_pairs(pairs);
    let body = dispatch(&st, &p);
    json_response(&st, &body)
}

fn dispatch(st: &PanelState, p: &Params) -> Value {
    let Some(user) = users::find(p.get("username"), p.get("password")) else {
        return auth_failed();
    };
    if user.kind == AccountKind::Banned {
        return json!({ "user_info": { "auth": 0, "status": "Banned" } });
    }
    let action = p.get("action").trim();
    if action.is_empty() {
        return login(st, user);
    }
    if !user.kind.can_stream() {
        return auth_failed();
    }
    let cat = &st.catalogue;
    match action {
        "get_live_categories" => categories(st, &cat.live_categories),
        "get_vod_categories" => categories(st, &cat.vod_categories),
        "get_series_categories" => categories(st, &cat.series_categories),
        "get_live_streams" => {
            let f = CategoryFilter::new(st, p.opt("category_id"));
            let sh = st.shape();
            let items = cat
                .live
                .iter()
                .filter(|c| f.matches(c.category_id))
                .map(|c| live_item(st, &sh, c))
                .collect();
            list(st, items, malformed_live)
        }
        "get_vod_streams" => {
            let f = CategoryFilter::new(st, p.opt("category_id"));
            let sh = st.shape();
            let items = cat
                .movies
                .iter()
                .filter(|m| f.matches(m.category_id))
                .map(|m| vod_item(st, &sh, m))
                .collect();
            list(st, items, malformed_vod)
        }
        "get_series" => {
            let f = CategoryFilter::new(st, p.opt("category_id"));
            let sh = st.shape();
            let items = cat
                .series
                .iter()
                .filter(|s| f.matches(s.category_id))
                .map(|s| series_item(st, &sh, s))
                .collect();
            list(st, items, malformed_series)
        }
        "get_vod_info" => p
            .get("vod_id")
            .trim()
            .parse()
            .ok()
            .and_then(|id| cat.movie(id))
            .map_or_else(
                || json!({ "info": [], "movie_data": [] }),
                |m| vod_info(st, m),
            ),
        "get_series_info" => p
            .get("series_id")
            .trim()
            .parse()
            .ok()
            .and_then(|id| cat.series_by_id(id))
            .map_or_else(
                || json!({ "seasons": [], "info": [], "episodes": [] }),
                |s| series_info(st, s),
            ),
        "get_short_epg" => epg::short_epg(st, p.opt("stream_id"), p.opt("limit")),
        "get_simple_data_table" => epg::simple_data_table(st, p.opt("stream_id")),
        _ => json!([]),
    }
}

fn login(st: &PanelState, user: &TestUser) -> Value {
    let now = now();
    let today = now - now.rem_euclid(86_400);
    let key = hash_str(user.username);
    let sh = st.shape();
    let exp = match user.kind {
        AccountKind::Active => Some(today + 365 * 86_400),
        AccountKind::Trial => Some(today + 3 * 86_400),
        AccountKind::Expired => Some(today - 30 * 86_400),
        AccountKind::Disabled => Some(today + 90 * 86_400),
        AccountKind::Unlimited | AccountKind::Banned => None,
    };
    let trial = user.kind == AccountKind::Trial;
    json!({
        "user_info": {
            "username": user.username,
            "password": user.password,
            "message": "Welcome to fake-panel (synthetic test data)",
            "auth": sh.int(key, "auth", 1),
            "status": user.kind.status(),
            "exp_date": exp.map_or(Value::Null, |e| sh.numstr(key, "exp_date", e)),
            "is_trial": sh.numstr(key, "is_trial", i64::from(trial)),
            "active_cons": sh.numstr(key, "active_cons", 0),
            "created_at": sh.numstr(key, "created_at", ADDED_BASE - 90 * 86_400),
            "max_connections": sh.numstr(key, "max_connections", if trial { 1 } else { 2 }),
            "allowed_output_formats": ["m3u8", "ts", "rtmp"],
        },
        "server_info": {
            "url": st.stream_host,
            "port": sh.numstr(key, "port", i64::from(st.port)),
            "https_port": sh.numstr(key, "https_port", i64::from(st.port.wrapping_add(1))),
            "server_protocol": "http",
            "rtmp_port": "1935",
            "timezone": TIMEZONE,
            "timestamp_now": sh.int(key, "timestamp_now", now),
            "time_now": local_string(now),
            "process": true,
        },
    })
}

/// Category filter semantics (see [`crate::Quirks::category_all_literal`]).
enum CategoryFilter {
    All,
    Only(u32),
    Nothing,
}

impl CategoryFilter {
    fn new(st: &PanelState, raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") => Self::All,
            Some(s) => match s.parse::<u32>() {
                Ok(id) => Self::Only(id),
                Err(_) if st.quirks.category_all_literal => Self::All,
                Err(_) => Self::Nothing,
            },
        }
    }

    fn matches(&self, id: u32) -> bool {
        match self {
            Self::All => true,
            Self::Only(x) => *x == id,
            Self::Nothing => false,
        }
    }
}

fn list(st: &PanelState, mut items: Vec<Value>, malformed: fn() -> Value) -> Value {
    if st.quirks.malformed_item && !items.is_empty() {
        items.insert(items.len().min(2), malformed());
    }
    Value::Array(items)
}

fn categories(st: &PanelState, cats: &[Category]) -> Value {
    let sh = st.shape();
    let items = cats
        .iter()
        .map(|c| {
            let k = u64::from(c.id);
            json!({
                "category_id": sh.numstr(k, "category_id", i64::from(c.id)),
                "category_name": c.name,
                "parent_id": sh.int(k, "parent_id", i64::from(c.parent_id)),
            })
        })
        .collect();
    list(
        st,
        items,
        || json!({ "category_id": null, "category_name": 12345, "parent_id": "not-a-number" }),
    )
}

fn malformed_live() -> Value {
    json!({
        "num": "NaN", "name": null, "stream_type": "live", "stream_id": "not-an-id",
        "stream_icon": 12, "epg_channel_id": [], "added": {}, "category_id": "1",
        "tv_archive": "yes", "tv_archive_duration": -1,
    })
}

fn malformed_vod() -> Value {
    json!({
        "num": [], "name": ["Broken", "Movie"], "stream_type": "movie", "stream_id": null,
        "stream_icon": false, "rating": "ten", "rating_5based": "five", "added": "yesterday",
        "category_id": {}, "container_extension": 42,
    })
}

fn malformed_series() -> Value {
    json!({
        "num": null, "name": {}, "series_id": "x", "cover": 0, "backdrop_path": 7,
        "rating": [], "last_modified": "never", "category_id": true,
    })
}

pub(crate) fn logo_url(st: &PanelState, ch: &LiveChannel) -> String {
    match ch.icon {
        Icon::Empty => String::new(),
        Icon::Generated | Icon::Broken => {
            format!("{}/images/logo/{}.png", st.api_base, ch.stream_id)
        }
    }
}

pub(crate) fn poster_url(st: &PanelState, m: &Movie) -> String {
    format!("{}/images/poster/{}.png", st.api_base, m.stream_id)
}

pub(crate) fn cover_url(st: &PanelState, s: &Series) -> String {
    format!("{}/images/cover/{}.png", st.api_base, s.series_id)
}

fn backdrop(st: &PanelState, sh: &Shape, key: u64, url: Option<String>) -> Value {
    let v = if st.quirks.backdrop_string {
        Value::String(url.unwrap_or_default())
    } else {
        Value::Array(url.into_iter().map(Value::String).collect())
    };
    sh.nullable(key, "backdrop_path", 8, v)
}

fn live_item(st: &PanelState, sh: &Shape, ch: &LiveChannel) -> Value {
    let k = ch.stream_id;
    json!({
        "num": sh.int(k, "num", i64::from(ch.num)),
        "name": ch.name,
        "stream_type": "live",
        "stream_id": sh.int(k, "stream_id", k as i64),
        "stream_icon": sh.text(k, "stream_icon", &logo_url(st, ch)),
        "epg_channel_id": ch
            .epg_channel_id
            .as_deref()
            .map_or(Value::Null, |id| sh.text(k, "epg_channel_id", id)),
        "added": sh.numstr(k, "added", ch.added),
        "is_adult": sh.numstr(k, "is_adult", i64::from(ch.is_adult)),
        "category_id": sh.nullable(k, "category_id_null", 3,
            sh.numstr(k, "category_id", i64::from(ch.category_id))),
        "category_ids": [ch.category_id],
        "custom_sid": sh.text(k, "custom_sid", ""),
        "tv_archive": sh.int(k, "tv_archive", i64::from(ch.tv_archive)),
        "direct_source": "",
        "tv_archive_duration": sh.int(k, "tv_archive_duration", i64::from(ch.tv_archive_duration)),
    })
}

fn rating5(r: f64) -> f64 {
    (r / 2.0 * 10.0).round() / 10.0
}

fn vod_item(st: &PanelState, sh: &Shape, m: &Movie) -> Value {
    let k = m.stream_id;
    json!({
        "num": sh.int(k, "num", i64::from(m.num)),
        "name": m.name,
        "title": m.title,
        "year": m.year.to_string(),
        "stream_type": "movie",
        "stream_id": sh.int(k, "stream_id", k as i64),
        "stream_icon": sh.text(k, "stream_icon", &poster_url(st, m)),
        "rating": sh.rating(k, "rating", m.rating),
        "rating_5based": sh.float(k, "rating_5based", rating5(m.rating)),
        "added": sh.numstr(k, "added", m.added),
        "is_adult": sh.numstr(k, "is_adult", i64::from(m.is_adult)),
        "category_id": sh.nullable(k, "category_id_null", 3,
            sh.numstr(k, "category_id", i64::from(m.category_id))),
        "category_ids": [m.category_id],
        "container_extension": m.container_extension,
        "custom_sid": sh.text(k, "custom_sid", ""),
        "direct_source": "",
    })
}

fn movie_backdrop(st: &PanelState, m: &Movie) -> Option<String> {
    m.has_backdrop
        .then(|| format!("{}/images/backdrop/movie-{}.png", st.api_base, m.stream_id))
}

fn series_backdrop(st: &PanelState, s: &Series) -> Option<String> {
    s.has_backdrop
        .then(|| format!("{}/images/backdrop/series-{}.png", st.api_base, s.series_id))
}

fn vod_info(st: &PanelState, m: &Movie) -> Value {
    let sh = st.shape();
    let k = m.stream_id;
    let poster = poster_url(st, m);
    json!({
        "info": {
            "kinopoisk_url": "",
            "tmdb_id": sh.int(k, "tmdb_id", m.tmdb_id as i64),
            "name": m.title,
            "o_name": m.title,
            "cover_big": poster,
            "movie_image": sh.text(k, "movie_image", &poster),
            "releasedate": sh.text(k, "releasedate", &m.release_date),
            "episode_run_time": 0,
            "youtube_trailer": "",
            "director": sh.text(k, "director", &m.director),
            "actors": m.cast.join(", "),
            "cast": sh.text(k, "cast", &m.cast.join(", ")),
            "description": m.plot,
            "plot": sh.text(k, "plot", &m.plot),
            "age": "",
            "mpaa_rating": "",
            "rating_count_kinopoisk": 0,
            "country": "Elsewhere",
            "genre": sh.text(k, "genre", &m.genre),
            "backdrop_path": backdrop(st, &sh, k, movie_backdrop(st, m)),
            "duration_secs": sh.int(k, "duration_secs", i64::from(m.duration_secs)),
            "duration": hms(m.duration_secs),
            "video": { "index": 0, "codec_name": "h264", "width": 854, "height": 480 },
            "audio": { "index": 1, "codec_name": "aac", "channels": 2 },
            "bitrate": 800,
            "rating": sh.rating(k, "info_rating", m.rating),
        },
        "movie_data": {
            "stream_id": sh.int(k, "stream_id", k as i64),
            "name": m.name,
            "title": m.title,
            "year": m.year.to_string(),
            "added": sh.numstr(k, "added", m.added),
            "category_id": sh.numstr(k, "category_id", i64::from(m.category_id)),
            "category_ids": [m.category_id],
            "container_extension": m.container_extension,
            "custom_sid": "",
            "direct_source": "",
        },
    })
}

fn series_item(st: &PanelState, sh: &Shape, s: &Series) -> Value {
    let k = s.series_id;
    json!({
        "num": sh.int(k, "num", i64::from(s.num)),
        "name": s.name,
        "title": s.name,
        "year": s.release_date.get(..4).unwrap_or_default(),
        "series_id": sh.int(k, "series_id", k as i64),
        "cover": sh.text(k, "cover", &cover_url(st, s)),
        "plot": sh.text(k, "plot", &s.plot),
        "cast": sh.text(k, "cast", &s.cast.join(", ")),
        "director": sh.text(k, "director", &s.director),
        "genre": sh.text(k, "genre", &s.genre),
        "releaseDate": sh.text(k, "releaseDate", &s.release_date),
        "release_date": s.release_date,
        "last_modified": sh.numstr(k, "last_modified", s.last_modified),
        "rating": sh.rating(k, "rating", s.rating),
        "rating_5based": sh.float(k, "rating_5based", rating5(s.rating)),
        "backdrop_path": backdrop(st, sh, k, series_backdrop(st, s)),
        "youtube_trailer": "",
        "episode_run_time": sh.numstr(k, "episode_run_time", i64::from(s.episode_run_time)),
        "category_id": sh.nullable(k, "category_id_null", 3,
            sh.numstr(k, "category_id", i64::from(s.category_id))),
        "category_ids": [s.category_id],
    })
}

fn episode_json(st: &PanelState, sh: &Shape, e: &Episode) -> Value {
    let k = e.id;
    json!({
        "id": sh.numstr(k, "id", k as i64),
        "episode_num": sh.int(k, "episode_num", i64::from(e.episode_num)),
        "title": e.title,
        "container_extension": e.container_extension,
        "info": {
            "releasedate": e.release_date,
            "plot": sh.text(k, "plot", &e.plot),
            "duration_secs": sh.int(k, "duration_secs", i64::from(e.duration_secs)),
            "duration": hms(e.duration_secs),
            "movie_image": sh.text(k, "movie_image",
                &format!("{}/images/episode/{}.png", st.api_base, e.id)),
            "rating": sh.float(k, "rating", e.rating),
            "season": e.season,
        },
        "custom_sid": "",
        "added": sh.numstr(k, "added", e.added),
        "season": sh.int(k, "season", i64::from(e.season)),
        "direct_source": "",
    })
}

fn series_info(st: &PanelState, s: &Series) -> Value {
    let sh = st.shape();
    let k = s.series_id;
    let cover = cover_url(st, s);
    let seasons: Vec<Value> = s
        .seasons
        .iter()
        .map(|season| {
            json!({
                "air_date": season.air_date,
                "episode_count": season.episodes.len(),
                "id": k * 100 + u64::from(season.number),
                "name": if season.number == 0 {
                    "Specials".to_owned()
                } else {
                    format!("Season {}", season.number)
                },
                "overview": "",
                "season_number": season.number,
                "vote_average": 0,
                "cover": cover,
                "cover_big": cover,
            })
        })
        .collect();
    let episodes = if st.quirks.episodes_array && s.series_id.is_multiple_of(3) {
        Value::Array(
            s.seasons
                .iter()
                .map(|season| {
                    Value::Array(
                        season
                            .episodes
                            .iter()
                            .map(|e| episode_json(st, &sh, e))
                            .collect(),
                    )
                })
                .collect(),
        )
    } else {
        let mut map = Map::new();
        for season in &s.seasons {
            map.insert(
                season.number.to_string(),
                Value::Array(
                    season
                        .episodes
                        .iter()
                        .map(|e| episode_json(st, &sh, e))
                        .collect(),
                ),
            );
        }
        Value::Object(map)
    };
    let mut info = series_item(st, &sh, s);
    if let Value::Object(m) = &mut info {
        m.remove("num");
        m.remove("series_id");
    }
    json!({ "seasons": seasons, "info": info, "episodes": episodes })
}

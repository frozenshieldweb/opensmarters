//! EPG: a deterministic schedule generated relative to *now*, served as Xtream
//! `get_short_epg` / `get_simple_data_table` listings and as XMLTV.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Write as _;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};

use crate::catalogue::{Icon, LiveChannel};
use crate::rng::{hash_str, mix, mix3};
use crate::server::{AppState, Params, State as PanelState};
use crate::shape::Shape;
use crate::time::{local_string, xmltv_string};
use crate::users;

const DAY: i64 = 86_400;
const LENGTHS_MIN: &[i64] = &[15, 30, 30, 45, 60, 60, 60, 90, 120];

/// How far back `get_simple_data_table` and XMLTV go.
pub(crate) const PAST_DAYS: i64 = 3;

/// One programme.
#[derive(Debug, Clone)]
pub(crate) struct Programme {
    pub(crate) id: u64,
    pub(crate) start: i64,
    pub(crate) stop: i64,
    pub(crate) title: String,
    pub(crate) description: String,
}

/// Programmes of `ch` overlapping `[from, to)`. Programme boundaries depend only on the
/// absolute time, so repeated calls agree with each other.
pub(crate) fn schedule(seed: u64, ch: &LiveChannel, from: i64, to: i64) -> Vec<Programme> {
    let Some(epg_id) = ch.epg_channel_id.as_deref() else {
        return Vec::new();
    };
    let key = mix(seed, hash_str(epg_id));
    let titles = ch.theme.epg_titles();
    let name = ch.name.trim();
    let mut out = Vec::new();
    for day in from.div_euclid(DAY)..=to.div_euclid(DAY) {
        let day_start = day * DAY;
        let day_end = day_start + DAY;
        let mut t = day_start;
        let mut k: u64 = 0;
        while t < day_end {
            let h = mix3(key, day as u64, k);
            let len = LENGTHS_MIN[(h % LENGTHS_MIN.len() as u64) as usize] * 60;
            let stop = (t + len).min(day_end);
            if stop > from && t < to {
                let base = titles[((h >> 16) % titles.len() as u64) as usize];
                let title = match (h >> 40) % 7 {
                    0 => format!("{base} (Live)"),
                    1 => format!("{base} — Repeat"),
                    _ => base.to_owned(),
                };
                let description = format!(
                    "{title} — a synthetic programme on {name}. Segment {} of the day's schedule; \
                     café-style chat and naïve plots, all invented for testing.",
                    k + 1
                );
                out.push(Programme {
                    id: 100_000_000 + (h >> 24) % 900_000_000,
                    start: t,
                    stop,
                    title,
                    description,
                });
            }
            t = stop;
            k += 1;
        }
    }
    out
}

fn has_archive(ch: &LiveChannel, p: &Programme, now: i64) -> bool {
    ch.tv_archive && p.stop <= now && p.start >= now - i64::from(ch.tv_archive_duration) * DAY
}

fn listing(sh: &Shape, ch: &LiveChannel, p: &Programme, now: i64) -> Value {
    let k = p.id;
    let now_playing = i64::from(p.start <= now && now < p.stop);
    json!({
        "id": sh.numstr(k, "id", p.id as i64),
        "epg_id": sh.numstr(k, "epg_id", 1),
        "title": B64.encode(p.title.as_bytes()),
        "lang": ch.theme.lang(),
        "start": local_string(p.start),
        "end": local_string(p.stop),
        "description": B64.encode(p.description.as_bytes()),
        "channel_id": ch.epg_channel_id.as_deref().unwrap_or_default(),
        "start_timestamp": sh.numstr(k, "start_timestamp", p.start),
        "stop_timestamp": sh.numstr(k, "stop_timestamp", p.stop),
        "now_playing": sh.int(k, "now_playing", now_playing),
        "has_archive": sh.int(k, "has_archive", i64::from(has_archive(ch, p, now))),
    })
}

fn malformed_listing(ch: &LiveChannel, now: i64) -> Value {
    json!({
        "id": "malformed",
        "epg_id": null,
        "title": "%%% this is not base64 %%%",
        "lang": 42,
        "start": "not a date",
        "end": local_string(now + 30 * DAY),
        "description": null,
        "channel_id": ch.epg_channel_id.as_deref().unwrap_or_default(),
        "start_timestamp": "soon",
        "stop_timestamp": {},
        "now_playing": "maybe",
        "has_archive": [],
    })
}

/// `get_short_epg`: the current programme first, then the next ones (default 4).
pub(crate) fn short_epg(st: &PanelState, stream_id: Option<&str>, limit: Option<&str>) -> Value {
    let now = crate::time::now();
    let limit = limit
        .and_then(|l| l.trim().parse::<usize>().ok())
        .filter(|l| *l > 0)
        .unwrap_or(4)
        .min(500);
    let Some(ch) = stream_id
        .and_then(|s| s.trim().parse().ok())
        .and_then(|id| st.catalogue.live_channel(id))
    else {
        return json!({ "epg_listings": [] });
    };
    let sh = st.shape();
    let mut listings: Vec<Value> = schedule(st.seed, ch, now, now + 2 * DAY)
        .iter()
        .take(limit)
        .map(|p| listing(&sh, ch, p, now))
        .collect();
    if st.quirks.malformed_item && !listings.is_empty() {
        listings.push(malformed_listing(ch, now));
    }
    json!({ "epg_listings": listings })
}

/// `get_simple_data_table`: the past [`PAST_DAYS`] days plus the next day.
pub(crate) fn simple_data_table(st: &PanelState, stream_id: Option<&str>) -> Value {
    let now = crate::time::now();
    let Some(ch) = stream_id
        .and_then(|s| s.trim().parse().ok())
        .and_then(|id| st.catalogue.live_channel(id))
    else {
        return json!({ "epg_listings": [] });
    };
    let sh = st.shape();
    let mut listings: Vec<Value> = schedule(st.seed, ch, now - PAST_DAYS * DAY, now + DAY)
        .iter()
        .map(|p| listing(&sh, ch, p, now))
        .collect();
    if st.quirks.malformed_item && !listings.is_empty() {
        listings.push(malformed_listing(ch, now));
    }
    json!({ "epg_listings": listings })
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Full XMLTV document: every channel with an EPG id, programmes from 3 days ago to
/// 2 days ahead, times in server-local time with explicit `+HHMM` offsets.
pub(crate) fn build_xmltv(st: &PanelState, now: i64) -> String {
    let from = now - PAST_DAYS * DAY;
    let to = now + 2 * DAY;
    let mut seen = HashSet::new();
    let channels: Vec<&LiveChannel> = st
        .catalogue
        .live
        .iter()
        .filter(|c| {
            c.epg_channel_id
                .as_ref()
                .is_some_and(|id| seen.insert(id.clone()))
        })
        .collect();
    let mut out = String::with_capacity(channels.len() * 40_000);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE tv SYSTEM \"xmltv.dtd\">\n");
    out.push_str(
        "<tv generator-info-name=\"fake-panel\" source-info-name=\"fake-panel synthetic EPG\">\n",
    );
    for c in &channels {
        let id = xml_escape(c.epg_channel_id.as_deref().unwrap_or_default());
        let _ = writeln!(out, "  <channel id=\"{id}\">");
        let _ = writeln!(
            out,
            "    <display-name lang=\"{}\">{}</display-name>",
            c.theme.lang(),
            xml_escape(c.name.trim())
        );
        if c.icon == Icon::Generated {
            let _ = writeln!(
                out,
                "    <icon src=\"{}/images/logo/{}.png\" />",
                st.api_base, c.stream_id
            );
        }
        out.push_str("  </channel>\n");
    }
    for c in &channels {
        let id = xml_escape(c.epg_channel_id.as_deref().unwrap_or_default());
        let lang = c.theme.lang();
        for p in schedule(st.seed, c, from, to) {
            let _ = writeln!(
                out,
                "  <programme start=\"{}\" stop=\"{}\" channel=\"{id}\">",
                xmltv_string(p.start),
                xmltv_string(p.stop)
            );
            let _ = writeln!(
                out,
                "    <title lang=\"{lang}\">{}</title>",
                xml_escape(&p.title)
            );
            let _ = writeln!(
                out,
                "    <desc lang=\"{lang}\">{}</desc>",
                xml_escape(&p.description)
            );
            let _ = writeln!(
                out,
                "    <category lang=\"en\">{}</category>",
                c.theme.genre()
            );
            out.push_str("  </programme>\n");
        }
    }
    out.push_str("</tv>\n");
    out
}

/// `GET /xmltv.php?username=&password=` (credentials required).
pub(crate) async fn xmltv_php(
    State(st): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let p = Params::from_pairs(pairs);
    if users::find_streamable(p.get("username"), p.get("password")).is_none() {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }
    let st2 = st.clone();
    match tokio::task::spawn_blocking(move || build_xmltv(&st2, crate::time::now())).await {
        Ok(xml) => (
            [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
            xml,
        )
            .into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "xmltv generation failed").into_response(),
    }
}

/// `GET /xmltv.xml.gz`: the same document gzipped (served as a file, not as
/// `Content-Encoding`). Credentials are optional, but rejected if present and wrong.
pub(crate) async fn xmltv_gz(
    State(st): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let p = Params::from_pairs(pairs);
    if (p.has("username") || p.has("password"))
        && users::find_streamable(p.get("username"), p.get("password")).is_none()
    {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }
    let st2 = st.clone();
    let gz = tokio::task::spawn_blocking(move || {
        let xml = build_xmltv(&st2, crate::time::now());
        let mut enc = GzEncoder::new(Vec::with_capacity(xml.len() / 6), Compression::fast());
        enc.write_all(xml.as_bytes())?;
        enc.finish()
    })
    .await;
    match gz {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, "application/gzip"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"xmltv.xml.gz\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "xmltv generation failed").into_response(),
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Property-based robustness tests: every parser must survive arbitrary provider input
//! without panicking, and a few invariants must hold. Kept small enough for normal
//! `cargo test` runs; the `fuzz/` targets go further.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;

use base64::Engine;
use chrono::{DateTime, Utc};
use opensmarters_core::catalog::{self, CatalogItem, SortOrder};
use opensmarters_core::de::{self, parse_list, parse_object};
use opensmarters_core::epg::{self, ServerTimeZone};
use opensmarters_core::model::{archive_programmes, parse_epg_listings};
use opensmarters_core::parental::KeywordMatcher;
use opensmarters_core::redact::{Redactor, redact_url};
use opensmarters_core::secrecy::SecretString;
use opensmarters_core::urls::{LiveContainer, StreamUrls, TimeshiftFormat};
use opensmarters_core::{
    Category, LiveStream, LoginInfo, Series, SeriesInfo, ServerInfo, ServerUrl, VodInfo, VodStream,
    XtreamAccount, m3u, xmltv,
};
use proptest::prelude::*;
use serde_json::Value;

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 1024,
        ..ProptestConfig::default()
    }
}

// ---------------------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------------------

const KEYS: &[&str] = &[
    "stream_id",
    "series_id",
    "category_id",
    "category_ids",
    "category_name",
    "parent_id",
    "name",
    "num",
    "added",
    "rating",
    "rating_5based",
    "backdrop_path",
    "episodes",
    "info",
    "seasons",
    "movie_data",
    "user_info",
    "server_info",
    "auth",
    "status",
    "exp_date",
    "is_trial",
    "max_connections",
    "allowed_output_formats",
    "epg_listings",
    "title",
    "description",
    "start",
    "end",
    "stop",
    "start_timestamp",
    "stop_timestamp",
    "has_archive",
    "now_playing",
    "id",
    "season",
    "episode_num",
    "container_extension",
    "port",
    "https_port",
    "server_protocol",
    "url",
    "timezone",
    "timestamp_now",
    "time_now",
    "tv_archive",
    "releaseDate",
    "release_date",
    "plot",
    "description",
    "0",
    "1",
    "2",
    "Season 1",
];

fn json_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_filter_map("finite", |f| serde_json::Number::from_f64(f)
            .map(Value::Number)),
        "[0-9]{0,12}".prop_map(Value::String),
        "-?[0-9]{1,6}\\.[0-9]{0,3}".prop_map(Value::String),
        "[ -~]{0,16}".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        prop::sample::select(vec![
            "",
            "N/A",
            "Active",
            "Expired",
            "1",
            "0",
            "true",
            "null",
            "2024-01-01 07:00:00",
            "Europe/Paris",
            "https",
            "TW9ybmluZyBOZXdz"
        ])
        .prop_map(|s| Value::String(s.to_owned())),
    ]
}

fn json_value() -> impl Strategy<Value = Value> {
    json_scalar().prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec(
                (
                    prop_oneof![
                        prop::sample::select(KEYS).prop_map(str::to_owned),
                        "[a-z_]{0,6}"
                    ],
                    inner
                ),
                0..8
            )
            .prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

fn exercise_json(body: &[u8]) {
    let account = XtreamAccount::new("panel.example.com:8080", "u").unwrap();
    let pw = || SecretString::from("p".to_owned());
    let now = Utc::now();
    let paris = ServerTimeZone::from_name("Europe/Paris").unwrap();

    if let Ok(l) = parse_list::<LiveStream>(body) {
        let _ = catalog::sorted_indices(&l.items, SortOrder::NameAsc);
        let _ = catalog::sorted_indices(&l.items, SortOrder::RecentlyAdded);
    }
    if let Ok(l) = parse_list::<VodStream>(body) {
        let _ = catalog::sorted_indices(&l.items, SortOrder::TopRated);
        for v in &l.items {
            let _ = v.rating_out_of_5();
        }
    }
    if let Ok(l) = parse_list::<Series>(body) {
        let _ = catalog::sorted_indices(&l.items, SortOrder::NumberDesc);
    }
    let _ = parse_list::<Category>(body);
    let _ = parse_object::<VodInfo>(body);
    if let Ok(s) = parse_object::<SeriesInfo>(body) {
        let _ = s.episode_count();
    }
    if let Ok(login) = LoginInfo::from_json(body) {
        let _ = login.check(now);
        let urls = StreamUrls::new(&login, &account, pw());
        let _ = urls.live(
            1,
            LiveContainer::preferred(LiveContainer::Ts, &login.user_info),
        );
        let _ = urls.movie(1, None);
        let _ = login.time_zone().to_utc(now.naive_utc());
    }
    for tz in [ServerTimeZone::Utc, paris] {
        if let Ok(l) = parse_epg_listings(body, &tz) {
            let urls = StreamUrls::new(&LoginInfo::default(), &account, pw());
            for p in archive_programmes(&l.items, &tz, now) {
                let _ = urls.timeshift(1, &p, TimeshiftFormat::Standard, LiveContainer::Ts);
                let _ = urls.timeshift(1, &p, TimeshiftFormat::SmartersCompat, LiveContainer::M3u8);
                let _ = p.server_date();
            }
        }
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn json_models_never_panic_on_structured_input(v in json_value()) {
        let body = serde_json::to_vec(&v).unwrap();
        exercise_json(&body);
        // Lists: every array element is either kept or counted as skipped.
        if let Value::Array(items) = &v {
            let l = parse_list::<LiveStream>(&body).unwrap();
            prop_assert_eq!(l.items.len() + l.skipped, items.len());
            prop_assert!(l.errors.len() <= de::MAX_ITEM_ERRORS);
        }
    }

    #[test]
    fn json_models_never_panic_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        exercise_json(&bytes);
    }

    #[test]
    fn list_items_round_trip(v in prop::collection::vec(json_value(), 0..6)) {
        // Whatever parses must serialise and parse back to the same value.
        let body = serde_json::to_vec(&Value::Array(v)).unwrap();
        let l = parse_list::<LiveStream>(&body).unwrap();
        let again = parse_list::<LiveStream>(&serde_json::to_vec(&l.items).unwrap()).unwrap();
        prop_assert_eq!(again.items, l.items);
        let s = parse_list::<Series>(&body).unwrap();
        let again = parse_list::<Series>(&serde_json::to_vec(&s.items).unwrap()).unwrap();
        prop_assert_eq!(again.items, s.items);
    }
}

// ---------------------------------------------------------------------------------------
// M3U
// ---------------------------------------------------------------------------------------

fn m3u_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("#EXTM3U".to_owned()),
        "#EXTM3U( (url-tvg|x-tvg-url|tvg-shift)=\"[^\"\r\n]{0,20}\"){0,3}",
        "#EXTINF:-?[0-9]{1,3}( [a-zA-Z-]{1,12}=(\"[^\"\r\n]{0,20}\"|'[^'\r\n]{0,10}'|[^ \",\r\n]{0,10})){0,4},[^\r\n]{0,20}",
        "#EXTINF[^\r\n]{0,40}",
        "#EXTGRP:[^\r\n]{0,10}",
        "#EXTVLCOPT:http-(user-agent|referrer|referer)=[^\r\n]{0,10}",
        "#KODIPROP:[^\r\n]{0,10}",
        "(http|https|rtmp|rtsp|udp|file)://[a-z]{1,8}(/[a-zA-Z0-9._%-]{0,8}){0,4}(\\?[a-z]=[0-9])?",
        "[^\r\n]{0,30}",
        Just(String::new()),
    ]
}

fn m3u_doc() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(m3u_line(), 0..40),
        prop::sample::select(vec!["\n", "\r\n", "\r"]),
        any::<bool>(),
    )
        .prop_map(|(lines, sep, bom)| {
            let body = lines.join(sep);
            if bom { format!("\u{FEFF}{body}") } else { body }
        })
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn m3u_never_panics_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = m3u::parse(&bytes, &m3u::M3uLimits::default());
        let _ = m3u::detect_xtream(&String::from_utf8_lossy(&bytes));
    }

    #[test]
    fn m3u_structured_invariants(doc in m3u_doc()) {
        let limits = m3u::M3uLimits::default();
        if let Ok(pl) = m3u::parse(doc.as_bytes(), &limits) {
            let lines: Vec<&str> = doc
                .split(['\r', '\n'])
                .map(|l| l.trim_start_matches('\u{FEFF}').trim())
                .collect();
            for e in &pl.entries {
                prop_assert!(!e.url.is_empty());
                prop_assert!(!e.url.starts_with('#'));
                prop_assert_eq!(e.url.trim(), e.url.as_str());
                prop_assert!(!e.title.contains(['\r', '\n']));
                // Never inherited: an URL cannot be used by more entries than it has lines.
                let used = pl.entries.iter().filter(|x| x.url == e.url).count();
                let present = lines.iter().filter(|l| **l == e.url).count();
                prop_assert!(used <= present, "url {:?} used {} times, present {}", e.url, used, present);
            }
            let _ = pl.categories();
            let _ = catalog::sorted_indices(&pl.entries, SortOrder::NameAsc);
            let _ = format!("{pl:?}");
        }
    }

    #[test]
    fn m3u_limits_hold(doc in m3u_doc(), max_entries in 0usize..5, max_line in 1usize..64) {
        let limits = m3u::M3uLimits { max_total_bytes: 4096, max_line_len: max_line, max_entries, ..m3u::M3uLimits::default() };
        if let Ok(pl) = m3u::parse(doc.as_bytes(), &limits) {
            prop_assert!(pl.entries.len() <= max_entries);
        }
    }
}

// ---------------------------------------------------------------------------------------
// XMLTV
// ---------------------------------------------------------------------------------------

fn xml_fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "<tv>", "</tv>", "<?xml version=\"1.0\"?>", "<!DOCTYPE tv [<!ENTITY a \"&b;\">]>",
            "<channel id=\"c1\">", "<channel id=\"C1\"/>", "</channel>", "<display-name lang=\"en\">",
            "</display-name>", "<icon src=\"https://img.example.org/x.png\"/>", "<url>", "</url>",
            "<programme channel=\"c1\" start=\"20240101000000 +0100\" stop=\"20240101010000 +0100\">",
            "<programme channel=\"c1\" start=\"20240101003000\">", "<programme channel=\"c2\" start=\"bad\"/>",
            "<programme start=\"20240101000000\">", "</programme>", "<title>", "<title lang=\"fr\">", "</title>",
            "<desc>", "</desc>", "<sub-title>", "</sub-title>", "<category>", "</category>",
            "<episode-num system=\"xmltv_ns\">", "</episode-num>", "&amp;", "&lt;", "&#x41;", "&#0;",
            "&#xFFFFFFFF;", "&bogus;", "&", "<![CDATA[x<y]]>", "<!-- c -->", "<", ">", "\"", "</b>", "<b>",
        ]).prop_map(str::to_owned),
        "[^<&]{0,12}",
        "[0-9]{8,14}( [+-][0-9]{2}:?[0-9]{2})?",
    ]
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn xmltv_never_panics_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let limits = xmltv::XmltvLimits::default();
        let _ = xmltv::parse_bytes(&bytes, &limits);
        let mut gz = vec![0x1f, 0x8b];
        gz.extend_from_slice(&bytes);
        let _ = xmltv::parse_bytes(&gz, &limits);
    }

    #[test]
    fn xmltv_structured_invariants(frags in prop::collection::vec(xml_fragment(), 0..60), compress in any::<bool>()) {
        let doc = frags.concat();
        let input = if compress { gzip(doc.as_bytes()) } else { doc.into_bytes() };
        let limits = xmltv::XmltvLimits { max_text_len: 32, ..xmltv::XmltvLimits::default() };
        if let Ok(epg) = xmltv::parse_bytes(&input, &limits) {
            for (channel, progs) in &epg.programmes {
                prop_assert!(progs.windows(2).all(|w| w[0].start <= w[1].start));
                for p in progs {
                    prop_assert_eq!(&p.channel, channel);
                    prop_assert!(p.stop.is_none_or(|s| s >= p.start));
                    prop_assert!(p.titles.iter().all(|t| t.text.len() <= 32));
                }
                let _ = epg.now_and_next(&channel.to_uppercase(), Utc::now());
            }
        }
    }

    #[test]
    fn xmltv_time_never_panics(s in "\\PC{0,30}", digits in "[0-9]{10,16}", off in "[+-]?[0-9:]{0,6}") {
        let _ = xmltv::parse_xmltv_time(&s);
        let _ = xmltv::parse_xmltv_time(&format!("{digits} {off}"));
    }
}

// ---------------------------------------------------------------------------------------
// EPG text and time
// ---------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]

    #[test]
    fn epg_text_never_panics(s in any::<String>()) {
        let out = epg::decode_text(&s);
        prop_assert!(out.len() <= epg::MAX_EPG_TEXT_LEN);
        let _ = epg::parse_server_datetime(&s);
        let _ = ServerTimeZone::from_name(&s);
    }

    #[test]
    fn base64_round_trip(s in "\\PC{0,200}") {
        let std = base64::engine::general_purpose::STANDARD.encode(&s);
        prop_assert_eq!(epg::decode_text(&std), s.trim());
        let url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&s);
        prop_assert_eq!(epg::decode_text(&url), s.trim());
    }

    #[test]
    fn time_zone_conversions_never_panic(secs in -10_000_000_000i64..10_000_000_000i64, tz_idx in 0usize..4) {
        let tz = [
            ServerTimeZone::Utc,
            ServerTimeZone::from_name("Europe/Paris").unwrap(),
            ServerTimeZone::from_name("Pacific/Chatham").unwrap(),
            ServerTimeZone::from_server_info(&ServerInfo {
                time_now: Some("2024-01-01 05:45:00".into()),
                timestamp_now: DateTime::from_timestamp(1_704_067_200, 0),
                ..ServerInfo::default()
            }),
        ][tz_idx];
        if let Some(dt) = DateTime::from_timestamp(secs, 0) {
            let back = tz.to_utc(tz.to_local(dt));
            match tz {
                // Fixed offsets are exact.
                ServerTimeZone::Utc | ServerTimeZone::Fixed(_) => prop_assert_eq!(back, dt),
                // Named zones may be off around (historical) transitions only.
                ServerTimeZone::Iana(_) => {
                    prop_assert!((back - dt).num_seconds().abs() <= 3 * 3600);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Redaction, parental, catalogue
// ---------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]

    #[test]
    fn redaction_never_panics(text in any::<String>(), secrets in prop::collection::vec(any::<String>(), 0..4)) {
        let _ = redact_url(&text);
        let r = Redactor::new();
        for s in &secrets {
            r.add_secret(s);
        }
        let _ = r.redact(&text);
    }

    #[test]
    fn stream_and_api_urls_are_always_redacted(
        user in "[a-zA-Z0-9 @/#?%&=+._~:-]{1,12}",
        pass in "pw[a-zA-Z0-9 @/#?%&=+._~:\u{e9}-]{1,12}",
        id in any::<u32>(),
    ) {
        // A path segment that is exactly "." or ".." cannot exist in a URL (documented in
        // `urls`); the known-secret layer still scrubs the password in that case.
        prop_assume!(user != "." && user != "..");
        let account = XtreamAccount::new("panel.example.com:8080", user.clone()).unwrap();
        let client = opensmarters_core::xtream::Client::new(
            account.clone(),
            SecretString::from(pass.clone()),
            &opensmarters_core::HttpConfig::default(),
        ).unwrap();
        let info = ServerInfo { url: Some("stream.example.net".into()), port: Some(8080), ..ServerInfo::default() };
        let urls = StreamUrls::from_server_info(&info, account.server(), &user, SecretString::from(pass.clone()));
        let live = urls.live(u64::from(id), LiveContainer::Ts);
        let encoded_pass = live.path_segments().unwrap().nth(2).unwrap().to_owned();
        for u in [live, urls.movie(u64::from(id), Some("mkv")), client.xmltv_url()] {
            let out = client.redactor().redact(u.as_str());
            prop_assert!(!out.contains(pass.as_str()), "{out}");
            prop_assert!(!out.contains(encoded_pass.as_str()), "{out}");
            // A fresh redactor (no known secrets) still hides the structure.
            let structural = redact_url(u.as_str());
            prop_assert!(!structural.contains(&format!("/{encoded_pass}/")), "{structural}");
        }
    }

    #[test]
    fn parental_whole_words(kw in "[a-z0-9]{1,6}\\+?", left in "[a-zA-Z ]{0,8}", right in "[a-zA-Z ]{0,8}", noise in any::<String>()) {
        let m = KeywordMatcher::new([kw.as_str()]);
        let text = format!("{left} {kw} {right}");
        prop_assert!(m.is_match(&text));
        prop_assert!(m.is_match(&text.to_uppercase()));
        let _ = KeywordMatcher::new([noise.as_str()]).is_match(&noise);
        // Glued on both sides by letters: never a match for an alphanumeric keyword.
        if kw.chars().all(|c| c.is_ascii_alphanumeric()) {
            let glued = format!("x{kw}x");
            prop_assert!(!m.is_match(&glued));
        }
    }

    #[test]
    fn sorting_is_a_sorted_permutation(names in prop::collection::vec(any::<String>(), 0..30), query in any::<String>()) {
        #[derive(Debug)]
        struct N(String);
        impl CatalogItem for N {
            type Key = String;
            fn key(&self) -> String { self.0.clone() }
            fn name(&self) -> &str { &self.0 }
            fn in_category(&self, _: &opensmarters_core::CategoryId) -> bool { false }
        }
        let items: Vec<N> = names.into_iter().map(N).collect();
        let idx = catalog::sorted_indices(&items, SortOrder::NameAsc);
        let mut seen = idx.clone();
        seen.sort_unstable();
        prop_assert_eq!(seen, (0..items.len()).collect::<Vec<_>>());
        for w in idx.windows(2) {
            prop_assert_ne!(
                catalog::natural_cmp(&items[w[0]].0, &items[w[1]].0),
                std::cmp::Ordering::Greater
            );
        }
        let q = catalog::SearchQuery::new(&query);
        let _ = catalog::search(&items, &q);
        let _ = catalog::SearchIndex::new(&items).search(&q);
    }

    #[test]
    fn server_url_never_panics(s in any::<String>(), t in "[a-zA-Z0-9:/.@?#\\[\\]% -]{0,40}") {
        let _ = ServerUrl::parse(&s);
        if let Ok(u) = ServerUrl::parse(&t) {
            prop_assert!(u.as_url().query().is_none());
            prop_assert!(u.as_url().username().is_empty());
            prop_assert!(matches!(u.scheme(), "http" | "https"));
            let again = ServerUrl::parse(&u.to_string()).unwrap();
            prop_assert_eq!(again, u);
        }
    }
}

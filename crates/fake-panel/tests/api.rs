//! API, EPG, M3U, XMLTV, image and quirk tests (no media needed).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use common::*;
use fake_panel::users::{self, SPECIAL_PASSWORD};
use image::GenericImageView as _;
use serde_json::{Value, json};

#[tokio::test]
async fn login_users_and_server_info() {
    let h = spawn_api("").await;

    let res = get(&api_url(&h, "demo", "demo", "")).await;
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()["content-type"].to_str().unwrap(),
        "application/json"
    );
    let v: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    let ui = &v["user_info"];
    assert_eq!(ui["auth"], 1);
    assert_eq!(ui["status"], "Active");
    assert_eq!(ui["username"], "demo");
    assert!(num(&ui["exp_date"]).unwrap() > now());
    assert_eq!(ui["is_trial"], "0");
    assert!(ui["allowed_output_formats"].is_array());
    let si = &v["server_info"];
    assert_eq!(si["url"], "127.0.0.1");
    assert_eq!(si["port"], h.port().to_string());
    assert!(si["https_port"].is_string());
    assert_eq!(si["server_protocol"], "http");
    assert_eq!(si["timezone"], "Europe/Paris");
    assert!((num(&si["timestamp_now"]).unwrap() - now()).abs() < 120);
    let time_now = si["time_now"].as_str().unwrap();
    assert_eq!(time_now.len(), 19, "{time_now}");
    assert_eq!(&time_now[10..11], " ");

    let expired = get_json(&api_url(&h, "expired", "expired", "")).await;
    assert_eq!(expired["user_info"]["auth"], 1);
    assert_eq!(expired["user_info"]["status"], "Expired");
    assert!(num(&expired["user_info"]["exp_date"]).unwrap() < now());

    let banned = get_json(&api_url(&h, "banned", "banned", "")).await;
    assert_eq!(banned["user_info"]["auth"], 0);

    let disabled = get_json(&api_url(&h, "disabled", "disabled", "")).await;
    assert_eq!(disabled["user_info"]["status"], "Disabled");

    let trial = get_json(&api_url(&h, "trial", "trial", "")).await;
    assert_eq!(trial["user_info"]["status"], "Active");
    assert_eq!(trial["user_info"]["is_trial"], "1");
    assert_eq!(trial["user_info"]["max_connections"], "1");

    let unlimited = get_json(&api_url(&h, "unlimited", "unlimited", "")).await;
    assert_eq!(unlimited["user_info"]["status"], "Active");
    assert!(unlimited["user_info"]["exp_date"].is_null());

    let wrong = get_json(&api_url(&h, "demo", "nope", "")).await;
    assert_eq!(wrong, json!({ "user_info": { "auth": 0 } }));
    let missing = get_json(&format!("{}/player_api.php", h.base_url())).await;
    assert_eq!(missing, json!({ "user_info": { "auth": 0 } }));

    // Actions for accounts that are not active are refused.
    let exp_action = get_json(&api_url(
        &h,
        "expired",
        "expired",
        "&action=get_live_categories",
    ))
    .await;
    assert_eq!(exp_action["user_info"]["auth"], 0);
    // Unknown action → [].
    assert_eq!(api(&h, "&action=get_everything").await, json!([]));
}

#[tokio::test]
async fn special_password_requires_percent_encoding() {
    let h = spawn_api("").await;
    // Fully percent-encoded: works.
    let ok = get_json(&api_url(&h, "special", SPECIAL_PASSWORD, "")).await;
    assert_eq!(ok["user_info"]["status"], "Active");
    assert_eq!(ok["user_info"]["password"], SPECIAL_PASSWORD);
    // Form-style space (`+`) is also fine, as long as the literal `+` is encoded.
    let form = format!(
        "{}/player_api.php?username=special&password=p%40ss%2Fw%23rd+%25%26%2B",
        h.base_url()
    );
    assert_eq!(get_json(&form).await["user_info"]["auth"], 1);
    // A raw `+` decodes to a space, so the password is wrong.
    let raw_plus = format!(
        "{}/player_api.php?username=special&password=p%40ss%2Fw%23rd%20%25%26+",
        h.base_url()
    );
    assert_eq!(get_json(&raw_plus).await["user_info"]["auth"], 0);
    // An unencoded `#` truncates the URL at the fragment.
    let raw_hash = format!(
        "{}/player_api.php?username=special&password=p@ss/w#rd %&+",
        h.base_url()
    );
    assert_eq!(get_json(&raw_hash).await["user_info"]["auth"], 0);
}

#[tokio::test]
async fn categories() {
    let h = spawn_api("").await;
    let live = api(&h, "&action=get_live_categories").await;
    let live = live.as_array().unwrap();
    assert_eq!(live.len(), 14);
    let names: Vec<&str> = live
        .iter()
        .map(|c| c["category_name"].as_str().unwrap())
        .collect();
    for want in [
        "Adult Night",
        "Sussex Local",
        "Essex FM",
        "Ελληνικά",
        "日本語",
        "Coming Soon",
    ] {
        assert!(names.contains(&want), "missing {want}");
    }
    assert!(names.iter().any(|n| n.contains('🎬')));
    assert!(names.iter().any(|n| n.chars().count() > 100));
    for c in live {
        assert!(c["category_id"].is_string());
        assert_eq!(c["parent_id"], 0);
    }
    // The empty category really is empty.
    let empty = live
        .iter()
        .find(|c| c["category_name"] == "Coming Soon")
        .unwrap();
    let id = empty["category_id"].as_str().unwrap();
    assert_eq!(
        api(&h, &format!("&action=get_live_streams&category_id={id}")).await,
        json!([])
    );

    let vod = api(&h, "&action=get_vod_categories").await;
    assert_eq!(vod.as_array().unwrap().len(), 10);
    let series = api(&h, "&action=get_series_categories").await;
    assert_eq!(series.as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn live_streams_and_category_filters() {
    let h = spawn_api("").await;
    let all = api(&h, "&action=get_live_streams").await;
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 250);
    assert_eq!(
        api(&h, "&action=get_live_streams&category_id=ALL")
            .await
            .as_array()
            .unwrap()
            .len(),
        250
    );
    assert_eq!(
        api(&h, "&action=get_live_streams&category_id=FAVOURITE")
            .await
            .as_array()
            .unwrap()
            .len(),
        250
    );
    let news = api(&h, "&action=get_live_streams&category_id=1").await;
    let news = news.as_array().unwrap();
    assert_eq!(news.len(), 30);
    assert!(news.iter().all(|c| c["category_id"] == "1"));

    let mut archive = 0;
    for (i, c) in all.iter().enumerate() {
        assert_eq!(c["stream_type"], "live");
        assert_eq!(c["num"], i as u64 + 1);
        assert!(c["stream_id"].is_u64());
        assert!(c["name"].is_string());
        assert!(c["stream_icon"].is_string());
        assert!(c["epg_channel_id"].is_string() || c["epg_channel_id"].is_null());
        assert!(c["added"].is_string());
        assert!(c["category_id"].is_string());
        let ta = c["tv_archive"].as_i64().unwrap();
        let dur = c["tv_archive_duration"].as_i64().unwrap();
        assert_eq!(ta == 1, dur > 0);
        archive += ta;
    }
    assert!((50..=100).contains(&archive), "archive channels: {archive}");
    let icon = all[0]["stream_icon"].as_str().unwrap();
    assert!(icon.starts_with(&format!("{}/images/logo/", h.base_url())));
    // Adult channels are flagged; local-news channels are not.
    let adult = all.iter().filter(|c| c["is_adult"] == "1").count();
    assert_eq!(adult, 8);
}

#[tokio::test]
async fn strict_category_quirk_off() {
    let h = spawn_api("no-category-all-literal").await;
    assert_eq!(
        api(&h, "&action=get_live_streams&category_id=ALL").await,
        json!([])
    );
    assert_eq!(
        api(&h, "&action=get_live_streams")
            .await
            .as_array()
            .unwrap()
            .len(),
        250
    );
    assert_eq!(
        api(&h, "&action=get_vod_streams&category_id=101")
            .await
            .as_array()
            .unwrap()
            .len(),
        22
    );
}

#[tokio::test]
async fn vod_streams_and_info() {
    let h = spawn_api("").await;
    let vods = api(&h, "&action=get_vod_streams&category_id=ALL").await;
    let vods = vods.as_array().unwrap();
    assert_eq!(vods.len(), 150);
    let mkv_mp4 = vods
        .iter()
        .filter(|m| matches!(m["container_extension"].as_str(), Some("mkv" | "mp4")))
        .count();
    assert!(mkv_mp4 >= 130, "{mkv_mp4}");
    for m in vods {
        assert_eq!(m["stream_type"], "movie");
        assert!(m["stream_id"].is_u64());
        assert!(m["rating"].is_string());
        assert!(m["rating_5based"].is_number());
        assert!(m["added"].is_string());
        assert!(
            m["stream_icon"]
                .as_str()
                .unwrap()
                .contains("/images/poster/")
        );
    }
    let id = vods[0]["stream_id"].as_u64().unwrap();
    let info = api(&h, &format!("&action=get_vod_info&vod_id={id}")).await;
    let i = &info["info"];
    for key in [
        "plot",
        "cast",
        "director",
        "genre",
        "releasedate",
        "duration",
        "movie_image",
    ] {
        assert!(i[key].is_string(), "info.{key}: {}", i[key]);
    }
    assert_eq!(i["youtube_trailer"], "");
    assert!(i["tmdb_id"].is_u64());
    assert!(i["backdrop_path"].is_array());
    assert_eq!(info["movie_data"]["stream_id"], id);
    assert_eq!(
        info["movie_data"]["container_extension"],
        vods[0]["container_extension"]
    );
    let missing = api(&h, "&action=get_vod_info&vod_id=999").await;
    assert_eq!(missing, json!({ "info": [], "movie_data": [] }));
}

#[tokio::test]
async fn series_and_info() {
    let h = spawn_api("").await;
    let series = api(&h, "&action=get_series").await;
    let series = series.as_array().unwrap();
    assert_eq!(series.len(), 30);
    for s in series {
        assert!(s["series_id"].is_u64());
        assert!(s["cover"].as_str().unwrap().contains("/images/cover/"));
        assert!(s["backdrop_path"].is_array());
        assert!(s["releaseDate"].is_string());
        assert!(s["last_modified"].is_string());
    }
    let info = api(&h, "&action=get_series_info&series_id=1").await;
    let episodes = info["episodes"].as_object().unwrap();
    let first = &episodes["1"][0];
    assert!(first["id"].is_string());
    assert_eq!(first["episode_num"], 1);
    assert!(first["title"].as_str().unwrap().contains("S01E01"));
    assert!(matches!(
        first["container_extension"].as_str(),
        Some("mkv" | "mp4")
    ));
    for key in ["movie_image", "plot", "duration"] {
        assert!(first["info"][key].is_string(), "{key}");
    }
    assert!(first["info"]["rating"].is_number());
    assert!(info["seasons"].is_array());
    // Series 2 has a season 0 ("Specials").
    let specials = api(&h, "&action=get_series_info&series_id=2").await;
    assert!(specials["episodes"].as_object().unwrap().contains_key("0"));
    let missing = api(&h, "&action=get_series_info&series_id=9999").await;
    assert_eq!(missing["episodes"], json!([]));
}

fn decode(s: &Value) -> String {
    String::from_utf8(B64.decode(s.as_str().unwrap()).unwrap()).unwrap()
}

#[tokio::test]
async fn short_epg_is_base64_and_current() {
    let h = spawn_api("").await;
    let cat = h.catalogue();
    let greek = cat
        .live
        .iter()
        .find(|c| c.category_id == 10 && c.epg_channel_id.is_some())
        .unwrap();
    let v = api(
        &h,
        &format!("&action=get_short_epg&stream_id={}", greek.stream_id),
    )
    .await;
    let l = v["epg_listings"].as_array().unwrap();
    assert_eq!(l.len(), 4);
    assert_eq!(l[0]["now_playing"], 1);
    let start = num(&l[0]["start_timestamp"]).unwrap();
    let stop = num(&l[0]["stop_timestamp"]).unwrap();
    assert!(start <= now() && now() < stop + 5);
    let title = decode(&l[0]["title"]);
    assert!(
        title
            .chars()
            .any(|c| ('\u{0370}'..='\u{03FF}').contains(&c)),
        "{title}"
    );
    let desc = decode(&l[0]["description"]);
    assert!(desc.contains('—') && desc.contains("café"), "{desc}");
    assert_eq!(l[0]["lang"], "el");
    assert_eq!(l[0]["start"].as_str().unwrap().len(), 19);
    assert_eq!(l[0]["channel_id"].as_str(), greek.epg_channel_id.as_deref());
    // Consecutive programmes.
    for w in l.windows(2) {
        assert_eq!(w[0]["stop_timestamp"], w[1]["start_timestamp"]);
        assert_eq!(w[0]["end"], w[1]["start"]);
    }
    let ten = api(
        &h,
        &format!(
            "&action=get_short_epg&stream_id={}&limit=10",
            greek.stream_id
        ),
    )
    .await;
    assert_eq!(ten["epg_listings"].as_array().unwrap().len(), 10);

    let no_epg = cat
        .live
        .iter()
        .find(|c| c.epg_channel_id.is_none())
        .unwrap();
    let v = api(
        &h,
        &format!("&action=get_short_epg&stream_id={}", no_epg.stream_id),
    )
    .await;
    assert_eq!(v, json!({ "epg_listings": [] }));
    let v = api(&h, "&action=get_short_epg&stream_id=424242").await;
    assert_eq!(v, json!({ "epg_listings": [] }));
}

#[tokio::test]
async fn simple_data_table_has_archive_window() {
    let h = spawn_api("").await;
    let cat = h.catalogue();
    let arch = cat
        .live
        .iter()
        .find(|c| c.tv_archive && c.tv_archive_duration >= 3 && c.epg_channel_id.is_some())
        .unwrap();
    let v = api(
        &h,
        &format!("&action=get_simple_data_table&stream_id={}", arch.stream_id),
    )
    .await;
    let l = v["epg_listings"].as_array().unwrap();
    let t = now();
    let first_start = num(&l[0]["start_timestamp"]).unwrap();
    assert!(
        first_start <= t - 3 * 86_400 + 3 * 3600,
        "goes back ~3 days"
    );
    assert!(num(&l.last().unwrap()["stop_timestamp"]).unwrap() > t + 12 * 3600);
    let playing = l.iter().filter(|p| p["now_playing"] == 1).count();
    assert_eq!(playing, 1);
    let archived: Vec<&Value> = l.iter().filter(|p| p["has_archive"] == 1).collect();
    assert!(archived.len() > 20);
    assert!(
        archived
            .iter()
            .all(|p| num(&p["stop_timestamp"]).unwrap() <= t)
    );
    let no_arch = cat
        .live
        .iter()
        .find(|c| !c.tv_archive && c.epg_channel_id.is_some())
        .unwrap();
    let v = api(
        &h,
        &format!(
            "&action=get_simple_data_table&stream_id={}",
            no_arch.stream_id
        ),
    )
    .await;
    let l = v["epg_listings"].as_array().unwrap();
    assert!(!l.is_empty());
    assert!(l.iter().all(|p| p["has_archive"] == 0));
}

#[tokio::test]
async fn xmltv_plain_and_gzip() {
    let h = spawn_api("").await;
    let res = get(&format!(
        "{}/xmltv.php?username=demo&password=demo",
        h.base_url()
    ))
    .await;
    assert_eq!(res.status(), 200);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("xml")
    );
    let xml = res.text().await.unwrap();
    assert!(xml.starts_with("<?xml"));
    assert!(xml.contains("<tv "));
    assert!(xml.trim_end().ends_with("</tv>"));
    let channels = xml.matches("<channel id=").count();
    assert!(channels > 200, "{channels}");
    let programmes = xml.matches("<programme ").count();
    assert!(programmes > channels * 50, "{programmes}");
    // Every programme time has a +HHMM offset.
    let first = xml.find("<programme start=\"").unwrap();
    let attr = &xml[first + 18..first + 18 + 20];
    assert!(
        attr.ends_with(" +0100") || attr.ends_with(" +0200"),
        "{attr}"
    );
    assert!(attr[..14].bytes().all(|b| b.is_ascii_digit()));
    assert!(xml.contains("&amp;") || xml.contains("&quot;"));

    let bad = get(&format!(
        "{}/xmltv.php?username=demo&password=bad",
        h.base_url()
    ))
    .await;
    assert_eq!(bad.status(), 401);

    let gz = get(&format!("{}/xmltv.xml.gz", h.base_url())).await;
    assert_eq!(gz.status(), 200);
    assert_eq!(
        gz.headers()["content-type"].to_str().unwrap(),
        "application/gzip"
    );
    let bytes = gz.bytes().await.unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
    let mut out = String::new();
    flate2::read::GzDecoder::new(&bytes[..])
        .read_to_string(&mut out)
        .unwrap();
    assert!(out.starts_with("<?xml"));
    assert_eq!(out.matches("<channel id=").count(), channels);
}

#[tokio::test]
async fn m3u_plus_playlist() {
    let h = spawn_api("").await;
    let url = format!(
        "{}/get.php?username=demo&password=demo&type=m3u_plus&output=ts",
        h.base_url()
    );
    let res = get(&url).await;
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.starts_with(&format!(
        "#EXTM3U url-tvg=\"{}/xmltv.php?username=demo&password=demo\"",
        h.base_url()
    )));
    let cat = h.catalogue();
    let expected = cat.live.len() + cat.movies.len() + cat.episodes().count();
    assert_eq!(body.matches("#EXTINF:").count(), expected);
    assert!(body.contains(&format!("{}/live/demo/demo/1001.ts\n", h.base_url())));
    assert!(body.contains("/movie/demo/demo/200001."));
    assert!(body.contains("/series/demo/demo/300001."));
    assert!(body.contains("tvg-id=\""));
    assert!(body.contains("tvg-logo=\""));
    assert!(body.contains("group-title=\"News\""));

    let m3u8 = get(&format!(
        "{}/get.php?username=demo&password=demo&type=m3u_plus&output=m3u8",
        h.base_url()
    ))
    .await
    .text()
    .await
    .unwrap();
    assert!(m3u8.contains("/live/demo/demo/1001.m3u8\n"));

    // Credentials in generated URLs are percent-encoded.
    let special = get(&format!(
        "{}/get.php?username=special&password={}&type=m3u_plus",
        h.base_url(),
        enc(SPECIAL_PASSWORD)
    ))
    .await
    .text()
    .await
    .unwrap();
    assert!(special.contains("/live/special/p%40ss%2Fw%23rd%20%25%26%2B/1001.ts"));

    let bad = get(&format!(
        "{}/get.php?username=demo&password=x&type=m3u_plus",
        h.base_url()
    ))
    .await;
    assert_eq!(bad.status(), 401);

    let head = client().head(&url).send().await.unwrap();
    assert_eq!(head.status(), 200);

    let plain = get(&format!(
        "{}/get.php?username=demo&password=demo&type=m3u",
        h.base_url()
    ))
    .await
    .text()
    .await
    .unwrap();
    assert!(plain.starts_with("#EXTM3U\n#EXTINF:-1,"));
}

#[tokio::test]
async fn nasty_playlist() {
    let h = spawn_api("").await;
    let bytes = get(&format!("{}/nasty.m3u", h.base_url()))
        .await
        .bytes()
        .await
        .unwrap();
    assert_eq!(&bytes[..3], b"\xEF\xBB\xBF");
    let s = std::str::from_utf8(&bytes).unwrap();
    for needle in [
        "\r\n",
        "#EXTINF:0 ",
        "#EXTINF:-1 ",
        "#EXTINF:123.456",
        "#EXTGRP:",
        "#EXTVLCOPT:http-user-agent=",
        "#EXTVLCOPT:http-referrer=",
        "group-title=\"News, Weather & Traffic\"",
        "rtmp://",
        "udp://@",
        "https://",
        "file:///etc/passwd",
        "file:///C:/",
        "lavfi://",
        "av://lavfi",
        "edl://",
        "\r\n\r\n\r\n",
        "Entry Without URL\r\n#EXTINF",
        "#extinf:",
    ] {
        assert!(s.contains(needle), "missing {needle:?}");
    }
    // Some LF-only lines in an otherwise CRLF file.
    assert!(s.contains(".ts\n#EXTINF") && !s.ends_with('\n'));
    assert!(s.lines().any(|l| l.len() > 500));
}

#[tokio::test]
async fn images_are_generated_pngs() {
    let h = spawn_api("").await;
    let cat = h.catalogue();
    let logo_id = cat
        .live
        .iter()
        .find(|c| c.icon == fake_panel::catalogue::Icon::Generated)
        .unwrap()
        .stream_id;
    let movie = &cat.movies[0];
    let backdrop_movie = cat.movies.iter().find(|m| m.has_backdrop).unwrap();
    let cases = [
        (format!("/images/logo/{logo_id}.png"), 256, 256),
        (format!("/images/poster/{}.png", movie.stream_id), 300, 450),
        (
            format!("/images/cover/{}.png", cat.series[0].series_id),
            300,
            450,
        ),
        (
            format!("/images/backdrop/movie-{}.png", backdrop_movie.stream_id),
            640,
            360,
        ),
        (
            format!("/images/backdrop/series-{}.png", cat.series[0].series_id),
            640,
            360,
        ),
        ("/images/episode/300001.png".to_owned(), 400, 225),
    ];
    for (path, w, hgt) in cases {
        let res = get(&h.url(&path)).await;
        assert_eq!(res.status(), 200, "{path}");
        assert_eq!(res.headers()["content-type"], "image/png");
        let bytes = res.bytes().await.unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!(img.dimensions(), (w, hgt), "{path}");
        let again = get(&h.url(&path)).await.bytes().await.unwrap();
        assert_eq!(bytes, again, "{path} deterministic");
    }
    let broken = cat
        .live
        .iter()
        .find(|c| c.icon == fake_panel::catalogue::Icon::Broken)
        .unwrap();
    let res = get(&h.url(&format!("/images/logo/{}.png", broken.stream_id))).await;
    assert_eq!(res.status(), 404);
    assert_eq!(get(&h.url("/images/logo/1.png")).await.status(), 404);
    assert_eq!(get(&h.url("/images/nope/1001.png")).await.status(), 404);
}

#[tokio::test]
async fn quirk_mixed_types() {
    let h = spawn_api("mixed-types").await;
    let live = api(&h, "&action=get_live_streams").await;
    let live = live.as_array().unwrap();
    assert_eq!(live.len(), 250);
    let str_ids = live.iter().filter(|c| c["stream_id"].is_string()).count();
    let int_ids = live.iter().filter(|c| c["stream_id"].is_u64()).count();
    assert!(str_ids > 50 && int_ids > 50, "{str_ids}/{int_ids}");
    assert!(live.iter().any(|c| c["num"].is_string()));
    assert!(live.iter().any(|c| c["category_id"].is_u64()));
    assert!(live.iter().any(|c| c["category_id"].is_null()));
    assert!(live.iter().any(|c| c["stream_icon"].is_null()));
    assert!(live.iter().any(|c| c["stream_icon"] == ""));
    assert!(live.iter().any(|c| c["added"].is_i64()));
    // Every id is still recoverable.
    assert!(live.iter().all(|c| num(&c["stream_id"]).is_some()));
    // Deterministic per item.
    let again = api(&h, "&action=get_live_streams").await;
    assert_eq!(&again.as_array().unwrap()[..], live);
    let vod = api(&h, "&action=get_vod_streams").await;
    let vod = vod.as_array().unwrap();
    assert!(vod.iter().any(|m| m["rating"].is_number()));
    assert!(vod.iter().any(|m| m["rating"].is_string()));
    let login = get_json(&api_url(&h, "demo", "demo", "")).await;
    assert!(num(&login["user_info"]["auth"]) == Some(1));
}

#[tokio::test]
async fn quirk_html_content_type() {
    let h = spawn_api("html-content-type").await;
    let res = get(&api_url(&h, "demo", "demo", "&action=get_live_categories")).await;
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let v: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 14);
}

#[tokio::test]
async fn quirk_backdrop_string() {
    let h = spawn_api("backdrop-string").await;
    let m = h
        .catalogue()
        .movies
        .iter()
        .find(|m| m.has_backdrop)
        .unwrap();
    let info = api(&h, &format!("&action=get_vod_info&vod_id={}", m.stream_id)).await;
    let bp = info["info"]["backdrop_path"].as_str().unwrap();
    assert!(bp.contains("/images/backdrop/movie-"), "{bp}");
    let series = api(&h, "&action=get_series").await;
    assert!(
        series
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["backdrop_path"].is_string())
    );
}

#[tokio::test]
async fn quirk_episodes_array() {
    let h = spawn_api("episodes-array").await;
    let s3 = api(&h, "&action=get_series_info&series_id=3").await;
    let seasons = s3["episodes"].as_array().unwrap();
    assert!(!seasons.is_empty());
    assert!(seasons[0].as_array().unwrap()[0]["id"].is_string());
    let s1 = api(&h, "&action=get_series_info&series_id=1").await;
    assert!(s1["episodes"].is_object());
}

#[tokio::test]
async fn quirk_malformed_item() {
    let h = spawn_api("malformed-item").await;
    let live = api(&h, "&action=get_live_streams").await;
    let live = live.as_array().unwrap();
    assert_eq!(live.len(), 251);
    assert!(live.iter().any(|c| c["stream_id"] == "not-an-id"));
    let cats = api(&h, "&action=get_live_categories").await;
    assert!(
        cats.as_array()
            .unwrap()
            .iter()
            .any(|c| c["category_id"].is_null())
    );
    let vod = api(&h, "&action=get_vod_streams").await;
    assert_eq!(vod.as_array().unwrap().len(), 151);
    let series = api(&h, "&action=get_series").await;
    assert_eq!(series.as_array().unwrap().len(), 31);
    let ch = h
        .catalogue()
        .live
        .iter()
        .find(|c| c.epg_channel_id.is_some())
        .unwrap();
    let epg = api(
        &h,
        &format!("&action=get_short_epg&stream_id={}", ch.stream_id),
    )
    .await;
    let l = epg["epg_listings"].as_array().unwrap();
    assert!(
        l.iter()
            .any(|p| B64.decode(p["title"].as_str().unwrap_or("")).is_err())
    );
    // An empty category stays empty.
    assert_eq!(
        api(&h, "&action=get_live_streams&category_id=13").await,
        json!([])
    );
}

#[tokio::test]
async fn quirk_stream_host() {
    let h = spawn_api("stream-host").await;
    let login = get_json(&api_url(&h, "demo", "demo", "")).await;
    assert_eq!(login["server_info"]["url"], "localhost");
    assert!(h.base_url().starts_with("http://127.0.0.1:"));
    assert_eq!(
        h.stream_base_url(),
        format!("http://localhost:{}", h.port())
    );
    let m3u = get(&format!(
        "{}/get.php?username=demo&password=demo&type=m3u_plus",
        h.base_url()
    ))
    .await
    .text()
    .await
    .unwrap();
    assert!(m3u.contains(&format!("http://localhost:{}/live/demo/demo/", h.port())));
}

#[tokio::test]
async fn quirk_huge() {
    let h = spawn_api("huge").await;
    let live = api(&h, "&action=get_live_streams&category_id=ALL").await;
    assert_eq!(live.as_array().unwrap().len(), 50_000);
    let news = api(&h, "&action=get_live_streams&category_id=1").await;
    assert!(news.as_array().unwrap().len() > 3000);
}

#[tokio::test]
async fn quirk_slow() {
    let h = spawn_api("slow").await;
    let started = std::time::Instant::now();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let v = api(&h, "&action=get_live_categories").await;
        assert_eq!(v.as_array().unwrap().len(), 14);
        assert!(t.elapsed().as_millis() <= 3600);
    }
    assert!(started.elapsed().as_millis() > 100, "slow adds latency");
}

#[tokio::test]
async fn combined_quirks() {
    let h = spawn_api(
        "mixed-types,html-content-type,backdrop-string,episodes-array,malformed-item,stream-host",
    )
    .await;
    let q = h.quirks();
    assert!(q.mixed_types && q.malformed_item && q.category_all_literal);
    let login = get_json(&api_url(&h, "demo", "demo", "")).await;
    assert_eq!(login["user_info"]["status"], "Active");
    let live = api(&h, "&action=get_live_streams&category_id=ALL").await;
    assert_eq!(live.as_array().unwrap().len(), 251);
    let s = api(&h, "&action=get_series_info&series_id=6").await;
    assert!(s["episodes"].is_array());
    assert!(s["info"]["backdrop_path"].is_string() || s["info"]["backdrop_path"].is_null());
}

#[tokio::test]
async fn debug_request_log() {
    let h = spawn_api("").await;
    let _ = api(&h, "&action=get_live_categories").await;
    let _ = get(&h.url("/does-not-exist")).await;
    let log = get_json(&h.url("/__debug/last-requests")).await;
    let reqs = log["requests"].as_array().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0]["path"], "/player_api.php");
    assert!(
        reqs[0]["query"]
            .as_str()
            .unwrap()
            .contains("action=get_live_categories")
    );
    assert_eq!(reqs[0]["route"], "/player_api.php");
    assert_eq!(reqs[0]["status"], 200);
    assert_eq!(reqs[1]["status"], 404);
    assert_eq!(h.requests().len(), 2);
    let limited = get_json(&h.url("/__debug/last-requests?limit=1")).await;
    assert_eq!(limited["requests"].as_array().unwrap().len(), 1);
    let del = client()
        .delete(h.url("/__debug/last-requests"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    assert!(h.requests().is_empty());
    assert_eq!(users::USERS.len(), 7);
}

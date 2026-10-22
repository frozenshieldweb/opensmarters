// SPDX-License-Identifier: GPL-3.0-or-later
//! Every catalogue endpoint against the fake panel, favourites, progress / Continue
//! watching, and the image proxy.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::common::*;

fn arr(v: &Value) -> &Vec<Value> {
    v.as_array().unwrap()
}

fn assert_server_image(v: &Value) {
    if let Some(s) = v.as_str() {
        assert!(s.starts_with("/api/img/"), "{s}");
        assert!(!s.contains("127.0.0.1") && !s.contains("http"), "{s}");
    } else {
        assert!(v.is_null(), "{v}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xtream_catalogue() {
    let panel = panel().await;
    let cat = panel.catalogue().clone();
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    let a = add_xtream(&c, &panel).await;
    let base = format!("/api/accounts/{a}");

    // Live categories in provider order, with counts.
    let cats = c.get(&format!("{base}/live/categories")).await.ok();
    assert_eq!(arr(&cats).len(), cat.live_categories.len());
    assert_eq!(cats[0]["name"], cat.live_categories[0].name.as_str());
    assert_eq!(cats[0]["id"], cat.live_categories[0].id.to_string());
    let news_count = cat
        .live
        .iter()
        .filter(|ch| ch.category_id == cat.live_categories[0].id)
        .count();
    assert_eq!(cats[0]["count"], news_count);

    // Channels: all, one category, favourites.
    let all = c
        .get(&format!("{base}/live/channels?category=all"))
        .await
        .ok();
    assert_eq!(arr(&all).len(), cat.live.len());
    assert_eq!(
        arr(&c.get(&format!("{base}/live/channels")).await.ok()).len(),
        cat.live.len()
    );
    let first = &all[0];
    assert_eq!(first["key"], cat.live[0].stream_id.to_string());
    assert_eq!(first["name"], cat.live[0].name.as_str());
    assert_eq!(first["number"], cat.live[0].num);
    assert_server_image(&first["logo"]);
    for k in [
        "key",
        "name",
        "number",
        "logo",
        "categories",
        "archive_days",
    ] {
        assert!(first.get(k).is_some(), "{k}");
    }
    let archive = arr(&all).iter().find(|ch| ch["archive_days"].is_number());
    assert!(archive.is_some(), "some channels have catch-up");
    let news = c
        .get(&format!("{base}/live/channels?category=1"))
        .await
        .ok();
    assert_eq!(arr(&news).len(), news_count);
    assert!(
        arr(&news)
            .iter()
            .all(|ch| arr(&ch["categories"]).iter().any(|x| x == "1"))
    );
    assert_eq!(
        c.get(&format!("{base}/live/channels?category=favourites"))
            .await
            .ok(),
        json!([])
    );

    // EPG now/next for a few keys; at most 200.
    let keys: Vec<String> = arr(&all)
        .iter()
        .take(5)
        .map(|ch| ch["key"].as_str().unwrap().to_owned())
        .collect();
    let now = c
        .get(&format!("{base}/epg/now?keys={}", keys.join(",")))
        .await
        .ok();
    let obj = now.as_object().unwrap();
    assert!(!obj.is_empty());
    let t = chrono::Utc::now().timestamp();
    for (k, rows) in obj {
        assert!(keys.contains(k));
        let rows = arr(rows);
        assert!(rows.len() <= 2);
        if let Some(p) = rows.first() {
            assert!(p["start"].as_i64().unwrap() <= t + 24 * 3600);
            assert!(p["title"].is_string());
            assert!(p.get("description").is_some());
        }
    }
    let many: Vec<String> = (0..201).map(|i| i.to_string()).collect();
    assert_eq!(
        c.get(&format!("{base}/epg/now?keys={}", many.join(",")))
            .await
            .status,
        400
    );

    // A channel's schedule over a window (≤ 48 h).
    let key = &keys[0];
    let sched = c
        .get(&format!(
            "{base}/epg/channel/{key}?from={}&to={}",
            t - 3 * 3600,
            t + 12 * 3600
        ))
        .await
        .ok();
    assert!(!arr(&sched).is_empty());
    for p in arr(&sched) {
        assert!(p["end"].as_i64().unwrap() > t - 3 * 3600);
        assert!(p["start"].as_i64().unwrap() < t + 12 * 3600);
    }
    let r = c
        .get(&format!(
            "{base}/epg/channel/{key}?from={t}&to={}",
            t + 49 * 3600
        ))
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(
        c.get(&format!(
            "{base}/epg/channel/999999?from={t}&to={}",
            t + 3600
        ))
        .await
        .status,
        404
    );

    // Films.
    let vcats = c.get(&format!("{base}/movies/categories")).await.ok();
    assert_eq!(arr(&vcats).len(), cat.vod_categories.len());
    assert_eq!(
        vcats[0],
        json!({"id": cat.vod_categories[0].id.to_string(), "name": cat.vod_categories[0].name})
    );
    let movies = c.get(&format!("{base}/movies?category=all")).await.ok();
    assert_eq!(arr(&movies).len(), cat.movies.len());
    let m0 = &movies[0];
    for k in ["id", "name", "poster", "year", "rating", "added"] {
        assert!(m0.get(k).is_some(), "{k}");
    }
    assert_server_image(&m0["poster"]);
    let action = c.get(&format!("{base}/movies?category=101")).await.ok();
    assert_eq!(
        arr(&action).len(),
        cat.movies.iter().filter(|m| m.category_id == 101).count()
    );
    let mid = cat.movies[0].stream_id;
    let detail = c.get(&format!("{base}/movies/{mid}")).await.ok();
    for k in [
        "id",
        "name",
        "poster",
        "backdrop",
        "year",
        "rating",
        "duration_secs",
        "genre",
        "plot",
        "cast",
        "director",
        "progress",
    ] {
        assert!(detail.get(k).is_some(), "{k}");
    }
    assert_eq!(detail["id"], mid.to_string());
    assert!(detail["progress"].is_null());
    assert!(detail["plot"].is_string());
    assert_eq!(
        c.get(&format!("{base}/movies/notanumber")).await.status,
        404
    );

    // Series.
    let scats = c.get(&format!("{base}/series/categories")).await.ok();
    assert_eq!(arr(&scats).len(), cat.series_categories.len());
    let series = c.get(&format!("{base}/series?category=all")).await.ok();
    assert_eq!(arr(&series).len(), cat.series.len());
    for k in ["id", "name", "poster", "year", "rating", "updated"] {
        assert!(series[0].get(k).is_some(), "{k}");
    }
    let s0 = &cat.series[0];
    let sd = c.get(&format!("{base}/series/{}", s0.series_id)).await.ok();
    for k in [
        "id", "name", "poster", "backdrop", "year", "rating", "genre", "plot", "cast", "seasons",
    ] {
        assert!(sd.get(k).is_some(), "{k}");
    }
    let seasons = arr(&sd["seasons"]);
    assert_eq!(seasons.len(), s0.seasons.len());
    let ep = &seasons[0]["episodes"][0];
    for k in [
        "id",
        "number",
        "title",
        "plot",
        "duration_secs",
        "still",
        "progress",
    ] {
        assert!(ep.get(k).is_some(), "{k}");
    }

    // Search.
    assert_eq!(c.get(&format!("{base}/search?q=a")).await.status, 400);
    let word = cat.live[0]
        .name
        .split_whitespace()
        .next()
        .unwrap()
        .to_lowercase();
    let found = c.get(&format!("{base}/search?q={word}")).await.ok();
    assert!(!arr(&found["channels"]).is_empty());
    assert!(arr(&found["channels"]).len() <= 50);
    assert!(found["movies"].is_array() && found["series"].is_array());
    let mword = cat.movies[0]
        .title
        .split_whitespace()
        .next()
        .unwrap()
        .to_lowercase();
    let found = c.get(&format!("{base}/search?q={mword}")).await.ok();
    assert!(!arr(&found["movies"]).is_empty(), "{mword}");
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn favourites_progress_and_continue() {
    let panel = panel().await;
    let cat = panel.catalogue().clone();
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    let a = add_xtream(&c, &panel).await;
    let base = format!("/api/accounts/{a}");
    let ch = cat.live[3].stream_id;
    let mv = cat.movies[2].stream_id;
    let sr = cat.series[1].series_id;

    assert_eq!(
        c.put(&format!("{base}/favourites/channel/{ch}"), None)
            .await
            .ok(),
        json!({})
    );
    c.put(&format!("{base}/favourites/movie/{mv}"), None)
        .await
        .ok();
    c.put(&format!("{base}/favourites/series/{sr}"), None)
        .await
        .ok();
    // Idempotent; unknown items and kinds are refused.
    c.put(&format!("{base}/favourites/channel/{ch}"), None)
        .await
        .ok();
    assert_eq!(
        c.put(&format!("{base}/favourites/channel/424242"), None)
            .await
            .status,
        404
    );
    assert_eq!(
        c.put(&format!("{base}/favourites/movie/999999999"), None)
            .await
            .status,
        404
    );
    assert_eq!(
        c.put(&format!("{base}/favourites/radio/1"), None)
            .await
            .status,
        400
    );

    let fav = c.get(&format!("{base}/favourites")).await.ok();
    assert_eq!(arr(&fav["channels"]).len(), 1);
    assert_eq!(fav["channels"][0]["key"], ch.to_string());
    assert_eq!(arr(&fav["movies"]).len(), 1);
    assert_eq!(fav["movies"][0]["id"], mv.to_string());
    assert_eq!(fav["movies"][0]["name"], cat.movies[2].name.as_str());
    assert_server_image(&fav["movies"][0]["poster"]);
    assert_eq!(arr(&fav["series"]).len(), 1);
    let live_favs = c
        .get(&format!("{base}/live/channels?category=favourites"))
        .await
        .ok();
    assert_eq!(arr(&live_favs).len(), 1);
    c.delete(&format!("{base}/favourites/channel/{ch}"))
        .await
        .ok();
    assert_eq!(
        arr(&c.get(&format!("{base}/favourites")).await.ok()["channels"]).len(),
        0
    );

    // Progress and Continue watching.
    let s1 = &cat.series[0];
    let e1 = s1.seasons[0].episodes[0].id;
    let put = |body: Value| {
        let c = &c;
        let base = base.clone();
        async move { c.put(&format!("{base}/progress"), Some(body)).await }
    };
    assert_eq!(
        put(json!({"kind": "movie", "id": mv.to_string(), "position": 120.5, "duration": 6000}))
            .await
            .status,
        200
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let r = put(json!({"kind": "episode", "id": e1.to_string(), "series_id": s1.series_id.to_string(), "position": 30, "duration": 1500})).await;
    assert_eq!(r.status, 200, "{}", r.json);
    assert_eq!(
        put(json!({"kind": "episode", "id": e1.to_string(), "position": 30, "duration": 1500}))
            .await
            .status,
        400
    );
    assert_eq!(
        put(json!({"kind": "movie", "id": "1", "position": -1, "duration": 10}))
            .await
            .status,
        400
    );
    assert_eq!(
        put(json!({"kind": "song", "id": "1", "position": 1, "duration": 10}))
            .await
            .status,
        400
    );

    let cont = c.get(&format!("{base}/continue")).await.ok();
    let list = arr(&cont);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["kind"], "episode", "newest first");
    assert_eq!(list[0]["series_id"], s1.series_id.to_string());
    assert!(list[0]["name"].as_str().unwrap().contains(&s1.name));
    assert_eq!(list[1]["kind"], "movie");
    assert_eq!(list[1]["name"], cat.movies[2].name.as_str());
    assert_eq!(list[1]["position"], 120.5);
    assert_eq!(list[1]["duration"], 6000.0);
    assert!(list[1]["series_id"].is_null());
    assert_server_image(&list[1]["poster"]);
    for k in [
        "kind",
        "id",
        "series_id",
        "name",
        "poster",
        "position",
        "duration",
        "updated",
    ] {
        assert!(list[0].get(k).is_some(), "{k}");
    }
    // The detail pages show it.
    let detail = c.get(&format!("{base}/movies/{mv}")).await.ok();
    assert_eq!(
        detail["progress"],
        json!({"position": 120.5, "duration": 6000.0})
    );
    let e1_id = e1.to_string();
    let sd = c.get(&format!("{base}/series/{}", s1.series_id)).await.ok();
    let ep = arr(&sd["seasons"][0]["episodes"])
        .iter()
        .find(|e| e["id"].as_str() == Some(e1_id.as_str()))
        .unwrap()
        .clone();
    assert_eq!(
        ep["progress"],
        json!({"position": 30.0, "duration": 1500.0})
    );

    // Finished (≥ 95 %) leaves Continue watching.
    put(json!({"kind": "movie", "id": mv.to_string(), "position": 5800, "duration": 6000}))
        .await
        .ok();
    let cont = c.get(&format!("{base}/continue")).await.ok();
    assert_eq!(arr(&cont).len(), 1);
    assert_eq!(cont[0]["kind"], "episode");
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3u_catalogue() {
    let panel = panel().await;
    let cat = panel.catalogue().clone();
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    let a = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": panel.url("/get.php?username=demo&password=demo&type=m3u_plus&output=ts")}),
        )
        .await
        .ok();
    let base = format!("/api/accounts/{}", a["id"].as_str().unwrap());
    let cats = c.get(&format!("{base}/live/categories")).await.ok();
    let names: HashSet<&str> = arr(&cats)
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(cat.live_categories[0].name.as_str()));
    let all = c
        .get(&format!("{base}/live/channels?category=all"))
        .await
        .ok();
    assert_eq!(arr(&all).len(), cat.live.len());
    let k = all[0]["key"].as_str().unwrap();
    assert!(k.starts_with("m3u:"), "{k}");
    assert_server_image(&all[0]["logo"]);
    // XMLTV now/next from the playlist's url-tvg.
    let now = c.get(&format!("{base}/epg/now?keys={k}")).await.ok();
    assert!(now.get(k).is_some(), "{now}");
    assert_eq!(
        c.get(&format!("{base}/movies/categories")).await.ok(),
        json!([])
    );
    assert_eq!(
        c.get(&format!("{base}/series/categories")).await.ok(),
        json!([])
    );
    assert_eq!(
        c.get(&format!("{base}/series?category=all")).await.ok(),
        json!([])
    );
    let movies = c.get(&format!("{base}/movies?category=all")).await.ok();
    assert_eq!(arr(&movies).len(), cat.movies.len());
    let mid = movies[0]["id"].as_str().unwrap();
    let detail = c.get(&format!("{base}/movies/{mid}")).await.ok();
    assert_eq!(detail["name"], movies[0]["name"]);
    c.put(&format!("{base}/favourites/channel/{k}"), None)
        .await
        .ok();
    let fav = c.get(&format!("{base}/favourites")).await.ok();
    assert_eq!(fav["channels"][0]["key"], k);
    let found = c
        .get(&format!(
            "{base}/search?q={}",
            cat.live[0].name.split_whitespace().next().unwrap()
        ))
        .await
        .ok();
    assert!(!arr(&found["channels"]).is_empty());
    app.shutdown().await;
}

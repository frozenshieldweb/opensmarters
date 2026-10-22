// SPDX-License-Identifier: GPL-3.0-or-later
//! Real streams through ffmpeg (skipped with a message when ffmpeg / ffprobe aren't on
//! PATH): HLS playlist and segments, owner-only access, limits, and that stopping —
//! by `DELETE` or by the idle timer — kills ffmpeg and closes the provider connection
//! (counted by a TCP proxy in front of the fake panel).

use std::net::SocketAddr;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

use crate::common::*;

fn ffmpeg_available() -> bool {
    let ok = |p: &str| {
        Command::new(p)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let found = ok("ffmpeg") && ok("ffprobe");
    if !found {
        eprintln!("SKIPPED: ffmpeg and ffprobe must be on PATH for the streaming tests");
    }
    found
}

fn process_alive(pid: u32) -> bool {
    if cfg!(windows) {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!(" {pid} ")))
    } else {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

async fn media_panel(quirks: &str) -> fake_panel::Handle {
    let config = fake_panel::Config {
        quirks: quirks.parse().unwrap(),
        ..fake_panel::Config::default()
    };
    let panel = fake_panel::spawn(config).await.unwrap();
    assert_eq!(
        panel.media_status(),
        fake_panel::MediaStatus::Ready,
        "test media"
    );
    panel
}

/// Fetches the playlist and one segment; returns the playlist text.
async fn fetch_hls(c: &Client, url: &str) -> String {
    let pl = c.get(url).await;
    assert_eq!(pl.status, 200);
    assert_eq!(pl.header("content-type"), "application/vnd.apple.mpegurl");
    assert_eq!(pl.header("cache-control"), "no-store");
    let text = String::from_utf8(pl.bytes).unwrap();
    assert!(text.starts_with("#EXTM3U"), "{text}");
    assert!(text.contains("#EXT-X-MAP:URI=\"init.mp4\""), "{text}");
    assert!(
        !text.contains("http"),
        "the playlist names only local files: {text}"
    );
    let dir = url.trim_end_matches("index.m3u8");
    let init = c.get(&format!("{dir}init.mp4")).await;
    assert_eq!(
        (init.status, init.header("content-type").as_str()),
        (200, "video/mp4")
    );
    let seg = text
        .lines()
        .find(|l| l.starts_with("seg-"))
        .expect("a segment")
        .to_owned();
    let s = c.get(&format!("{dir}{seg}")).await;
    assert_eq!(s.status, 200);
    assert!(s.bytes.len() > 1000);
    // The pair is playable media (fragmented MP4 with H.264 and AAC).
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("probe.mp4");
    let mut joined = init.bytes.clone();
    joined.extend_from_slice(&s.bytes);
    std::fs::write(&file, joined).unwrap();
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let codecs = String::from_utf8_lossy(&out.stdout);
    assert!(
        codecs.contains("h264") && codecs.contains("aac"),
        "{codecs}"
    );
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_stream_lifecycle() {
    if !ffmpeg_available() {
        return;
    }
    let panel = media_panel("live-endless").await;
    let proxy = proxy(panel.addr()).await;
    let list = format!(
        "#EXTM3U\n#EXTINF:-1 group-title=\"T\",Counted\nhttp://{}/live/demo/demo/1001.ts\n#EXTINF:-1 group-title=\"T\",Other\nhttp://{}/live/demo/demo/1002.ts\n",
        proxy.addr, proxy.addr
    );
    let pl: SocketAddr =
        serve(Router::new().route("/list.m3u", get(move || async move { list }))).await;
    let app = app_with(&[panel.addr(), proxy.addr, pl], |c| {
        c.stream_idle_timeout = Duration::from_secs(3);
    })
    .await;
    let c = admin(&app).await;
    let a = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": format!("http://{pl}/list.m3u")}),
        )
        .await
        .ok();
    let account = a["id"].as_str().unwrap().to_owned();
    let channels = c
        .get(&format!("/api/accounts/{account}/live/channels"))
        .await
        .ok();
    let keys: Vec<String> = channels
        .as_array()
        .unwrap()
        .iter()
        .map(|ch| ch["key"].as_str().unwrap().to_owned())
        .collect();
    let play = |key: &str| json!({"account": account, "kind": "live", "id": key, "hevc": false});

    let r = c.post("/api/play", play(&keys[0])).await.ok();
    let sid = r["stream"].as_str().unwrap().to_owned();
    assert_eq!(r["url"], format!("/api/streams/{sid}/index.m3u8"));
    assert_eq!(r["live"], true);
    assert!(r["duration"].is_null());
    assert_eq!(r["start"], 0.0);
    let url = r["url"].as_str().unwrap().to_owned();
    fetch_hls(&c, &url).await;
    assert_eq!(
        proxy.active.load(Ordering::SeqCst),
        1,
        "one provider connection"
    );
    let stream = app.server.state.streams.get(&sid).unwrap();
    let pid = stream.ffmpeg_pid().expect("ffmpeg runs");
    assert!(process_alive(pid));
    let dir = stream.dir.clone();
    assert!(dir.join("index.m3u8").exists());

    // Only the owner can reach it.
    let bob = invite(&app, &c, "bob", "bob's password").await;
    assert_eq!(bob.get(&url).await.status, 404);
    assert_eq!(bob.delete(&format!("/api/streams/{sid}")).await.status, 404);
    assert_eq!(
        c.get(&format!("/api/streams/{sid}/..%2Fsecret"))
            .await
            .status,
        404
    );
    assert_eq!(
        c.get(&format!("/api/streams/{sid}/index.m3u8.tmp"))
            .await
            .status,
        404
    );

    // The same person in another browser: one stream per person.
    let other = app.client();
    other
        .post(
            "/api/login",
            json!({"name": ADMIN, "password": ADMIN_PASSWORD}),
        )
        .await
        .ok();
    let r = other.post("/api/play", play(&keys[1])).await;
    assert_eq!((r.status, r.code()), (503, "limit_reached"), "{}", r.json);
    assert!(
        r.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another device")
    );

    // DELETE: ffmpeg killed, provider connection closed, folder removed.
    assert_eq!(
        c.delete(&format!("/api/streams/{sid}")).await.ok(),
        json!({})
    );
    assert!(
        eventually(10, || proxy.active.load(Ordering::SeqCst) == 0).await,
        "the provider connection stayed open"
    );
    assert!(
        eventually(10, || !process_alive(pid)).await,
        "ffmpeg still runs"
    );
    assert!(!dir.exists(), "the stream folder is still there");
    assert_eq!(c.get(&url).await.status, 404);

    // Zapping in the same browser replaces the stream (never two connections).
    let first = c.post("/api/play", play(&keys[0])).await.ok();
    let second = c.post("/api/play", play(&keys[1])).await.ok();
    let (s1, s2) = (
        first["stream"].as_str().unwrap(),
        second["stream"].as_str().unwrap(),
    );
    assert_ne!(s1, s2);
    assert!(app.server.state.streams.get(s1).is_none());
    assert!(eventually(10, || proxy.active.load(Ordering::SeqCst) == 1).await);
    let pid2 = app
        .server
        .state
        .streams
        .get(s2)
        .unwrap()
        .ffmpeg_pid()
        .unwrap();

    // Idle stop: nobody fetches it for 3 s.
    assert!(
        eventually(20, || app.server.state.streams.get(s2).is_none()).await,
        "the idle stream wasn't stopped"
    );
    assert!(
        eventually(10, || proxy.active.load(Ordering::SeqCst) == 0).await,
        "the provider connection stayed open"
    );
    assert!(
        eventually(10, || !process_alive(pid2)).await,
        "ffmpeg still runs after the idle stop"
    );
    assert_eq!(app.server.state.streams.count(), 0);
    assert!(
        proxy.total.load(Ordering::SeqCst) >= 3,
        "every start opened its own connection"
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xtream_film_episode_and_hls_source() {
    if !ffmpeg_available() {
        return;
    }
    let panel = media_panel("").await;
    let cat = panel.catalogue().clone();
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    let account = add_xtream(&c, &panel).await;

    // A film from 10 s in (resume).
    let movie = cat
        .movies
        .iter()
        .find(|m| m.container_extension == "mp4")
        .unwrap();
    let r = c
        .post("/api/play", json!({"account": account, "kind": "movie", "id": movie.stream_id.to_string(), "start": 10, "hevc": false}))
        .await
        .ok();
    assert_eq!(r["live"], false);
    assert_eq!(r["start"], 10.0);
    assert!(r["duration"].as_f64().unwrap() > 0.0);
    let text = fetch_hls(&c, r["url"].as_str().unwrap()).await;
    assert!(text.contains("#EXT-X-PLAYLIST-TYPE:EVENT"), "{text}");

    // An episode (its series is found from the series details).
    let series = &cat.series[0];
    let episode = series.seasons[0].episodes[0].id;
    c.get(&format!(
        "/api/accounts/{account}/series/{}",
        series.series_id
    ))
    .await
    .ok();
    let r = c
        .post("/api/play", json!({"account": account, "kind": "episode", "id": episode.to_string(), "hevc": false}))
        .await
        .ok();
    assert_eq!(r["live"], false);
    fetch_hls(&c, r["url"].as_str().unwrap()).await;

    // A live channel whose provider stream is HLS (walked by the server, piped to ffmpeg).
    c.put("/api/settings", Some(json!({"live_format": "m3u8"})))
        .await
        .ok();
    let r = c
        .post("/api/play", json!({"account": account, "kind": "live", "id": cat.live[0].stream_id.to_string(), "hevc": false}))
        .await
        .ok();
    assert_eq!(r["live"], true);
    fetch_hls(&c, r["url"].as_str().unwrap()).await;
    let sid = r["stream"].as_str().unwrap().to_owned();
    c.delete(&format!("/api/streams/{sid}")).await.ok();
    assert_eq!(app.server.state.streams.count(), 0);

    // Errors: unknown kinds and items; catch-up needs its parameters.
    let r = c
        .post(
            "/api/play",
            json!({"account": account, "kind": "radio", "id": "1", "hevc": false}),
        )
        .await;
    assert_eq!(r.status, 400);
    let r = c
        .post(
            "/api/play",
            json!({"account": account, "kind": "live", "id": "abc", "hevc": false}),
        )
        .await;
    assert_eq!(r.status, 404);
    let r = c
        .post(
            "/api/play",
            json!({"account": account, "kind": "catchup", "id": "1001", "hevc": false}),
        )
        .await;
    assert_eq!(r.status, 400);
    let missing: Value = c
        .post(
            "/api/play",
            json!({"account": account, "kind": "live", "id": "999999", "hevc": false}),
        )
        .await
        .json;
    assert_eq!(missing["error"]["code"], "provider_error", "{missing}");
    app.shutdown().await;
}

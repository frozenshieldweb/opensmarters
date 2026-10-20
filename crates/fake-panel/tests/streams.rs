//! Stream endpoint tests. With ffmpeg available the media is generated on first use into
//! `<workspace>/testdata/generated`; without it every stream endpoint must answer 503.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use fake_panel::users::SPECIAL_PASSWORD;
use fake_panel::{Config, Handle, MediaMode, MediaStatus, StartFormat};
use serde_json::Value;

async fn spawn_media() -> Handle {
    fake_panel::spawn(Config::default()).await.expect("spawn")
}

fn ready(h: &Handle) -> bool {
    match h.media_status() {
        MediaStatus::Ready => true,
        other => {
            eprintln!("media not ready ({other:?}); checking 503 behaviour instead");
            false
        }
    }
}

fn live_url(h: &Handle, id: u64, ext: &str) -> String {
    format!("{}/live/demo/demo/{id}.{ext}", h.base_url())
}

#[tokio::test]
async fn live_ts_full_and_range() {
    let h = spawn_media().await;
    let url = live_url(&h, 1001, "ts");
    let res = get(&url).await;
    if !ready(&h) {
        assert_eq!(res.status(), 503);
        assert!(res.text().await.unwrap().contains("unavailable"));
        return;
    }
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "video/mp2t");
    assert_eq!(res.headers()["accept-ranges"], "bytes");
    let full = res.bytes().await.unwrap();
    assert!(full.len() > 100_000);
    assert_eq!(full[0], 0x47, "MPEG-TS sync byte");
    assert_eq!(full[188], 0x47);

    let part = client()
        .get(&url)
        .header("Range", "bytes=0-187")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(
        part.headers()["content-range"].to_str().unwrap(),
        format!("bytes 0-187/{}", full.len())
    );
    let body = part.bytes().await.unwrap();
    assert_eq!(&body[..], &full[..188]);

    let tail = client()
        .get(&url)
        .header("Range", "bytes=188-")
        .send()
        .await
        .unwrap();
    assert_eq!(tail.status(), 206);
    assert_eq!(tail.bytes().await.unwrap().len(), full.len() - 188);

    let head = client().head(&url).send().await.unwrap();
    assert_eq!(head.status(), 200);

    // Channels map onto different clips.
    let other = get(&live_url(&h, 1002, "ts")).await.bytes().await.unwrap();
    assert_ne!(other.len(), full.len());

    // Short form `/{u}/{p}/{id}` and extensionless ids.
    let short = get(&format!("{}/demo/demo/1001", h.base_url())).await;
    assert_eq!(short.status(), 200);
    assert_eq!(short.bytes().await.unwrap().len(), full.len());

    // The request log saw the Range header.
    assert!(
        h.requests()
            .iter()
            .any(|r| r.range.as_deref() == Some("bytes=0-187"))
    );
}

#[tokio::test]
async fn live_hls_playlist_and_segments() {
    let h = spawn_media().await;
    let res = get(&live_url(&h, 1003, "m3u8")).await;
    if !ready(&h) {
        assert_eq!(res.status(), 503);
        return;
    }
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()["content-type"],
        "application/vnd.apple.mpegurl"
    );
    let playlist = res.text().await.unwrap();
    assert!(playlist.starts_with("#EXTM3U"));
    assert!(playlist.contains("#EXT-X-ENDLIST"));
    let segs: Vec<&str> = playlist
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .collect();
    assert!(segs.len() >= 4, "{playlist}");
    assert!(segs.iter().all(|s| s.starts_with("/hls/")));
    let seg = get(&h.url(segs[0])).await;
    assert_eq!(seg.status(), 200);
    assert_eq!(seg.bytes().await.unwrap()[0], 0x47);

    let token = segs[0].split('/').nth(2).unwrap();
    let bad_token = segs[0].replace(token, "0000000000000000");
    assert_eq!(get(&h.url(&bad_token)).await.status(), 403);
    let traversal = format!("/hls/{token}/0/..%2F..%2Fmovie.mp4");
    assert_eq!(get(&h.url(&traversal)).await.status(), 404);
    assert_eq!(
        get(&h.url(&format!("/hls/{token}/9/seg-000.ts")))
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn stream_credentials_and_ids() {
    let h = spawn_media().await;
    let b = h.base_url().to_owned();
    for path in [
        "/live/demo/wrong/1001.ts",
        "/movie/demo/wrong/200001.mkv",
        "/series/demo/wrong/300001.mkv",
        "/timeshift/demo/wrong/60/2026-01-01:10-00/1001.ts",
        "/live/expired/expired/1001.ts",
        "/live/banned/banned/1001.ts",
        "/live/nobody/nothing/1001.m3u8",
    ] {
        assert_eq!(get(&format!("{b}{path}")).await.status(), 403, "{path}");
    }
    for path in [
        "/live/demo/demo/999999.ts",
        "/movie/demo/demo/1.mkv",
        "/series/demo/demo/1.mkv",
        "/live/demo/demo/1001.mp4",
    ] {
        assert_eq!(get(&format!("{b}{path}")).await.status(), 404, "{path}");
    }

    // Percent-encoded special password in the path segment.
    let enc_url = format!("{b}/live/special/{}/1001.ts", enc(SPECIAL_PASSWORD));
    let res = get(&enc_url).await;
    let want = if ready(&h) { 200 } else { 503 };
    assert_eq!(res.status(), want);
    assert!(
        h.requests()
            .iter()
            .any(|r| r.path.contains("/special/p%40ss%2Fw%23rd%20%25%26%2B/"))
    );
    // `+` is literal in a path, so leaving it raw also works.
    let plus = format!("{b}/live/special/p%40ss%2Fw%23rd%20%25%26+/1001.ts");
    assert_eq!(get(&plus).await.status(), want);
    // An unencoded `/` splits the segment: no route matches.
    let raw = format!("{b}/live/special/p@ss/w%23rd%20%25%26%2B/1001.ts");
    assert_eq!(get(&raw).await.status(), 404);
}

#[tokio::test]
async fn movie_and_episode_files() {
    let h = spawn_media().await;
    let cat = h.catalogue();
    let mkv = cat
        .movies
        .iter()
        .find(|m| m.container_extension == "mkv")
        .unwrap();
    let mp4 = cat
        .movies
        .iter()
        .find(|m| m.container_extension == "mp4")
        .unwrap();
    let (_, ep) = cat.episodes().next().unwrap();
    let b = h.base_url().to_owned();
    let mkv_url = format!("{b}/movie/demo/demo/{}.mkv", mkv.stream_id);
    let res = get(&mkv_url).await;
    if !ready(&h) {
        assert_eq!(res.status(), 503);
        return;
    }
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "video/x-matroska");
    let bytes = res.bytes().await.unwrap();
    assert_eq!(&bytes[..4], &[0x1A, 0x45, 0xDF, 0xA3], "EBML magic");

    let part = client()
        .get(&mkv_url)
        .header("Range", "bytes=1000-1999")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(&part.bytes().await.unwrap()[..], &bytes[1000..2000]);

    let res = get(&format!("{b}/movie/demo/demo/{}.mp4", mp4.stream_id)).await;
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "video/mp4");
    assert_eq!(&res.bytes().await.unwrap()[4..8], b"ftyp");

    let res = get(&format!(
        "{b}/series/demo/demo/{}.{}",
        ep.id, ep.container_extension
    ))
    .await;
    assert_eq!(res.status(), 200);
}

/// `YYYY-MM-DD HH:MM:SS` → (`YYYY-MM-DD:HH-MM`, `YYYY-MM-DD:HH:MM:SS`).
fn both_formats(start: &str) -> (String, String) {
    let dash = format!("{}:{}-{}", &start[..10], &start[11..13], &start[14..16]);
    let colon = start.replacen(' ', ":", 1);
    (dash, colon)
}

#[tokio::test]
async fn timeshift_accepts_both_start_formats() {
    let h = spawn_media().await;
    let ch = h
        .catalogue()
        .live
        .iter()
        .find(|c| c.tv_archive && c.epg_channel_id.is_some())
        .unwrap()
        .clone();
    let table = api(
        &h,
        &format!("&action=get_simple_data_table&stream_id={}", ch.stream_id),
    )
    .await;
    let prog = table["epg_listings"]
        .as_array()
        .unwrap()
        .iter()
        // Avoid the repeated hour of an autumn DST change (ambiguous local time).
        .find(|p| p["has_archive"] == 1 && p["start"].as_str().is_some_and(|s| &s[11..13] != "02"))
        .unwrap()
        .clone();
    let start = prog["start"].as_str().unwrap();
    let start_ts = num(&prog["start_timestamp"]).unwrap();
    let minutes = (num(&prog["stop_timestamp"]).unwrap() - start_ts) / 60;
    let (dash, colon) = both_formats(start);
    let b = h.base_url().to_owned();
    let want = if ready(&h) { 200 } else { 503 };

    h.clear_requests();
    let u1 = format!(
        "{b}/timeshift/demo/demo/{minutes}/{dash}/{}.ts",
        ch.stream_id
    );
    assert_eq!(get(&u1).await.status(), want, "{u1}");
    // Smarters style: non-rounded duration, colon-separated seconds.
    let u2 = format!(
        "{b}/timeshift/demo/demo/{minutes}.5/{colon}/{}.ts",
        ch.stream_id
    );
    assert_eq!(get(&u2).await.status(), want, "{u2}");
    // Percent-encoded colons decode to the same thing.
    let u3 = format!(
        "{b}/timeshift/demo/demo/{minutes}/{}/{}.m3u8",
        colon.replace(':', "%3A"),
        ch.stream_id
    );
    assert_eq!(get(&u3).await.status(), want, "{u3}");
    // Query form.
    let u4 = format!(
        "{b}/streaming/timeshift.php?username=demo&password=demo&stream={}&start={dash}&duration={minutes}",
        ch.stream_id
    );
    assert_eq!(get(&u4).await.status(), want, "{u4}");

    let recs: Vec<_> = h
        .requests()
        .into_iter()
        .filter_map(|r| r.timeshift)
        .collect();
    assert_eq!(recs.len(), 4);
    assert_eq!(recs[0].start_format, StartFormat::DashMinutes);
    assert_eq!(recs[1].start_format, StartFormat::ColonSeconds);
    assert_eq!(recs[2].start_format, StartFormat::ColonSeconds);
    assert_eq!(recs[3].start_format, StartFormat::DashMinutes);
    assert_eq!(recs[3].via, "timeshift.php");
    assert_eq!(recs[2].extension, "m3u8");
    for r in &recs {
        assert_eq!(r.start_timestamp, start_ts, "{r:?}");
        assert_eq!(r.start_local, start);
        assert_eq!(r.stream_id, ch.stream_id);
    }
    assert!((recs[1].duration_minutes - (minutes as f64 + 0.5)).abs() < 1e-9);
    assert_eq!(recs[1].duration_raw, format!("{minutes}.5"));

    // The same data over HTTP.
    let log = get_json(&format!(
        "{b}/__debug/last-requests?path_prefix=/timeshift/"
    ))
    .await;
    let fmts: Vec<&Value> = log["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["timeshift"]["start_format"])
        .collect();
    assert_eq!(
        fmts,
        vec![
            "YYYY-MM-DD:HH-MM",
            "YYYY-MM-DD:HH:MM:SS",
            "YYYY-MM-DD:HH:MM:SS"
        ]
    );

    // Rejections.
    for bad in [
        "2026-10-20 10:00:00",
        "2026-10-20:10:00",
        "1792486800",
        "garbage",
    ] {
        let u = format!(
            "{b}/timeshift/demo/demo/60/{}/{}.ts",
            enc(bad),
            ch.stream_id
        );
        assert_eq!(get(&u).await.status(), 400, "{bad}");
    }
    let u = format!("{b}/timeshift/demo/demo/abc/{dash}/{}.ts", ch.stream_id);
    assert_eq!(get(&u).await.status(), 400);
    let no_archive = h
        .catalogue()
        .live
        .iter()
        .find(|c| !c.tv_archive)
        .unwrap()
        .stream_id;
    let u = format!("{b}/timeshift/demo/demo/60/{dash}/{no_archive}.ts");
    assert_eq!(get(&u).await.status(), 404);
}

#[tokio::test]
async fn ffprobe_sees_multitrack_movie_over_http() {
    let h = spawn_media().await;
    if !ready(&h) {
        return;
    }
    let mkv = h
        .catalogue()
        .movies
        .iter()
        .find(|m| m.container_extension == "mkv")
        .unwrap();
    let url = format!("{}/movie/demo/demo/{}.mkv", h.base_url(), mkv.stream_id);
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "csv=p=0",
                &url,
            ])
            .output()
    })
    .await
    .unwrap();
    let Ok(out) = out else {
        eprintln!("ffprobe not available; skipping");
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let types = String::from_utf8_lossy(&out.stdout);
    let types: Vec<&str> = types.lines().map(str::trim).collect();
    assert_eq!(types, vec!["video", "audio", "audio", "subtitle"]);
}

#[tokio::test]
async fn missing_media_answers_503() {
    let dir = std::env::temp_dir().join(format!("fake-panel-empty-{}", std::process::id()));
    let cfg = Config {
        media_dir: dir.clone(),
        media: MediaMode::Never,
        ..Config::default()
    };
    let h = fake_panel::spawn(cfg).await.unwrap();
    assert!(matches!(h.media_status(), MediaStatus::Unavailable(_)));
    for path in [
        "/live/demo/demo/1001.ts",
        "/live/demo/demo/1001.m3u8",
        "/movie/demo/demo/200001.mkv",
        "/series/demo/demo/300001.mkv",
    ] {
        let res = get(&h.url(path)).await;
        assert_eq!(res.status(), 503, "{path}");
        let msg = res.text().await.unwrap();
        assert!(msg.contains("unavailable"), "{msg}");
    }
    // Auth still comes first.
    assert_eq!(get(&h.url("/live/demo/bad/1001.ts")).await.status(), 403);
    assert!(!dir.exists(), "Never mode must not create the directory");
}

#[tokio::test]
async fn recording_quirks() {
    let q: fake_panel::Quirks = "slow-media,live-endless".parse().unwrap();
    let h = fake_panel::spawn(Config::default().with_quirks(q))
        .await
        .expect("spawn");
    if !ready(&h) {
        return;
    }
    let b = h.base_url().to_owned();
    let mkv = h
        .catalogue()
        .movies
        .iter()
        .find(|m| m.container_extension == "mkv")
        .unwrap()
        .stream_id;
    // slow-media: still ranged, but paced (~256 KiB/s).
    let url = format!("{b}/movie/demo/demo/{mkv}.mkv");
    let started = std::time::Instant::now();
    let part = client()
        .get(&url)
        .header("Range", "bytes=1000-")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert!(
        part.headers()["content-range"]
            .to_str()
            .unwrap()
            .starts_with("bytes 1000-")
    );
    let mut part = part;
    let mut got = 0usize;
    while got < 512 * 1024 {
        match part.chunk().await.unwrap() {
            Some(c) => got += c.len(),
            None => break,
        }
    }
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(1500),
        "512 KiB should take about 2 s"
    );
    // live-endless: keeps sending past the clip's end, starting with TS packets.
    let mut live = get(&live_url(&h, 1001, "ts")).await;
    assert_eq!(live.status(), 200);
    assert!(
        live.content_length().is_none(),
        "an endless stream has no length"
    );
    let first = live.chunk().await.unwrap().unwrap();
    assert_eq!(first[0], 0x47);
    drop(live);

    // media-html / media-playlist: 200 OK, but not video.
    let q: fake_panel::Quirks = "media-html".parse().unwrap();
    let h = fake_panel::spawn(Config::default().with_quirks(q))
        .await
        .expect("spawn");
    let res = get(&format!("{}/movie/demo/demo/{mkv}.mkv", h.base_url())).await;
    assert_eq!(res.status(), 200);
    assert!(res.text().await.unwrap().contains("<html>"));
    let res = get(&live_url(&h, 1001, "ts")).await;
    assert!(res.text().await.unwrap().contains("<html>"));
    let q: fake_panel::Quirks = "media-playlist".parse().unwrap();
    let h = fake_panel::spawn(Config::default().with_quirks(q))
        .await
        .expect("spawn");
    let res = get(&format!("{}/movie/demo/demo/{mkv}.mkv", h.base_url())).await;
    assert_eq!(res.status(), 200);
    assert!(res.text().await.unwrap().starts_with("#EXTM3U"));
}

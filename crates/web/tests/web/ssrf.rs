// SPDX-License-Identifier: GPL-3.0-or-later
//! SSRF refusals: typed addresses, redirects, and addresses inside playlists, HLS
//! manifests and image fields. The "evil" host is allowed (it plays a provider on the
//! internet); the victim is a loopback service that must never be reached.

use std::sync::atomic::Ordering;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::{Value, json};

use crate::common::*;

fn redirect(to: String) -> impl IntoResponse {
    (StatusCode::FOUND, [(header::LOCATION, to)], "")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_addresses_are_refused() {
    let (victim, hits) = victim().await;
    let panel = panel().await;
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    let port = panel.port();
    let vport = victim.port();
    for server in [
        format!("http://{victim}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
        format!("http://[::ffff:127.0.0.1]:{vport}"),
        format!("http://[::ffff:7f00:1]:{vport}"),
        format!("http://127.0.0.2:{port}"),
        format!("http://2130706433:{vport}"),
        format!("http://0.0.0.0:{port}"),
        "http://10.0.0.1".to_owned(),
        "http://192.168.1.1:8080".to_owned(),
        "http://172.17.0.1".to_owned(),
        "http://100.64.0.1".to_owned(),
        "http://169.254.169.254".to_owned(),
        "http://[fd00::1]".to_owned(),
        "http://[fe80::1]".to_owned(),
        "http://[::ffff:10.0.0.1]".to_owned(),
        "http://224.0.0.1".to_owned(),
    ] {
        let r = c
            .post(
                "/api/accounts",
                json!({"kind": "xtream", "server": server, "username": "demo", "password": "demo"}),
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (400, "blocked_address"),
            "{server}: {}",
            r.json
        );
        let r = c
            .post("/api/accounts", json!({"kind": "m3u", "m3u_url": format!("{server}/get.php?username=demo&password=demo")}))
            .await;
        assert_eq!(
            (r.status, r.code()),
            (400, "blocked_address"),
            "{server}: {}",
            r.json
        );
    }
    // The EPG address is checked too.
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": panel.url("/get.php?username=demo&password=demo"), "epg_url": format!("http://{victim}/epg.xml")}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "blocked_address"), "{}", r.json);
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the victim was reached");
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_supplied_addresses_are_refused() {
    let (victim, hits) = victim().await;
    let vport = victim.port();
    // The hostile provider: its own address is fine, everything it points to isn't.
    let evil_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let evil = evil_listener.local_addr().unwrap();
    let playlist = format!(
        "#EXTM3U\n\
         #EXTINF:-1 tvg-logo=\"http://{evil}/logo-redirect.png\" group-title=\"Evil\",Loopback\nhttp://{victim}/live.ts\n\
         #EXTINF:-1 tvg-logo=\"http://{victim}/logo.png\" group-title=\"Evil\",Private\nhttp://10.255.255.1/live.ts\n\
         #EXTINF:-1 group-title=\"Evil\",Mapped\nhttp://[::ffff:127.0.0.1]:{vport}/live.ts\n\
         #EXTINF:-1 group-title=\"Evil\",Unique local\nhttp://[fd00::1]/live.ts\n\
         #EXTINF:-1 group-title=\"Evil\",Redirect\nhttp://{evil}/redirect.ts\n\
         #EXTINF:-1 group-title=\"Evil\",Manifest\nhttp://{evil}/hls/index.m3u8\n\
         #EXTINF:-1 group-title=\"Evil\",Master\nhttp://{evil}/hls/master.m3u8\n"
    );
    let media = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.0,\nhttp://{victim}/seg-0.ts\n#EXTINF:4.0,\nhttp://10.0.0.7/seg-1.ts\n#EXT-X-ENDLIST\n"
    );
    let master =
        format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000\nhttp://{victim}/variant.m3u8\n");
    let router = Router::new()
        .route("/list.m3u", get(move || async move { playlist }))
        .route(
            "/hls/index.m3u8",
            get(move || async move {
                (
                    [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
                    media,
                )
            }),
        )
        .route("/hls/master.m3u8", get(move || async move { master }))
        .route(
            "/redirect.ts",
            get(move || async move { redirect(format!("http://{victim}/stream.ts")) }),
        )
        .route(
            "/logo-redirect.png",
            get(move || async move { redirect(format!("http://{victim}/logo.png")) }),
        )
        .route(
            "/xtream/player_api.php",
            get(move || async move {
                redirect(format!(
                    "http://{victim}/player_api.php?username=demo&password=demo"
                ))
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(evil_listener, router).await;
    });

    let app = app(&[evil]).await;
    let c = admin(&app).await;

    // A redirect to a private address while checking an account.
    let r = c
        .post("/api/accounts", json!({"kind": "xtream", "server": format!("http://{evil}/xtream"), "username": "demo", "password": "demo"}))
        .await;
    assert_eq!((r.status, r.code()), (400, "blocked_address"), "{}", r.json);

    // The playlist host itself is allowed.
    let a = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": format!("http://{evil}/list.m3u")}),
        )
        .await
        .ok();
    let base = format!("/api/accounts/{}", a["id"].as_str().unwrap());
    let channels = c
        .get(&format!("{base}/live/channels?category=all"))
        .await
        .ok();
    let list: Vec<Value> = channels.as_array().unwrap().clone();
    assert_eq!(list.len(), 7);
    for ch in &list {
        let name = ch["name"].as_str().unwrap();
        let r = c
            .post(
                "/api/play",
                json!({"account": a["id"], "kind": "live", "id": ch["key"], "hevc": false}),
            )
            .await;
        assert_eq!(r.status, 502, "{name}: {}", r.json);
        assert_eq!(r.code(), "blocked_address", "{name}: {}", r.json);
    }
    // Artwork pointing at (or redirecting to) the victim.
    for ch in &list {
        if let Some(logo) = ch["logo"].as_str() {
            let r = c.get(logo).await;
            assert_eq!(
                (r.status, r.code()),
                (502, "image_unavailable"),
                "{}",
                ch["name"]
            );
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the victim was reached");
    app.shutdown().await;
}

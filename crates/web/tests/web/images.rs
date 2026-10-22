// SPDX-License-Identifier: GPL-3.0-or-later
//! The image proxy: only tokens this server issued, images only, size limits, cache.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::header;
use axum::routing::get;
use serde_json::json;

use crate::common::*;

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h))
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn image_proxy() {
    let ok_hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&ok_hits);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = listener.local_addr().unwrap();
    let names = ["ok", "octet", "html", "svg", "big", "wide", "notimage"];
    let mut playlist = String::from("#EXTM3U\n");
    for n in names {
        playlist.push_str(&format!(
            "#EXTINF:-1 tvg-logo=\"http://{host}/{n}.png\" group-title=\"Images\",{n}\nhttp://{host}/{n}.ts\n"
        ));
    }
    let small = png(8, 6);
    let small2 = small.clone();
    let router =
        Router::new()
            .route("/list.m3u", get(move || async move { playlist }))
            .route(
                "/ok.png",
                get(move || {
                    let h = Arc::clone(&h);
                    let body = small.clone();
                    async move {
                        h.fetch_add(1, Ordering::SeqCst);
                        ([(header::CONTENT_TYPE, "image/png")], body)
                    }
                }),
            )
            .route(
                "/octet.png",
                get(move || async move {
                    ([(header::CONTENT_TYPE, "application/octet-stream")], small2)
                }),
            )
            .route(
                "/html.png",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/html")],
                        "<html><script>alert(1)</script></html>",
                    )
                }),
            )
            .route(
                "/svg.png",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "image/svg+xml")],
                        "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
                    )
                }),
            )
            .route(
                "/big.png",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "image/png")],
                        vec![0x89u8; 6 * 1024 * 1024],
                    )
                }),
            )
            .route(
                "/wide.png",
                get(|| async { ([(header::CONTENT_TYPE, "image/png")], png(9000, 1)) }),
            )
            .route(
                "/notimage.png",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "image/png")],
                        "definitely not a png",
                    )
                }),
            );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let app = app(&[host]).await;
    let c = admin(&app).await;
    let a = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": format!("http://{host}/list.m3u")}),
        )
        .await
        .ok();
    let channels = c
        .get(&format!(
            "/api/accounts/{}/live/channels",
            a["id"].as_str().unwrap()
        ))
        .await
        .ok();
    let logos: HashMap<String, String> = channels
        .as_array()
        .unwrap()
        .iter()
        .map(|ch| {
            (
                ch["name"].as_str().unwrap().to_owned(),
                ch["logo"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(logos.len(), names.len());
    for logo in logos.values() {
        assert!(
            !logo.contains(&host.to_string()),
            "the provider's address leaks: {logo}"
        );
    }

    let ok = c.get(&logos["ok"]).await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.header("content-type"), "image/png");
    assert_eq!(ok.header("x-content-type-options"), "nosniff");
    assert!(
        ok.header("content-security-policy")
            .starts_with("default-src 'none'")
    );
    assert_eq!(ok.bytes, png(8, 6));
    // Cached: the provider isn't asked again.
    assert_eq!(c.get(&logos["ok"]).await.status, 200);
    assert_eq!(ok_hits.load(Ordering::SeqCst), 1);
    // A generic binary type is fine when the bytes are an image (served as image/png).
    let octet = c.get(&logos["octet"]).await;
    assert_eq!(
        (octet.status, octet.header("content-type").as_str()),
        (200, "image/png")
    );

    for bad in ["html", "svg", "big", "wide", "notimage"] {
        let r = c.get(&logos[bad]).await;
        assert_eq!((r.status, r.code()), (502, "image_unavailable"), "{bad}");
    }

    // Only this server's tokens: tampered, forged and foreign tokens are 404.
    let token = logos["ok"].trim_start_matches("/api/img/").to_owned();
    let mut chars: Vec<char> = token.chars().collect();
    let i = chars.len() / 2;
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    let tampered: String = chars.into_iter().collect();
    assert_eq!(c.get(&format!("/api/img/{tampered}")).await.status, 404);
    assert_eq!(
        c.get("/api/img/aHR0cDovL2V4YW1wbGUuY29tL3gucG5n")
            .await
            .status,
        404
    );
    let other = app_with(&[host], |_| {}).await;
    let oc = admin(&other).await;
    assert_eq!(
        oc.get(&logos["ok"]).await.status,
        404,
        "another instance's key"
    );
    // A session is needed.
    assert_eq!(app.client().get(&logos["ok"]).await.status, 401);
    other.shutdown().await;
    app.shutdown().await;
}

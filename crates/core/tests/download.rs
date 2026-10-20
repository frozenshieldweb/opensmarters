// SPDX-License-Identifier: GPL-3.0-or-later
//! The streaming downloader against local test servers: Range resume and its checks,
//! the redirect policy, timeouts and the URL scheme guard.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use opensmarters_core::Error;
use opensmarters_core::download::{DownloadConfig, DownloadRequest, Downloader};
use opensmarters_core::redact::Redactor;
use opensmarters_core::url::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn body() -> Vec<u8> {
    (0..1000u32).map(|i| (i % 251) as u8).collect()
}

fn downloader(read_timeout: Duration) -> Downloader {
    Downloader::new(
        &DownloadConfig {
            read_timeout,
            connect_timeout: Duration::from_secs(5),
            ..DownloadConfig::default()
        },
        Redactor::new(),
    )
    .unwrap()
}

fn at(offset: u64) -> DownloadRequest<'static> {
    DownloadRequest {
        offset,
        ..DownloadRequest::default()
    }
}

async fn read_all(mut d: opensmarters_core::download::Download) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(c) = d.chunk().await.unwrap() {
        out.extend_from_slice(&c);
    }
    out
}

/// A server that honours `Range: bytes=n-` like a well-behaved panel.
async fn ranged_server() -> MockServer {
    let server = MockServer::start().await;
    let data = body();
    Mock::given(method("GET"))
        .and(path("/movie/u/p/1.mkv"))
        .respond_with(move |req: &Request| {
            let range = req
                .headers
                .get("range")
                .and_then(|v| v.to_str().ok())
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.strip_suffix('-'))
                .and_then(|n| n.parse::<usize>().ok());
            match range {
                Some(n) if n < data.len() => ResponseTemplate::new(206)
                    .insert_header(
                        "content-range",
                        format!("bytes {n}-{}/{}", data.len() - 1, data.len()),
                    )
                    .set_body_bytes(data[n..].to_vec()),
                Some(_) => ResponseTemplate::new(416)
                    .insert_header("content-range", format!("bytes */{}", data.len())),
                None => ResponseTemplate::new(200).set_body_bytes(data.clone()),
            }
        })
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn full_download_and_range_resume() {
    let server = ranged_server().await;
    let url = Url::parse(&format!("{}/movie/u/p/1.mkv", server.uri())).unwrap();
    let dl = downloader(Duration::from_secs(5));

    let d = dl.open(&url, &at(0)).await.unwrap();
    assert_eq!(d.start(), 0);
    assert_eq!(d.total(), Some(1000));
    assert_eq!(read_all(d).await, body());

    // Resume at 600: the rest only, with the total from Content-Range.
    let d = dl.open(&url, &at(600)).await.unwrap();
    assert_eq!(d.start(), 600);
    assert_eq!(d.total(), Some(1000));
    assert_eq!(read_all(d).await, body()[600..].to_vec());
    let sent = server.received_requests().await.unwrap();
    assert_eq!(
        sent.last()
            .and_then(|r| r.headers.get("range"))
            .and_then(|v| v.to_str().ok()),
        Some("bytes=600-")
    );
    assert_eq!(
        sent.last()
            .and_then(|r| r.headers.get("accept-encoding"))
            .and_then(|v| v.to_str().ok()),
        Some("identity"),
        "no transparent decompression: offsets must be the file's own"
    );

    // Past the end: 416 with the size, so the caller knows the file is complete.
    match dl.open(&url, &at(1000)).await {
        Err(Error::RangeNotSatisfiable { total }) => assert_eq!(total, Some(1000)),
        other => panic!("expected 416, got {other:?}"),
    }
}

#[tokio::test]
async fn ignored_and_mismatched_ranges() {
    let server = MockServer::start().await;
    let data = body();
    // Ignores Range: the whole file again (start over).
    Mock::given(path("/ignores"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
        .mount(&server)
        .await;
    // Answers 206 with the wrong part.
    Mock::given(path("/wrong-part"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-999/1000")
                .set_body_bytes(data.clone()),
        )
        .mount(&server)
        .await;
    // 206 without any Content-Range.
    Mock::given(path("/no-range-header"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(data[500..].to_vec()))
        .mount(&server)
        .await;
    // Content-Range that doesn't match the body length.
    Mock::given(path("/short-body"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 500-999/1000")
                .set_body_bytes(data[500..600].to_vec()),
        )
        .mount(&server)
        .await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let dl = downloader(Duration::from_secs(5));
    let u = |p: &str| Url::parse(&format!("{}{p}", server.uri())).unwrap();

    let d = dl.open(&u("/ignores"), &at(500)).await.unwrap();
    assert_eq!(d.start(), 0, "the server ignored the range: start over");
    assert_eq!(read_all(d).await.len(), 1000);
    for p in ["/wrong-part", "/no-range-header", "/short-body"] {
        assert!(
            matches!(dl.open(&u(p), &at(500)).await, Err(Error::RangeMismatch)),
            "{p}"
        );
    }
    assert!(matches!(
        dl.open(&u("/missing"), &at(0)).await,
        Err(Error::Http { status: 404 })
    ));
}

#[tokio::test]
async fn redirect_policy() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(path("/to-ftp"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "ftp://files.example/movie.mkv"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/to-file"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "file:///C:/x.mkv"))
        .mount(&server)
        .await;
    Mock::given(path("/hop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/final"))
        .mount(&server)
        .await;
    Mock::given(path("/final"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x47data".to_vec()))
        .mount(&server)
        .await;
    // An endless chain: /loop/n → /loop/n+1.
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"^/loop/\d+$"))
        .respond_with(|req: &Request| {
            let n: u32 = req
                .url
                .path()
                .rsplit('/')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            ResponseTemplate::new(302).insert_header("location", format!("/loop/{}", n + 1))
        })
        .mount(&server)
        .await;
    let dl = downloader(Duration::from_secs(5));
    let u = |p: &str| Url::parse(&format!("{base}{p}")).unwrap();

    let d = dl.open(&u("/hop"), &at(0)).await.unwrap();
    assert!(d.url().as_str().ends_with("/final"));
    assert_eq!(read_all(d).await, b"\x47data");
    // Refused by the policy (a network error), or not followed at all (reqwest hands
    // back the redirect itself for a `file:` location, which is not a success).
    for p in ["/to-ftp", "/to-file", "/loop/0"] {
        let e = dl.open(&u(p), &at(0)).await;
        assert!(
            matches!(
                e,
                Err(Error::Network { .. } | Error::Http { status: 300..=399 })
            ),
            "{p}: {e:?}"
        );
    }
    // Five hops are fine, the sixth is refused.
    let hops = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/loop/"))
        .count();
    assert_eq!(hops, 6, "1 request + 5 redirects followed");
    // Only web addresses can be opened at all.
    assert!(matches!(
        dl.open(&Url::parse("file:///C:/Windows/win.ini").unwrap(), &at(0))
            .await,
        Err(Error::InvalidUrl { .. })
    ));
}

/// A raw server: answers each connection with `head` (if any), then stays silent.
async fn stalling_server(head: Option<&'static [u8]>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                if let Some(h) = head {
                    let _ = sock.write_all(h).await;
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
                drop(sock);
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn read_idle_timeouts() {
    let dl = downloader(Duration::from_millis(400));
    // No answer at all.
    let silent = stalling_server(None).await;
    let started = std::time::Instant::now();
    let e = dl
        .open(&Url::parse(&format!("{silent}/x.ts")).unwrap(), &at(0))
        .await;
    assert!(matches!(e, Err(Error::Timeout)), "{e:?}");
    assert!(started.elapsed() < Duration::from_secs(10));

    // Headers and a few bytes, then nothing: the next read times out.
    let stalls = stalling_server(Some(
        b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nContent-Type: video/mp2t\r\n\r\nGGGG",
    ))
    .await;
    let mut d = dl
        .open(&Url::parse(&format!("{stalls}/live.ts")).unwrap(), &at(0))
        .await
        .unwrap();
    let mut got = 0usize;
    let err = loop {
        match d.chunk().await {
            Ok(Some(c)) => got += c.len(),
            Ok(None) => panic!("the body can't end early"),
            Err(e) => break e,
        }
    };
    assert_eq!(got, 4);
    assert!(matches!(err, Error::Timeout), "{err:?}");
}

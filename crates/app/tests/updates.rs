// SPDX-License-Identifier: GPL-3.0-or-later
//! "Check for updates" against the fake panel's GitHub-shaped
//! `/__updates/releases/latest` and a raw local socket (never the real GitHub API):
//! available / up to date / drafts / hostile tags / 404 / 403 / 429 / oversized body /
//! offline, the exact request headers, and the service's state handling.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::services::updates::{
    self, CURRENT, CheckError, Origin, Outcome, STATE, Source, Status, Updates, Version,
};
use opensmarters_app::services::{Paths, Services};
use opensmarters_core::Redactor;
use opensmarters_store::{MemorySecretStore, Store, StoreOptions};

const RELEASE_PAGE: &str = "https://github.com/frozenshieldweb/opensmarters/releases/tag/v";

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn fake_source(panel: &fake_panel::Handle, query: &str) -> Source {
    Source::custom(
        &panel.url(&format!("/__updates/releases/latest{query}")),
        true,
    )
    .unwrap()
}

#[test]
fn fake_panel_answers_map_to_outcomes() {
    let rt = rt();
    let panel = rt
        .block_on(fake_panel::spawn(fake_panel::Config::api_only()))
        .unwrap();
    let cur = v("0.2.0");
    let run = |q: &str| {
        rt.block_on(updates::check(
            &fake_source(&panel, q),
            &cur,
            Redactor::new(),
        ))
    };

    assert_eq!(
        run("?tag=v0.3.0"),
        Ok(Outcome::Available {
            version: v("0.3.0"),
            url: Some(format!("{RELEASE_PAGE}0.3.0")),
        }),
        "the link is built locally, never the answer's html_url"
    );
    assert_eq!(
        run("?tag=0.2.1"),
        Ok(Outcome::Available {
            version: v("0.2.1"),
            url: Some(format!("{RELEASE_PAGE}0.2.1")),
        }),
        "v-less tag"
    );
    assert_eq!(
        run("?tag=v0.2.0"),
        Ok(Outcome::UpToDate {
            latest: Some(v("0.2.0"))
        })
    );
    assert_eq!(
        run("?tag=v0.1.9"),
        Ok(Outcome::UpToDate {
            latest: Some(v("0.1.9"))
        })
    );
    assert_eq!(
        run("?tag=v9.0.0&draft=true"),
        Ok(Outcome::UpToDate { latest: None })
    );
    assert_eq!(
        run("?tag=v9.0.0&prerelease=true"),
        Ok(Outcome::UpToDate { latest: None })
    );
    // Hostile or malformed tags.
    for tag in [
        "v1.0.0%22%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E",
        "latest",
        "v1.0",
        "v99999999999.0.0",
        &format!("v1.0.0-{}", "a".repeat(80)),
    ] {
        assert_eq!(
            run(&format!("?tag={tag}")),
            Err(CheckError::Failed),
            "{tag}"
        );
    }
    assert_eq!(run("?status=404"), Err(CheckError::Failed));
    assert_eq!(run("?status=500"), Err(CheckError::Failed));
    assert_eq!(run("?status=403"), Err(CheckError::RateLimited));
    assert_eq!(run("?status=429"), Err(CheckError::RateLimited));
    assert_eq!(
        run("?big=1"),
        Err(CheckError::Failed),
        "over the 256 KiB cap"
    );
    // The default tag is one minor version ahead of this build.
    assert!(matches!(
        rt.block_on(updates::check(
            &fake_source(&panel, ""),
            &Version::current().unwrap(),
            Redactor::new()
        )),
        Ok(Outcome::Available { .. })
    ));
    // Every request said who it is (GitHub requires a User-Agent) and nothing else
    // identifying (see `the_request_carries_only_these_headers`).
    let reqs: Vec<_> = panel
        .requests()
        .into_iter()
        .filter(|r| r.path == "/__updates/releases/latest")
        .collect();
    assert!(reqs.len() >= 15);
    for r in reqs {
        assert_eq!(r.method, "GET");
        assert_eq!(
            r.user_agent.as_deref(),
            Some(format!("opensmarters/{CURRENT}").as_str())
        );
    }
}

#[test]
fn unreachable_means_offline() {
    let rt = rt();
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let src = Source::custom(&format!("http://127.0.0.1:{port}/releases/latest"), true).unwrap();
    assert_eq!(
        rt.block_on(updates::check(&src, &v("0.2.0"), Redactor::new())),
        Err(CheckError::Offline)
    );
}

/// One request on a raw socket: the request line and headers exactly as sent.
#[test]
fn the_request_carries_only_these_headers() {
    let rt = rt();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = r#"{"tag_name":"v0.0.1","html_url":"https://evil.example.invalid/"}"#;
        write!(
            s,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        String::from_utf8(buf).unwrap()
    });
    let src = Source::custom(
        &format!("http://127.0.0.1:{port}/repos/o/r/releases/latest"),
        true,
    )
    .unwrap();
    let out = rt.block_on(updates::check(&src, &v("0.2.0"), Redactor::new()));
    assert_eq!(
        out,
        Ok(Outcome::UpToDate {
            latest: Some(v("0.0.1"))
        })
    );
    let request = server.join().unwrap();
    let mut lines = request.split("\r\n");
    assert_eq!(
        lines.next(),
        Some("GET /repos/o/r/releases/latest HTTP/1.1")
    );
    let headers: BTreeMap<String, String> = lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (k, val) = l.split_once(':').unwrap();
            (k.trim().to_ascii_lowercase(), val.trim().to_owned())
        })
        .collect();
    assert_eq!(
        headers.keys().map(String::as_str).collect::<Vec<_>>(),
        ["accept", "accept-encoding", "host", "user-agent"],
        "{request}"
    );
    assert_eq!(headers["accept"], "application/vnd.github+json");
    assert_eq!(headers["user-agent"], format!("opensmarters/{CURRENT}"));
    assert_eq!(headers["host"], format!("127.0.0.1:{port}"));
}

fn services(rt: &tokio::runtime::Runtime) -> Services {
    let store = Arc::new(
        Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    );
    Services::new(
        rt.handle().clone(),
        store,
        Redactor::new(),
        Paths::default(),
        true,
        egui::Context::default(),
    )
}

fn wait_idle(u: &Updates) {
    let t0 = Instant::now();
    while u.view().checking {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "check never finished"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_service_checks_remembers_and_dismisses() {
    let rt = rt();
    let svc = services(&rt);
    let panel = rt
        .block_on(fake_panel::spawn(fake_panel::Config::api_only()))
        .unwrap();

    // A manual check that fails shows its error; nothing to announce.
    let u = Updates::with_source(Some(fake_source(&panel, "?status=429")));
    assert!(u.supported());
    assert_eq!(u.view().status, Status::NeverChecked);
    u.start(&svc, Origin::Manual);
    wait_idle(&u);
    assert_eq!(u.view().status, Status::Failed(CheckError::RateLimited));
    assert!(u.state().last_failure.is_some());

    // An automatic check that finds a newer version: indicator + one toast.
    let u = Updates::with_source(Some(fake_source(&panel, "?tag=v99.0.0")));
    u.start(&svc, Origin::Automatic);
    wait_idle(&u);
    assert_eq!(u.indicator(), Some(v("99.0.0")));
    assert_eq!(u.take_toast(), Some(v("99.0.0")));
    assert_eq!(u.take_toast(), None);
    assert!(matches!(
        u.view().status,
        Status::Available {
            dismissed: false,
            ..
        }
    ));
    // The same version found again isn't announced twice.
    u.start(&svc, Origin::Automatic);
    wait_idle(&u);
    assert_eq!(u.take_toast(), None);

    // "Not now": no indicator; the About page still says it exists.
    u.dismiss(&svc);
    assert_eq!(u.indicator(), None);
    assert!(matches!(
        u.view().status,
        Status::Available {
            dismissed: true,
            ..
        }
    ));

    // The state reached the store (machine-local key).
    let stored = rt
        .block_on(async {
            svc.write("flush", |_| Ok(())).await.unwrap();
            svc.read(|s| s.settings().get(&STATE)).await
        })
        .unwrap();
    assert_eq!(stored.latest_seen.as_deref(), Some("99.0.0"));
    assert_eq!(stored.dismissed.as_deref(), Some("99.0.0"));
    assert!(stored.last_success.is_some());
    assert!(
        opensmarters_app::services::backup::LOCAL_ONLY_KEYS.contains(&STATE.key()),
        "never exported"
    );
}

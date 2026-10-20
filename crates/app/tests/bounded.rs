// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider load stays bounded (docs/PROGRESS.md: real panels allow few connections and
//! ban floods): paging fast through Live TV with a slow panel fires only a handful of
//! EPG requests, the rows finally on screen fill first, a closed (replaced) session stops
//! talking to its provider, and store writes land in order.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::services::epg::EpgState;
use opensmarters_app::services::session::guide::ROW_FETCHES;
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_app::sessions::SessionManager;
use opensmarters_core::Redactor;
use opensmarters_core::chrono::Utc;
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::settings::Setting;
use opensmarters_store::{
    MemorySecretStore, NewProfile, NewProfileSource, ProfileId, ProfileSettings, Store,
    StoreOptions,
};

struct Env {
    rt: tokio::runtime::Runtime,
    services: Services,
    panel: fake_panel::Handle,
    id: ProfileId,
}

fn env(slow: bool) -> Env {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let quirks = fake_panel::Quirks {
        slow,
        ..fake_panel::Quirks::default()
    };
    let panel = rt
        .block_on(fake_panel::spawn(
            fake_panel::Config::api_only().with_quirks(quirks),
        ))
        .unwrap();
    let store = Arc::new(
        Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    );
    let services = Services::new(
        rt.handle().clone(),
        store,
        Redactor::new(),
        Paths::default(),
        true,
        egui::Context::default(),
    );
    let id = services
        .store
        .profiles()
        .create(NewProfile {
            name: "Test".into(),
            source: NewProfileSource::Xtream {
                server_url: panel.base_url().to_owned(),
                username: "demo".into(),
                password: SecretString::from("demo".to_owned()),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id;
    Env {
        rt,
        services,
        panel,
        id,
    }
}

fn open(env: &Env) -> Arc<Session> {
    env.rt
        .block_on(Session::open(env.services.clone(), env.id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error))
}

fn epg_requests(panel: &fake_panel::Handle) -> usize {
    panel
        .requests()
        .iter()
        .filter(|r| {
            r.query
                .as_deref()
                .is_some_and(|q| q.contains("get_short_epg") || q.contains("get_simple_data_table"))
        })
        .count()
}

#[test]
fn paging_fast_through_live_tv_keeps_epg_requests_bounded() {
    let env = env(true); // 0–3 s latency per request
    let session = open(&env);
    let cat = env.rt.block_on(session.live_catalog()).unwrap();
    let rows = 6;
    let pages = 40;
    assert!(cat.channels.len() >= rows * pages);
    env.panel.clear_requests();
    let now = Utc::now();
    // One frame per page, like holding Page Down.
    for page in 0..pages {
        for ch in &cat.channels[page * rows..(page + 1) * rows] {
            let _ = session.now_next(ch, now);
        }
        std::thread::sleep(Duration::from_millis(16));
        let s = session.epg_queue_stats();
        assert!(s.running <= ROW_FETCHES && s.workers <= ROW_FETCHES);
    }
    // Then the last page stays on screen until every row is filled.
    let visible = &cat.channels[(pages - 1) * rows..pages * rows];
    let t0 = Instant::now();
    loop {
        // Like a screen: every visible row is asked for every frame (no short-circuit,
        // or the rows not asked for would rightly be dropped as "scrolled away").
        let states: Vec<EpgState> = visible.iter().map(|ch| session.epg(ch)).collect();
        if states.iter().all(|s| !matches!(s, EpgState::Loading)) {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(25),
            "visible rows still loading: {:?}",
            session.epg_queue_stats()
        );
        std::thread::sleep(Duration::from_millis(16));
    }
    let filled = t0.elapsed();
    let stats = session.epg_queue_stats();
    let sent = epg_requests(&env.panel);
    eprintln!(
        "paged {} rows: {sent} EPG requests, peak {} at once, {} dropped, visible rows filled in {filled:?}",
        rows * pages,
        stats.peak_running,
        stats.dropped
    );
    assert!(stats.peak_running <= ROW_FETCHES);
    // Before: one request per row that was ever on screen (240 here, ~470 when paging
    // 40 pages of a real list). Now: the visible rows plus what was in flight.
    assert!(
        sent <= rows + 3 * ROW_FETCHES,
        "{sent} EPG requests for {rows} visible rows"
    );
    assert!(stats.dropped > 0, "rows paged past were dropped");
    // A cached row is served without a request.
    let before = epg_requests(&env.panel);
    for ch in visible {
        assert!(matches!(
            session.epg(ch),
            EpgState::Ready(_) | EpgState::Missing
        ));
    }
    assert_eq!(epg_requests(&env.panel), before);
}

#[test]
fn guide_and_now_next_share_one_cache() {
    let env = env(false);
    let session = open(&env);
    let cat = env.rt.block_on(session.live_catalog()).unwrap();
    let ch = cat
        .channels
        .iter()
        .find(|c| c.epg_id.is_some())
        .cloned()
        .unwrap();
    env.panel.clear_requests();
    let row = loop {
        if let opensmarters_app::services::session::guide::RowState::Ready(r) =
            session.guide_row(&ch)
        {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(row.full);
    let sent = epg_requests(&env.panel);
    // Now/next for the same channel is served from the guide row: no new request.
    assert!(matches!(session.epg(&ch), EpgState::Ready(_)));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(epg_requests(&env.panel), sent);
}

#[test]
fn a_replaced_session_stops_calling_its_provider() {
    let env = env(true);
    let old = open(&env);
    let cat = env.rt.block_on(old.live_catalog()).unwrap();
    // Queue many rows (kept "on screen" for a moment).
    let now = Utc::now();
    for _ in 0..5 {
        for ch in cat.channels.iter().take(60) {
            let _ = old.now_next(ch, now);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(old.epg_queue_stats().pending > 0);
    let mut manager = SessionManager::new();
    manager.set_current(Arc::clone(&old));
    let new = open(&env);
    manager.set_current(Arc::clone(&new));
    assert!(old.is_closed() && !new.is_closed());
    let at_close = epg_requests(&env.panel);
    assert_eq!(old.epg_queue_stats().pending, 0, "queued work dropped");
    // In-flight requests finish (at most one per worker); nothing new starts.
    std::thread::sleep(Duration::from_secs(4));
    for ch in cat.channels.iter().take(60) {
        let _ = old.now_next(ch, now);
    }
    std::thread::sleep(Duration::from_millis(200));
    let after = epg_requests(&env.panel);
    assert!(
        after <= at_close + ROW_FETCHES,
        "{} requests after close",
        after - at_close
    );
    let s = old.epg_queue_stats();
    assert_eq!((s.pending, s.running), (0, 0));
    // The new session works normally.
    assert!(!new.is_closed());
}

const ORDER: Setting<Vec<u32>> = Setting::new("test.write_order", Vec::new);

#[test]
fn background_writes_land_in_submission_order() {
    let env = env(false);
    // Read-modify-write jobs (like search history or the last category): on an
    // unordered pool some would overwrite others.
    for i in 0..200u32 {
        env.services.blocking("test write", move |s| {
            let mut v = s.settings().get(&ORDER)?;
            v.push(i);
            s.settings().set(&ORDER, &v)
        });
    }
    let got = env
        .rt
        .block_on(env.services.write("read", |s| s.settings().get(&ORDER)))
        .unwrap();
    assert_eq!(got, (0..200).collect::<Vec<_>>());
    // A failing job is reported as a background error; later writes still run.
    env.services.blocking("fail on purpose", |_| {
        Err(opensmarters_store::StoreError::ProfileNotFound(
            ProfileId::new_random(),
        ))
    });
    env.services
        .blocking("panic on purpose", |_| panic!("write panicked on purpose"));
    let got = env
        .rt
        .block_on(
            env.services
                .write("read again", |s| s.settings().get(&ORDER)),
        )
        .unwrap();
    assert_eq!(got.len(), 200);
    let errors = env.services.take_background_errors();
    assert!(errors.contains(&"fail on purpose"));
    assert!(errors.contains(&"panic on purpose"));
}

#[test]
fn a_panicking_background_task_is_reported() {
    let env = env(false);
    let h = env.services.spawn(async {
        panic!("background task panicked on purpose");
    });
    let r = env.rt.block_on(h);
    assert!(r.is_err_and(|e| e.is_panic()));
    let errors = env.services.take_background_errors();
    assert_eq!(errors, vec![opensmarters_app::services::INTERNAL_FAILURE]);
    assert!(
        opensmarters_app::services::background_error_message(errors[0])
            .starts_with("Something went wrong")
    );
}

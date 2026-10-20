// SPDX-License-Identifier: GPL-3.0-or-later
//! Guide and catch-up session extensions against the in-process fake panel: lazy guide
//! rows, archive days, both timeshift formats, the session/profile format choice, and
//! the bounded XMLTV guide (EPG URL override) agreeing with the API schedule.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::screens::catchup::archive_days;
use opensmarters_app::services::catalog::LiveChannel;
use opensmarters_app::services::session::guide::{RowState, XmltvStatus};
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_core::Redactor;
use opensmarters_core::chrono::Utc;
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{
    MemorySecretStore, NewProfile, NewProfileSource, ProfileId, ProfileSettings, Store,
    StoreOptions, TimeshiftFormat,
};

struct Env {
    rt: tokio::runtime::Runtime,
    services: Services,
    panel: fake_panel::Handle,
}

fn env() -> Env {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let panel = rt
        .block_on(fake_panel::spawn(fake_panel::Config::api_only()))
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
    Env {
        rt,
        services,
        panel,
    }
}

fn profile(env: &Env) -> ProfileId {
    env.services
        .store
        .profiles()
        .create(NewProfile {
            name: "Test".into(),
            source: NewProfileSource::Xtream {
                server_url: env.panel.base_url().to_owned(),
                username: "demo".into(),
                password: SecretString::from("demo".to_owned()),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id
}

fn open(env: &Env, id: ProfileId) -> Arc<Session> {
    env.rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error))
}

fn archive_channel(env: &Env, session: &Arc<Session>) -> LiveChannel {
    let cat = env.rt.block_on(session.live_catalog()).unwrap();
    cat.channels
        .iter()
        .find(|c| c.archive && c.epg_id.is_some())
        .cloned()
        .expect("the fake panel has archive channels")
}

/// Polls `f` until it returns `Some`, for at most 10 s.
fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn archive_days_and_both_timeshift_formats() {
    let env = env();
    let session = open(&env, profile(&env));
    let ch = archive_channel(&env, &session);
    let row = env.rt.block_on(session.full_row(&ch)).unwrap();
    assert!(row.full);
    let now = Utc::now();
    assert!(row.has_any_archive(now));
    let days = archive_days(&row, &session.server_time_zone(), now);
    assert!(!days.is_empty());
    let p = days[0].programmes.last().expect("a replayable programme");

    // Standard: YYYY-MM-DD:HH-MM; Smarters: YYYY-MM-DD:HH:MM:SS (server-local start).
    let local = p.start_local;
    let std_req = session
        .catchup_request_with(&ch, p, TimeshiftFormat::Standard, None)
        .unwrap();
    let smarters = session
        .catchup_request_with(&ch, p, TimeshiftFormat::SmartersCompat, Some(12.0))
        .unwrap();
    let std_url = std_req.url.as_str();
    assert!(
        std_url.contains(&local.format("/%Y-%m-%d:%H-%M/").to_string()),
        "standard start in the path"
    );
    assert!(
        smarters
            .url
            .as_str()
            .contains(&local.format("/%Y-%m-%d:%H:%M:%S/").to_string())
    );
    assert_eq!(smarters.resume_from, Some(12.0));
    // Resume points are looked up with the id the player saves them under.
    assert_eq!(
        std_req.item.progress_id(),
        Some(opensmarters_app::services::session::catchup::progress_id(
            ch.stream_id().unwrap(),
            p
        ))
    );
    assert!(std_url.contains(&format!("/{}/", p.duration_minutes())));

    // The session choice overrides the profile until it is saved.
    assert_eq!(session.timeshift_format(), TimeshiftFormat::Standard);
    session.set_timeshift_format(TimeshiftFormat::SmartersCompat, false);
    assert_eq!(session.timeshift_format(), TimeshiftFormat::SmartersCompat);
    assert_eq!(session.saved_timeshift_format(), TimeshiftFormat::Standard);
    session.set_timeshift_format(TimeshiftFormat::SmartersCompat, true);
    assert_eq!(
        session.saved_timeshift_format(),
        TimeshiftFormat::SmartersCompat
    );
    let store = Arc::clone(&env.services.store);
    let pid = session.profile_id();
    wait_for(|| {
        let p = store.profiles().get(pid).ok()??;
        (p.settings.timeshift_format == TimeshiftFormat::SmartersCompat).then_some(())
    });
}

#[test]
fn guide_rows_load_lazily_and_are_cached() {
    let env = env();
    let session = open(&env, profile(&env));
    assert!(!session.guide_uses_xmltv());
    let cat = env.rt.block_on(session.live_catalog()).unwrap();
    let ch = cat
        .channels
        .iter()
        .find(|c| c.epg_id.is_some())
        .cloned()
        .unwrap();
    let row = wait_for(|| match session.guide_row(&ch) {
        RowState::Ready(r) => Some(r),
        _ => None,
    });
    assert!(!row.programmes.is_empty());
    for w in row.programmes.windows(2) {
        assert!(w[0].stop <= w[1].start, "no overlaps");
    }
    assert!(
        session
            .cached_guide_rows()
            .iter()
            .any(|(k, _)| *k == ch.key)
    );
    // A channel without a guide ends up Missing, not Loading forever.
    if let Some(no_epg) = cat.channels.iter().find(|c| c.epg_id.is_none()) {
        wait_for(|| match session.guide_row(no_epg) {
            RowState::Missing => Some(()),
            _ => None,
        });
    }
}

#[test]
fn xmltv_override_matches_the_api_schedule() {
    let env = env();
    let id = profile(&env);
    let url = SecretString::from(format!(
        "{}/xmltv.xml.gz",
        env.panel.base_url().trim_end_matches('/')
    ));
    env.services
        .store
        .profiles()
        .set_epg_url_override(id, Some(&url))
        .unwrap();
    let session = open(&env, id);
    assert!(session.guide_uses_xmltv());
    let ch = archive_channel(&env, &session);
    session.start_guide();
    let status = wait_for(|| match session.xmltv_status() {
        s @ (XmltvStatus::Ready { .. } | XmltvStatus::Failed(_)) => Some(s),
        _ => None,
    });
    let XmltvStatus::Ready {
        programmes,
        truncated,
        ..
    } = status
    else {
        panic!("guide failed: {status:?}");
    };
    assert!(programmes > 0 && !truncated);
    let xml_row = wait_for(|| match session.guide_row(&ch) {
        RowState::Ready(r) => Some(r),
        _ => None,
    });
    // Same instants from both sources (XMLTV +HHMM offsets vs API timestamps).
    let api_row = env.rt.block_on(session.full_row(&ch)).unwrap();
    let now = Utc::now();
    let x = xml_row
        .programmes
        .iter()
        .find(|p| p.start > now)
        .expect("upcoming programme in XMLTV");
    let a = api_row
        .programmes
        .iter()
        .find(|p| p.start == x.start)
        .expect("same start in the API schedule");
    assert_eq!(a.stop, x.stop);
    assert_eq!(a.title, x.title);
    // The override URL carries no secrets here, but it is registered with the redactor.
    assert!(session.loaded_xmltv().is_some());
}

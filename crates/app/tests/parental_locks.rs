// SPDX-License-Identifier: GPL-3.0-or-later
//! Parental locks at the point of use (SECURITY.md SR-02) against the in-process fake
//! panel: the request builders refuse locked items with a typed error, every category of
//! an item counts, unknown categories are looked up (fail closed meanwhile), lists can be
//! filtered, and an unlock lets the retry through.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::error::AppError;
use opensmarters_app::services::session::ItemLock;
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_core::secrecy::SecretString;
use opensmarters_core::{CategoryId, Redactor};
use opensmarters_store::{
    CategoryLock, ContentKind, ItemSnapshot, MemorySecretStore, NewProfile, NewProfileSource,
    PinHashParams, PlaybackKind, PlaybackSnapshot, ProfileId, ProfileSettings, Store, StoreOptions,
};

struct Env {
    rt: tokio::runtime::Runtime,
    services: Services,
    id: ProfileId,
    _panel: fake_panel::Handle,
}

fn env(pin: bool) -> Env {
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
            StoreOptions::in_memory().with_pin_hash(PinHashParams {
                m_cost_kib: 64,
                t_cost: 1,
                p_cost: 1,
            }),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    );
    if pin {
        store
            .parental()
            .set_pin(&SecretString::from("1234"))
            .unwrap();
    }
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
        id,
        _panel: panel,
    }
}

fn open(env: &Env) -> Arc<Session> {
    env.rt
        .block_on(Session::open(env.services.clone(), env.id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error))
}

fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn live_requests_refuse_locked_channels_until_unlocked() {
    let env = env(true);
    let session = open(&env);
    let cat = env.rt.block_on(session.live_catalog()).unwrap();
    let adult = CategoryId::from("7"); // "Adult Night": keyword-locked
    let locked = cat
        .channels
        .iter()
        .find(|c| c.categories.contains(&adult))
        .cloned()
        .unwrap();
    let news = cat
        .channels
        .iter()
        .find(|c| c.categories == vec![CategoryId::from("1")])
        .cloned()
        .unwrap();
    assert!(session.live_request(&news, None).is_ok());
    let err = session.live_request(&locked, None).unwrap_err();
    let l = err.locked().expect("a typed parental error");
    assert_eq!(l.kind, ContentKind::Live);
    assert_eq!(l.category.as_ref(), Some(&adult));
    assert_eq!(l.category_name.as_deref(), Some("Adult Night"));
    assert!(err.user_message().contains("Adult Night"));

    // A channel listed under an open category *and* a locked one is locked (all
    // categories count, not only the primary one).
    let mut both = news.clone();
    both.categories = vec![CategoryId::from("1"), adult.clone()];
    assert!(session.is_item_locked(ContentKind::Live, &both.categories));
    assert_eq!(
        session.locked_category(ContentKind::Live, &both.categories),
        Some(adult.clone())
    );
    assert!(matches!(
        session.live_request(&both, None),
        Err(AppError::Locked(_))
    ));
    // Catch-up of a locked channel is refused too.
    let programme = opensmarters_core::ArchiveProgramme {
        title: "T".into(),
        description: String::new(),
        start: opensmarters_core::chrono::Utc::now()
            - opensmarters_core::chrono::Duration::hours(2),
        stop: opensmarters_core::chrono::Utc::now() - opensmarters_core::chrono::Duration::hours(1),
        start_local: opensmarters_core::chrono::Utc::now().naive_utc(),
    };
    assert!(matches!(
        session.catchup_request(&locked, &programme, None),
        Err(AppError::Locked(_))
    ));
    // Lists can be filtered with the same rule.
    let visible = session.filter_unlocked([&news, &locked, &both]);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].key, news.key);

    // PIN → unlock → retry.
    session.unlock_category(ContentKind::Live, &adult);
    assert!(session.live_request(&locked, None).is_ok());
    assert!(session.live_request(&both, None).is_ok());
    session.relock_all();
    assert!(session.live_request(&locked, None).is_err());
}

#[test]
fn manual_locks_apply_after_reload_and_no_pin_means_no_locks() {
    let env = env(true);
    let session = open(&env);
    let cat = env.rt.block_on(session.live_catalog()).unwrap();
    let news = cat
        .channels
        .iter()
        .find(|c| c.categories == vec![CategoryId::from("1")])
        .cloned()
        .unwrap();
    assert!(session.live_request(&news, None).is_ok());
    env.services
        .store
        .parental()
        .set_category_lock(env.id, ContentKind::Live, "1", Some(CategoryLock::Locked))
        .unwrap();
    env.rt.block_on(session.reload_parental());
    assert!(session.live_request(&news, None).is_err());

    let free = self::env(false);
    let s2 = open(&free);
    let cat2 = free.rt.block_on(s2.live_catalog()).unwrap();
    for ch in &cat2.channels {
        assert!(
            s2.live_request(ch, None).is_ok(),
            "no PIN: nothing is locked"
        );
    }
}

#[test]
fn films_are_checked_by_the_index_and_unknown_ones_are_looked_up() {
    let env = env(true);
    let session = open(&env);
    // Nothing loaded yet: the film's categories are unknown → refused (fail closed), a
    // look-up starts in the background.
    let locked_cat = CategoryId::from("109"); // "18+ After Dark"
    let films = {
        // Ask the panel directly which films are in the locked category (no session
        // list is loaded).
        let probe = open(&env);
        env.rt
            .block_on(probe.vod_streams(Some(locked_cat.clone())))
            .unwrap()
    };
    let film = films[0].clone();
    let fresh = open(&env);
    let err = fresh
        .movie_request(film.stream_id, None, &film.name, None, None, None)
        .unwrap_err();
    let l = err.locked().expect("typed").clone();
    assert!(l.is_unverified(), "unknown categories are not guessed");
    // verify_item looks the film up (cached get_vod_info + categories) and now names the
    // category.
    let verified = env
        .rt
        .block_on(fresh.verify_item(ContentKind::Movie, &film.stream_id.to_string()));
    let l = verified.unwrap_err().locked().cloned().expect("typed");
    assert_eq!(l.category.as_ref(), Some(&locked_cat));
    // The background look-up alone also settles it.
    let other = films.get(1).cloned().unwrap_or(film.clone());
    let _ = fresh.movie_request(other.stream_id, None, &other.name, None, None, None);
    wait_for(
        || match fresh.item_lock(ContentKind::Movie, &other.stream_id.to_string(), None) {
            ItemLock::Locked { .. } => Some(()),
            _ => None,
        },
    );

    // A film from an open category plays once its list and the category names (for
    // keyword locks) are known; before the names arrive it is not guessed.
    let open_films = env
        .rt
        .block_on(session.vod_streams(Some(CategoryId::from("101"))))
        .unwrap();
    let ok = &open_films[0];
    let early = session.movie_request(ok.stream_id, None, &ok.name, None, None, None);
    assert!(early.is_err_and(|e| e.locked().is_some_and(|l| l.is_unverified())));
    env.rt.block_on(session.vod_categories()).unwrap();
    assert!(
        session
            .movie_request(ok.stream_id, None, &ok.name, None, None, None)
            .is_ok()
    );
    // Lists of films filter on every category id.
    let mut mixed = (*open_films).clone();
    mixed[0].category_ids = vec![CategoryId::from("101"), locked_cat.clone()];
    session.retain_unlocked(&mut mixed);
    assert_eq!(mixed.len(), open_films.len() - 1);

    // Continue watching rebuilds go through the same check.
    let pb = env.services.store.playback();
    let p = pb
        .record_progress(
            env.id,
            PlaybackKind::Movie,
            &film.stream_id.to_string(),
            120.0,
            Some(3600.0),
            &PlaybackSnapshot {
                title: film.name.clone(),
                ..PlaybackSnapshot::default()
            },
        )
        .unwrap();
    assert!(matches!(fresh.resume_request(&p), Err(AppError::Locked(_))));
    assert!(fresh.is_locked(&p));
    fresh.unlock_category(ContentKind::Movie, &locked_cat);
    assert!(fresh.resume_request(&p).is_ok());

    // Favourites without categories are resolved through the index too.
    let fav_ok = opensmarters_store::Favourite {
        kind: ContentKind::Movie,
        item_id: ok.stream_id.to_string(),
        position: 0,
        added_at: opensmarters_store::Timestamp::now(),
        snapshot: ItemSnapshot::named("x"),
    };
    assert!(!session.is_locked(&fav_ok));
}

#[test]
fn locked_artwork_is_registered_as_private() {
    let env = env(true);
    let session = open(&env);
    // Lists loaded before the category names: conservatively private (memory only).
    let early = open(&env);
    let early_films = env
        .rt
        .block_on(early.vod_streams(Some(CategoryId::from("102"))))
        .unwrap();
    let url = early_films
        .iter()
        .find_map(|f| f.stream_icon.clone())
        .unwrap();
    assert!(env.services.images.is_private(&url));
    env.rt.block_on(session.vod_categories()).unwrap();
    let films = env
        .rt
        .block_on(session.vod_streams(Some(CategoryId::from("109"))))
        .unwrap();
    let url = films.iter().find_map(|f| f.stream_icon.clone()).unwrap();
    assert!(env.services.images.is_private(&url), "18+ category art");
    let open_films = env
        .rt
        .block_on(session.vod_streams(Some(CategoryId::from("101"))))
        .unwrap();
    let url = open_films
        .iter()
        .find_map(|f| f.stream_icon.clone())
        .unwrap();
    assert!(!env.services.images.is_private(&url), "open category art");
}

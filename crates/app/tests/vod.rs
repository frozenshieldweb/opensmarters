// SPDX-License-Identifier: GPL-3.0-or-later
//! Movies/series against the in-process fake panel: request building (titles, URLs,
//! resume), watch state, mark watched/unwatched, the series "Continue" target, the next
//! episode (specials, gaps) and the remembered browse category.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::playback::{MediaKind, PlaybackItem};
use opensmarters_app::services::vod::{self, ContinueKind};
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_core::Redactor;
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{
    ContentKind, MemorySecretStore, NewProfile, NewProfileSource, PlaybackKind, PlaybackSnapshot,
    ProfileSettings, Store, StoreOptions,
};

struct Env {
    rt: tokio::runtime::Runtime,
    services: Services,
    session: Arc<Session>,
    _panel: fake_panel::Handle,
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
    let session = rt
        .block_on(Session::open(services.clone(), id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error));
    Env {
        rt,
        services,
        session,
        _panel: panel,
    }
}

#[test]
fn episode_requests_progress_watched_and_continue() {
    let env = env();
    let s = &env.session;
    // Series 2 of the fake panel has specials (season 0) plus seasons 1 and 2.
    let info = env.rt.block_on(s.series_info(2)).unwrap();
    assert!(info.episodes.contains_key(&0));
    let name = info.info.name.clone().unwrap_or_default();
    let first = vod::first_episode_id(&info).unwrap();
    let (season, number, ep) = vod::locate_episode(&info, first).unwrap();
    assert_eq!(
        (season, number),
        (1, 1),
        "specials are not the first episode"
    );

    // Request: display title, S/E subtitle, URL from the episode id + extension.
    let req = s
        .series_episode_request(2, &name, ep, season, number, None, Some(42.0))
        .unwrap();
    assert_eq!(req.kind, MediaKind::Episode);
    assert_eq!(req.resume_from, Some(42.0));
    assert!(req.title.starts_with(vod::display_title(&name).title));
    assert!(req.title.ends_with("Pilot"), "{}", req.title);
    assert_eq!(req.subtitle.as_deref(), Some("S1 · E1"));
    assert!(
        req.url
            .as_str()
            .contains(&format!("/series/demo/demo/{first}."))
    );
    assert!(matches!(
        req.item,
        PlaybackItem::Episode {
            series_id: 2,
            season: Some(1),
            episode: Some(1),
            ..
        }
    ));

    // No history: start with S1·E1.
    let t = s.resume_thresholds();
    let watch = env.rt.block_on(s.series_watch(2)).unwrap();
    let c = vod::continue_target(&info, watch.last.as_ref(), &t).unwrap();
    assert_eq!((c.episode_id, c.kind), (first, ContinueKind::Start));

    // Half-watched → resume it.
    let profile = s.profile_id();
    let snap = PlaybackSnapshot {
        title: req.title.clone(),
        series_id: Some("2".into()),
        season: Some(1),
        episode: Some(1),
        ..Default::default()
    };
    env.services
        .store
        .playback()
        .record_progress(
            profile,
            PlaybackKind::Episode,
            &first.to_string(),
            600.0,
            Some(2400.0),
            &snap,
        )
        .unwrap();
    let watch = env.rt.block_on(s.series_watch(2)).unwrap();
    assert!(watch.get(first).is_some() && !watch.watched(first));
    let c = vod::continue_target(&info, watch.last.as_ref(), &t).unwrap();
    assert_eq!((c.episode_id, c.kind), (first, ContinueKind::Resume(600.0)));
    // Home's Continue watching has it (one entry per series).
    let cw = env
        .services
        .store
        .playback()
        .continue_watching(profile, 10)
        .unwrap();
    assert_eq!(cw.len(), 1);
    assert_eq!(cw[0].snapshot.series_id.as_deref(), Some("2"));

    // Marked watched → "Continue" moves to the next episode.
    env.rt
        .block_on(s.set_episode_watched(2, &name, ep, season, number, None, true))
        .unwrap();
    let watch = env.rt.block_on(s.series_watch(2)).unwrap();
    assert!(watch.watched(first));
    let next = vod::next_episode_id(&info, first).unwrap();
    let c = vod::continue_target(&info, watch.last.as_ref(), &t).unwrap();
    assert_eq!((c.episode_id, c.kind), (next, ContinueKind::Next));
    assert!(
        env.services
            .store
            .playback()
            .continue_watching(profile, 10)
            .unwrap()
            .is_empty(),
        "a finished episode leaves Continue watching"
    );

    // Unwatched → forgotten.
    env.rt
        .block_on(s.set_episode_watched(2, &name, ep, season, number, None, false))
        .unwrap();
    let watch = env.rt.block_on(s.series_watch(2)).unwrap();
    assert!(watch.get(first).is_none());
}

#[test]
fn gaps_in_seasons_chain_to_the_next_existing_season() {
    let env = env();
    // Series 5 has seasons 1, 2 and 4.
    let info = env.rt.block_on(env.session.series_info(5)).unwrap();
    assert_eq!(
        info.episodes.keys().copied().collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    let last_of_2 = info.episodes[&2].last().unwrap().id;
    let first_of_4 = info.episodes[&4].first().unwrap().id;
    assert_eq!(vod::next_episode_id(&info, last_of_2), Some(first_of_4));
    let last = info.episodes[&4].last().unwrap().id;
    assert_eq!(vod::next_episode_id(&info, last), None);
}

#[test]
fn movie_progress_resume_and_remembered_category() {
    let env = env();
    let s = &env.session;
    let cats = env.rt.block_on(s.vod_categories()).unwrap();
    let movies = env
        .rt
        .block_on(s.vod_streams(Some(cats[0].id.clone())))
        .unwrap();
    let m = &movies[0];
    let req = s
        .movie_request(
            m.stream_id,
            m.container_extension.as_deref(),
            "Title",
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        req.url
            .as_str()
            .contains(&format!("/movie/demo/demo/{}.", m.stream_id))
    );

    // A resume point → detail's "Resume" and the poster progress map.
    let profile = s.profile_id();
    let snap = PlaybackSnapshot {
        title: "Title".into(),
        container_extension: m.container_extension.clone(),
        ..Default::default()
    };
    env.services
        .store
        .playback()
        .record_progress(
            profile,
            PlaybackKind::Movie,
            &m.stream_id.to_string(),
            600.0,
            Some(6000.0),
            &snap,
        )
        .unwrap();
    let p = env
        .rt
        .block_on(s.movie_progress(m.stream_id))
        .unwrap()
        .unwrap();
    assert!(p.is_resumable(&s.resume_thresholds()));
    let map = env.rt.block_on(s.movie_progress_map()).unwrap();
    assert_eq!(map.get(&m.stream_id).and_then(|p| p.fraction()), Some(0.1));
    let resumed = s.resume_request(&p).unwrap();
    assert_eq!(resumed.resume_from, Some(600.0));
    assert_eq!(resumed.kind, MediaKind::Movie);

    // The browsed category is remembered per profile (written in the background).
    assert_eq!(
        env.rt.block_on(s.last_vod_category(ContentKind::Movie)),
        None
    );
    s.remember_vod_category(ContentKind::Movie, &cats[1].id);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = None;
    while Instant::now() < deadline && got.is_none() {
        got = env.rt.block_on(s.last_vod_category(ContentKind::Movie));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(got.as_ref(), Some(&cats[1].id));
    assert_eq!(
        env.rt.block_on(s.last_vod_category(ContentKind::Series)),
        None
    );
}

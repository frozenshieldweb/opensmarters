// SPDX-License-Identifier: GPL-3.0-or-later
//! Drives the app's session layer against the in-process fake panel:
//! profile → login → categories → streams → stream URL → `PlayableUrl` accepted,
//! plus caching, favourites, M3U, refused accounts and redaction.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use eframe::egui;
use opensmarters_app::playback::{MediaKind, ZapContext};
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_core::Redactor;
use opensmarters_core::catalog::CategorySelection;
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{
    ContentKind, ItemSnapshot, MemorySecretStore, NewProfile, NewProfileSource, ProfileId,
    ProfileSettings, Store, StoreOptions, UserAgent,
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

fn xtream_profile(env: &Env, user: &str, pass: &str, ua: UserAgent) -> ProfileId {
    env.services
        .store
        .profiles()
        .create(NewProfile {
            name: "Test".into(),
            source: NewProfileSource::Xtream {
                server_url: env.panel.base_url().to_owned(),
                username: user.into(),
                password: SecretString::from(pass.to_owned()),
            },
            settings: ProfileSettings {
                user_agent: ua,
                ..ProfileSettings::default()
            },
        })
        .unwrap()
        .id
}

#[test]
fn login_categories_streams_url_playable() {
    let env = env();
    let id = xtream_profile(&env, "demo", "demo", UserAgent::Default);
    let session = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error));
    assert!(session.is_xtream());
    assert!(session.is_insecure(), "the fake panel is plain HTTP");
    let acc = session.account().expect("account summary");
    assert!(acc.exp_date.is_some());

    let catalog = env.rt.block_on(session.live_catalog()).unwrap();
    assert_eq!(catalog.channels.len(), env.panel.catalogue().live.len());
    assert!(!catalog.categories.is_empty());
    let first_cat = catalog.categories[0].id.clone();
    let in_cat = catalog.select(
        &CategorySelection::Category(first_cat.clone()),
        &Default::default(),
    );
    assert!(!in_cat.is_empty());
    assert_eq!(in_cat.len(), catalog.count(&first_cat));

    // Stream URL → PlayableUrl, with the profile UA and the zapping context.
    let channels = Arc::new(catalog.channels.clone());
    let ch = &channels[0];
    let req = session
        .live_request(
            ch,
            Some(ZapContext {
                channels: Arc::clone(&channels),
                index: 0,
            }),
        )
        .unwrap();
    assert_eq!(req.kind, MediaKind::Live);
    assert_eq!(req.url.scheme(), "http");
    assert!(req.url.as_str().contains("/live/demo/demo/"));
    assert!(
        req.headers
            .user_agent
            .as_ref()
            .is_some_and(|ua| ua.as_str().starts_with("opensmarters/"))
    );
    assert!(
        !format!("{req:?}").contains("/live/demo/demo/"),
        "Debug must not leak the URL"
    );
    // The redactor knows the password: any log line with the URL is scrubbed.
    let line = env.services.redactor.redact(req.url.as_str());
    assert!(!line.contains("demo/demo"), "{line}");

    // Second call is served from memory/cache without hitting the panel again.
    env.panel.clear_requests();
    let again = env.rt.block_on(session.live_catalog()).unwrap();
    assert_eq!(again.channels.len(), catalog.channels.len());
    assert!(
        env.panel
            .requests()
            .iter()
            .all(|r| !r.path.contains("player_api")),
        "catalogue must come from the cache"
    );

    // VOD and series lists, details.
    let movies = env.rt.block_on(session.vod_streams(None)).unwrap();
    assert_eq!(movies.len(), env.panel.catalogue().movies.len());
    let m = &movies[0];
    let info = env.rt.block_on(session.vod_info(m.stream_id)).unwrap();
    assert!(info.info.name.is_some());
    let mreq = session
        .movie_request(
            m.stream_id,
            m.container_extension.as_deref(),
            &m.name,
            None,
            None,
            Some(42.0),
        )
        .unwrap();
    assert!(mreq.url.as_str().contains("/movie/demo/demo/"));
    assert_eq!(mreq.resume_from, Some(42.0));
    let series = env.rt.block_on(session.series_list(None)).unwrap();
    let sinfo = env
        .rt
        .block_on(session.series_info(series[0].series_id))
        .unwrap();
    let ep = sinfo
        .episodes
        .values()
        .flatten()
        .next()
        .expect("an episode");
    let ereq = session
        .episode_request(series[0].series_id, &series[0].name, ep, None, None)
        .unwrap();
    assert!(ereq.url.as_str().contains("/series/demo/demo/"));
}

#[test]
fn favourites_persist_and_smarters_user_agent_applies() {
    let env = env();
    let id = xtream_profile(&env, "demo", "demo", UserAgent::SmartersCompat);
    let session = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("{}", f.error));
    assert_eq!(session.user_agent(), "IPTVSmartersPro");
    let catalog = env.rt.block_on(session.live_catalog()).unwrap();
    let ch = &catalog.channels[3];
    assert!(session.toggle_favourite(ContentKind::Live, &ch.key, ItemSnapshot::named(&ch.name)));
    assert!(session.is_favourite(ContentKind::Live, &ch.key));
    // Persisted in the background: poll the store briefly.
    let store = Arc::clone(&env.services.store);
    let mut ok = false;
    for _ in 0..100 {
        if store
            .favourites()
            .contains(id, ContentKind::Live, &ch.key)
            .unwrap()
        {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(ok, "favourite stored");
    let req = session.live_request(ch, None).unwrap();
    assert_eq!(
        req.headers.user_agent.as_ref().map(|u| u.as_str()),
        Some("IPTVSmartersPro")
    );
}

#[test]
fn refused_accounts_report_status_not_crash() {
    let env = env();
    let id = xtream_profile(&env, "expired", "expired", UserAgent::Default);
    let failure = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .expect_err("expired accounts don't open");
    assert!(failure.error.is_auth());
    assert!(failure.account.is_some(), "status shown to the user");
    assert!(!failure.error.user_message().is_empty());

    let id = xtream_profile(&env, "demo", "wrong-password", UserAgent::Default);
    let failure = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .expect_err("wrong password");
    assert!(failure.error.is_auth());
    assert!(!failure.error.user_message().contains("wrong-password"));
}

#[test]
fn m3u_url_profile_loads_channels_and_rejects_unsafe_urls() {
    let env = env();
    let url = format!(
        "{}/get.php?username=demo&password=demo&type=m3u_plus&output=ts",
        env.panel.base_url()
    );
    let id = env
        .services
        .store
        .profiles()
        .create(NewProfile {
            name: "Playlist".into(),
            source: NewProfileSource::M3uUrl {
                url: SecretString::from(url.clone()),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id;
    let session = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("{}", f.error));
    assert!(!session.is_xtream());
    // The playlist URL is a registered secret.
    assert_eq!(env.services.redactor.redact(&url), "***");
    let catalog = env.rt.block_on(session.live_catalog()).unwrap();
    assert!(!catalog.channels.is_empty());
    let ch = &catalog.channels[0];
    assert!(ch.key.starts_with("m3u:"));
    let req = session.live_request(ch, None).unwrap();
    assert_eq!(req.kind, MediaKind::Live);

    // The torture playlist's "Must Not Play" entries are refused before reaching mpv.
    let nasty = env.rt.block_on(async {
        opensmarters_core::net::HttpClient::new(&Default::default(), Redactor::new())
            .unwrap()
            .get_bytes(&opensmarters_core::url::Url::parse(&env.panel.url("/nasty.m3u")).unwrap())
            .await
            .unwrap()
    });
    let playlist = opensmarters_core::m3u::parse(&nasty, &Default::default()).unwrap();
    let bad = opensmarters_app::services::catalog::LiveCatalog::from_playlist(&playlist);
    let refused = bad
        .channels
        .iter()
        .filter(|c| c.categories.iter().any(|g| g.as_str() == "Must Not Play"))
        .filter(|c| session.live_request(c, None).is_err())
        .count();
    let total = bad
        .channels
        .iter()
        .filter(|c| c.categories.iter().any(|g| g.as_str() == "Must Not Play"))
        .count();
    assert!(total > 0);
    assert_eq!(refused, total, "every unsafe URL is rejected");
}

#[test]
fn parental_locks_follow_keywords_pin_and_session_unlock() {
    let env = env();
    // Setting a PIN turns the keyword locks on.
    env.services
        .store
        .parental()
        .set_pin(&SecretString::from("1234".to_owned()))
        .unwrap();
    let id = xtream_profile(&env, "demo", "demo", UserAgent::Default);
    let session = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("{}", f.error));
    assert!(session.parental_enabled());
    let catalog = env.rt.block_on(session.live_catalog()).unwrap();
    let find = |needle: &str| {
        catalog
            .categories
            .iter()
            .find(|c| c.name.contains(needle))
            .cloned()
            .unwrap_or_else(|| panic!("no category containing {needle}"))
    };
    let adult = find("Adult");
    let sussex = find("Sussex");
    assert!(session.is_category_locked(ContentKind::Live, &adult.id, &adult.name));
    assert!(
        !session.is_category_locked(ContentKind::Live, &sussex.id, &sussex.name),
        "whole-word matching: Sussex is not adult"
    );
    session.unlock_category(ContentKind::Live, &adult.id);
    assert!(!session.is_category_locked(ContentKind::Live, &adult.id, &adult.name));
    assert!(
        !session.is_selection_locked(ContentKind::Live, &CategorySelection::All, "All"),
        "All is never locked"
    );
}

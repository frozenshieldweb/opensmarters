// SPDX-License-Identifier: GPL-3.0-or-later
//! Deleting a profile also forgets its recent searches and its remembered Movies/Series
//! category (both are app settings keyed by profile id, outside the cascading tables).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::screens::common::profile_data::delete_profile;
use opensmarters_app::services::{Paths, Services, Session, search_history};
use opensmarters_core::Redactor;
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{
    ContentKind, MemorySecretStore, NewProfile, NewProfileSource, ProfileSettings, Store,
    StoreOptions,
};

#[test]
fn deleting_a_profile_forgets_searches_and_browse_categories() {
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
        Arc::clone(&store),
        Redactor::new(),
        Paths::default(),
        true,
        egui::Context::default(),
    );
    let id = store
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
    let cats = rt.block_on(session.vod_categories()).unwrap();
    let cat = cats.first().expect("the demo panel has film categories");

    // Remembered in the background by the session (the real setting key).
    session.remember_vod_category(ContentKind::Movie, &cat.id);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = None;
    while Instant::now() < deadline && got.is_none() {
        got = rt.block_on(session.last_vod_category(ContentKind::Movie));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(got.as_ref(), Some(&cat.id));
    search_history::record(&store, id, "news").unwrap();

    assert!(delete_profile(&store, id).unwrap());
    assert!(store.profiles().get(id).unwrap().is_none());
    assert!(search_history::load(&store, id).unwrap().is_empty());
    assert_eq!(
        rt.block_on(session.last_vod_category(ContentKind::Movie)),
        None,
        "the remembered category goes with the profile"
    );
}

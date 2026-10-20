// SPDX-License-Identifier: GPL-3.0-or-later
//! Fast start (`Session::open_cached` + `Session::verify`) against the in-process fake
//! panel: no cache → normal sign-in; cached login + catalogue → the session opens without
//! the provider and is checked afterwards (accepted, refused, unreachable).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use eframe::egui;
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_core::Redactor;
use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{
    MemorySecretStore, NewProfile, NewProfileSource, ProfileId, ProfileSettings, Store,
    StoreOptions,
};

struct Env {
    rt: tokio::runtime::Runtime,
    services: Services,
    panel: Option<fake_panel::Handle>,
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
        panel: Some(panel),
    }
}

fn profile(env: &Env) -> ProfileId {
    env.services
        .store
        .profiles()
        .create(NewProfile {
            name: "Test".into(),
            source: NewProfileSource::Xtream {
                server_url: env.panel.as_ref().unwrap().base_url().to_owned(),
                username: "demo".into(),
                password: SecretString::from("demo".to_owned()),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id
}

/// Signs in once and loads the live catalogue, as a first run would.
fn warm_cache(env: &Env, id: ProfileId) {
    let s = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error));
    env.rt.block_on(s.live_catalog()).unwrap();
    // Store writes go through the FIFO writer: a no-op write lands after the cached login
    // and catalogue.
    env.rt
        .block_on(env.services.write("flush", |_| Ok(())))
        .unwrap();
}

#[test]
fn nothing_cached_means_a_normal_sign_in() {
    let env = env();
    let id = profile(&env);
    assert!(
        env.rt
            .block_on(Session::open_cached(env.services.clone(), id))
            .is_none()
    );
}

#[test]
fn a_cached_profile_opens_without_the_provider_and_verifies() {
    let env = env();
    let id = profile(&env);
    warm_cache(&env, id);
    let s = env
        .rt
        .block_on(Session::open_cached(env.services.clone(), id))
        .expect("opens from the cache");
    assert!(s.is_xtream() && !s.is_offline());
    assert!(
        s.account().is_some(),
        "account summary from the cached login"
    );
    let cat = env.rt.block_on(s.live_catalog()).unwrap();
    assert!(!cat.channels.is_empty(), "catalogue served from the cache");
    env.rt
        .block_on(s.verify())
        .unwrap_or_else(|f| panic!("verify failed: {}", f.error));
    assert!(!s.is_offline());
}

#[test]
fn a_refused_account_is_reported_by_the_background_check() {
    let env = env();
    let id = profile(&env);
    warm_cache(&env, id);
    // The password was changed at the provider (here: in the keychain) since last time.
    env.services
        .store
        .profiles()
        .set_password(id, &SecretString::from("wrong".to_owned()))
        .unwrap();
    let s = env
        .rt
        .block_on(Session::open_cached(env.services.clone(), id))
        .expect("still opens from the cache");
    let f = env.rt.block_on(s.verify()).expect_err("refused");
    assert!(f.error.is_auth(), "{}", f.error);
    assert!(!s.is_offline());
}

#[test]
fn an_unreachable_provider_switches_the_session_offline() {
    let mut env = env();
    let id = profile(&env);
    warm_cache(&env, id);
    let panel = env.panel.take().unwrap();
    env.rt.block_on(panel.shutdown());
    let s = env
        .rt
        .block_on(Session::open_cached(env.services.clone(), id))
        .expect("opens from the cache");
    let f = env.rt.block_on(s.verify()).expect_err("unreachable");
    assert!(f.error.is_network(), "{}", f.error);
    assert!(s.is_offline());
}

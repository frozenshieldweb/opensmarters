// SPDX-License-Identifier: GPL-3.0-or-later
//! Fast start: open an Xtream profile from its **cached** login and catalogue without
//! waiting for the provider, then check the account in the background.
//!
//! [`Session::open`] logs in before anything is shown (the "Signing in to…" screen). When
//! the profile was used before, everything Home needs is already in the encrypted store
//! cache, so the app opens the session at once with [`Session::open_cached`] (store and
//! keychain reads only) and calls [`Session::verify`] afterwards:
//!
//! - verified: nothing changes (the cached login is refreshed);
//! - provider unreachable: the session switches to **offline** (the header shows it);
//! - account refused (expired, banned, password changed): the caller shows a quiet
//!   notice; streams will fail with the provider's own error.
//!
//! Only Xtream profiles with a cached login *and* a cached live catalogue take this path,
//! and never when the cached login already says the account expired. Everything else uses
//! [`Session::open`].

use std::sync::Arc;
use std::sync::atomic::Ordering;

use opensmarters_core::chrono::Utc;
use opensmarters_core::xtream::Client;
use opensmarters_core::{HttpConfig, LoginInfo, XtreamAccount};
use opensmarters_store::{CacheScope, ContentKind, ProfileId, ProfileSource};

use super::{
    AccountSummary, InsecureReason, LOGIN_TTL, OpenFailure, Session, Source, blocking,
    connection_budget, guide, load_parental, locks, user_agent_string,
};
use crate::error::AppError;
use crate::services::Services;
use opensmarters_core::net::HttpClient;

/// Store cache key of the parsed login.
const LOGIN_KEY: &str = "xt:login";
/// Store cache key of the live stream list (the catalogue Home and Live TV need first).
const LIVE_KEY: &str = "xt:live_streams:all";

/// True if a cached login can stand in for a fresh one: the account was accepted then and
/// hasn't expired since (by this computer's clock).
fn usable(info: &LoginInfo) -> bool {
    let now = Utc::now();
    info.check(now).is_ok() && info.user_info.exp_date.is_none_or(|e| e > now)
}

impl Session {
    /// Opens `profile_id` from the cache, without contacting the provider. `None` when it
    /// isn't an Xtream profile, nothing is cached yet, the cached account had expired, or
    /// a secret is missing; then use [`Session::open`].
    pub async fn open_cached(services: Services, profile_id: ProfileId) -> Option<Arc<Session>> {
        let store = Arc::clone(&services.store);
        let profile = blocking(&store, move |s| s.profiles().get(profile_id))
            .await
            .ok()??;
        let ProfileSource::Xtream {
            server_url,
            username,
        } = &profile.source
        else {
            return None;
        };
        let (login, has_catalogue) = blocking(&store, move |s| {
            let cache = s.cache();
            let login = cache.get_stale_ok(CacheScope::Profile(profile_id), LOGIN_KEY)?;
            let live = cache.get_stale_ok(CacheScope::Profile(profile_id), LIVE_KEY)?;
            Ok((login, live.is_some()))
        })
        .await
        .ok()?;
        let info: LoginInfo = serde_json::from_slice(&login?.data).ok()?;
        if !has_catalogue || !usable(&info) {
            return None;
        }
        let password = blocking(&store, move |s| s.profiles().password(profile_id))
            .await
            .ok()??;
        let user_agent = user_agent_string(&profile.settings.user_agent);
        let http_config = HttpConfig {
            user_agent: user_agent.clone(),
            ..HttpConfig::default()
        };
        let redactor = services.redactor.clone();
        let http = HttpClient::new(&http_config, redactor.clone()).ok()?;
        let acc = XtreamAccount::new(server_url, username.clone()).ok()?;
        let plain_account = !acc.server().is_https();
        let client = Arc::new(Client::with_redactor(acc, password, &http_config, redactor).ok()?);
        client.set_server_time_zone(info.time_zone());
        let account = Some(AccountSummary::from_login(&info));
        let urls = client.stream_urls(&info);
        // Same rule as `Session::open` (SR-12): an https panel may hand out http streams.
        let insecure = if plain_account {
            Some(InsecureReason::PlainAccount)
        } else if urls.base().scheme() != "https" {
            Some(InsecureReason::PlainStreams)
        } else {
            None
        };

        let favourites = blocking(&store, move |s| {
            let mut out = std::collections::HashMap::new();
            for kind in ContentKind::ALL {
                out.insert(kind, s.favourites().ids(profile_id, kind)?);
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
        let parental = load_parental(&store, profile_id).await.unwrap_or_default();
        services.blocking("mark profile used", move |s| {
            s.profiles().mark_used(profile_id)
        });
        let favourites = favourites
            .into_iter()
            .map(|(k, v)| (k, Arc::new(v)))
            .collect();
        let connections = connection_budget(&store, profile_id, account.as_ref(), false).await;
        tracing::info!("profile opened from its cached login; checking the account");
        Some(Arc::new_cyclic(|me| Session {
            // Identity and provider connection (as `Session::open` builds them).
            me: me.clone(),
            profile,
            services,
            source: Source::Xtream {
                client,
                login: Box::new(info),
                urls,
            },
            user_agent,
            http,
            account,
            insecure,
            // Lifecycle.
            offline: std::sync::atomic::AtomicBool::new(false),
            closed: std::sync::atomic::AtomicBool::new(false),
            // Caches.
            memo: std::sync::Mutex::new(std::collections::HashMap::new()),
            key_locks: std::sync::Mutex::new(std::collections::HashMap::new()),
            live: std::sync::Mutex::new(None),
            ext: Arc::new(guide::ExtState::default()),
            xmltv: tokio::sync::OnceCell::new(),
            // Favourites.
            favourites: std::sync::RwLock::new(favourites),
            favourites_generation: std::sync::atomic::AtomicU64::new(0),
            // Parental control.
            parental: std::sync::RwLock::new(parental),
            lock_state: locks::LockState::default(),
            // Connection budget (Phase 7).
            connections,
        }))
    }

    /// Checks the account with the provider (after [`Session::open_cached`]). A network
    /// failure switches the session offline; a refused account is reported (expired,
    /// banned, wrong password) with the provider's account summary.
    ///
    /// # Errors
    ///
    /// The [`OpenFailure`] the same login would have shown at sign-in.
    pub async fn verify(self: &Arc<Self>) -> Result<(), OpenFailure> {
        let Some(client) = self.xtream().cloned() else {
            return Ok(());
        };
        match client.account_info().await {
            Ok(info) => {
                if let Err(status) = info.check(Utc::now()) {
                    return Err(OpenFailure {
                        error: AppError::Provider(opensmarters_core::Error::Auth { status }),
                        account: Some(Box::new(AccountSummary::from_login(&info))),
                    });
                }
                self.offline.store(false, Ordering::Relaxed);
                self.connections
                    .set_account(info.user_info.max_connections, info.user_info.active_cons);
                // In order with the other store writes (FIFO writer); not awaited.
                if let Ok(bytes) = serde_json::to_vec(&info) {
                    let profile = self.profile.id;
                    drop(self.services.write("cache the login", move |s| {
                        s.cache()
                            .put(CacheScope::Profile(profile), LOGIN_KEY, &bytes, LOGIN_TTL)
                            .map(|_| ())
                    }));
                }
                Ok(())
            }
            Err(e) => {
                let error = AppError::from(e);
                if error.is_network() {
                    self.offline.store(true, Ordering::Relaxed);
                }
                Err(error.into())
            }
        }
    }
}

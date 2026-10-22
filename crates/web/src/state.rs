// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared server state.

use std::net::IpAddr;
use std::ops::Deref;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use opensmarters_core::Redactor;

use crate::auth::{Limiter, Passwords};
use crate::config::Config;
use crate::crypto::Keys;
use crate::db::{self, Db};
use crate::images::ImageProxy;
use crate::net::{Net, Policy};
use crate::provider::ProviderCache;
use crate::streams::StreamManager;

/// Everything the handlers share (cheap to clone).
#[derive(Clone, Debug)]
pub struct AppState(Arc<Inner>);

/// The shared parts.
#[derive(Debug)]
pub struct Inner {
    /// Configuration.
    pub config: Config,
    /// Database.
    pub db: Db,
    /// Derived keys.
    pub keys: Keys,
    /// SSRF-safe outbound client.
    pub net: Net,
    /// Provider caches.
    pub providers: ProviderCache,
    /// Running streams.
    pub streams: Arc<StreamManager>,
    /// Image proxy.
    pub images: ImageProxy,
    /// Password hashing.
    pub passwords: Passwords,
    /// Sign-in / secret-check rate limiter.
    pub limiter: Limiter,
    setup_code: Mutex<Option<String>>,
}

impl Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn new_setup_code() -> anyhow::Result<String> {
    let bytes = crate::crypto::random_bytes::<12>()?;
    let mut code = String::with_capacity(14);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            code.push('-');
        }
        code.push(char::from(
            CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()],
        ));
    }
    Ok(code)
}

fn normalise_code(s: &str) -> Vec<u8> {
    s.bytes()
        .filter(|b| !matches!(b, b'-' | b' ' | b'\t'))
        .map(|b| b.to_ascii_uppercase())
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn docker_host_addresses() -> Vec<IpAddr> {
    let mut out = crate::net::default_gateways();
    if let Ok(Ok(addrs)) = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::lookup_host(("host.docker.internal", 0u16)),
    )
    .await
    {
        out.extend(addrs.map(|a| a.ip()));
    }
    out.sort();
    out.dedup();
    out
}

impl AppState {
    /// Opens the data folder and database and builds the shared state. Logs the setup
    /// code when no user exists yet.
    ///
    /// # Errors
    ///
    /// Data folder, database or TLS initialisation failures.
    pub async fn new(config: Config, redactor: Redactor) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        restrict_dir(&config.data_dir);
        let db = Db::open(&config.data_dir)?;
        let keys = Keys::new(&config.secret_key);
        let policy = Policy {
            allow_private: config.allow_private_providers,
            dev_loopback_allow: config.dev_loopback_allow.clone(),
            docker_host: docker_host_addresses().await,
        };
        if !policy.docker_host.is_empty() {
            tracing::info!(addresses = ?policy.docker_host, "the Docker host's addresses are always refused for providers");
        }
        let net = Net::new(policy, redactor).map_err(|e| anyhow::anyhow!("{e}"))?;
        let images = ImageProxy::new(config.data_dir.join("cache").join("images"));
        let streams = Arc::new(StreamManager::new(&config));
        let users = db.call(|c| db::user_count(c)).await?;
        let setup_code = if users == 0 {
            match &config.setup_code {
                Some(code) => Some(code.clone()),
                None => Some(new_setup_code()?),
            }
        } else {
            None
        };
        let state = AppState(Arc::new(Inner {
            config,
            db,
            keys,
            net,
            providers: ProviderCache::default(),
            streams,
            images,
            passwords: Passwords::new(),
            limiter: Limiter::default(),
            setup_code: Mutex::new(setup_code),
        }));
        Ok(state)
    }

    /// Prints the setup code to the log (once, at start-up) while no user exists.
    pub fn announce_setup(&self) {
        let guard = self
            .setup_code
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(code) = guard.as_deref() {
            if self.config.setup_code.is_some() {
                tracing::warn!(
                    "no administrator yet: open the web page and use the setup code from OPENSMARTERS_SETUP_CODE"
                );
            } else {
                tracing::warn!(
                    "no administrator yet: open the web page and enter this one-time setup code: {code}"
                );
            }
        }
    }

    /// Whether the first-run setup is still open.
    pub fn setup_open(&self) -> bool {
        self.setup_code
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Checks a setup code (constant time).
    pub fn setup_code_matches(&self, code: &str) -> bool {
        let guard = self
            .setup_code
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        guard.as_deref().is_some_and(|expected| {
            constant_time_eq(&normalise_code(expected), &normalise_code(code))
        })
    }

    /// Closes the setup (single use).
    pub fn close_setup(&self) {
        *self
            .setup_code
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(unix)]
fn restrict_dir(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir(_path: &std::path::Path) {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn setup_codes() {
        let c = new_setup_code().unwrap();
        assert_eq!(c.len(), 14);
        assert_eq!(c.matches('-').count(), 2);
        assert!(constant_time_eq(
            &normalise_code(&c),
            &normalise_code(&c.to_lowercase().replace('-', " "))
        ));
        assert!(!constant_time_eq(
            &normalise_code(&c),
            &normalise_code("AAAA-AAAA-AAAA")
        ));
    }
}

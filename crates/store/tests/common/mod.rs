//! Shared helpers for integration tests. Synthetic data only.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use opensmarters_store::{
    ManualClock, MemorySecretStore, NewProfile, NewProfileSource, PinHashParams, Profile,
    ProfileSettings, Store, StoreOptions, Timestamp,
};
use secrecy::SecretString;

/// Cheap Argon2 parameters so debug-build tests stay fast. Production defaults are
/// exercised by `default_params_hash_and_verify` in api.rs.
pub const FAST_PIN: PinHashParams = PinHashParams {
    m_cost_kib: 256,
    t_cost: 1,
    p_cost: 1,
};

pub struct Env {
    pub dir: tempfile::TempDir,
    pub secrets: Arc<MemorySecretStore>,
    pub clock: Arc<ManualClock>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            secrets: Arc::new(MemorySecretStore::new()),
            clock: Arc::new(ManualClock::new(Timestamp::from_unix_secs(1_800_000_000))),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir.path().join("store.db")
    }

    pub fn options(&self) -> StoreOptions {
        StoreOptions::at_path(self.db_path())
            .with_clock(self.clock.clone())
            .with_pin_hash(FAST_PIN)
    }

    pub fn open(&self) -> Store {
        Store::open(self.options(), self.secrets.clone()).unwrap()
    }
}

pub fn xtream(name: &str, password: &str) -> NewProfile {
    NewProfile {
        name: name.into(),
        source: NewProfileSource::Xtream {
            server_url: "http://panel.example.invalid:8080".into(),
            username: "synthetic_user".into(),
            password: SecretString::from(password),
        },
        settings: ProfileSettings::default(),
    }
}

pub fn m3u_url(name: &str, url: &str) -> NewProfile {
    NewProfile {
        name: name.into(),
        source: NewProfileSource::M3uUrl {
            url: SecretString::from(url),
        },
        settings: ProfileSettings::default(),
    }
}

pub fn m3u_file(name: &str, path: &Path) -> NewProfile {
    NewProfile {
        name: name.into(),
        source: NewProfileSource::M3uFile {
            path: path.to_path_buf(),
        },
        settings: ProfileSettings::default(),
    }
}

pub fn make_xtream(store: &Store) -> Profile {
    store
        .profiles()
        .create(xtream("Synthetic", "synthetic-password"))
        .unwrap()
}

pub fn pin(s: &str) -> SecretString {
    SecretString::from(s)
}

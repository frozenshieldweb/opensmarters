// SPDX-License-Identifier: GPL-3.0-or-later
//! `--demo` (cargo feature `demo`, dev only): runs the synthetic `fake-panel` provider
//! in-process and creates a demo profile (`demo`/`demo`) pointing at it. With an empty
//! store (`--ephemeral`) it also sets the parental PIN to [`DEMO_PIN`] so locked
//! categories ("Adult Night") can be exercised.

use opensmarters_core::secrecy::SecretString;
use opensmarters_store::{NewProfile, NewProfileSource, ProfileId, ProfileSettings, Store};

/// The parental PIN set by `--demo` on a store without one.
pub const DEMO_PIN: &str = "1234";

/// The running demo provider.
#[derive(Debug)]
pub struct Demo {
    /// The fake panel (stops when dropped).
    pub panel: fake_panel::Handle,
    /// The demo profile.
    pub profile: ProfileId,
}

/// Starts the fake panel on `rt` and creates the demo profile in `store`. `set_pin`
/// (only for `--ephemeral`) sets [`DEMO_PIN`] when no PIN exists.
///
/// Media is generated in the background on first use (needs ffmpeg); until then streams
/// answer 503 and the player shows a reconnect message.
///
/// # Errors
///
/// The panel could not bind, or the profile could not be created.
pub fn start(rt: &tokio::runtime::Runtime, store: &Store, set_pin: bool) -> anyhow::Result<Demo> {
    // Dev only: `OPENSMARTERS_DEMO_QUIRKS=huge,timeshift-smarters-only` makes the fake
    // panel misbehave like real ones (see `fake_panel::Quirks`).
    let quirks = match std::env::var("OPENSMARTERS_DEMO_QUIRKS") {
        Ok(list) => fake_panel::Quirks::parse_list([list])?,
        Err(_) => fake_panel::Quirks::default(),
    };
    let config = fake_panel::Config {
        media: fake_panel::MediaMode::Background,
        quirks,
        ..fake_panel::Config::default()
    };
    let panel = rt.block_on(fake_panel::spawn(config))?;
    let profile = store.profiles().create(NewProfile {
        name: "Demo provider".into(),
        source: NewProfileSource::Xtream {
            server_url: panel.base_url().to_owned(),
            username: fake_panel::users::DEMO.username.to_owned(),
            password: SecretString::from(fake_panel::users::DEMO.password.to_owned()),
        },
        settings: ProfileSettings::default(),
    })?;
    if set_pin && !store.parental().is_pin_set()? {
        store
            .parental()
            .set_pin(&SecretString::from(DEMO_PIN.to_owned()))?;
    }
    tracing::info!("demo provider running on 127.0.0.1:{}", panel.port());
    Ok(Demo {
        panel,
        profile: profile.id,
    })
}

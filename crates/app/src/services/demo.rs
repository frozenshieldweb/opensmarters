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
    // Dev only (harness builds): `OPENSMARTERS_UPDATE_URL=fake[?tag=v0.3.0]` points the
    // update check at the panel's GitHub-shaped `/__updates/releases/latest`.
    #[cfg(feature = "harness")]
    crate::services::updates::set_demo_panel(panel.base_url());
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
    // Dev only: `OPENSMARTERS_DEMO_FAVOURITES=1` pre-fills a few favourites (synthetic).
    if std::env::var_os("OPENSMARTERS_DEMO_FAVOURITES").is_some() {
        seed_favourites(store, profile.id, &panel)?;
    }
    // Dev only (p7b): `OPENSMARTERS_DEMO_RECORDINGS=1` adds synthetic recordings rows.
    if std::env::var_os("OPENSMARTERS_DEMO_RECORDINGS").is_some() {
        crate::dvr::seed_demo(store, profile.id, &panel)?;
    }
    // Dev only: `OPENSMARTERS_DEMO_XMLTV=1` sets the profile's EPG URL override to the
    // panel's gzipped XMLTV, so the guide runs on the XMLTV path.
    if std::env::var_os("OPENSMARTERS_DEMO_XMLTV").is_some() {
        let url = SecretString::from(format!(
            "{}/xmltv.xml.gz",
            panel.base_url().trim_end_matches('/')
        ));
        store
            .profiles()
            .set_epg_url_override(profile.id, Some(&url))?;
    }
    // Dev only: `OPENSMARTERS_DEMO_MAX_STREAMS=4` sets the profile's "simultaneous
    // streams" override (the demo account allows 2), e.g. to try multiscreen with 1, 3 or
    // 4 tiles.
    if let Some(n) = std::env::var("OPENSMARTERS_DEMO_MAX_STREAMS")
        .ok()
        .and_then(|v| v.trim().parse::<u8>().ok())
        .filter(|n| *n > 0)
    {
        use crate::services::connections::MAX_STREAMS_OVERRIDE;
        let mut limits = store.settings().get(&MAX_STREAMS_OVERRIDE)?;
        limits.insert(profile.id.to_string(), n);
        store.settings().set(&MAX_STREAMS_OVERRIDE, &limits)?;
    }
    tracing::info!("demo provider running on 127.0.0.1:{}", panel.port());
    Ok(Demo {
        panel,
        profile: profile.id,
    })
}

fn seed_favourites(
    store: &Store,
    profile: ProfileId,
    panel: &fake_panel::Handle,
) -> anyhow::Result<()> {
    use opensmarters_store::{ContentKind, ItemSnapshot};
    let base = panel.base_url().trim_end_matches('/');
    let cat = panel.catalogue();
    let favs = store.favourites();
    for c in cat.live.iter().take(5) {
        let snap = ItemSnapshot {
            name: c.name.trim().to_owned(),
            icon_url: Some(format!("{base}/images/logo/{}.png", c.stream_id)),
            category_id: Some(c.category_id.to_string()),
        };
        favs.add(profile, ContentKind::Live, &c.stream_id.to_string(), &snap)?;
    }
    for m in cat.movies.iter().take(4) {
        let snap = ItemSnapshot {
            name: m.name.clone(),
            icon_url: Some(format!("{base}/images/poster/{}.png", m.stream_id)),
            category_id: Some(m.category_id.to_string()),
        };
        favs.add(profile, ContentKind::Movie, &m.stream_id.to_string(), &snap)?;
    }
    for s in cat.series.iter().take(3) {
        let snap = ItemSnapshot {
            name: s.name.clone(),
            icon_url: Some(format!("{base}/images/cover/{}.png", s.series_id)),
            category_id: Some(s.category_id.to_string()),
        };
        favs.add(
            profile,
            ContentKind::Series,
            &s.series_id.to_string(),
            &snap,
        )?;
    }
    Ok(())
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! `--demo`: the fake panel (`crates/fake-panel`, synthetic data and generated media)
//! runs in-process on `127.0.0.1:<random port>`, that exact address is allowed by the
//! SSRF policy (nothing else on loopback is), and a demo person with a demo provider is
//! created, like the Windows app's `--demo`.
//!
//! The demo sign-in is **`demo` / `demo-password`** (an administrator). The demo
//! provider is the fake panel's Xtream account `demo` / `demo`; it is re-pointed at the
//! panel's new port at every start. `--demo-fresh` skips the person and the provider so
//! the first-run screens can be tried (the setup code and the panel address are logged).
//!
//! Live channels always loop at their real bitrate (`live-endless`), like a real live
//! stream. Without it a channel is a short clip sent at once: ffmpeg finishes in a few
//! milliseconds and the browser sees a live stream that has already ended.
//!
//! Dev only: `OPENSMARTERS_DEMO_QUIRKS=timeshift-smarters-only` (any comma-separated
//! `fake_panel::Quirks` names) makes the panel misbehave like real ones, as in the
//! Windows app's demo.

use std::path::Path;

use secrecy::SecretString;

use crate::db::{self, AccountRow, User};
use crate::provider::{self, Creds};
use crate::state::AppState;

/// The demo person's name.
pub const DEMO_USER: &str = "demo";
/// The demo person's password (printed in the log; demo only).
pub const DEMO_PASSWORD: &str = "demo-password";
/// The demo provider account's name.
pub const DEMO_ACCOUNT: &str = "Demo provider";

/// Starts the fake panel; its media is generated with ffmpeg into `data_dir/demo-media`
/// before it returns (about 4 s on the first start, nothing after), so no stream is ever
/// asked for before its media exists (in the background, a channel opened in the first
/// seconds got a 503 and the player showed "couldn't start").
///
/// # Errors
///
/// When the panel can't bind, or `OPENSMARTERS_DEMO_QUIRKS` names an unknown quirk.
pub async fn start_panel(data_dir: &Path) -> anyhow::Result<fake_panel::Handle> {
    let mut quirks = match std::env::var("OPENSMARTERS_DEMO_QUIRKS") {
        Ok(list) if !list.trim().is_empty() => fake_panel::Quirks::parse_list([list])?,
        _ => fake_panel::Quirks::default(),
    };
    quirks.live_endless = true;
    let config = fake_panel::Config {
        media: fake_panel::MediaMode::Generate,
        media_dir: data_dir.join("demo-media"),
        quirks,
        ..fake_panel::Config::default()
    };
    Ok(fake_panel::spawn(config).await?)
}

/// Creates (or refreshes) the demo person and their demo provider.
///
/// # Errors
///
/// Database or hashing failures.
pub async fn ensure_demo_user(state: &AppState, panel: &fake_panel::Handle) -> anyhow::Result<()> {
    let existing = state.db.call(|c| db::user_by_name(c, DEMO_USER)).await?;
    let hash = state
        .passwords
        .hash(&SecretString::from(DEMO_PASSWORD.to_owned()))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let user = match existing {
        Some((u, _)) => {
            let (id, h) = (u.id.clone(), hash.clone());
            state
                .db
                .call(move |c| {
                    db::set_password_hash(c, &id, &h)?;
                    db::set_disabled(c, &id, false)
                })
                .await?;
            u
        }
        None => {
            let u = User {
                id: crate::crypto::random_id()?,
                name: DEMO_USER.to_owned(),
                is_admin: true,
                disabled: false,
                created_at: db::now(),
                last_seen_at: None,
            };
            let (row, h) = (u.clone(), hash.clone());
            state.db.call(move |c| db::insert_user(c, &row, &h)).await?;
            u
        }
    };
    state.close_setup();

    let server = opensmarters_core::ServerUrl::parse(panel.base_url())?;
    let uid = user.id.clone();
    let accounts = state.db.call(move |c| db::list_accounts(c, &uid)).await?;
    let (id, created_at, timeshift_format) = match accounts.iter().find(|a| a.name == DEMO_ACCOUNT)
    {
        Some(a) => (a.id.clone(), a.created_at, a.timeshift_format.clone()),
        None => (crate::crypto::random_id()?, db::now(), None),
    };
    let creds = Creds::Xtream {
        server: server.clone(),
        username: fake_panel::users::DEMO.username.to_owned(),
        password: SecretString::from(fake_panel::users::DEMO.password.to_owned()),
    };
    let row = AccountRow {
        id: id.clone(),
        user_id: user.id.clone(),
        name: DEMO_ACCOUNT.to_owned(),
        kind: "xtream".to_owned(),
        host: server.host_str().to_owned(),
        server: Some(server.to_string()),
        username: Some(fake_panel::users::DEMO.username.to_owned()),
        secret: provider::seal(state, &user.id, &id, &creds).map_err(|e| anyhow::anyhow!("{e}"))?,
        status: "active".to_owned(),
        expires_at: None,
        max_connections: Some(2),
        active_connections: Some(0),
        created_at,
        // The panel is plain http on loopback.
        insecure: Some(provider::insecure_of(&creds, None)),
        timeshift_format,
    };
    state.db.call(move |c| db::upsert_account(c, &row)).await?;
    state.providers.drop_account(&id);
    tracing::warn!(
        "demo: sign in as \"{DEMO_USER}\" with the password \"{DEMO_PASSWORD}\" (demo data only)"
    );
    Ok(())
}

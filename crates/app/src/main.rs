// SPDX-License-Identifier: GPL-3.0-or-later
//! opensmarters desktop app entry point: parses the command line, sets up logging, the
//! tokio runtime and the store, then runs the eframe app.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use eframe::egui;
use opensmarters_app::app::{AppConfig, OpensmartersApp, Startup};
use opensmarters_app::cli::Cli;
use opensmarters_app::services::{Paths, Prefs, Services};
use opensmarters_core::Redactor;
use opensmarters_store::{
    DbLocation, KeyringSecretStore, MemorySecretStore, SecretStore, Store, StoreOptions, settings,
};

/// Routes panics through `tracing` (the redacting writer) instead of the default hook,
/// which prints the raw message to stderr. Unwinding and the exit code are unchanged: a
/// panic on the main thread still ends the process (code 101), a panic in a worker task
/// still only ends that task.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(non-text panic payload)".to_owned());
        let location = info
            .location()
            .map_or_else(String::new, |l| format!("{}:{}", l.file(), l.line()));
        let thread = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_owned();
        let backtrace = std::backtrace::Backtrace::capture();
        tracing::error!(
            target: "panic",
            %thread,
            %location,
            "panic: {message}\n{backtrace}"
        );
    }));
}

fn main() -> anyhow::Result<()> {
    // First of all: DLLs loaded by name later (libmpv's dependencies, GPU drivers, the
    // window system) must never come from the working directory or PATH (SR-06).
    let _ = opensmarters_player::harden_dll_search_path();
    let cli = Cli::parse();
    if cli.demo && !cfg!(feature = "demo") {
        anyhow::bail!("--demo needs a build with `--features demo`");
    }

    let paths = Paths {
        data_dir: match (&cli.data_dir, cli.ephemeral) {
            (Some(d), _) => Some(d.clone()),
            (None, true) => None,
            (None, false) => Paths::default_data_dir(),
        },
    };
    let redactor = Redactor::new();
    let log_file = opensmarters_app::logging::init(
        &redactor,
        paths.log_dir().as_deref(),
        cfg!(debug_assertions) || cli.log_stderr,
    )?;
    // Panic text can carry provider data: log it through the redacting writer (SR-16).
    install_panic_hook();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ephemeral = cli.ephemeral,
        log = ?log_file,
        "starting opensmarters"
    );

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("opensmarters-io")
            .enable_all()
            .build()
            .context("tokio runtime")?,
    );

    let (location, secrets): (DbLocation, Arc<dyn SecretStore>) = if cli.ephemeral {
        (DbLocation::Memory, Arc::new(MemorySecretStore::new()))
    } else {
        let loc = match &paths.data_dir {
            Some(d) => DbLocation::Path(d.join(opensmarters_store::DB_FILE_NAME)),
            None => DbLocation::Default,
        };
        (loc, Arc::new(KeyringSecretStore::new()?))
    };
    let store = Arc::new(Store::open(
        StoreOptions {
            location,
            ..StoreOptions::default()
        },
        secrets,
    )?);
    let prefs = Prefs::load(&store).unwrap_or_default();
    // Window size/position from the last run (never for ephemeral or harness runs).
    let window = (!cli.ephemeral && cli.window_size.is_none()).then(|| {
        store
            .settings()
            .get(&opensmarters_app::window_state::WINDOW)
            .ok()
            .flatten()
            .and_then(|w| w.sanitized())
    });

    #[cfg(feature = "demo")]
    let demo = if cli.demo {
        Some(opensmarters_app::services::demo::start(
            &rt,
            &store,
            cli.ephemeral,
        )?)
    } else {
        None
    };

    // Which profile to open first.
    let profiles = store.profiles().list()?;
    #[allow(unused_mut)]
    let mut startup = if profiles.is_empty() {
        Startup::Onboarding
    } else {
        let last = store.settings().get(&settings::LAST_PROFILE).ok().flatten();
        match last.and_then(|id| profiles.iter().find(|p| p.id == id)) {
            Some(p) => Startup::Open(p.id, p.name.clone()),
            None if profiles.len() == 1 => Startup::Open(profiles[0].id, profiles[0].name.clone()),
            None => Startup::Profiles,
        }
    };
    #[cfg(feature = "demo")]
    if let Some(d) = &demo {
        startup = Startup::Open(d.profile, "Demo provider".into());
    }

    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon/icon-256.png"))
            .context("window icon")?;
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("opensmarters")
        .with_app_id("opensmarters")
        .with_icon(icon)
        .with_inner_size([1440.0, 860.0])
        .with_min_inner_size(if cli.window_size.is_some() {
            [320.0, 240.0]
        } else {
            [1024.0, 640.0]
        });
    if let Some(Some(state)) = &window {
        viewport = state.apply(viewport);
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport,
        ..Default::default()
    };

    let rt_handle = rt.handle().clone();
    let ephemeral = cli.ephemeral;
    let config = AppConfig {
        cli,
        startup,
        prefs,
        window,
        #[cfg(feature = "demo")]
        demo,
    };
    eframe::run_native(
        "opensmarters",
        options,
        Box::new(move |cc| {
            let services = Services::new(
                rt_handle,
                Arc::clone(&store),
                redactor,
                paths,
                ephemeral,
                cc.egui_ctx.clone(),
            );
            Ok(Box::new(OpensmartersApp::new(cc, services, config)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("window error: {e}"))?;
    // Keep the runtime alive until the window is gone; then shut down quickly.
    match Arc::try_unwrap(rt) {
        Ok(rt) => rt.shutdown_timeout(std::time::Duration::from_secs(2)),
        Err(rt) => drop(rt),
    }
    Ok(())
}

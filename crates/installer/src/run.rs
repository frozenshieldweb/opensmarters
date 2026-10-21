// SPDX-License-Identifier: GPL-3.0-or-later
//! The program's flow: harden the DLL search order, parse the command line, pick the
//! environment, log, relaunch from `%TEMP%` when running from an install folder, take the
//! lock, then run quietly or show the window.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser as _;

use crate::cli::{Cli, EXIT_ERROR, EXIT_OK};
use crate::env::Environment;
use crate::install::{self, Control, Options};
use crate::manifest::InstallManifest;
use crate::payload::Payload;
use crate::system::{self, SetupLock};
use crate::uninstall;
use crate::version::{self, Plan};
use crate::{brand, shortcuts};

/// What the machine looks like before we start.
#[derive(Debug, Clone)]
pub struct Situation {
    /// A valid install (folder + manifest), if any.
    pub installed: Option<(PathBuf, InstallManifest)>,
    /// What to offer.
    pub plan: Plan,
    /// Where to install: the existing install folder, else the default.
    pub dir: PathBuf,
    /// Our Start menu shortcut exists.
    pub start_menu_exists: bool,
    /// Our desktop shortcut exists.
    pub desktop_exists: bool,
}

/// Looks at the registry, the install folder and the shortcuts.
pub fn survey(env: &Environment, payload: &Payload<'_>) -> Situation {
    let installed = install::installed(env);
    let plan = version::decide(
        installed.as_ref().map(|(_, m)| m.version.as_str()),
        &payload.manifest.version,
    );
    let dir = installed
        .as_ref()
        .map_or_else(|| env.default_install_dir.clone(), |(d, _)| d.clone());
    let ours = |lnk: PathBuf| {
        installed
            .as_ref()
            .is_some_and(|(d, _)| shortcuts::is_ours(&lnk, d))
    };
    Situation {
        start_menu_exists: ours(env.start_menu_shortcut()),
        desktop_exists: ours(env.desktop_shortcut()),
        installed,
        plan,
        dir,
    }
}

/// Files the uninstaller's temporary copy deletes after the parent has exited.
#[derive(Debug, Clone, Default)]
pub struct Pending {
    /// The install folder.
    pub dir: PathBuf,
    /// Files still in use.
    pub files: Vec<PathBuf>,
}

/// The program's entry point; returns the exit code.
pub fn main() -> i32 {
    // First of all: DLLs loaded by name later (GPU drivers, the shell) never come from the
    // folder setup runs in (Downloads). Static imports are covered by /DEPENDENTLOADFLAG.
    if !opensmarters_player::harden_dll_search_path() {
        // Nothing else to do; logged once logging is up.
    }
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            let ok = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
            system::tell(&e.to_string(), !ok);
            return if ok { EXIT_OK } else { EXIT_ERROR };
        }
    };
    let env = match environment(&cli) {
        Ok(env) => env,
        Err(msg) => {
            system::tell(&msg, true);
            return EXIT_ERROR;
        }
    };
    let log = crate::log::init(cli.log.as_deref().unwrap_or(&env.default_log()));
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "can't locate the running program");
            system::tell("Setup couldn't find its own file. Download it again.", true);
            return EXIT_ERROR;
        }
    };
    system::clean_temp_copies(&env, &self_exe);

    // Running as uninstall.exe from an install folder: work from a temporary copy, so the
    // folder can be removed or replaced.
    if !cli.from_temp && !is_preview(&cli) && system::installed_dir_of(&self_exe).is_some() {
        return match system::relaunch_from_temp(&env, &self_exe, &args[1..]) {
            Ok(code) => code,
            Err(e) => {
                tracing::error!(error = %e, "could not start the temporary copy");
                if !cli.quiet {
                    system::show_message("Setup couldn't start. Try again.", true);
                }
                EXIT_ERROR
            }
        };
    }

    let _lock = match SetupLock::acquire(&env) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            tracing::warn!("another setup is running");
            if !cli.quiet {
                system::show_message("opensmarters setup is already running.", false);
            }
            report(&cli, EXIT_ERROR, None);
            return EXIT_ERROR;
        }
        Err(e) => {
            tracing::warn!(error = %e, "no setup lock; continuing");
            None
        }
    };
    let payload = match Payload::embedded() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "embedded payload");
            system::tell(
                "The installer is damaged. Download it again and check its SHA-256.",
                true,
            );
            report(&cli, EXIT_ERROR, None);
            return EXIT_ERROR;
        }
    };
    let (code, pending) = if cli.quiet {
        quiet(&cli, &env, &payload, &self_exe)
    } else {
        crate::ui::run(crate::ui::Config {
            cli: cli.clone(),
            env: env.clone(),
            payload,
            self_exe: self_exe.clone(),
            log,
        })
    };
    tracing::info!(code, "setup ends");
    report(&cli, code, pending);
    code
}

fn is_preview(cli: &Cli) -> bool {
    cli.preview.is_some()
}

fn environment(cli: &Cli) -> Result<Environment, String> {
    if let Some(root) = &cli.sandbox {
        return Environment::sandbox(root).map_err(|e| format!("sandbox {}: {e}", root.display()));
    }
    Environment::real().map_err(|e| e.to_string())
}

/// In the uninstaller's temporary copy: hands the exit code to the waiting parent, then
/// finishes deleting what the parent held.
fn report(cli: &Cli, code: i32, pending: Option<Pending>) {
    if !cli.from_temp {
        return;
    }
    system::report_to_parent(code);
    if let Some(p) = pending
        && !p.files.is_empty()
    {
        let done = uninstall::finish_pending(&p.dir, &p.files, Duration::from_secs(15));
        tracing::info!(done, dir = %p.dir.display(), "removed the remaining files");
    }
}

/// The options a quiet run uses: the command line, else what is installed, else defaults.
pub fn quiet_options(cli: &Cli, s: &Situation) -> Options {
    let update = s.installed.is_some();
    Options {
        dir: cli.dir.clone().unwrap_or_else(|| s.dir.clone()),
        start_menu: cli.start_menu(),
        desktop: cli.desktop_shortcut || (update && !cli.no_shortcuts && s.desktop_exists),
    }
}

/// `--quiet`: no window.
pub fn quiet(
    cli: &Cli,
    env: &Environment,
    payload: &Payload<'_>,
    self_exe: &Path,
) -> (i32, Option<Pending>) {
    let ctl = Control::default();
    if cli.uninstall {
        let opts = uninstall::Options {
            purge_data: cli.purge_data,
            purge_passwords: cli.purge_passwords,
        };
        return match uninstall::uninstall(env, opts, &ctl, self_exe) {
            Ok(o) => {
                if o.nothing_installed {
                    tracing::info!("nothing was installed");
                }
                let pending = o.dir.clone().map(|dir| Pending {
                    dir,
                    files: o.pending.clone(),
                });
                (EXIT_OK, pending)
            }
            Err(e) => {
                tracing::error!(error = %e, "uninstall failed");
                (e.exit_code(), None)
            }
        };
    }
    let s = survey(env, payload);
    if let (Some((installed, _)), Some(wanted)) = (&s.installed, &cli.dir)
        && crate::paths::validate_install_dir(wanted, &env.protected)
            .ok()
            .as_ref()
            != Some(installed)
    {
        tracing::error!(installed = %installed.display(), "already installed elsewhere; uninstall first to move it");
        return (EXIT_ERROR, None);
    }
    if let Plan::Downgrade { installed } = &s.plan {
        tracing::error!(%installed, this = %payload.manifest.version, "a newer version is installed; run setup with its window to install this older one");
        return (EXIT_ERROR, None);
    }
    let opts = quiet_options(cli, &s);
    match install::install(env, payload, &opts, self_exe, &ctl) {
        Ok(outcome) => {
            if !cli.no_launch {
                launch(env, &outcome.app_exe);
            }
            (EXIT_OK, None)
        }
        Err(e) => {
            tracing::error!(error = %e, message = %e.message(), "install failed");
            (e.exit_code(), None)
        }
    }
}

/// Opens the app (never in a sandbox: its "app" is synthetic).
pub fn launch(env: &Environment, app_exe: &Path) {
    if env.is_sandbox() {
        tracing::info!(app = %app_exe.display(), "sandbox: not opening the app");
        return;
    }
    if let Err(e) = system::launch_app(app_exe) {
        tracing::warn!(error = %e, "could not open {}", brand::PRODUCT);
    }
}

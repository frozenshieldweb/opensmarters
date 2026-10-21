// SPDX-License-Identifier: GPL-3.0-or-later
//! Uninstall (LAUNCH_PROMPT §B3.7, §B5).
//!
//! - Deletes **only the files listed** in the install manifest, then the folders they
//!   leave empty, then the install folder if it is empty. Files the user added stay.
//! - Removes our shortcuts (only `.lnk` files that point into the install folder) and the
//!   Apps & features entry.
//! - The app's data folder and saved passwords are touched **only** when asked
//!   (`purge_data`, `purge_passwords`; unticked by default). Recordings and downloads in
//!   the Videos folder are never touched.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::brand;
use crate::env::Environment;
use crate::fsops;
use crate::install::{Control, Phase, app_running};
use crate::manifest::InstallManifest;
use crate::paths::{self, FolderState};
use crate::shortcuts;

/// What to remove besides the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// Also delete the app's data folder (favourites, history, settings).
    pub purge_data: bool,
    /// Also delete the app's saved passwords from Windows Credential Manager.
    pub purge_passwords: bool,
}

/// Why an uninstall stopped.
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    /// The app is running.
    #[error("opensmarters is running")]
    AppRunning,
    /// Removing the Apps & features entry failed.
    #[error("registry: {0}")]
    Registry(std::io::Error),
}

impl UninstallError {
    /// The plain sentence for the user.
    pub fn message(&self) -> String {
        match self {
            Self::AppRunning => "opensmarters is open. Close it to continue.".to_owned(),
            Self::Registry(_) => {
                "Windows didn't let opensmarters remove itself from Apps. Try again.".to_owned()
            }
        }
    }

    /// Exit code for `--quiet`: 3 app running, else 1.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::AppRunning => 3,
            Self::Registry(_) => 1,
        }
    }
}

/// What happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The install folder, if one was found.
    pub dir: Option<PathBuf>,
    /// Nothing was installed (no registry entry, no folder).
    pub nothing_installed: bool,
    /// Listed files that couldn't be deleted yet (e.g. the uninstaller that started us).
    pub pending: Vec<PathBuf>,
    /// The install folder still exists (it holds files the user added, or pending ones).
    pub folder_kept: bool,
    /// The data folder was deleted.
    pub data_removed: bool,
    /// How many saved passwords were deleted.
    pub passwords_removed: usize,
    /// Non-fatal problems (plain text, for the log and "Copy details").
    pub problems: Vec<String>,
}

/// Finds the install: the registry's `InstallLocation`, validated and owned.
fn locate(env: &Environment) -> (Option<PathBuf>, Option<InstallManifest>, bool) {
    let info = env.registry.read_installed().ok().flatten();
    let registered = info.is_some();
    let Some(info) = info else {
        return (None, None, false);
    };
    let Ok(dir) = paths::validate_install_dir(Path::new(&info.location), &env.protected) else {
        return (None, None, registered);
    };
    match paths::folder_state(&dir) {
        Ok(FolderState::Owned(m)) => (Some(dir), Some(m), registered),
        _ => (Some(dir), None, registered),
    }
}

/// Removes opensmarters. `self_exe` (the running program) is never deleted here; if it is
/// inside the install folder it ends up in [`Outcome::pending`].
///
/// # Errors
///
/// [`UninstallError::AppRunning`] (nothing was changed) or a registry failure (files may
/// already be gone; running uninstall again finishes the job).
pub fn uninstall(
    env: &Environment,
    opts: Options,
    ctl: &Control,
    self_exe: &Path,
) -> Result<Outcome, UninstallError> {
    let p = &ctl.progress;
    p.set(Phase::Removing, 0.0);
    let (dir, manifest, registered) = locate(env);
    let mut out = Outcome {
        dir: dir.clone(),
        nothing_installed: !registered && manifest.is_none(),
        ..Outcome::default()
    };
    tracing::info!(dir = ?dir, registered, purge_data = opts.purge_data, purge_passwords = opts.purge_passwords, "uninstall starts");
    if let (Some(dir), Some(_)) = (&dir, &manifest)
        && app_running(dir)
    {
        return Err(UninstallError::AppRunning);
    }

    // 1. Files (0% … 80%).
    if let (Some(dir), Some(manifest)) = (&dir, &manifest) {
        let self_canon = self_exe.canonicalize().ok();
        let mut listed: Vec<String> = Vec::new();
        for f in &manifest.files {
            let path = dir.join(f.native());
            let is_self = self_canon
                .as_ref()
                .is_some_and(|s| path.canonicalize().ok().as_ref() == Some(s));
            if is_self {
                out.pending.push(path);
            } else {
                listed.push(f.path.clone());
            }
        }
        let total = listed.len().max(1);
        for (i, chunk) in listed.chunks(8).enumerate() {
            let r = fsops::remove_listed(dir, chunk);
            out.pending.extend(r.failed);
            p.set(
                Phase::Removing,
                0.8 * ((i + 1) * 8).min(total) as f32 / total as f32,
            );
            if let Some(d) = ctl.slow {
                std::thread::sleep(d);
            }
        }
        if out.pending.is_empty() {
            let r = fsops::remove_listed(dir, &[brand::INSTALL_MANIFEST.to_owned()]);
            out.folder_kept = !r.dir_removed;
        } else {
            out.folder_kept = true;
        }
    }

    // 2. Shortcuts (80% … 85%).
    p.set(Phase::Shortcuts, 0.8);
    if let Some(dir) = &dir {
        for lnk in [env.start_menu_shortcut(), env.desktop_shortcut()] {
            if let Err(e) = shortcuts::remove_if_ours(&lnk, dir) {
                out.problems.push(format!("{}: {e}", lnk.display()));
            }
        }
    }

    // 3. Apps & features (85% … 90%).
    p.set(Phase::Finishing, 0.85);
    env.registry.delete().map_err(UninstallError::Registry)?;
    // A sandbox leaves no empty test registry tree behind.
    if env.registry.is_test() {
        env.registry.delete_test_tree();
    }

    // 4. Only if asked: the data folder (90% … 95%) and saved passwords (95% … 100%).
    p.set(Phase::Finishing, 0.9);
    if opts.purge_data {
        match purge_data(&env.data_dir) {
            Ok(removed) => out.data_removed = removed,
            Err(e) => out
                .problems
                .push(format!("data folder {}: {e}", env.data_dir.display())),
        }
    }
    p.set(Phase::Finishing, 0.95);
    if opts.purge_passwords {
        match env.keychain.purge() {
            Ok(n) => out.passwords_removed = n,
            Err(e) => out.problems.push(e.to_string()),
        }
    }
    p.set(Phase::Finishing, 1.0);
    for pr in &out.problems {
        tracing::warn!("uninstall: {pr}");
    }
    tracing::info!(
        pending = out.pending.len(),
        folder_kept = out.folder_kept,
        "uninstall done"
    );
    Ok(out)
}

/// Deletes the app's data folder. Refuses a link (removes nothing behind it).
///
/// # Errors
///
/// I/O errors (e.g. the app is running and holds the database).
pub fn purge_data(data_dir: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(data_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the data folder is a link or a file; left alone",
        )),
        // `remove_dir_all` removes links inside as links, never following them.
        Ok(_) => std::fs::remove_dir_all(data_dir).map(|()| true),
    }
}

/// After the parent uninstaller has exited: retries deleting `pending` files for up to
/// `timeout`, then the install manifest and the folder if it is empty.
pub fn finish_pending(dir: &Path, pending: &[PathBuf], timeout: Duration) -> bool {
    let start = Instant::now();
    let mut left: Vec<PathBuf> = pending.to_vec();
    while !left.is_empty() && start.elapsed() < timeout {
        left.retain(|p| match std::fs::remove_file(p) {
            Ok(()) => false,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        });
        if !left.is_empty() {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    let rels: Vec<String> = pending
        .iter()
        .filter_map(|p| p.strip_prefix(dir).ok())
        .filter_map(|r| {
            let parts: Option<Vec<&str>> = r.components().map(|c| c.as_os_str().to_str()).collect();
            parts.map(|v| v.join("/"))
        })
        .chain([brand::INSTALL_MANIFEST.to_owned()])
        .collect();
    let r = fsops::remove_listed(dir, &rels);
    left.is_empty() && r.dir_removed
}

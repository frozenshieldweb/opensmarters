// SPDX-License-Identifier: GPL-3.0-or-later
//! Install, update and repair: one atomic, recoverable transaction (ADR 0002, decision 4).
//!
//! ```text
//! archive hash ─▶ validate the folder ─▶ app closed? ─▶ extract to <dir>.new (hash each file)
//!   ─▶ copy self as uninstall.exe, write install-manifest.txt ─▶ read back and verify all
//!   ─▶ swap <dir> → <dir>.old, <dir>.new → <dir> ─▶ shortcuts ─▶ registry
//!   ─▶ commit: delete <dir>.old (listed files only)
//! ```
//!
//! Every step before the commit is undone on any failure or on cancel ([`Journal`]).
//! Registry and shortcuts are written last. The light in the UI follows [`Progress`],
//! which reports real work only (bytes extracted, bytes verified, steps done).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::brand;
use crate::env::Environment;
use crate::fsops;
use crate::manifest::{FileEntry, InstallManifest};
use crate::paths::{self, FolderState, PathProblem};
use crate::payload::{self, Payload, PayloadError};
use crate::registry::{Snapshot, UninstallEntry};
use crate::shortcuts::{self, Backup};

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Install folder (validated again here).
    pub dir: PathBuf,
    /// Start menu shortcut.
    pub start_menu: bool,
    /// Desktop shortcut.
    pub desktop: bool,
}

/// The stage of the work, for the one status line (in order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Phase {
    /// Checking the installer and the folder.
    #[default]
    Preparing,
    /// Extracting files.
    Unpacking,
    /// Reading every file back and checking its hash.
    Checking,
    /// Shortcuts.
    Shortcuts,
    /// Registry and clean-up.
    Finishing,
    /// Removing files (uninstall).
    Removing,
    /// Undoing after a failure or cancel.
    RollingBack,
}

#[derive(Debug, Default, Clone, Copy)]
struct ProgressState {
    phase: Phase,
    fraction: f32,
}

/// Shared progress (worker writes, UI reads). `fraction` only grows.
#[derive(Debug, Clone, Default)]
pub struct Progress(Arc<Mutex<ProgressState>>);

impl Progress {
    /// Current phase and overall fraction (0..1).
    pub fn get(&self) -> (Phase, f32) {
        let s = *self.0.lock().unwrap_or_else(PoisonError::into_inner);
        (s.phase, s.fraction)
    }

    /// Sets the phase and moves the fraction forward to `fraction` (never back, except for
    /// [`Phase::RollingBack`], which the UI shows differently anyway).
    pub fn set(&self, phase: Phase, fraction: f32) {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.phase = phase;
        let f = fraction.clamp(0.0, 1.0);
        if f > s.fraction || phase == Phase::RollingBack {
            s.fraction = f;
        }
    }
}

/// Knobs shared by install and uninstall.
#[derive(Debug, Clone, Default)]
pub struct Control {
    /// Progress for the UI.
    pub progress: Progress,
    /// Set to cancel (honoured until the swap).
    pub cancel: Arc<AtomicBool>,
    /// Dev builds only: slow the work down so the progress can be watched.
    pub slow: Option<Duration>,
    /// Tests only: fail on purpose after this step to exercise the rollback.
    #[doc(hidden)]
    pub fail_after: Option<FailPoint>,
}

/// Where a test makes the transaction fail on purpose.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    /// After the folders were swapped.
    Swap,
    /// After the registry entry was written (the last step before the commit).
    Registry,
}

fn injected(ctl: &Control, at: FailPoint) -> Result<(), InstallError> {
    if ctl.fail_after == Some(at) {
        return Err(InstallError::Io {
            step: "test",
            path: PathBuf::new(),
            source: io::Error::other("injected failure"),
        });
    }
    Ok(())
}

/// Why an install stopped. [`InstallError::message`] is the plain sentence for the user.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// The archive or a file failed its hash check.
    #[error("damaged: {0}")]
    Damaged(String),
    /// The user cancelled; everything was rolled back.
    #[error("cancelled")]
    Cancelled,
    /// The app is running from the install folder.
    #[error("opensmarters is running")]
    AppRunning,
    /// The install folder can't be used.
    #[error("install folder: {0:?}")]
    Path(PathProblem),
    /// Something (a window, a terminal, an antivirus scan) holds the folder.
    #[error("the folder {0} is in use")]
    FolderBusy(PathBuf),
    /// A leftover folder from an interrupted run holds files we don't know.
    #[error("{0} is in the way")]
    InTheWay(PathBuf),
    /// An I/O error at a named step.
    #[error("{step}: {path}: {source}")]
    Io {
        /// What we were doing.
        step: &'static str,
        /// On what.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: io::Error,
    },
}

impl InstallError {
    /// The plain sentence shown to the user (what happened and what to try).
    pub fn message(&self) -> String {
        match self {
            Self::Damaged(_) => {
                "The installer is damaged. Download it again and check its SHA-256.".to_owned()
            }
            Self::Cancelled => "Installation stopped. Nothing was changed.".to_owned(),
            Self::AppRunning => "opensmarters is open. Close it to continue.".to_owned(),
            Self::Path(p) => p.to_string(),
            Self::FolderBusy(_) => "Another window or program is using the opensmarters folder. Close it, then try again.".to_owned(),
            Self::InTheWay(p) => format!(
                "A folder from an earlier attempt is in the way: {}. Move or delete it, then try again.",
                p.display()
            ),
            Self::Io { step, .. } => match *step {
                "registry" => "Windows didn't let opensmarters add itself to Apps. Nothing was changed. Try again.".to_owned(),
                "shortcut" => "The shortcut couldn't be created. Nothing was changed. Try again, or turn shortcuts off in Options.".to_owned(),
                _ => "Some files couldn't be written. Nothing was changed. Check that the drive has space, then try again.".to_owned(),
            },
        }
    }

    /// The process exit code for `--quiet` runs: 2 cancelled, 3 app running, else 1.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Cancelled => 2,
            Self::AppRunning => 3,
            _ => 1,
        }
    }
}

fn io_err<'a>(step: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> InstallError + 'a {
    move |source| InstallError::Io {
        step,
        path: path.to_path_buf(),
        source,
    }
}

/// The result of a successful install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The install folder (canonical).
    pub dir: PathBuf,
    /// The app executable.
    pub app_exe: PathBuf,
    /// Things left behind that the user may want to know about (e.g. an old folder that
    /// held files of theirs).
    pub leftovers: Vec<PathBuf>,
}

/// True if the app (or its DLL) in `dir` is mapped by a running process.
pub fn app_running(dir: &Path) -> bool {
    [brand::APP_EXE, brand::MPV_DLL]
        .iter()
        .any(|f| fsops::in_use(&dir.join(f)))
}

/// What the transaction did so far, so it can be undone.
#[derive(Debug, Default)]
struct Journal {
    /// `<dir>.new` while it exists.
    staged: Option<PathBuf>,
    /// The swap happened: `(dir, <dir>.old if there was an old install)`.
    swapped: Option<(PathBuf, Option<PathBuf>)>,
    /// An empty target folder we removed to make room (recreated on rollback).
    removed_empty: Option<PathBuf>,
    /// Shortcut backups, in the order taken.
    shortcuts: Vec<Backup>,
    /// The registry before we wrote it.
    registry: Option<Snapshot>,
}

impl Journal {
    /// Undo everything, best effort, in reverse order. Returns what couldn't be undone.
    fn rollback(&mut self, env: &Environment, known: &[String]) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(snap) = self.registry.take()
            && let Err(e) = env.registry.restore(&snap)
        {
            problems.push(format!("registry: {e}"));
        }
        for b in self.shortcuts.drain(..).rev() {
            if let Err(e) = b.restore() {
                problems.push(format!("shortcut: {e}"));
            }
        }
        if let Some((dir, old)) = self.swapped.take() {
            // Move the new install aside, put the old one back, delete the new one.
            let aside = paths::sibling(&dir, ".new");
            match std::fs::rename(&dir, &aside) {
                Ok(()) => {
                    if let Some(old) = old
                        && let Err(e) = std::fs::rename(&old, &dir)
                    {
                        problems.push(format!("restoring {}: {e}", old.display()));
                    }
                    self.staged = Some(aside);
                }
                Err(e) => problems.push(format!("moving the new files aside: {e}")),
            }
        }
        if let Some(staged) = self.staged.take() {
            let r = fsops::remove_listed(&staged, known);
            if !r.dir_removed {
                problems.push(format!("{} was not fully removed", staged.display()));
            }
        }
        if let Some(empty) = self.removed_empty.take()
            && !empty.exists()
        {
            let _ = std::fs::create_dir(&empty);
        }
        problems
    }
}

/// Every relative path the installer may create in an install folder, lower-cased.
fn known_set(payload: &Payload<'_>, old: Option<&InstallManifest>) -> HashSet<String> {
    let mut known: HashSet<String> = payload
        .manifest
        .files
        .iter()
        .map(|f| f.path.to_lowercase())
        .collect();
    if let Some(old) = old {
        known.extend(old.files.iter().map(|f| f.path.to_lowercase()));
    }
    known.insert(brand::UNINSTALL_EXE.to_lowercase());
    known.insert(brand::INSTALL_MANIFEST.to_lowercase());
    known
}

fn known_list(payload: &Payload<'_>) -> Vec<String> {
    let mut v: Vec<String> = payload
        .manifest
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect();
    v.push(brand::UNINSTALL_EXE.to_owned());
    v.push(brand::INSTALL_MANIFEST.to_owned());
    v
}

/// Installs (or updates, or repairs) `payload` into `opts.dir`. `self_exe` is copied as
/// `uninstall.exe`.
///
/// # Errors
///
/// An [`InstallError`]; everything has been rolled back when it returns.
pub fn install(
    env: &Environment,
    payload: &Payload<'_>,
    opts: &Options,
    self_exe: &Path,
    ctl: &Control,
) -> Result<Outcome, InstallError> {
    let p = &ctl.progress;
    p.set(Phase::Preparing, 0.0);
    tracing::info!(dir = %opts.dir.display(), version = %payload.manifest.version, "install starts");
    payload
        .verify_archive()
        .map_err(|e| InstallError::Damaged(e.to_string()))?;
    let (dir, state) =
        paths::check_target(&opts.dir, &env.protected).map_err(InstallError::Path)?;
    let old_manifest = match &state {
        FolderState::Owned(m) => Some(m.clone()),
        _ => None,
    };
    if old_manifest.is_some() && app_running(&dir) {
        return Err(InstallError::AppRunning);
    }
    let staged = paths::sibling(&dir, ".new");
    let old = paths::sibling(&dir, ".old");
    let known = known_set(payload, old_manifest.as_ref());
    for s in [&staged, &old] {
        fsops::clean_stale(s, &known).map_err(|e| InstallError::InTheWay(e.0))?;
    }
    let mut journal = Journal::default();
    let known_files = known_list(payload);
    let result = run(
        env,
        payload,
        opts,
        self_exe,
        ctl,
        &dir,
        &state,
        &mut journal,
    );
    match result {
        Ok(()) => {
            // Commit: the old install goes (listed files only).
            let mut leftovers = Vec::new();
            if let Some((_, Some(old_dir))) = &journal.swapped {
                // Files the user added to the install folder move into the new one.
                let moved = fsops::carry_over(old_dir, &dir, &known);
                if !moved.is_empty() {
                    tracing::info!(count = moved.len(), "kept files the user added");
                }
                let mut listed: Vec<String> = old_manifest
                    .as_ref()
                    .map(|m| m.files.iter().map(|f| f.path.clone()).collect())
                    .unwrap_or_default();
                listed.push(brand::INSTALL_MANIFEST.to_owned());
                let r = fsops::remove_listed(old_dir, &listed);
                if !r.dir_removed && !fsops::prune_empty_dirs(old_dir) {
                    tracing::warn!(dir = %old_dir.display(), failed = r.failed.len(), "the old install folder kept some files");
                    leftovers.push(old_dir.clone());
                }
            }
            p.set(Phase::Finishing, 1.0);
            tracing::info!(dir = %dir.display(), "install done");
            Ok(Outcome {
                app_exe: dir.join(brand::APP_EXE),
                dir,
                leftovers,
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "install failed, rolling back");
            p.set(Phase::RollingBack, 0.0);
            let problems = journal.rollback(env, &known_files);
            for pr in &problems {
                tracing::error!("rollback: {pr}");
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    env: &Environment,
    payload: &Payload<'_>,
    opts: &Options,
    self_exe: &Path,
    ctl: &Control,
    dir: &Path,
    state: &FolderState,
    journal: &mut Journal,
) -> Result<(), InstallError> {
    let p = &ctl.progress;
    let total = payload.manifest.installed_size.max(1) as f32;
    let staged = paths::sibling(dir, ".new");
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(io_err("create folder", parent))?;
    }
    std::fs::create_dir(&staged).map_err(io_err("create folder", &staged))?;
    journal.staged = Some(staged.clone());

    // 1. Unpack (2% … 78%).
    p.set(Phase::Unpacking, 0.02);
    payload
        .extract(
            &staged,
            &|done| p.set(Phase::Unpacking, 0.02 + 0.76 * (done as f32 / total)),
            &ctl.cancel,
            ctl.slow,
        )
        .map_err(|e| match e {
            PayloadError::Damaged(d) => InstallError::Damaged(d),
            PayloadError::Cancelled => InstallError::Cancelled,
            PayloadError::Write { path, source } => InstallError::Io {
                step: "write",
                path: PathBuf::from(path),
                source,
            },
        })?;

    // 2. The uninstaller and the install manifest.
    let uninstaller = staged.join(brand::UNINSTALL_EXE);
    std::fs::copy(self_exe, &uninstaller).map_err(io_err("write", &uninstaller))?;
    let (hash, size) = payload::hash_file(&uninstaller).map_err(io_err("write", &uninstaller))?;
    let mut files = payload.manifest.files.clone();
    files.push(FileEntry {
        path: brand::UNINSTALL_EXE.to_owned(),
        size,
        sha256: hash,
    });
    let manifest = InstallManifest {
        product: brand::PRODUCT.to_owned(),
        version: payload.manifest.version.clone(),
        files,
    };
    let manifest_path = staged.join(brand::INSTALL_MANIFEST);
    std::fs::write(&manifest_path, manifest.to_text()).map_err(io_err("write", &manifest_path))?;

    // 3. Read every file back (78% … 94%).
    p.set(Phase::Checking, 0.78);
    let check_total = manifest.total_size().max(1) as f32;
    let checked = std::cell::Cell::new(0_u64);
    for f in &manifest.files {
        if ctl.cancel.load(Ordering::Relaxed) {
            return Err(InstallError::Cancelled);
        }
        let ok = payload::file_matches(&staged.join(f.native()), f, &|n| {
            checked.set(checked.get() + n);
            p.set(
                Phase::Checking,
                0.78 + 0.16 * (checked.get() as f32 / check_total),
            );
        })
        .map_err(io_err("verify", &staged))?;
        if !ok {
            return Err(InstallError::Damaged(format!(
                "{} changed after it was written",
                f.path
            )));
        }
        if let Some(d) = ctl.slow {
            std::thread::sleep(d);
        }
    }
    if ctl.cancel.load(Ordering::Relaxed) {
        return Err(InstallError::Cancelled);
    }

    // 4. Swap. From here on, cancel is no longer offered (the rest takes milliseconds).
    let old = paths::sibling(dir, ".old");
    let busy = |e: io::Error, path: &Path| {
        if fsops::is_busy(&e) {
            if app_running(dir) {
                InstallError::AppRunning
            } else {
                InstallError::FolderBusy(path.to_path_buf())
            }
        } else {
            InstallError::Io {
                step: "swap",
                path: path.to_path_buf(),
                source: e,
            }
        }
    };
    let had_old = match state {
        FolderState::Owned(_) => {
            std::fs::rename(dir, &old).map_err(|e| busy(e, dir))?;
            true
        }
        FolderState::Empty => {
            std::fs::remove_dir(dir).map_err(|e| busy(e, dir))?;
            journal.removed_empty = Some(dir.to_path_buf());
            false
        }
        FolderState::Missing | FolderState::Foreign => false,
    };
    if let Err(e) = std::fs::rename(&staged, dir) {
        if had_old {
            let _ = std::fs::rename(&old, dir);
        }
        return Err(busy(e, dir));
    }
    journal.staged = None;
    journal.swapped = Some((dir.to_path_buf(), had_old.then(|| old.clone())));
    injected(ctl, FailPoint::Swap)?;

    // 5. Shortcuts (94% … 97%).
    p.set(Phase::Shortcuts, 0.94);
    let app_exe = dir.join(brand::APP_EXE);
    for (wanted, lnk) in [
        (opts.start_menu, env.start_menu_shortcut()),
        (opts.desktop, env.desktop_shortcut()),
    ] {
        journal
            .shortcuts
            .push(Backup::take(&lnk).map_err(io_err("shortcut", &lnk))?);
        if wanted {
            shortcuts::create(&lnk, &app_exe).map_err(io_err("shortcut", &lnk))?;
        } else {
            shortcuts::remove_if_ours(&lnk, dir).map_err(io_err("shortcut", &lnk))?;
        }
        if let Some(d) = ctl.slow {
            std::thread::sleep(d);
        }
    }

    // 6. Apps & features (97% … 99%).
    p.set(Phase::Finishing, 0.97);
    journal.registry = Some(
        env.registry
            .snapshot()
            .map_err(io_err("registry", Path::new(&env.registry.key_path())))?,
    );
    let kib = u32::try_from(manifest.total_size() / 1024).unwrap_or(u32::MAX);
    env.registry
        .write(&UninstallEntry {
            version: manifest.version.clone(),
            location: dir.to_string_lossy().into_owned(),
            estimated_size_kib: kib,
        })
        .map_err(io_err("registry", Path::new(&env.registry.key_path())))?;
    p.set(Phase::Finishing, 0.99);
    injected(ctl, FailPoint::Registry)?;
    Ok(())
}

/// The installed app's folder and manifest, if the registry entry points at a valid one.
pub fn installed(env: &Environment) -> Option<(PathBuf, InstallManifest)> {
    let info = env.registry.read_installed().ok()??;
    let dir = paths::validate_install_dir(Path::new(&info.location), &env.protected).ok()?;
    match paths::folder_state(&dir).ok()? {
        FolderState::Owned(m) => Some((dir, m)),
        _ => None,
    }
}

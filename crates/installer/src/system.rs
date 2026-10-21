// SPDX-License-Identifier: GPL-3.0-or-later
//! Process-level helpers, all safe `std` code:
//!
//! - [`SetupLock`]: one installer at a time (`File::try_lock` on `%TEMP%\opensmarters-setup.lock`).
//! - Self-removal (ADR 0002): [`installed_dir_of`] tells whether this exe runs from an
//!   install folder; [`relaunch_from_temp`] then copies it to `%TEMP%` and runs the copy,
//!   waiting for its result line ([`report_to_parent`]); [`clean_temp_copies`] deletes old
//!   copies on the next run.
//! - [`launch_app`]: opens the app as the desktop user, through `explorer.exe` by absolute
//!   path, so it never inherits elevation.
//! - [`stdout_attached`] / [`show_message`]: `--help` prints to a redirected stdout, or
//!   shows a message box (the installer is a Windows-subsystem program without a console).

use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::brand;
use crate::env::Environment;
use crate::paths::{self, FolderState};

/// Held while this installer runs.
#[derive(Debug)]
pub struct SetupLock {
    _file: std::fs::File,
}

impl SetupLock {
    /// Takes the lock. `Ok(None)` if another installer holds it.
    ///
    /// # Errors
    ///
    /// The lock file can't be opened.
    pub fn acquire(env: &Environment) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(&env.temp_dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(env.temp_dir.join(brand::LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

/// If `exe` lives in an opensmarters install folder (a valid install manifest next to
/// it), that folder.
pub fn installed_dir_of(exe: &Path) -> Option<PathBuf> {
    let dir = exe.canonicalize().ok()?.parent()?.to_path_buf();
    match paths::folder_state(&dir) {
        Ok(FolderState::Owned(_)) => Some(dir),
        _ => None,
    }
}

/// The line the temporary copy prints for its parent.
pub const RESULT_PREFIX: &str = "opensmarters-setup-result ";

/// Copies `self_exe` to `%TEMP%\opensmarters-uninstall-<pid>.exe`, runs it with `args`
/// plus `--from-temp`, and returns the exit code it reports (1 if it ends without one).
///
/// # Errors
///
/// The copy or the start failed.
pub fn relaunch_from_temp(
    env: &Environment,
    self_exe: &Path,
    args: &[OsString],
) -> io::Result<i32> {
    std::fs::create_dir_all(&env.temp_dir)?;
    let copy = env.temp_dir.join(format!(
        "{}{}.exe",
        brand::TEMP_UNINSTALLER_PREFIX,
        std::process::id()
    ));
    std::fs::copy(self_exe, &copy)?;
    tracing::info!(copy = %copy.display(), "running the temporary copy");
    let mut child = Command::new(&copy)
        .args(args)
        .arg("--from-temp")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut code = None;
    if let Some(out) = child.stdout.take() {
        for line in io::BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if let Some(c) = line.trim().strip_prefix(RESULT_PREFIX) {
                code = c.trim().parse().ok();
                break;
            }
        }
    }
    // Don't wait for the child: it now deletes this exe, which needs us gone.
    Ok(code.unwrap_or(crate::cli::EXIT_ERROR))
}

/// In the temporary copy: tells the waiting parent the exit code.
pub fn report_to_parent(code: i32) {
    let mut out = io::stdout();
    let _ = writeln!(out, "{RESULT_PREFIX}{code}");
    let _ = out.flush();
}

/// Deletes temporary uninstaller copies left by earlier runs (those still running are
/// skipped). Never deletes `self_exe`.
pub fn clean_temp_copies(env: &Environment, self_exe: &Path) {
    let Ok(rd) = std::fs::read_dir(&env.temp_dir) else {
        return;
    };
    let me = self_exe.canonicalize().ok();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with(brand::TEMP_UNINSTALLER_PREFIX) && name.ends_with(".exe")) {
            continue;
        }
        let path = entry.path();
        if me.as_ref() == path.canonicalize().ok().as_ref() {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// `%SystemRoot%\explorer.exe`, by absolute path (never a bare name, SR-16).
fn explorer() -> io::Result<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the Windows folder isn't set"))?;
    let exe = root.join("explorer.exe");
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "explorer.exe is missing",
        ))
    }
}

/// Opens the installed app as the desktop user (unelevated even if setup was elevated):
/// the running shell starts it. The path is one argument; no shell parsing.
///
/// # Errors
///
/// The app or Explorer can't be found or started.
pub fn launch_app(app_exe: &Path) -> io::Result<()> {
    if !app_exe.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the app is missing",
        ));
    }
    Command::new(explorer()?).arg(app_exe).spawn().map(|_| ())
}

/// True if stdout is connected to something (a console, a pipe or a file). A
/// Windows-subsystem program started from Explorer has no stdout at all.
pub fn stdout_attached() -> bool {
    use std::os::windows::io::AsRawHandle as _;
    !io::stdout().as_raw_handle().is_null()
}

/// A plain message box (for `--help` and fatal start-up errors).
pub fn show_message(text: &str, error: bool) {
    let _ = rfd::MessageDialog::new()
        .set_title(brand::PRODUCT)
        .set_description(text)
        .set_level(if error {
            rfd::MessageLevel::Error
        } else {
            rfd::MessageLevel::Info
        })
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Prints `text` if stdout is attached, else shows it in a message box.
pub fn tell(text: &str, error: bool) {
    if stdout_attached() {
        if error {
            eprintln!("{text}");
        }
        println!("{text}");
    } else {
        show_message(text, error);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn one_installer_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Environment::sandbox(tmp.path()).unwrap();
        let first = SetupLock::acquire(&env).unwrap();
        assert!(first.is_some());
        assert!(SetupLock::acquire(&env).unwrap().is_none());
        drop(first);
        assert!(SetupLock::acquire(&env).unwrap().is_some());
    }

    #[test]
    fn old_temp_copies_are_cleaned() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Environment::sandbox(tmp.path()).unwrap();
        let old = env.temp_dir.join("opensmarters-uninstall-123.exe");
        let other = env.temp_dir.join("something-else.exe");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();
        clean_temp_copies(&env, Path::new(r"C:\nowhere\setup.exe"));
        assert!(!old.exists());
        assert!(other.exists());
    }

    #[test]
    fn explorer_is_an_absolute_path() {
        let e = explorer().unwrap();
        assert!(e.is_absolute() && e.is_file());
    }
}

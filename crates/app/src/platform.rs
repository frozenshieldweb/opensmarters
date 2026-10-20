// SPDX-License-Identifier: GPL-3.0-or-later
//! Small platform queries and actions, all **safe Rust** (the app crate forbids `unsafe`):
//!
//! - [`animations_enabled`]: the OS "show animations" switch. On Windows this is
//!   `Windows.UI.ViewManagement.UISettings.AnimationsEnabled` (Settings → Accessibility →
//!   Visual effects → *Animation effects*, the same value as `SPI_GETCLIENTAREAANIMATION`),
//!   read through the `windows` crate's WinRT projection, whose methods are safe. Other
//!   platforms report `None` (unknown), and the manual setting applies.
//! - [`SystemMotion`]: polls that value every few seconds so turning animations off in
//!   Windows applies while the app runs.
//! - [`open_folder`] / [`open_url`]: show a folder in the file manager or an https page in
//!   the browser through the system launcher ([`launcher`]: Explorer, `open`,
//!   `xdg-open`), always started by **absolute path** (never resolved through the current
//!   directory or `PATH`), with the path/URL as a single argument, never through a shell.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often [`SystemMotion::poll`] asks the OS again.
pub const MOTION_POLL_EVERY: Duration = Duration::from_secs(3);

/// Whether the OS wants animations (`Some(false)` = the user turned them off). `None` if
/// unknown or unsupported.
pub fn animations_enabled() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::UI::ViewManagement::UISettings;
        match UISettings::new().and_then(|s| s.AnimationsEnabled()) {
            Ok(on) => Some(on),
            Err(e) => {
                tracing::debug!(error = %e, "UISettings.AnimationsEnabled unavailable");
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Caches [`animations_enabled`] and refreshes it every [`MOTION_POLL_EVERY`].
#[derive(Debug, Default)]
pub struct SystemMotion {
    checked: Option<Instant>,
    value: Option<bool>,
}

impl SystemMotion {
    /// The OS animation setting, re-read at most every [`MOTION_POLL_EVERY`].
    pub fn poll(&mut self) -> Option<bool> {
        if self
            .checked
            .is_none_or(|t| t.elapsed() >= MOTION_POLL_EVERY)
        {
            self.checked = Some(Instant::now());
            self.value = animations_enabled();
        }
        self.value
    }

    /// The last value read (no OS call).
    pub fn last(&self) -> Option<bool> {
        self.value
    }
}

/// The system "open this" launcher by **absolute path** (Explorer, `open`, `xdg-open`).
///
/// Never a bare program name: `Command::new("explorer")` searches the current directory
/// and `PATH` first on Windows, so a planted `explorer.exe` next to the working directory
/// would run instead (SR-16). On Windows the path is `%SystemRoot%\explorer.exe`, and the
/// variable must be an absolute path to an existing directory.
///
/// # Errors
///
/// `NotFound` if the launcher can't be located safely.
pub fn launcher() -> std::io::Result<PathBuf> {
    let not_found = |what: &str| std::io::Error::new(std::io::ErrorKind::NotFound, what.to_owned());
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .or_else(|| std::env::var_os("windir"))
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
            .ok_or_else(|| not_found("the Windows folder (%SystemRoot%) isn't set"))?;
        let exe = root.join("explorer.exe");
        if exe.is_file() {
            Ok(exe)
        } else {
            Err(not_found("explorer.exe isn't in the Windows folder"))
        }
    }
    #[cfg(target_os = "macos")]
    {
        let exe = PathBuf::from("/usr/bin/open");
        if exe.is_file() {
            Ok(exe)
        } else {
            Err(not_found("/usr/bin/open is missing"))
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        [
            "/usr/bin/xdg-open",
            "/bin/xdg-open",
            "/usr/local/bin/xdg-open",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| not_found("xdg-open isn't installed"))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(not_found("no launcher on this platform"))
    }
}

/// Opens `dir` in the system file manager. The launcher is started by absolute path
/// ([`launcher`]) and the folder is its single argument (no shell involved).
///
/// # Errors
///
/// The folder doesn't exist or the file manager couldn't be started.
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the folder doesn't exist",
        ));
    }
    std::process::Command::new(launcher()?)
        .arg(dir)
        .spawn()
        .map(|_| ())
}

/// Opens an `https://` URL in the default browser through the system launcher (by
/// absolute path, the URL as its single argument, no shell). Callers validate the URL
/// against their own allow-list first; this only refuses anything that isn't https.
///
/// # Errors
///
/// Not an https URL, or the launcher couldn't be started.
pub fn open_url(url: &str) -> std::io::Result<()> {
    let ok = url.starts_with("https://")
        && url.len() <= 2048
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '`' | '\\' | '^' | '|'));
    if !ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only plain https:// addresses are opened",
        ));
    }
    // explorer.exe reports 1 even when it worked, so only a failure to start counts.
    std::process::Command::new(launcher()?)
        .arg(url)
        .status()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_motion_caches_between_polls() {
        let mut m = SystemMotion::default();
        let first = m.poll();
        assert_eq!(m.last(), first);
        // A second poll within the interval doesn't ask the OS again.
        let t = m.checked;
        let _ = m.poll();
        assert_eq!(m.checked, t);
    }

    #[test]
    fn open_folder_rejects_missing_dirs() {
        let missing = std::env::temp_dir().join("opensmarters-no-such-dir-4b1c");
        assert!(open_folder(&missing).is_err());
    }

    #[test]
    fn the_launcher_is_an_absolute_path() {
        // Never a bare name resolved through the current directory or PATH (SR-16).
        if let Ok(exe) = launcher() {
            assert!(exe.is_absolute(), "{}", exe.display());
            assert!(exe.is_file());
            #[cfg(windows)]
            assert!(
                exe.file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("explorer.exe"))
            );
        }
    }

    #[test]
    fn open_url_refuses_non_https_and_shell_characters() {
        for bad in [
            "http://example.org/",
            "file:///C:/Windows/System32/calc.exe",
            "https://example.org/\"&calc",
            "https://example.org/ x",
            "javascript:alert(1)",
            "",
        ] {
            assert!(open_url(bad).is_err(), "{bad}");
        }
    }
}

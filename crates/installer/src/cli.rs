// SPDX-License-Identifier: GPL-3.0-or-later
//! The command line (LAUNCH_PROMPT §B5): for power users and IT.
//!
//! The dev/test flags (`--sandbox`, `--preview`, `--keys`, `--screenshot-after`,
//! `--screenshot-out`, `--exit-after`, `--ppp`, `--reduced-motion`, `--slow`) exist only
//! with the `dev` cargo feature. Without it the fields are always empty and the parser
//! rejects the flags, so a release installer can't be pointed at another registry root or
//! scripted (the same rule as the app's `harness` feature, SR-03).

use std::path::PathBuf;

use clap::Parser;

/// Exit codes, printed by `--help`.
pub const EXIT_CODES: &str = "Exit codes:
  0  done
  1  error (the log says what happened)
  2  cancelled
  3  opensmarters is open: close it, then run setup again

The log is %TEMP%\\opensmarters-setup.log unless --log says otherwise.
Setup is a Windows program: to wait for it and read the exit code, use
  start /wait opensmarters-setup.exe --quiet   (cmd: then echo %errorlevel%)
  Start-Process -Wait -PassThru …               (PowerShell: .ExitCode)";

/// Exit code: done.
pub const EXIT_OK: i32 = 0;
/// Exit code: error.
pub const EXIT_ERROR: i32 = 1;
/// Exit code: cancelled.
pub const EXIT_CANCELLED: i32 = 2;
/// Exit code: the app must be closed first.
pub const EXIT_APP_RUNNING: i32 = 3;

/// opensmarters setup: installs, updates, repairs or removes opensmarters for the current
/// Windows user. No administrator rights are needed and nothing is downloaded.
#[derive(Debug, Clone, Parser, Default)]
#[command(
    name = "opensmarters-setup",
    version,
    after_help = EXIT_CODES
)]
pub struct Cli {
    /// No window: install, update or repair with the defaults (or uninstall with
    /// --uninstall).
    #[arg(long)]
    pub quiet: bool,

    /// Install into this folder (default: %LOCALAPPDATA%\Programs\opensmarters).
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,

    /// Don't create any shortcut (Start menu or desktop).
    #[arg(long, conflicts_with = "desktop_shortcut")]
    pub no_shortcuts: bool,

    /// Also create a desktop shortcut.
    #[arg(long)]
    pub desktop_shortcut: bool,

    /// Don't open opensmarters when setup is done.
    #[arg(long)]
    pub no_launch: bool,

    /// Remove opensmarters. Your data and saved passwords stay unless you add
    /// --purge-data / --purge-passwords. Recordings in your Videos folder always stay.
    #[arg(long, conflicts_with_all = ["dir", "no_shortcuts", "desktop_shortcut", "no_launch"])]
    pub uninstall: bool,

    /// With --uninstall: also delete the app's data (favourites, history, settings).
    #[arg(long, requires = "uninstall")]
    pub purge_data: bool,

    /// With --uninstall: also remove saved passwords from Windows Credential Manager.
    #[arg(long, requires = "uninstall")]
    pub purge_passwords: bool,

    /// Write the log to this file instead of %TEMP%\opensmarters-setup.log.
    #[arg(long, value_name = "FILE")]
    pub log: Option<PathBuf>,

    /// Internal: this process is the uninstaller's temporary copy.
    #[arg(long, hide = true)]
    pub from_temp: bool,

    /// Dev: keep everything (folders, registry, keychain) inside this folder.
    #[cfg_attr(feature = "dev", arg(long, value_name = "DIR"))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub sandbox: Option<PathBuf>,

    /// Dev: show one screen without doing any work: install, options, update, same,
    /// downgrade, running, installing[:0.6], done, uninstall, removing[:0.5], removed,
    /// error, damaged, licence.
    #[cfg_attr(feature = "dev", arg(long, value_name = "SCREEN"))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub preview: Option<String>,

    /// Dev: scripted keys (`Tab`, `Enter`, `Esc`, `Down`, `wait:<s>`, `shot:<png>`, `quit`).
    #[cfg_attr(feature = "dev", arg(long, value_name = "SCRIPT"))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub keys: Option<String>,

    /// Dev: take a screenshot this many seconds after start, then exit.
    #[cfg_attr(
        feature = "dev",
        arg(long, value_name = "SECS", requires = "screenshot_out")
    )]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub screenshot_after: Option<f64>,

    /// Dev: where to write the --screenshot-after PNG.
    #[cfg_attr(
        feature = "dev",
        arg(long, value_name = "PNG", requires = "screenshot_after")
    )]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub screenshot_out: Option<PathBuf>,

    /// Dev: exit this many seconds after start.
    #[cfg_attr(feature = "dev", arg(long, value_name = "SECS"))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub exit_after: Option<f64>,

    /// Dev: render at this many pixels per point (1 = 100%, 2 = 200%), whatever the monitor.
    #[cfg_attr(feature = "dev", arg(long, value_name = "PPP"))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub ppp: Option<f32>,

    /// Dev: force reduced motion (instead of following Windows' Animation effects).
    #[cfg_attr(feature = "dev", arg(long))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub reduced_motion: bool,

    /// Dev: slow the real work down so the progress can be watched.
    #[cfg_attr(feature = "dev", arg(long))]
    #[cfg_attr(not(feature = "dev"), arg(skip))]
    pub slow: bool,
}

impl Cli {
    /// Start menu shortcut wanted.
    pub fn start_menu(&self) -> bool {
        !self.no_shortcuts
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("opensmarters-setup").chain(args.iter().copied()))
    }

    #[test]
    fn parses_the_documented_flags() {
        let c = parse(&[
            "--quiet",
            "--dir",
            r"D:\Apps\os",
            "--desktop-shortcut",
            "--no-launch",
            "--log",
            "x.log",
        ])
        .unwrap();
        assert!(c.quiet && c.desktop_shortcut && c.no_launch && c.start_menu());
        assert_eq!(c.dir, Some(PathBuf::from(r"D:\Apps\os")));
        let c = parse(&[
            "--uninstall",
            "--purge-data",
            "--purge-passwords",
            "--quiet",
        ])
        .unwrap();
        assert!(c.uninstall && c.purge_data && c.purge_passwords);
        let c = parse(&["--no-shortcuts"]).unwrap();
        assert!(!c.start_menu());
        assert!(parse(&["--uninstall", "--from-temp"]).unwrap().from_temp);
    }

    #[test]
    fn rejects_inconsistent_flags() {
        assert!(parse(&["--purge-data"]).is_err());
        assert!(parse(&["--purge-passwords"]).is_err());
        assert!(parse(&["--uninstall", "--dir", "C:\\x"]).is_err());
        assert!(parse(&["--no-shortcuts", "--desktop-shortcut"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
    }

    #[test]
    fn help_lists_the_exit_codes() {
        let e = parse(&["--help"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelp);
        let text = e.to_string();
        assert!(text.contains("Exit codes"));
        assert!(text.contains("--uninstall"));
        assert!(!text.contains("--from-temp"));
    }

    #[cfg(not(feature = "dev"))]
    #[test]
    fn dev_flags_are_rejected_without_the_feature() {
        for args in [
            &["--sandbox", "C:\\x"][..],
            &["--preview", "done"],
            &["--keys", "Enter"],
            &["--ppp", "2"],
            &["--slow"],
            &["--reduced-motion"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[cfg(feature = "dev")]
    #[test]
    fn dev_flags_parse_with_the_feature() {
        let c = parse(&[
            "--sandbox",
            "C:\\x",
            "--preview",
            "installing:0.6",
            "--ppp",
            "2",
            "--slow",
        ])
        .unwrap();
        assert_eq!(c.preview.as_deref(), Some("installing:0.6"));
        assert_eq!(c.ppp, Some(2.0));
        assert!(c.slow);
    }
}

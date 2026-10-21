// SPDX-License-Identifier: GPL-3.0-or-later
//! The real binary, end to end, in a sandbox (`dev` feature: `--sandbox`): `--help`, exit
//! codes, a headless `--quiet` install, the app-running check (exit 3), and a quiet
//! uninstall through the **installed** `uninstall.exe` (which copies itself to the temp
//! folder, runs from there and removes the install folder, itself included).

#![cfg(all(windows, feature = "dev"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Sandbox, assert_no_siblings, payload};
use opensmarters_installer::{manifest::InstallManifest, shortcuts};

const SETUP: &str = env!("CARGO_BIN_EXE_opensmarters-setup");

fn setup(sb: &Sandbox) -> Command {
    let mut c = Command::new(SETUP);
    c.arg("--sandbox").arg(sb.tmp.path());
    c.stdin(Stdio::null());
    c
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn help_lists_flags_and_exit_codes() {
    let out = Command::new(SETUP).arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "--quiet",
        "--dir",
        "--no-shortcuts",
        "--desktop-shortcut",
        "--no-launch",
        "--uninstall",
        "--purge-data",
        "--purge-passwords",
        "--log",
        "Exit codes",
        "3  opensmarters is open",
    ] {
        assert!(text.contains(needle), "--help lacks {needle}:\n{text}");
    }
    let out = Command::new(SETUP).arg("--frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn quiet_install_and_uninstall_through_the_installed_uninstaller() {
    let sb = Sandbox::new();
    let p = payload();
    let status = setup(&sb)
        .args(["--quiet", "--no-launch", "--desktop-shortcut"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let dir = sb.env.default_install_dir.clone();
    let m = InstallManifest::read(&dir).unwrap().expect("installed");
    assert_eq!(m.version, p.manifest.version);
    // uninstall.exe is a copy of the setup binary.
    assert_eq!(
        std::fs::read(dir.join("uninstall.exe")).unwrap(),
        std::fs::read(SETUP).unwrap()
    );
    assert!(shortcuts::is_ours(&sb.env.start_menu_shortcut(), &dir));
    assert!(shortcuts::is_ours(&sb.env.desktop_shortcut(), &dir));
    let installed = sb.env.registry.read_installed().unwrap().unwrap();
    assert_eq!(Path::new(&installed.location), dir);
    // The log went to the (sandbox) temp folder.
    assert!(sb.env.default_log().exists());

    // A second quiet run is a repair: exit 0, same files.
    let status = setup(&sb)
        .args(["--quiet", "--no-launch"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // Uninstall the way Windows does (QuietUninstallString), from the install folder.
    let status = Command::new(dir.join("uninstall.exe"))
        .args(["--uninstall", "--quiet", "--sandbox"])
        .arg(sb.tmp.path())
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    // The temporary copy deletes uninstall.exe and the folder once we (the parent) exit.
    wait_until("the install folder to go", || !dir.exists());
    assert_no_siblings(&dir);
    assert!(!sb.env.start_menu_shortcut().exists());
    assert!(!sb.env.desktop_shortcut().exists());
    assert_eq!(sb.env.registry.read_installed().unwrap(), None);
    // The next run cleans the temporary copy up.
    let copies = || {
        std::fs::read_dir(&sb.env.temp_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("opensmarters-uninstall-")
            })
            .count()
    };
    wait_until("the temporary copy to exit", || {
        let status = setup(&sb)
            .args(["--quiet", "--uninstall"])
            .status()
            .unwrap();
        status.code() == Some(0) && copies() == 0
    });
}

#[test]
fn quiet_runs_exit_3_while_the_app_is_open() {
    let sb = Sandbox::new();
    let status = setup(&sb)
        .args(["--quiet", "--no-launch"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let dir = sb.env.default_install_dir.clone();
    // Stand-in for a running app: a long-running program started from the install folder
    // as opensmarters.exe (ping on the loopback address; no network involved).
    let system = std::env::var_os("SystemRoot").unwrap();
    let ping = Path::new(&system).join(r"System32\PING.EXE");
    std::fs::copy(&ping, dir.join("opensmarters.exe")).unwrap();
    let mut app = Command::new(dir.join("opensmarters.exe"))
        .args(["-n", "60", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let before = common::tree(&dir);
    let repair = setup(&sb)
        .args(["--quiet", "--no-launch"])
        .status()
        .unwrap();
    let remove = setup(&sb)
        .args(["--quiet", "--uninstall"])
        .status()
        .unwrap();
    let _ = app.kill();
    let _ = app.wait();
    assert_eq!(repair.code(), Some(3));
    assert_eq!(remove.code(), Some(3));
    assert_eq!(
        common::tree(&dir),
        before,
        "nothing changed while the app was open"
    );
    // Closed: the uninstall goes through.
    let status = setup(&sb)
        .args(["--quiet", "--uninstall"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(!dir.exists());
}

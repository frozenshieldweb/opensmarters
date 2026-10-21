// SPDX-License-Identifier: GPL-3.0-or-later
//! Rollback (LAUNCH_PROMPT §B10): make the install fail at different points (a corrupted
//! archive, a hash mismatch half-way through extraction, a read-only file where a shortcut
//! goes, a file held open in the install folder, an injected failure after the registry,
//! a cancel) and assert that the previous install is intact: same files and hashes, same
//! registry values, same shortcuts, no `.new` / `.old` folders left.

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::Ordering;

use common::{Sandbox, assert_no_siblings, payload, tree};
use opensmarters_installer::install::{self, Control, FailPoint, InstallError};
use opensmarters_installer::payload::Payload;

/// A sandbox with an installed "0.0.1" (so the next install is an update that must roll
/// back to exactly this state).
fn installed() -> (Sandbox, std::path::PathBuf) {
    let sb = Sandbox::new();
    let p = payload();
    let dir = sb.install(&p, &sb.options()).unwrap().dir;
    sb.pretend_installed_version(&dir, "0.0.1");
    (sb, dir)
}

struct Before {
    files: std::collections::BTreeMap<String, String>,
    registry: Vec<(String, String)>,
    start_menu: Option<Vec<u8>>,
}

fn before(sb: &Sandbox, dir: &std::path::Path) -> Before {
    Before {
        files: tree(dir),
        registry: sb.env.registry.values().unwrap(),
        start_menu: std::fs::read(sb.env.start_menu_shortcut()).ok(),
    }
}

fn assert_intact(sb: &Sandbox, dir: &std::path::Path, b: &Before) {
    assert_eq!(tree(dir), b.files, "the previous install changed");
    assert_eq!(
        sb.env.registry.values().unwrap(),
        b.registry,
        "the registry changed"
    );
    assert_eq!(
        std::fs::read(sb.env.start_menu_shortcut()).ok(),
        b.start_menu,
        "the shortcut changed"
    );
    assert_no_siblings(dir);
}

#[test]
fn a_corrupted_archive_changes_nothing() {
    let (sb, dir) = installed();
    let b = before(&sb, &dir);
    let p = payload();
    let mut bytes = p.archive.to_vec();
    let mid = bytes.len() / 3;
    bytes[mid] ^= 0x10;
    let bad = Payload::new(&bytes, p.manifest.clone());
    let e = sb.install(&bad, &sb.options()).unwrap_err();
    assert!(matches!(e, InstallError::Damaged(_)), "{e:?}");
    assert_eq!(
        e.message(),
        "The installer is damaged. Download it again and check its SHA-256."
    );
    assert_intact(&sb, &dir, &b);
}

#[test]
fn a_mismatch_half_way_through_extraction_rolls_back() {
    let (sb, dir) = installed();
    let b = before(&sb, &dir);
    let p = payload();
    let mut m = p.manifest.clone();
    // The archive is intact (its hash still matches), but one of the later files' expected
    // hash is wrong: extraction fails after writing earlier files.
    let i = m.files.len() - 2;
    m.files[i].sha256[5] ^= 0xff;
    let bad = Payload::new(p.archive, m);
    let e = sb.install(&bad, &sb.options()).unwrap_err();
    assert!(matches!(e, InstallError::Damaged(_)), "{e:?}");
    assert_intact(&sb, &dir, &b);
}

#[test]
fn a_read_only_file_in_the_way_of_a_shortcut_rolls_back_after_the_swap() {
    let (sb, dir) = installed();
    // Someone else's read-only file where our Start menu shortcut goes.
    let lnk = sb.env.start_menu_shortcut();
    std::fs::write(&lnk, b"not ours").unwrap();
    let mut perms = std::fs::metadata(&lnk).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&lnk, perms).unwrap();
    let b = before(&sb, &dir);
    let e = sb.install(&payload(), &sb.options()).unwrap_err();
    assert!(
        matches!(
            e,
            InstallError::Io {
                step: "shortcut",
                ..
            }
        ),
        "{e:?}"
    );
    assert_intact(&sb, &dir, &b);
    let mut perms = std::fs::metadata(&lnk).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&lnk, perms).unwrap();
}

#[test]
fn a_failure_after_the_registry_restores_it() {
    let (sb, dir) = installed();
    let b = before(&sb, &dir);
    let ctl = Control {
        fail_after: Some(FailPoint::Registry),
        ..Control::default()
    };
    let e =
        install::install(&sb.env, &payload(), &sb.options(), &sb.setup_exe(), &ctl).unwrap_err();
    assert!(matches!(e, InstallError::Io { step: "test", .. }), "{e:?}");
    assert_intact(&sb, &dir, &b);
}

#[test]
fn a_failure_on_a_fresh_install_leaves_nothing() {
    let sb = Sandbox::new();
    let ctl = Control {
        fail_after: Some(FailPoint::Registry),
        ..Control::default()
    };
    let opts = sb.options();
    let e = install::install(&sb.env, &payload(), &opts, &sb.setup_exe(), &ctl).unwrap_err();
    assert!(matches!(e, InstallError::Io { .. }));
    assert!(!opts.dir.exists());
    assert!(!sb.env.start_menu_shortcut().exists());
    assert_eq!(sb.env.registry.read_installed().unwrap(), None);
    assert!(sb.env.registry.values().unwrap().is_empty());
    assert_no_siblings(&opts.dir);
}

#[test]
fn a_file_held_open_blocks_the_swap_and_nothing_changes() {
    use std::os::windows::fs::OpenOptionsExt as _;
    let (sb, dir) = installed();
    let b = before(&sb, &dir);
    // A program (an editor, a terminal) holds a file in the install folder without sharing.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(dir.join("README.txt"))
        .unwrap();
    let e = sb.install(&payload(), &sb.options()).unwrap_err();
    assert!(matches!(e, InstallError::FolderBusy(_)), "{e:?}");
    drop(held);
    assert_intact(&sb, &dir, &b);
}

#[test]
fn cancel_half_way_rolls_back() {
    let (sb, dir) = installed();
    let b = before(&sb, &dir);
    let ctl = Control {
        slow: Some(std::time::Duration::from_millis(40)),
        ..Control::default()
    };
    let env = sb.env.clone();
    let opts = sb.options();
    let exe = sb.setup_exe();
    let ctl2 = ctl.clone();
    let worker = std::thread::spawn(move || install::install(&env, &payload(), &opts, &exe, &ctl2));
    let start = std::time::Instant::now();
    while ctl.progress.get().1 < 0.3 && start.elapsed().as_secs() < 30 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    ctl.cancel.store(true, Ordering::Relaxed);
    let e = worker.join().unwrap().unwrap_err();
    assert!(matches!(e, InstallError::Cancelled), "{e:?}");
    assert_eq!(e.exit_code(), 2);
    assert_intact(&sb, &dir, &b);
}

#[test]
fn a_stale_folder_with_unknown_files_is_never_deleted() {
    let (sb, dir) = installed();
    let staged = opensmarters_installer::paths::sibling(&dir, ".new");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(staged.join("not-ours.txt"), b"keep").unwrap();
    let e = sb.install(&payload(), &sb.options()).unwrap_err();
    assert!(matches!(e, InstallError::InTheWay(_)), "{e:?}");
    assert!(staged.join("not-ours.txt").exists());
    std::fs::remove_dir_all(&staged).unwrap();
    // A stale folder holding only our files (an interrupted run) is cleaned up.
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(staged.join("opensmarters.exe"), b"partial").unwrap();
    sb.install(&payload(), &sb.options()).unwrap();
    assert_no_siblings(&dir);
}

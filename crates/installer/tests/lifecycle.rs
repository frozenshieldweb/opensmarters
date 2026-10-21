// SPDX-License-Identifier: GPL-3.0-or-later
//! Install, update, repair and uninstall into a sandbox (temp folders, the test registry
//! root, temp shortcut folders, a fake keychain), asserting files, hashes, registry values
//! and shortcuts, then complete removal (LAUNCH_PROMPT §B10).

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Sandbox, assert_installed, assert_no_siblings, payload, tree};
use opensmarters_installer::install::Control;
use opensmarters_installer::run;
use opensmarters_installer::shortcuts;
use opensmarters_installer::uninstall::{self, Options as RemoveOptions};
use opensmarters_installer::version::Plan;

fn value(values: &[(String, String)], key: &str) -> String {
    values
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("no {key}"))
}

#[test]
fn install_then_uninstall_leaves_nothing_but_user_data() {
    let sb = Sandbox::new();
    let p = payload();
    assert_eq!(run::survey(&sb.env, &p).plan, Plan::Fresh);
    let mut opts = sb.options();
    opts.desktop = true;
    let out = sb.install(&p, &opts).unwrap();
    let dir = out.dir.clone();
    assert!(
        dir.starts_with(
            sb.tmp
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
        )
    );
    assert_installed(&dir, &p, &sb.setup_exe());
    assert_no_siblings(&dir);

    // Apps & features entry.
    let values = sb.env.registry.values().unwrap();
    let d = dir.to_string_lossy().into_owned();
    assert_eq!(value(&values, "DisplayName"), "opensmarters");
    assert_eq!(value(&values, "DisplayVersion"), p.manifest.version);
    assert_eq!(value(&values, "Publisher"), "opensmarters contributors");
    assert_eq!(value(&values, "InstallLocation"), d);
    assert_eq!(
        value(&values, "DisplayIcon"),
        format!(r"{d}\opensmarters.exe,0")
    );
    assert_eq!(
        value(&values, "UninstallString"),
        format!(r#""{d}\uninstall.exe" --uninstall"#)
    );
    assert_eq!(
        value(&values, "QuietUninstallString"),
        format!(r#""{d}\uninstall.exe" --uninstall --quiet"#)
    );
    assert_eq!(value(&values, "NoModify"), "1");
    assert_eq!(value(&values, "NoRepair"), "0");
    assert!(value(&values, "URLInfoAbout").starts_with("https://"));
    let kib: u64 = value(&values, "EstimatedSize").parse().unwrap();
    assert!(kib >= p.manifest.installed_size / 1024);

    // Shortcuts.
    assert!(shortcuts::is_ours(&sb.env.start_menu_shortcut(), &dir));
    assert!(shortcuts::is_ours(&sb.env.desktop_shortcut(), &dir));
    assert_eq!(run::survey(&sb.env, &p).plan, Plan::Same);

    // The app's data and saved passwords exist; a default uninstall keeps them.
    std::fs::create_dir_all(sb.env.data_dir.join(r"opensmarters\data")).unwrap();
    std::fs::write(
        sb.env.data_dir.join(r"opensmarters\data\opensmarters.db"),
        b"db",
    )
    .unwrap();
    let keychain = sb.tmp.path().join("keychain.txt");
    std::fs::write(
        &keychain,
        "profile/1/password.opensmarters\nsomething.else\n",
    )
    .unwrap();
    let o = uninstall::uninstall(
        &sb.env,
        RemoveOptions::default(),
        &Control::default(),
        &sb.setup_exe(),
    )
    .unwrap();
    assert!(
        !o.nothing_installed && !o.folder_kept && o.pending.is_empty(),
        "{o:?}"
    );
    assert!(!dir.exists(), "the install folder is gone");
    assert!(!sb.env.start_menu_shortcut().exists());
    assert!(!sb.env.desktop_shortcut().exists());
    assert_eq!(sb.env.registry.read_installed().unwrap(), None);
    assert!(sb.env.registry.values().unwrap().is_empty());
    assert!(
        sb.env
            .data_dir
            .join(r"opensmarters\data\opensmarters.db")
            .exists(),
        "data kept by default"
    );
    assert_eq!(
        std::fs::read_to_string(&keychain).unwrap(),
        "profile/1/password.opensmarters\nsomething.else\n",
        "passwords kept by default"
    );
    assert_eq!(run::survey(&sb.env, &p).plan, Plan::Fresh);
    // Uninstalling again is harmless.
    let o = uninstall::uninstall(
        &sb.env,
        RemoveOptions::default(),
        &Control::default(),
        &sb.setup_exe(),
    )
    .unwrap();
    assert!(o.nothing_installed);
}

#[test]
fn uninstall_purges_data_and_passwords_only_when_asked() {
    let sb = Sandbox::new();
    let p = payload();
    let dir = sb.install(&p, &sb.options()).unwrap().dir;
    std::fs::create_dir_all(sb.env.data_dir.join(r"opensmarters\data\images")).unwrap();
    std::fs::write(
        sb.env.data_dir.join(r"opensmarters\data\opensmarters.db"),
        b"db",
    )
    .unwrap();
    let keychain = sb.tmp.path().join("keychain.txt");
    std::fs::write(
        &keychain,
        "profile/1/password.opensmarters\ncache_key.opensmarters\nother.app\n",
    )
    .unwrap();
    // A recording in a (sandbox) Videos folder: uninstall never looks there.
    let videos = sb.tmp.path().join(r"Videos\opensmarters");
    std::fs::create_dir_all(&videos).unwrap();
    std::fs::write(videos.join("Film (2024).mkv"), b"video").unwrap();
    let o = uninstall::uninstall(
        &sb.env,
        RemoveOptions {
            purge_data: true,
            purge_passwords: true,
        },
        &Control::default(),
        &sb.setup_exe(),
    )
    .unwrap();
    assert!(o.data_removed);
    assert_eq!(o.passwords_removed, 2);
    assert!(!sb.env.data_dir.exists());
    assert_eq!(std::fs::read_to_string(&keychain).unwrap(), "other.app\n");
    assert!(videos.join("Film (2024).mkv").exists());
    assert!(!dir.exists());
}

#[test]
fn uninstall_deletes_only_listed_files() {
    let sb = Sandbox::new();
    let p = payload();
    let dir = sb.install(&p, &sb.options()).unwrap().dir;
    std::fs::write(dir.join("my notes.txt"), b"mine").unwrap();
    std::fs::create_dir_all(dir.join(r"licenses\extra")).unwrap();
    std::fs::write(dir.join(r"licenses\extra\keep.txt"), b"mine").unwrap();
    let o = uninstall::uninstall(
        &sb.env,
        RemoveOptions::default(),
        &Control::default(),
        &sb.setup_exe(),
    )
    .unwrap();
    assert!(o.folder_kept);
    let left = tree(&dir);
    assert_eq!(
        left.keys().cloned().collect::<Vec<_>>(),
        vec![
            "licenses/extra/keep.txt".to_owned(),
            "my notes.txt".to_owned()
        ]
    );
    assert_eq!(sb.env.registry.read_installed().unwrap(), None);
}

#[test]
fn update_keeps_shortcut_choices_user_files_and_data() {
    let sb = Sandbox::new();
    let p = payload();
    let mut opts = sb.options();
    opts.desktop = true;
    let dir = sb.install(&p, &opts).unwrap().dir;
    sb.pretend_installed_version(&dir, "0.0.1");
    std::fs::write(dir.join("my notes.txt"), b"mine").unwrap();
    std::fs::create_dir_all(sb.env.data_dir.join("opensmarters")).unwrap();
    std::fs::write(sb.env.data_dir.join(r"opensmarters\settings"), b"keep").unwrap();

    let s = run::survey(&sb.env, &p);
    assert_eq!(
        s.plan,
        Plan::Update {
            from: "0.0.1".into()
        }
    );
    assert_eq!(s.dir, dir);
    assert!(s.start_menu_exists && s.desktop_exists);
    // The quiet defaults keep both shortcuts on an update.
    let cli = opensmarters_installer::cli::Cli::default();
    let q = run::quiet_options(&cli, &s);
    assert!(q.start_menu && q.desktop);
    let out = sb.install(&p, &q).unwrap();
    assert_eq!(out.dir, dir);
    assert!(out.leftovers.is_empty(), "{:?}", out.leftovers);
    assert_installed(&dir, &p, &sb.setup_exe());
    assert_no_siblings(&dir);
    assert_eq!(std::fs::read(dir.join("my notes.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(sb.env.data_dir.join(r"opensmarters\settings")).unwrap(),
        b"keep"
    );
    assert_eq!(
        sb.env.registry.read_installed().unwrap().unwrap().version,
        p.manifest.version
    );
    assert!(shortcuts::is_ours(&sb.env.desktop_shortcut(), &dir));
    assert_eq!(run::survey(&sb.env, &p).plan, Plan::Same);
}

#[test]
fn repair_puts_back_changed_and_missing_files() {
    let sb = Sandbox::new();
    let p = payload();
    let dir = sb.install(&p, &sb.options()).unwrap().dir;
    std::fs::write(dir.join("libmpv-2.dll"), b"tampered").unwrap();
    std::fs::remove_file(dir.join("README.txt")).unwrap();
    assert_eq!(run::survey(&sb.env, &p).plan, Plan::Same);
    sb.install(&p, &sb.options()).unwrap();
    assert_installed(&dir, &p, &sb.setup_exe());
    assert_no_siblings(&dir);
}

#[test]
fn a_newer_install_is_a_downgrade_question() {
    let sb = Sandbox::new();
    let p = payload();
    let dir = sb.install(&p, &sb.options()).unwrap().dir;
    sb.pretend_installed_version(&dir, "99.0.0");
    assert_eq!(
        run::survey(&sb.env, &p).plan,
        Plan::Downgrade {
            installed: "99.0.0".into()
        }
    );
    // Quiet mode refuses (exit 1) and changes nothing.
    let cli = opensmarters_installer::cli::Cli {
        quiet: true,
        no_launch: true,
        ..Default::default()
    };
    let before = tree(&dir);
    let (code, _) = run::quiet(&cli, &sb.env, &p, &sb.setup_exe());
    assert_eq!(code, 1);
    assert_eq!(tree(&dir), before);
    // The window's "Install <older>" goes ahead.
    sb.install(&p, &sb.options()).unwrap();
    assert_installed(&dir, &p, &sb.setup_exe());
}

#[test]
fn refuses_a_folder_with_someone_elses_files() {
    let sb = Sandbox::new();
    let p = payload();
    let theirs = sb.tmp.path().join("Theirs");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(theirs.join("document.txt"), b"keep").unwrap();
    let mut opts = sb.options();
    opts.dir = theirs.clone();
    let e = sb.install(&p, &opts).unwrap_err();
    assert!(
        matches!(e, opensmarters_installer::install::InstallError::Path(_)),
        "{e:?}"
    );
    assert_eq!(tree(&theirs).len(), 1);
    // An empty folder is fine, and so is the sandbox's default.
    let empty = sb.tmp.path().join("Empty");
    std::fs::create_dir_all(&empty).unwrap();
    opts.dir = empty.clone();
    sb.install(&p, &opts).unwrap();
    assert_installed(&empty, &p, &sb.setup_exe());
}

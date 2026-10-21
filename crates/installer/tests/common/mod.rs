// SPDX-License-Identifier: GPL-3.0-or-later
//! Test helpers: a sandboxed environment (temp folders, the **test** registry root
//! `HKCU\Software\opensmartersTest\<id>`, a file-backed keychain). Nothing here touches the
//! real Uninstall key, real shortcuts, `%LOCALAPPDATA%\Programs`, the app's data folder or
//! Credential Manager.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opensmarters_installer::env::Environment;
use opensmarters_installer::install::{self, Control, Options};
use opensmarters_installer::manifest::InstallManifest;
use opensmarters_installer::payload::{self, Payload};
use opensmarters_installer::registry::UninstallEntry;

/// A sandbox that removes its test registry tree when dropped.
pub struct Sandbox {
    pub tmp: tempfile::TempDir,
    pub env: Environment,
}

impl Sandbox {
    pub fn new() -> Self {
        // Under target/tmp rather than %TEMP%: CI runners set TEMP with an 8.3 short name
        // (C:\Users\RUNNER~1\...), which the install-folder check rightly refuses.
        let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let env = Environment::sandbox(tmp.path()).unwrap();
        assert!(
            env.registry.is_test(),
            "tests must use the test registry root"
        );
        env.registry.delete_test_tree();
        Self { tmp, env }
    }

    /// A stand-in for the setup exe (copied as uninstall.exe).
    pub fn setup_exe(&self) -> PathBuf {
        let p = self.tmp.path().join("setup-under-test.exe");
        if !p.exists() {
            std::fs::write(&p, b"MZ synthetic setup exe for tests").unwrap();
        }
        p
    }

    pub fn options(&self) -> Options {
        Options {
            dir: self.env.default_install_dir.clone(),
            start_menu: true,
            desktop: false,
        }
    }

    pub fn install(
        &self,
        payload: &Payload<'_>,
        opts: &Options,
    ) -> Result<install::Outcome, install::InstallError> {
        install::install(
            &self.env,
            payload,
            opts,
            &self.setup_exe(),
            &Control::default(),
        )
    }

    /// Pretends the installed copy is `version` (manifest and registry).
    pub fn pretend_installed_version(&self, dir: &Path, version: &str) {
        let mut m = InstallManifest::read(dir).unwrap().unwrap();
        m.version = version.to_owned();
        std::fs::write(dir.join("install-manifest.txt"), m.to_text()).unwrap();
        self.env
            .registry
            .write(&UninstallEntry {
                version: version.to_owned(),
                location: dir.to_string_lossy().into_owned(),
                estimated_size_kib: 1,
            })
            .unwrap();
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.env.registry.delete_test_tree();
    }
}

pub fn payload() -> Payload<'static> {
    Payload::embedded().unwrap()
}

/// Every file under `dir` with its SHA-256 (hex), sorted.
pub fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeMap<String, String>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let (h, _) = payload::hash_file(&p).unwrap();
                out.insert(rel, opensmarters_installer::pack::hex(&h));
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// The install folder holds exactly the payload, uninstall.exe and the manifest, with the
/// right hashes.
pub fn assert_installed(dir: &Path, payload: &Payload<'_>, setup_exe: &Path) {
    let m = InstallManifest::read(dir)
        .unwrap()
        .expect("install manifest");
    assert_eq!(m.product, "opensmarters");
    assert_eq!(m.version, payload.manifest.version);
    for f in &payload.manifest.files {
        assert!(
            payload::file_matches(&dir.join(f.native()), f, &|_| {}).unwrap(),
            "{} is wrong",
            f.path
        );
    }
    let (h, _) = payload::hash_file(&dir.join("uninstall.exe")).unwrap();
    let (s, _) = payload::hash_file(setup_exe).unwrap();
    assert_eq!(h, s, "uninstall.exe is a copy of the setup exe");
    assert_eq!(m.files.len(), payload.manifest.files.len() + 1);
}

/// No `<dir>.new` / `<dir>.old` left behind.
pub fn assert_no_siblings(dir: &Path) {
    for s in [".new", ".old"] {
        let p = opensmarters_installer::paths::sibling(dir, s);
        assert!(!p.exists(), "{} was left behind", p.display());
    }
}

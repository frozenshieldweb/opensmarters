// SPDX-License-Identifier: GPL-3.0-or-later
//! The Apps & features entry (LAUNCH_PROMPT §B5), through `winreg`'s safe API.
//!
//! The entry lives under a [`RegistryRoot`] in `HKEY_CURRENT_USER` (per-user, no
//! elevation): [`RegistryRoot::real`] is `Software\Microsoft\Windows\CurrentVersion\
//! Uninstall`; tests and sandbox runs use [`RegistryRoot::test`] under
//! `Software\opensmartersTest\<id>`, never the real key.

use std::io;

use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, RegType};

use crate::brand;

/// The real Uninstall key (relative to `HKEY_CURRENT_USER`).
pub const REAL_UNINSTALL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
/// Parent of every test registry root (relative to `HKEY_CURRENT_USER`).
pub const TEST_ROOT_PARENT: &str = r"Software\opensmartersTest";

/// Where the Uninstall entry is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRoot {
    uninstall_root: String,
    test_id: Option<String>,
}

/// What the registry says is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// `DisplayVersion`.
    pub version: String,
    /// `InstallLocation`.
    pub location: String,
}

/// The values we write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallEntry {
    /// `DisplayVersion`.
    pub version: String,
    /// The install folder.
    pub location: String,
    /// `EstimatedSize` in KiB.
    pub estimated_size_kib: u32,
}

/// A copy of the key's values before we changed them (for rollback).
#[derive(Debug, Clone, Default)]
pub struct Snapshot(Option<Vec<(String, Vec<u8>, RegType)>>);

fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

impl RegistryRoot {
    /// The real per-user Uninstall key.
    pub fn real() -> Self {
        Self {
            uninstall_root: REAL_UNINSTALL_ROOT.to_owned(),
            test_id: None,
        }
    }

    /// A test root: `Software\opensmartersTest\<id>\Uninstall`. `id` is reduced to ASCII
    /// letters, digits and `-`.
    pub fn test(id: &str) -> Self {
        let id: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(64)
            .collect();
        let id = if id.is_empty() {
            "default".to_owned()
        } else {
            id
        };
        Self {
            uninstall_root: format!(r"{TEST_ROOT_PARENT}\{id}\Uninstall"),
            test_id: Some(id),
        }
    }

    /// True for a test root.
    pub fn is_test(&self) -> bool {
        self.test_id.is_some()
    }

    /// The full key path under `HKEY_CURRENT_USER`.
    pub fn key_path(&self) -> String {
        format!(r"{}\{}", self.uninstall_root, brand::REGISTRY_KEY)
    }

    fn open(&self) -> io::Result<Option<RegKey>> {
        match hkcu().open_subkey_with_flags(self.key_path(), KEY_READ) {
            Ok(k) => Ok(Some(k)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The installed version and folder, if the entry exists and has both.
    ///
    /// # Errors
    ///
    /// Registry access errors (a missing key is `Ok(None)`).
    pub fn read_installed(&self) -> io::Result<Option<Installed>> {
        let Some(key) = self.open()? else {
            return Ok(None);
        };
        let version: Result<String, _> = key.get_value("DisplayVersion");
        let location: Result<String, _> = key.get_value("InstallLocation");
        Ok(match (version, location) {
            (Ok(version), Ok(location)) if !location.is_empty() => {
                Some(Installed { version, location })
            }
            _ => None,
        })
    }

    /// Every value of the entry as text (tests and logs).
    ///
    /// # Errors
    ///
    /// Registry access errors.
    pub fn values(&self) -> io::Result<Vec<(String, String)>> {
        let Some(key) = self.open()? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for v in key.enum_values() {
            let (name, value) = v?;
            out.push((name, value.to_string()));
        }
        out.sort();
        Ok(out)
    }

    /// Captures the entry's current values (or its absence).
    ///
    /// # Errors
    ///
    /// Registry access errors.
    pub fn snapshot(&self) -> io::Result<Snapshot> {
        let Some(key) = self.open()? else {
            return Ok(Snapshot(None));
        };
        let mut values = Vec::new();
        for v in key.enum_values() {
            let (name, value) = v?;
            values.push((name, value.bytes.into_owned(), value.vtype));
        }
        Ok(Snapshot(Some(values)))
    }

    /// Writes the entry (replacing every value we own).
    ///
    /// # Errors
    ///
    /// Registry access errors.
    pub fn write(&self, e: &UninstallEntry) -> io::Result<()> {
        let (key, _) = hkcu().create_subkey(self.key_path())?;
        let dir = e.location.trim_end_matches('\\');
        let uninstaller = format!(r#""{dir}\{}""#, brand::UNINSTALL_EXE);
        key.set_value("DisplayName", &brand::PRODUCT)?;
        key.set_value("DisplayVersion", &e.version)?;
        key.set_value("Publisher", &brand::PUBLISHER)?;
        key.set_value("DisplayIcon", &format!(r"{dir}\{},0", brand::APP_EXE))?;
        key.set_value("InstallLocation", &dir)?;
        key.set_value("EstimatedSize", &e.estimated_size_kib)?;
        key.set_value("UninstallString", &format!("{uninstaller} --uninstall"))?;
        key.set_value(
            "QuietUninstallString",
            &format!("{uninstaller} --uninstall --quiet"),
        )?;
        key.set_value("NoModify", &1_u32)?;
        key.set_value("NoRepair", &0_u32)?;
        key.set_value("URLInfoAbout", &brand::URL_INFO_ABOUT)?;
        key.set_value("HelpLink", &brand::URL_INFO_ABOUT)?;
        Ok(())
    }

    /// Puts back a [`Snapshot`]: deletes the key if it didn't exist, else restores the old
    /// values exactly.
    ///
    /// # Errors
    ///
    /// Registry access errors.
    pub fn restore(&self, snap: &Snapshot) -> io::Result<()> {
        match &snap.0 {
            None => self.delete(),
            Some(values) => {
                self.delete()?;
                let (key, _) = hkcu().create_subkey(self.key_path())?;
                for (name, bytes, vtype) in values {
                    key.set_raw_value(
                        name,
                        &winreg::RegValue {
                            bytes: bytes.as_slice().into(),
                            vtype: vtype.clone(),
                        },
                    )?;
                }
                Ok(())
            }
        }
    }

    /// Deletes the entry (`Ok` if it doesn't exist).
    ///
    /// # Errors
    ///
    /// Registry access errors.
    pub fn delete(&self) -> io::Result<()> {
        match hkcu().delete_subkey_all(self.key_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Test roots only: deletes `Software\opensmartersTest\<id>` and, if it is then
    /// empty, `Software\opensmartersTest`. Does nothing for the real root.
    pub fn delete_test_tree(&self) {
        let Some(id) = &self.test_id else {
            return;
        };
        let _ = hkcu().delete_subkey_all(format!(r"{TEST_ROOT_PARENT}\{id}"));
        if let Ok(parent) = hkcu().open_subkey_with_flags(TEST_ROOT_PARENT, KEY_READ)
            && parent.enum_keys().next().is_none()
            && parent.enum_values().next().is_none()
        {
            drop(parent);
            let _ = hkcu().delete_subkey(TEST_ROOT_PARENT);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_roots_never_touch_the_real_key() {
        let t = RegistryRoot::test(r"..\..\Microsoft\x y");
        assert!(t.key_path().starts_with(TEST_ROOT_PARENT));
        assert!(!t.key_path().contains(".."));
        assert_eq!(
            RegistryRoot::real().key_path(),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall\opensmarters"
        );
    }

    #[test]
    fn write_snapshot_restore_delete() {
        let root = RegistryRoot::test("unit-registry");
        root.delete_test_tree();
        assert_eq!(root.read_installed().unwrap(), None);
        let empty = root.snapshot().unwrap();
        let entry = UninstallEntry {
            version: "0.1.0".into(),
            location: r"C:\Somewhere\opensmarters".into(),
            estimated_size_kib: 1234,
        };
        root.write(&entry).unwrap();
        assert_eq!(
            root.read_installed().unwrap(),
            Some(Installed {
                version: "0.1.0".into(),
                location: r"C:\Somewhere\opensmarters".into()
            })
        );
        let values = root.values().unwrap();
        let get = |k: &str| {
            values
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("DisplayName"), "opensmarters");
        assert_eq!(
            get("UninstallString"),
            r#""C:\Somewhere\opensmarters\uninstall.exe" --uninstall"#
        );
        assert_eq!(get("NoModify"), "1");
        let v1 = root.snapshot().unwrap();
        root.write(&UninstallEntry {
            version: "0.2.0".into(),
            ..entry
        })
        .unwrap();
        root.restore(&v1).unwrap();
        assert_eq!(root.read_installed().unwrap().unwrap().version, "0.1.0");
        root.restore(&empty).unwrap();
        assert_eq!(root.read_installed().unwrap(), None);
        root.delete_test_tree();
    }
}

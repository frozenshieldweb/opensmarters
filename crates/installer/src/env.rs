// SPDX-License-Identifier: GPL-3.0-or-later
//! [`Environment`]: every place the installer reads or writes, in one value (ADR 0002,
//! decision 6).
//!
//! - [`Environment::real`] is what the shipped installer uses: the per-user known folders
//!   (`%LOCALAPPDATA%\Programs`, the Start menu's Programs folder, the Desktop), the app's
//!   data folder, `%TEMP%`, the real Uninstall key and Windows Credential Manager.
//! - [`Environment::sandbox`] points everything into one folder: a fake
//!   `LocalAppData\Programs`, Start menu, Desktop, data folder and temp folder, the **test
//!   registry root** (`HKCU\Software\opensmartersTest\<id>`) and a file-backed fake
//!   keychain. Tests use it directly; the binary reaches it only through the `dev`
//!   feature's `--sandbox` flag. It never touches real user state.

use std::io;
use std::path::{Path, PathBuf};

use known_folders::{KnownFolder, get_known_folder_path};
use sha2::{Digest, Sha256};

use crate::brand;
use crate::keychain::Keychain;
use crate::paths::Protected;
use crate::registry::RegistryRoot;

/// A required folder couldn't be found.
#[derive(Debug, thiserror::Error)]
#[error("Windows did not report the {0} folder")]
pub struct EnvError(&'static str);

/// Every location the installer touches.
#[derive(Debug, Clone)]
pub struct Environment {
    /// `Some(root)` for a sandbox.
    pub sandbox: Option<PathBuf>,
    /// The default install folder (`%LOCALAPPDATA%\Programs\opensmarters`).
    pub default_install_dir: PathBuf,
    /// The Start menu's Programs folder (`%APPDATA%\Microsoft\Windows\Start Menu\Programs`).
    pub programs_dir: PathBuf,
    /// The Desktop.
    pub desktop_dir: PathBuf,
    /// The app's data folder (`%LOCALAPPDATA%\opensmarters`).
    pub data_dir: PathBuf,
    /// The temp folder (log, lock, the uninstaller's temporary copy).
    pub temp_dir: PathBuf,
    /// Where the Apps & features entry goes.
    pub registry: RegistryRoot,
    /// Where saved passwords are purged from.
    pub keychain: Keychain,
    /// Folders the install folder may not be.
    pub protected: Protected,
}

fn folder(f: KnownFolder, name: &'static str) -> Result<PathBuf, EnvError> {
    get_known_folder_path(f).ok_or(EnvError(name))
}

impl Environment {
    /// The real per-user environment.
    ///
    /// # Errors
    ///
    /// Windows doesn't report a required known folder.
    pub fn real() -> Result<Self, EnvError> {
        let local = folder(KnownFolder::LocalAppData, "LocalAppData")?;
        let user_programs = get_known_folder_path(KnownFolder::UserProgramFiles)
            .unwrap_or_else(|| local.join("Programs"));
        Ok(Self {
            sandbox: None,
            default_install_dir: user_programs.join(brand::INSTALL_DIR_NAME),
            programs_dir: folder(KnownFolder::Programs, "Start menu")?,
            desktop_dir: folder(KnownFolder::Desktop, "Desktop")?,
            data_dir: local.join(brand::DATA_DIR_NAME),
            temp_dir: std::env::temp_dir(),
            registry: RegistryRoot::real(),
            keychain: Keychain::Real,
            protected: Self::real_protected(),
        })
    }

    /// The protected-folder list of the real system (also used inside sandboxes).
    pub fn real_protected() -> Protected {
        let get = |f| get_known_folder_path(f);
        let mut tree: Vec<PathBuf> = [
            KnownFolder::Windows,
            KnownFolder::System,
            KnownFolder::SystemX86,
            KnownFolder::ProgramFiles,
            KnownFolder::ProgramFilesX64,
            KnownFolder::ProgramFilesX86,
            KnownFolder::ProgramData,
        ]
        .into_iter()
        .filter_map(get)
        .collect();
        if let Some(local) = get(KnownFolder::LocalAppData) {
            tree.push(local.join(brand::DATA_DIR_NAME));
        }
        if let Some(videos) = get(KnownFolder::Videos) {
            tree.push(videos.join(brand::RECORDINGS_DIR_NAME));
        }
        let mut exact: Vec<PathBuf> = [
            KnownFolder::Profile,
            KnownFolder::UserProfiles,
            KnownFolder::LocalAppData,
            KnownFolder::RoamingAppData,
            KnownFolder::UserProgramFiles,
            KnownFolder::Desktop,
            KnownFolder::Documents,
            KnownFolder::Downloads,
            KnownFolder::Videos,
            KnownFolder::Programs,
            KnownFolder::StartMenu,
        ]
        .into_iter()
        .filter_map(get)
        .collect();
        exact.push(std::env::temp_dir());
        Protected { tree, exact }
    }

    /// A sandbox under `root` (created if needed). See the module docs.
    ///
    /// # Errors
    ///
    /// The folders can't be created.
    pub fn sandbox(root: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let root = std::fs::canonicalize(root)?;
        let root = PathBuf::from(
            root.to_string_lossy()
                .strip_prefix(r"\\?\")
                .map_or_else(|| root.to_string_lossy().into_owned(), str::to_owned),
        );
        let local = root.join("LocalAppData");
        let programs = root.join(r"Roaming\Microsoft\Windows\Start Menu\Programs");
        let desktop = root.join("Desktop");
        let temp = root.join("Temp");
        for d in [&local.join("Programs"), &programs, &desktop, &temp] {
            std::fs::create_dir_all(d)?;
        }
        let id = crate::pack::hex(&Sha256::digest(
            root.to_string_lossy().to_lowercase().as_bytes(),
        ));
        let mut protected = Self::real_protected();
        protected.tree.push(local.join(brand::DATA_DIR_NAME));
        protected.exact.extend([
            root.clone(),
            local.clone(),
            local.join("Programs"),
            desktop.clone(),
            programs.clone(),
            temp.clone(),
        ]);
        Ok(Self {
            sandbox: Some(root.clone()),
            default_install_dir: local.join("Programs").join(brand::INSTALL_DIR_NAME),
            programs_dir: programs,
            desktop_dir: desktop,
            data_dir: local.join(brand::DATA_DIR_NAME),
            temp_dir: temp,
            registry: RegistryRoot::test(&id[..16]),
            keychain: Keychain::Fake(root.join("keychain.txt")),
            protected,
        })
    }

    /// True for a sandbox.
    pub fn is_sandbox(&self) -> bool {
        self.sandbox.is_some()
    }

    /// The Start menu shortcut's path.
    pub fn start_menu_shortcut(&self) -> PathBuf {
        self.programs_dir.join(brand::SHORTCUT)
    }

    /// The desktop shortcut's path.
    pub fn desktop_shortcut(&self) -> PathBuf {
        self.desktop_dir.join(brand::SHORTCUT)
    }

    /// The default log file (`%TEMP%\opensmarters-setup.log`).
    pub fn default_log(&self) -> PathBuf {
        self.temp_dir.join(brand::LOG_FILE)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_stays_in_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Environment::sandbox(tmp.path()).unwrap();
        let root = env.sandbox.clone().unwrap();
        for p in [
            &env.default_install_dir,
            &env.programs_dir,
            &env.desktop_dir,
            &env.data_dir,
            &env.temp_dir,
        ] {
            assert!(p.starts_with(&root), "{}", p.display());
        }
        assert!(env.registry.is_test());
        assert!(matches!(env.keychain, Keychain::Fake(_)));
        let real = Environment::real().unwrap();
        assert!(!real.registry.is_test());
        assert!(real.default_install_dir.ends_with(r"Programs\opensmarters"));
    }
}

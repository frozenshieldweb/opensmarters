// SPDX-License-Identifier: GPL-3.0-or-later
//! Every name the installer shares with the app, **in one place** (LAUNCH_PROMPT §0, §B5).
//!
//! A future rename changes them here once. `tests::names_match_the_app` pins the data
//! folder and the keychain service to what `opensmarters-store` really uses, so the two
//! can't drift apart silently.

/// The product name, always lowercase in the UI and in prose.
pub const PRODUCT: &str = "opensmarters";
/// The app executable inside the install folder.
pub const APP_EXE: &str = "opensmarters.exe";
/// libmpv, loaded by the app from its own folder (in use while the app runs).
pub const MPV_DLL: &str = "libmpv-2.dll";
/// The uninstaller: a copy of the setup exe in the install folder.
pub const UNINSTALL_EXE: &str = "uninstall.exe";
/// The ownership marker + list of installed files in the install folder.
pub const INSTALL_MANIFEST: &str = "install-manifest.txt";
/// The install folder's name under `%LOCALAPPDATA%\Programs`.
pub const INSTALL_DIR_NAME: &str = "opensmarters";
/// The Apps & features key name under `…\CurrentVersion\Uninstall`.
pub const REGISTRY_KEY: &str = "opensmarters";
/// Shortcut file name (Start menu and desktop).
pub const SHORTCUT: &str = "opensmarters.lnk";
/// Publisher shown in Apps & features.
pub const PUBLISHER: &str = "opensmarters contributors";
/// The app's data folder under `%LOCALAPPDATA%`: the root of the store's
/// `ProjectDirs::from("org", "opensmarters", "opensmarters")` tree
/// (`%LOCALAPPDATA%\opensmarters\opensmarters\data\…`).
pub const DATA_DIR_NAME: &str = "opensmarters";
/// The keychain service of the app's Windows Credential Manager entries (target names
/// `<key>.opensmarters`, written by `keyring`).
pub const KEYCHAIN_SERVICE: &str = "opensmarters";
/// The app's recordings folder name under the user's Videos folder (never deleted).
pub const RECORDINGS_DIR_NAME: &str = "opensmarters";
/// Log file name in `%TEMP%`.
pub const LOG_FILE: &str = "opensmarters-setup.log";
/// Lock file name in `%TEMP%` (one installer at a time).
pub const LOCK_FILE: &str = "opensmarters-setup.lock";
/// Prefix of the uninstaller's temporary copies in `%TEMP%`.
pub const TEMP_UNINSTALLER_PREFIX: &str = "opensmarters-uninstall-";
/// Where people read about the project (Apps & features "URLInfoAbout" and "HelpLink"):
/// the workspace's `repository` field, so it is set in one place (the root `Cargo.toml`).
pub const URL_INFO_ABOUT: &str = env!("CARGO_PKG_REPOSITORY");
/// This installer's version (the workspace version, the same as the app it carries).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn names_match_the_app() {
        assert_eq!(
            KEYCHAIN_SERVICE,
            opensmarters_store::KeyringSecretStore::SERVICE
        );
        // %LOCALAPPDATA%\<DATA_DIR_NAME>\opensmarters\data\opensmarters.db
        let db = opensmarters_store::default_db_path().unwrap();
        let local =
            known_folders::get_known_folder_path(known_folders::KnownFolder::LocalAppData).unwrap();
        let rel = db.strip_prefix(&local).unwrap();
        let first = rel.components().next().unwrap();
        assert_eq!(first.as_os_str().to_str(), Some(DATA_DIR_NAME));
        assert!(PRODUCT.chars().all(|c| c.is_ascii_lowercase()));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Removing the app's saved passwords at uninstall, **only** when the user ticks "Also
//! remove saved passwords from Windows Credential Manager" (or passes
//! `--purge-passwords`).
//!
//! The app stores each secret as a Windows generic credential through `keyring`, with the
//! target name `<key>.opensmarters` (`opensmarters_store::KeyringSecretStore`). The real
//! backend enumerates generic credentials with `windows-native-keyring-store` (the same
//! store `keyring` uses, safe API) and deletes those whose service part is exactly
//! [`brand::KEYCHAIN_SERVICE`]. Sandbox runs and tests use [`Keychain::Fake`], a text file
//! of target names, and never touch Credential Manager.

use std::path::PathBuf;

use crate::brand;

/// A keychain failure (plain message, no secrets: we never read secret values).
#[derive(Debug, thiserror::Error)]
#[error("Windows Credential Manager: {0}")]
pub struct KeychainError(String);

/// Which credential store to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keychain {
    /// Windows Credential Manager (the shipped installer only).
    Real,
    /// A text file with one credential target name per line (tests, sandbox runs).
    Fake(PathBuf),
}

fn is_ours(target: &str) -> bool {
    target
        .rsplit_once('.')
        .is_some_and(|(user, service)| !user.is_empty() && service == brand::KEYCHAIN_SERVICE)
}

impl Keychain {
    /// How many of the app's credentials exist.
    ///
    /// # Errors
    ///
    /// The store can't be enumerated.
    pub fn count(&self) -> Result<usize, KeychainError> {
        match self {
            Self::Real => Ok(real::entries()?.len()),
            Self::Fake(path) => Ok(fake_lines(path)?.iter().filter(|l| is_ours(l)).count()),
        }
    }

    /// Deletes every credential of the app. Returns how many were deleted.
    ///
    /// # Errors
    ///
    /// The store can't be enumerated or an entry can't be deleted (the others still are).
    pub fn purge(&self) -> Result<usize, KeychainError> {
        match self {
            Self::Real => real::purge(),
            Self::Fake(path) => {
                let lines = fake_lines(path)?;
                let (ours, rest): (Vec<_>, Vec<_>) = lines.into_iter().partition(|l| is_ours(l));
                let mut text = rest.join("\n");
                if !text.is_empty() {
                    text.push('\n');
                }
                std::fs::write(path, text).map_err(|e| KeychainError(e.to_string()))?;
                Ok(ours.len())
            }
        }
    }
}

fn fake_lines(path: &PathBuf) -> Result<Vec<String>, KeychainError> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(KeychainError(e.to_string())),
    }
}

mod real {
    use std::collections::HashMap;

    use keyring_core::api::CredentialStoreApi as _;

    use super::KeychainError;
    use crate::brand;

    fn err(e: keyring_core::Error) -> KeychainError {
        // keyring errors that carry credential bytes are never produced by enumeration or
        // deletion; still, only the variant's description is kept.
        KeychainError(match e {
            keyring_core::Error::BadEncoding(_) | keyring_core::Error::BadDataFormat(..) => {
                "malformed credential".to_owned()
            }
            other => other.to_string(),
        })
    }

    pub(super) fn entries() -> Result<Vec<keyring_core::Entry>, KeychainError> {
        let store = windows_native_keyring_store::Store::new().map_err(err)?;
        // The service is plain ASCII letters (checked by a test), so no regex escaping.
        let pattern = format!(r"\.{}$", brand::KEYCHAIN_SERVICE);
        let spec = HashMap::from([("pattern", pattern.as_str())]);
        let found = store.search(&spec).map_err(err)?;
        Ok(found
            .into_iter()
            .filter(|e| {
                e.get_specifiers().is_some_and(|(service, user)| {
                    service == brand::KEYCHAIN_SERVICE && !user.is_empty()
                })
            })
            .collect())
    }

    pub(super) fn purge() -> Result<usize, KeychainError> {
        let mut deleted = 0;
        let mut first_error = None;
        for entry in entries()? {
            match entry.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => deleted += 1,
                Err(e) => {
                    first_error.get_or_insert(err(e));
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(deleted),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_entries_match() {
        assert!(
            brand::KEYCHAIN_SERVICE
                .chars()
                .all(|c| c.is_ascii_alphanumeric())
        );
        assert!(is_ours("profile/1234/password.opensmarters"));
        assert!(is_ours("cache_key.opensmarters"));
        assert!(is_ours("a.b.opensmarters"));
        assert!(!is_ours("opensmarters"));
        assert!(!is_ours(".opensmarters"));
        assert!(!is_ours("profile.opensmarters.example"));
        assert!(!is_ours("git:https://example.org"));
        assert!(!is_ours("x.opensmartersTest"));
    }

    #[test]
    fn the_fake_keychain_purges_only_ours() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("keychain.txt");
        std::fs::write(
            &file,
            "profile/1/password.opensmarters\nother.app\ncache_key.opensmarters\n",
        )
        .unwrap();
        let k = Keychain::Fake(file.clone());
        assert_eq!(k.count().unwrap(), 2);
        assert_eq!(k.purge().unwrap(), 2);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "other.app\n");
        assert_eq!(k.count().unwrap(), 0);
    }
}

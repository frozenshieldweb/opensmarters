// SPDX-License-Identifier: GPL-3.0-or-later
//! Start menu and desktop shortcuts, written with `mslnk` (a pure-Rust `.lnk` writer).
//!
//! The app defines no AppUserModelID, so none is set. A shortcut counts as **ours** only if
//! it names the install folder (its working directory is stored as UTF-16 inside the
//! `.lnk`), so uninstall never deletes a same-named shortcut that points elsewhere.

use std::io;
use std::path::{Path, PathBuf};

/// A shortcut that existed before we wrote ours (restored on rollback).
#[derive(Debug, Clone)]
pub struct Backup {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

impl Backup {
    /// Captures what is at `path` now.
    ///
    /// # Errors
    ///
    /// I/O errors other than "not found".
    pub fn take(path: &Path) -> io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: path.to_path_buf(),
            bytes,
        })
    }

    /// Puts it back (or removes ours if there was nothing).
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn restore(&self) -> io::Result<()> {
        match &self.bytes {
            Some(b) => std::fs::write(&self.path, b),
            None => match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }
}

/// Writes `lnk` pointing at `target` (working directory: the target's folder).
///
/// # Errors
///
/// The target isn't a valid Unicode drive path, or writing failed. `mslnk` panics on
/// paths it can't encode; that is caught and reported as an error.
pub fn create(lnk: &Path, target: &Path) -> io::Result<()> {
    let target_str = target
        .to_str()
        .filter(|s| s.as_bytes().get(1) == Some(&b':') && s.as_bytes().get(2) == Some(&b'\\'))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "shortcut target must be a drive path",
            )
        })?
        .to_owned();
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lnk = lnk.to_path_buf();
    let result = std::panic::catch_unwind(move || -> Result<(), String> {
        let mut link = mslnk::ShellLink::new(&target_str).map_err(|e| e.to_string())?;
        link.set_name(Some(crate::brand::PRODUCT.to_owned()));
        link.create_lnk(&lnk).map_err(|e| e.to_string())
    });
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(io::Error::other(e)),
        Err(_) => Err(io::Error::other("the shortcut could not be encoded")),
    }
}

/// True if the `.lnk` at `lnk` mentions `install_dir` (case-insensitive, UTF-16).
pub fn is_ours(lnk: &Path, install_dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(lnk) else {
        return false;
    };
    let needle: Vec<u16> = install_dir
        .to_string_lossy()
        .trim_end_matches('\\')
        .to_lowercase()
        .encode_utf16()
        .collect();
    if needle.is_empty() || bytes.len() < needle.len() * 2 {
        return false;
    }
    let hay: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let lower = |u: u16| -> u16 {
        if (u16::from(b'A')..=u16::from(b'Z')).contains(&u) {
            u + 32
        } else {
            u
        }
    };
    // The string can start at an odd byte offset, so try both alignments.
    let hay_odd: Vec<u16> = bytes
        .get(1..)
        .unwrap_or(&[])
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    [hay, hay_odd].iter().any(|h| {
        h.windows(needle.len())
            .any(|w| w.iter().zip(&needle).all(|(a, b)| lower(*a) == *b))
    })
}

/// Removes `lnk` if it is ours. Returns whether it was removed.
///
/// # Errors
///
/// I/O errors other than "not found".
pub fn remove_if_ours(lnk: &Path, install_dir: &Path) -> io::Result<bool> {
    if !is_ours(lnk, install_dir) {
        return Ok(false);
    }
    match std::fs::remove_file(lnk) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn creates_recognises_and_restores() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        let app_dir = t.join("Programs").join("opensmarters");
        std::fs::create_dir_all(&app_dir).unwrap();
        let exe = app_dir.join("opensmarters.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let lnk = t.join("Start").join("opensmarters.lnk");
        let backup = Backup::take(&lnk).unwrap();
        create(&lnk, &exe).unwrap();
        let bytes = std::fs::read(&lnk).unwrap();
        assert_eq!(&bytes[..4], &[0x4c, 0, 0, 0], "a shell link header");
        assert!(is_ours(&lnk, &app_dir));
        assert!(is_ours(
            &lnk,
            Path::new(&app_dir.to_string_lossy().to_uppercase())
        ));
        assert!(!is_ours(&lnk, &t.join("elsewhere")));
        assert!(!remove_if_ours(&lnk, &t.join("elsewhere")).unwrap());
        backup.restore().unwrap();
        assert!(!lnk.exists());
        // A pre-existing file comes back byte for byte.
        std::fs::write(&lnk, b"theirs").unwrap();
        let backup = Backup::take(&lnk).unwrap();
        create(&lnk, &exe).unwrap();
        assert!(remove_if_ours(&lnk, &app_dir).unwrap());
        backup.restore().unwrap();
        assert_eq!(std::fs::read(&lnk).unwrap(), b"theirs");
    }

    #[test]
    fn refuses_non_drive_targets() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            create(
                &tmp.path().join("x.lnk"),
                Path::new(r"\\server\share\a.exe")
            )
            .is_err()
        );
    }
}

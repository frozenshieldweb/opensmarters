// SPDX-License-Identifier: GPL-3.0-or-later
//! The payload: the embedded xz archive and its manifest (ADR 0002, decision 3).
//!
//! Integrity first: [`Payload::verify_archive`] checks the archive's SHA-256 before
//! anything is extracted; [`Payload::extract`] hashes every file while writing it and
//! stops at the first mismatch, a short stream or trailing data. Any of these is
//! [`PayloadError::Damaged`]: "The installer is damaged. Download it again and check its
//! SHA-256."

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::manifest::{FileEntry, PayloadManifest};

const EMBEDDED_XZ: &[u8] = include_bytes!(env!("OPENSMARTERS_PAYLOAD_XZ"));
const EMBEDDED_MANIFEST: &str = include_str!(env!("OPENSMARTERS_PAYLOAD_MANIFEST"));

/// What went wrong with the payload.
#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    /// The archive or a file doesn't match its hash, or the stream is short or corrupt.
    #[error("the installer is damaged: {0}")]
    Damaged(String),
    /// Cancelled by the user.
    #[error("cancelled")]
    Cancelled,
    /// Writing a file failed.
    #[error("could not write {path}: {source}")]
    Write {
        /// The file.
        path: String,
        /// The OS error.
        #[source]
        source: io::Error,
    },
}

/// Progress callback: bytes written so far.
pub type OnBytes<'a> = &'a dyn Fn(u64);

/// An archive plus its manifest.
#[derive(Debug, Clone)]
pub struct Payload<'a> {
    /// The xz archive.
    pub archive: &'a [u8],
    /// Its manifest.
    pub manifest: PayloadManifest,
}

impl Payload<'static> {
    /// The payload embedded at build time (synthetic in dev builds, the real app in
    /// release packages).
    ///
    /// # Errors
    ///
    /// The embedded manifest doesn't parse (a broken build).
    pub fn embedded() -> Result<Self, PayloadError> {
        let manifest = PayloadManifest::parse(EMBEDDED_MANIFEST)
            .map_err(|e| PayloadError::Damaged(format!("embedded manifest: {e}")))?;
        Ok(Self {
            archive: EMBEDDED_XZ,
            manifest,
        })
    }
}

impl<'a> Payload<'a> {
    /// A payload from bytes (tests use corrupted fixtures).
    pub fn new(archive: &'a [u8], manifest: PayloadManifest) -> Self {
        Self { archive, manifest }
    }

    /// Checks the archive's size and SHA-256 against the manifest.
    ///
    /// # Errors
    ///
    /// [`PayloadError::Damaged`] on a mismatch.
    pub fn verify_archive(&self) -> Result<(), PayloadError> {
        if self.archive.len() as u64 != self.manifest.archive_size {
            return Err(PayloadError::Damaged(format!(
                "archive is {} bytes, expected {}",
                self.archive.len(),
                self.manifest.archive_size
            )));
        }
        let digest = Sha256::digest(self.archive);
        if digest.as_slice() != self.manifest.archive_sha256.as_slice() {
            return Err(PayloadError::Damaged(
                "archive SHA-256 does not match".to_owned(),
            ));
        }
        Ok(())
    }

    /// Extracts every file into `dest` (which must exist and be empty), creating each file
    /// new (never overwriting), hashing as it writes. `on_bytes` gets the running total.
    /// `slow` throttles the work (dev builds only, to watch the progress).
    ///
    /// # Errors
    ///
    /// [`PayloadError::Damaged`], [`PayloadError::Cancelled`] or [`PayloadError::Write`].
    /// The caller removes `dest` on error.
    pub fn extract(
        &self,
        dest: &Path,
        on_bytes: OnBytes<'_>,
        cancel: &AtomicBool,
        slow: Option<Duration>,
    ) -> Result<(), PayloadError> {
        let damaged = |what: String| PayloadError::Damaged(what);
        let mut xz = lzma_rust2::XzReader::new(self.archive, false);
        let mut buf = vec![0_u8; 256 * 1024];
        let mut done: u64 = 0;
        for entry in &self.manifest.files {
            let path = dest.join(entry.native());
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| PayloadError::Write {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
            let write_err = |source: io::Error| PayloadError::Write {
                path: path.display().to_string(),
                source,
            };
            let mut file = fs::File::create_new(&path).map_err(write_err)?;
            let mut hasher = Sha256::new();
            let mut left = entry.size;
            while left > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(PayloadError::Cancelled);
                }
                let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
                let n = xz
                    .read(&mut buf[..want])
                    .map_err(|e| damaged(format!("{}: {e}", entry.path)))?;
                if n == 0 {
                    return Err(damaged(format!("{}: the archive ends early", entry.path)));
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n]).map_err(write_err)?;
                left -= n as u64;
                done += n as u64;
                on_bytes(done);
                if let Some(d) = slow {
                    std::thread::sleep(d);
                }
            }
            file.flush().map_err(write_err)?;
            if hasher.finalize().as_slice() != entry.sha256.as_slice() {
                return Err(damaged(format!(
                    "{} does not match its SHA-256",
                    entry.path
                )));
            }
        }
        // Nothing may follow the last file.
        let n = xz
            .read(&mut buf[..1])
            .map_err(|e| damaged(format!("end of archive: {e}")))?;
        if n != 0 {
            return Err(damaged("unexpected data after the last file".to_owned()));
        }
        Ok(())
    }
}

/// Hashes `path` (streaming) and compares it with `entry`. `on_bytes` gets bytes read.
///
/// # Errors
///
/// I/O errors; `Ok(false)` on a size or hash mismatch.
pub fn file_matches(path: &Path, entry: &FileEntry, on_bytes: OnBytes<'_>) -> io::Result<bool> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() != entry.size {
        return Ok(false);
    }
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        on_bytes(n as u64);
    }
    Ok(hasher.finalize().as_slice() == entry.sha256.as_slice())
}

/// SHA-256 and size of a file.
///
/// # Errors
///
/// I/O errors.
pub fn hash_file(path: &Path) -> io::Result<([u8; 32], u64)> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 256 * 1024];
    let mut size = 0_u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hasher.finalize().into(), size))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_payload_is_intact() {
        let p = Payload::embedded().unwrap();
        p.verify_archive().unwrap();
        assert_eq!(p.manifest.version, crate::brand::VERSION);
        assert!(
            p.manifest
                .files
                .iter()
                .any(|f| f.path == crate::brand::APP_EXE)
        );
    }

    #[test]
    fn extracts_and_verifies() {
        let p = Payload::embedded().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let total = std::cell::Cell::new(0);
        p.extract(dir.path(), &|n| total.set(n), &AtomicBool::new(false), None)
            .unwrap();
        assert_eq!(total.get(), p.manifest.installed_size);
        for f in &p.manifest.files {
            assert!(file_matches(&dir.path().join(f.native()), f, &|_| {}).unwrap());
        }
    }

    #[test]
    fn a_flipped_byte_is_damage() {
        let p = Payload::embedded().unwrap();
        let mut bytes = p.archive.to_vec();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x40;
        let bad = Payload::new(&bytes, p.manifest.clone());
        assert!(matches!(
            bad.verify_archive(),
            Err(PayloadError::Damaged(_))
        ));
        // Even if someone skipped the archive check, extraction catches it.
        let dir = tempfile::tempdir().unwrap();
        let r = bad.extract(dir.path(), &|_| {}, &AtomicBool::new(false), None);
        assert!(matches!(r, Err(PayloadError::Damaged(_))), "{r:?}");
    }

    #[test]
    fn a_wrong_file_hash_is_damage() {
        let p = Payload::embedded().unwrap();
        let mut m = p.manifest.clone();
        let last = m.files.len() - 1;
        m.files[last].sha256[0] ^= 1;
        let bad = Payload::new(p.archive, m);
        let dir = tempfile::tempdir().unwrap();
        let r = bad.extract(dir.path(), &|_| {}, &AtomicBool::new(false), None);
        assert!(matches!(r, Err(PayloadError::Damaged(_))), "{r:?}");
    }

    #[test]
    fn cancel_stops_extraction() {
        let p = Payload::embedded().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let r = p.extract(dir.path(), &|_| {}, &AtomicBool::new(true), None);
        assert!(matches!(r, Err(PayloadError::Cancelled)));
    }
}

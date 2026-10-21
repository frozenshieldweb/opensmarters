// SPDX-License-Identifier: GPL-3.0-or-later
//! The two manifests (ADR 0002):
//!
//! - [`PayloadManifest`]: embedded next to the archive (written by [`crate::pack`]).
//! - [`InstallManifest`]: `install-manifest.txt` in the install folder: the ownership
//!   marker and the list of installed files. **Uninstall deletes only what it lists.**
//!
//! Both are plain text with `file <sha256> <size> <path>` lines. Parsing is strict and
//! bounded: the install manifest is read back from disk, so its paths are validated
//! ([`crate::pack::is_valid_rel_path`]: relative, no `..`, no drive or device names).

use std::fmt::Write as _;
use std::path::Path;

use crate::brand;
use crate::pack::{self, MAX_FILES, PAYLOAD_HEADER};

/// First line of an install manifest.
pub const INSTALL_HEADER: &str = "opensmarters-install 1";
/// Largest manifest we read (bytes).
pub const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// A malformed manifest.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    /// Wrong first line.
    #[error("unknown manifest format")]
    Header,
    /// A required field is missing.
    #[error("the manifest has no {0}")]
    Missing(&'static str),
    /// A line could not be parsed.
    #[error("line {0} is malformed")]
    Line(usize),
    /// An unsafe or invalid relative path.
    #[error("line {0} names an invalid path")]
    Path(usize),
    /// The same path twice.
    #[error("line {0} repeats a path")]
    Duplicate(usize),
    /// Too many files or too large.
    #[error("the manifest is too large")]
    TooLarge,
    /// Not this product.
    #[error("the manifest belongs to another product")]
    Product,
}

/// One file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path with `/` separators (validated).
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the content.
    pub sha256: [u8; 32],
}

impl FileEntry {
    /// The path as a native relative path.
    pub fn native(&self) -> std::path::PathBuf {
        self.path.split('/').collect()
    }

    fn line(&self) -> String {
        format!(
            "file {} {} {}",
            pack::hex(&self.sha256),
            self.size,
            self.path
        )
    }
}

/// The embedded payload's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadManifest {
    /// Product name.
    pub product: String,
    /// The app version inside.
    pub version: String,
    /// Size of the xz archive.
    pub archive_size: u64,
    /// SHA-256 of the xz archive.
    pub archive_sha256: [u8; 32],
    /// Sum of the file sizes.
    pub installed_size: u64,
    /// Files in archive order.
    pub files: Vec<FileEntry>,
}

/// `install-manifest.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallManifest {
    /// Product name (must be [`brand::PRODUCT`]).
    pub product: String,
    /// Installed version.
    pub version: String,
    /// Installed files (the manifest itself is not listed).
    pub files: Vec<FileEntry>,
}

fn parse_hash(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0_u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

struct Parsed {
    fields: Vec<(String, String)>,
    files: Vec<FileEntry>,
}

fn parse_common(text: &str, header: &str) -> Result<Parsed, ManifestError> {
    if text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    let mut lines = text.lines().map(|l| l.strip_suffix('\r').unwrap_or(l));
    if lines.next() != Some(header) {
        return Err(ManifestError::Header);
    }
    let mut fields = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, line) in lines.enumerate() {
        let n = i + 2;
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("file ") {
            let (hash, rest) = rest.split_once(' ').ok_or(ManifestError::Line(n))?;
            let (size, path) = rest.split_once(' ').ok_or(ManifestError::Line(n))?;
            let sha256 = parse_hash(hash).ok_or(ManifestError::Line(n))?;
            let size: u64 = size.parse().map_err(|_| ManifestError::Line(n))?;
            if !pack::is_valid_rel_path(path) {
                return Err(ManifestError::Path(n));
            }
            if !seen.insert(path.to_ascii_lowercase()) {
                return Err(ManifestError::Duplicate(n));
            }
            files.push(FileEntry {
                path: path.to_owned(),
                size,
                sha256,
            });
            if files.len() > MAX_FILES {
                return Err(ManifestError::TooLarge);
            }
        } else {
            let (k, v) = line.split_once(' ').ok_or(ManifestError::Line(n))?;
            fields.push((k.to_owned(), v.to_owned()));
        }
    }
    Ok(Parsed { fields, files })
}

impl Parsed {
    fn get(&self, key: &'static str) -> Result<&str, ManifestError> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .ok_or(ManifestError::Missing(key))
    }

    fn number(&self, key: &'static str) -> Result<u64, ManifestError> {
        self.get(key)?
            .parse()
            .map_err(|_| ManifestError::Missing(key))
    }
}

impl PayloadManifest {
    /// Parses a payload manifest.
    ///
    /// # Errors
    ///
    /// Any [`ManifestError`].
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let p = parse_common(text, PAYLOAD_HEADER)?;
        let product = p.get("product")?.to_owned();
        if product != brand::PRODUCT {
            return Err(ManifestError::Product);
        }
        let archive_sha256 =
            parse_hash(p.get("archive-sha256")?).ok_or(ManifestError::Missing("archive-sha256"))?;
        let installed_size = p.number("installed-size")?;
        if p.files.iter().map(|f| f.size).sum::<u64>() != installed_size {
            return Err(ManifestError::Missing("installed-size"));
        }
        Ok(Self {
            product,
            version: p.get("version")?.to_owned(),
            archive_size: p.number("archive-size")?,
            archive_sha256,
            installed_size,
            files: p.files,
        })
    }
}

impl InstallManifest {
    /// Parses `install-manifest.txt`.
    ///
    /// # Errors
    ///
    /// Any [`ManifestError`].
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let p = parse_common(text, INSTALL_HEADER)?;
        let product = p.get("product")?.to_owned();
        if product != brand::PRODUCT {
            return Err(ManifestError::Product);
        }
        Ok(Self {
            product,
            version: p.get("version")?.to_owned(),
            files: p.files,
        })
    }

    /// The manifest as text.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        s.push_str(INSTALL_HEADER);
        s.push('\n');
        let _ = writeln!(s, "product {}", self.product);
        let _ = writeln!(s, "version {}", self.version);
        for f in &self.files {
            s.push_str(&f.line());
            s.push('\n');
        }
        s
    }

    /// Reads `<dir>\install-manifest.txt`: `Ok(None)` if there is none.
    ///
    /// # Errors
    ///
    /// The file exists but can't be read, is a link, is too large or is malformed.
    pub fn read(dir: &Path) -> Result<Option<Self>, ReadManifestError> {
        let path = dir.join(brand::INSTALL_MANIFEST);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ReadManifestError::Io(e)),
        };
        if !meta.is_file() {
            return Err(ReadManifestError::Invalid(ManifestError::Header));
        }
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(ReadManifestError::Invalid(ManifestError::TooLarge));
        }
        let text = std::fs::read_to_string(&path).map_err(ReadManifestError::Io)?;
        Self::parse(&text)
            .map(Some)
            .map_err(ReadManifestError::Invalid)
    }

    /// Total size of the listed files.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Reading `install-manifest.txt` failed.
#[derive(Debug, thiserror::Error)]
pub enum ReadManifestError {
    /// I/O error.
    #[error("could not read the install manifest: {0}")]
    Io(std::io::Error),
    /// Malformed.
    #[error("the install manifest is not valid: {0}")]
    Invalid(ManifestError),
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn h(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    #[test]
    fn payload_manifest_round_trip() {
        let text = format!(
            "{PAYLOAD_HEADER}\nproduct opensmarters\nversion 0.2.0\narchive-size 10\narchive-sha256 {}\ninstalled-size 7\nfile {} 3 a b.txt\nfile {} 4 licenses/x.txt\n",
            h('a'),
            h('b'),
            h('c')
        );
        let m = PayloadManifest::parse(&text).unwrap();
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files[0].path, "a b.txt");
        assert_eq!(m.files[1].native(), std::path::Path::new("licenses\\x.txt"));
        assert_eq!(m.archive_sha256, [0xaa; 32]);
    }

    #[test]
    fn rejects_unsafe_paths_and_bad_lines() {
        for path in [
            "../x",
            "a/../../x",
            "C:/x",
            "c:x",
            "/abs",
            "a\\b",
            "a//b",
            "dir/./x",
            "CON",
            "nul.txt",
            "lpt1",
            "x.",
            "a/b ",
            "\u{7}bell",
        ] {
            let text = format!(
                "{INSTALL_HEADER}\nproduct opensmarters\nversion 1\nfile {} 1 {path}\n",
                h('0')
            );
            assert_eq!(
                InstallManifest::parse(&text),
                Err(ManifestError::Path(4)),
                "{path:?}"
            );
        }
        let bad = format!("{INSTALL_HEADER}\nproduct opensmarters\nversion 1\nfile zz 1 a\n");
        assert_eq!(InstallManifest::parse(&bad), Err(ManifestError::Line(4)));
        let dup = format!(
            "{INSTALL_HEADER}\nproduct opensmarters\nversion 1\nfile {0} 1 A.txt\nfile {0} 1 a.TXT\n",
            h('1')
        );
        assert_eq!(
            InstallManifest::parse(&dup),
            Err(ManifestError::Duplicate(5))
        );
        let other = format!("{INSTALL_HEADER}\nproduct something\nversion 1\n");
        assert_eq!(InstallManifest::parse(&other), Err(ManifestError::Product));
        assert_eq!(InstallManifest::parse("hello"), Err(ManifestError::Header));
    }

    #[test]
    fn install_manifest_round_trip() {
        let m = InstallManifest {
            product: "opensmarters".into(),
            version: "0.1.0".into(),
            files: vec![FileEntry {
                path: "uninstall.exe".into(),
                size: 5,
                sha256: [7; 32],
            }],
        };
        assert_eq!(InstallManifest::parse(&m.to_text()).unwrap(), m);
        assert_eq!(m.total_size(), 5);
    }

    #[test]
    fn hashing_matches_the_packer() {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(b"abc");
        assert_eq!(
            pack::hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            parse_hash(&pack::hex(&digest)).unwrap().as_slice(),
            digest.as_slice()
        );
    }
}

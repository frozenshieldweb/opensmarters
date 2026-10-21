// SPDX-License-Identifier: GPL-3.0-or-later
//! Packs a folder into the installer payload (ADR 0002, decision 3).
//!
//! - `payload.xz`: one xz stream (BCJ x86 + LZMA2) of every file's bytes, concatenated in
//!   **sorted path order** (byte order of the `/`-separated relative path). No names, times
//!   or permissions inside the stream, so identical inputs give a byte-identical archive.
//! - `payload.manifest`: plain text, one `key value` header line each, then one
//!   `file <sha256> <size> <path>` line per file, in archive order:
//!
//! ```text
//! opensmarters-payload 1
//! product opensmarters
//! version 0.1.0
//! archive-size 42391520
//! archive-sha256 3f2a…
//! installed-size 152614323
//! file 82bae3b6… 121043968 libmpv-2.dll
//! ```
//!
//! This file is shared verbatim by `build.rs` (the synthetic dev/test payload) and the
//! `opensmarters-pack` release tool, so it depends only on `std`, `lzma-rust2` and `sha2`.
//! The runtime parser is `crate::manifest`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// First line of a payload manifest.
pub const PAYLOAD_HEADER: &str = "opensmarters-payload 1";
/// Most files a payload may hold.
pub const MAX_FILES: usize = 4096;
/// Longest relative path (bytes).
pub const MAX_PATH_BYTES: usize = 512;

/// A packed payload.
#[derive(Debug)]
pub struct Packed {
    /// The xz archive.
    pub archive: Vec<u8>,
    /// The manifest text.
    pub manifest: String,
}

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// True if `p` is a relative path we accept in a manifest: `/`-separated, no empty, `.` or
/// `..` components, no drive, backslash, colon, control or Windows-reserved characters, no
/// trailing dot or space in a component, no device names (`CON`, `NUL`, `COM1`…), at most
/// [`MAX_PATH_BYTES`].
pub fn is_valid_rel_path(p: &str) -> bool {
    if p.is_empty() || p.len() > MAX_PATH_BYTES || p.starts_with('/') {
        return false;
    }
    p.split('/').all(|c| {
        !c.is_empty()
            && c != "."
            && c != ".."
            && !c.ends_with(['.', ' '])
            && !is_device_name(c)
            && !c.chars().any(|ch| {
                ch.is_control() || matches!(ch, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            })
    })
}

fn is_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}

/// Every regular file under `root` as `(relative path with '/', absolute path)`, sorted by
/// the relative path's bytes. Links (symlinks, junctions) are refused.
///
/// # Errors
///
/// I/O errors, a link, a non-Unicode or invalid name, or more than [`MAX_FILES`] files.
pub fn collect(root: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(invalid(format!("{} is a link", path.display())));
        }
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| invalid(format!("{} is outside the folder", path.display())))?;
            let mut parts = Vec::new();
            for c in rel.components() {
                let s = c
                    .as_os_str()
                    .to_str()
                    .ok_or_else(|| invalid(format!("{} is not a Unicode name", path.display())))?;
                parts.push(s.to_owned());
            }
            let rel = parts.join("/");
            if !is_valid_rel_path(&rel) {
                return Err(invalid(format!("{rel:?} is not a valid file name")));
            }
            out.push((rel, path));
            if out.len() > MAX_FILES {
                return Err(invalid(format!("more than {MAX_FILES} files")));
            }
        }
    }
    Ok(())
}

/// Packs every file under `root` with the given xz `preset` (0–9; releases use 9).
///
/// # Errors
///
/// See [`collect`]; a file that changes size while it is read; an empty folder.
pub fn pack(root: &Path, product: &str, version: &str, preset: u32) -> io::Result<Packed> {
    let files = collect(root)?;
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "nothing to pack",
        ));
    }
    let mut options = lzma_rust2::XzOptions::with_preset(preset.min(9));
    // x86 branch/call filter: ~1-3% smaller for the exe and the DLL, still lossless for text.
    options.prepend_pre_filter(lzma_rust2::FilterType::BcjX86, 0);
    let mut xz = lzma_rust2::XzWriter::new(Vec::new(), options)?;
    let mut lines = Vec::with_capacity(files.len());
    let mut installed: u64 = 0;
    let mut buf = vec![0_u8; 1 << 20];
    for (rel, abs) in &files {
        let mut f = fs::File::open(abs)?;
        let expected = f.metadata()?.len();
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            xz.write_all(&buf[..n])?;
            size += n as u64;
        }
        if size != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{rel} changed while it was packed"),
            ));
        }
        installed += size;
        lines.push(format!("file {} {size} {rel}", hex(&hasher.finalize())));
    }
    let archive = xz.finish()?;
    let mut manifest = String::new();
    manifest.push_str(PAYLOAD_HEADER);
    manifest.push('\n');
    manifest.push_str(&format!("product {product}\nversion {version}\n"));
    manifest.push_str(&format!("archive-size {}\n", archive.len()));
    manifest.push_str(&format!(
        "archive-sha256 {}\n",
        hex(&Sha256::digest(&archive))
    ));
    manifest.push_str(&format!("installed-size {installed}\n"));
    for l in lines {
        manifest.push_str(&l);
        manifest.push('\n');
    }
    Ok(Packed { archive, manifest })
}

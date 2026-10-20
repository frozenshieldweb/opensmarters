// SPDX-License-Identifier: GPL-3.0-or-later
//! Files in the recordings folder: safe unique names, the `.part` file a job writes, the
//! final rename, deletion and the free-space guard.
//!
//! Rules (docs/SECURITY.md §4.10):
//! - names come only from [`super::file_name`] / [`download_name`] (provider text reduced
//!   to a safe set of characters) and are checked again with the store's
//!   `validate_file_name` before any path is built ([`path_in`]);
//! - a job writes `<name>.part` and renames it to `<name>` only when it is complete and
//!   its first bytes were recognised as video;
//! - nothing is ever overwritten: a new `.part` is created with `create_new`, and a name
//!   that exists (as a file, a `.part`, or in the database) gets `" (2)"`, `" (3)"`…;
//! - only regular files directly inside the folder are deleted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use opensmarters_player::{Container, SNIFF_BYTES, sniff_container};
use opensmarters_store::recordings::validate_file_name;

/// Suffix of a file being written.
pub const PART_SUFFIX: &str = ".part";

/// Extensions a saved file may get (the containers [`sniff_container`] knows).
const EXTENSIONS: [&str; 7] = ["ts", "mkv", "mp4", "avi", "flv", "mpg", "wmv"];

/// A known extension for `ext` (`m4v`/`mov` → `mp4`, `webm` → `mkv`), else `None`.
pub fn known_extension(ext: &str) -> Option<&'static str> {
    let e = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    match e.as_str() {
        "m4v" | "mov" => Some("mp4"),
        "webm" => Some("mkv"),
        "mpeg" => Some("mpg"),
        _ => EXTENSIONS.iter().find(|k| **k == e).copied(),
    }
}

/// The extension that matches what the file really is.
pub fn container_extension(c: Container) -> &'static str {
    known_extension(c.extension()).unwrap_or("ts")
}

/// A download's file name: `"<title>.<ext>"` (no date, unlike recordings), with the same
/// sanitising as [`super::file_name`]. `ext` falls back to `mkv`.
pub fn download_name(title: &str, ext: &str) -> String {
    let t = super::clean_title(title);
    let ext = known_extension(ext).unwrap_or("mkv");
    format!("{t}.{ext}")
}

/// `name` with `" (n)"` before the extension (`n` ≥ 2).
pub fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// `name` with its extension replaced by `ext`.
pub fn with_extension(name: &str, ext: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => format!("{stem}.{ext}"),
        _ => format!("{name}.{ext}"),
    }
}

/// `dir/name`, only for a valid bare file name.
pub fn path_in(dir: &Path, name: &str) -> Option<PathBuf> {
    validate_file_name(name).ok()?;
    Some(dir.join(name))
}

/// `dir/name.part`.
pub fn part_path(dir: &Path, name: &str) -> Option<PathBuf> {
    path_in(dir, name).map(|p| {
        let mut s = p.into_os_string();
        s.push(PART_SUFFIX);
        PathBuf::from(s)
    })
}

/// `base` or the first free `numbered(base, n)`: not in `taken` (names the database
/// holds) and neither `dir/<name>` nor `dir/<name>.part` exists. Blocking (file system).
pub fn unique_name(dir: &Path, taken: &HashSet<String>, base: &str) -> Option<String> {
    let free = |name: &str| {
        !taken.contains(name)
            && path_in(dir, name).is_some_and(|p| !p.exists())
            && part_path(dir, name).is_some_and(|p| !p.exists())
    };
    if free(base) {
        return Some(base.to_owned());
    }
    (2..1000).map(|n| numbered(base, n)).find(|n| free(n))
}

/// Free bytes on the disk holding `dir` (`None` if unknown). Blocking.
pub fn free_space(dir: &Path) -> Option<u64> {
    fs4::available_space(dir).ok()
}

/// Reads up to [`SNIFF_BYTES`] from the start of `path`. Blocking.
pub fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    buf.truncate(n);
    Ok(buf)
}

/// The container of an existing file, if its first bytes are video. Blocking.
pub fn sniff_file(path: &Path) -> Option<Container> {
    read_head(path).ok().and_then(|h| sniff_container(&h))
}

/// Deletes `dir/name` and `dir/name.part` when they are regular files (never a folder or
/// a link). Blocking. Returns false if a file existed but couldn't be removed.
pub fn delete(dir: &Path, name: &str) -> bool {
    let mut ok = true;
    for p in [path_in(dir, name), part_path(dir, name)]
        .into_iter()
        .flatten()
    {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_file() => {
                if let Err(e) = std::fs::remove_file(&p) {
                    tracing::warn!(error = %e, "couldn't delete a recording file");
                    ok = false;
                }
            }
            _ => {}
        }
    }
    ok
}

/// Size of `dir/name` (or its `.part`), if it exists. Blocking.
pub fn size(dir: &Path, name: &str) -> Option<u64> {
    [path_in(dir, name), part_path(dir, name)]
        .into_iter()
        .flatten()
        .find_map(|p| std::fs::metadata(p).ok().filter(|m| m.is_file()))
        .map(|m| m.len())
}

/// True if a media segment's first bytes fit the recording's container: TS packets, or
/// an ISO-BMFF box (fragmented MP4 media segments start with `moof`/`styp`/`sidx`…,
/// which [`sniff_container`] alone doesn't list).
pub fn segment_looks_like(container: Container, head: &[u8]) -> bool {
    match container {
        Container::MpegTs => {
            head.first() == Some(&0x47) && head.get(188).is_none_or(|b| *b == 0x47)
        }
        Container::Mp4 => {
            let boxes: [&[u8]; 8] = [
                b"moof", b"styp", b"sidx", b"emsg", b"prft", b"ftyp", b"mdat", b"free",
            ];
            head.get(4..8).is_some_and(|t| boxes.contains(&t))
        }
        _ => sniff_container(head) == Some(container),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_numbers() {
        assert_eq!(
            download_name("FR | Le Film (2003)", "MKV"),
            "FR Le Film (2003).mkv"
        );
        assert_eq!(download_name("x", "m4v"), "x.mp4");
        assert_eq!(download_name("x", "exe"), "x.mkv");
        assert_eq!(download_name("../..", "ts"), "Recording.ts");
        assert_eq!(
            numbered("News 2026-10-20 20-00.ts", 2),
            "News 2026-10-20 20-00 (2).ts"
        );
        assert_eq!(numbered("noext", 3), "noext (3)");
        assert_eq!(with_extension("Film.ts", "mkv"), "Film.mkv");
        assert!(path_in(Path::new("C:/r"), "../x.ts").is_none());
        assert!(path_in(Path::new("C:/r"), "CON.ts").is_none());
        assert!(
            part_path(Path::new("C:/r"), "a.ts")
                .is_some_and(|p| p.to_string_lossy().ends_with("a.ts.part"))
        );
        assert_eq!(known_extension(".MOV"), Some("mp4"));
        assert_eq!(known_extension("bat"), None);
    }

    #[test]
    fn unique_names_skip_files_parts_and_database_names() {
        let dir = std::env::temp_dir().join(format!("os-dvr-files-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let base = "Film.mkv";
        let _ = std::fs::write(dir.join("Film.mkv"), b"x");
        let _ = std::fs::write(dir.join("Film (2).mkv.part"), b"x");
        let taken: HashSet<String> = ["Film (3).mkv".to_owned()].into();
        assert_eq!(
            unique_name(&dir, &taken, base).as_deref(),
            Some("Film (4).mkv")
        );
        assert_eq!(
            unique_name(&dir, &HashSet::new(), "Other.ts").as_deref(),
            Some("Other.ts")
        );
        assert!(delete(&dir, "Film.mkv"));
        assert!(!dir.join("Film.mkv").exists());
        assert!(delete(&dir, "Film (2).mkv"));
        assert!(!dir.join("Film (2).mkv.part").exists());
        assert!(free_space(&dir).is_some_and(|b| b > 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn segment_heads() {
        let mut ts = vec![0u8; 376];
        ts[0] = 0x47;
        ts[188] = 0x47;
        assert!(segment_looks_like(Container::MpegTs, &ts));
        assert!(!segment_looks_like(Container::MpegTs, b"<html>"));
        assert!(segment_looks_like(Container::Mp4, b"\0\0\0\x18moof...."));
        assert!(!segment_looks_like(Container::Mp4, b"#EXTM3U\n"));
    }
}

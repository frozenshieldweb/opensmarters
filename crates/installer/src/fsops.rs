// SPDX-License-Identifier: GPL-3.0-or-later
//! File operations the installer relies on, all limited to names it knows:
//!
//! - [`in_use`]: is a program file mapped by a running process?
//! - [`remove_listed`]: delete only the listed files under a folder (never following
//!   links), then the folders they leave empty. Uninstall and rollback use nothing else.
//! - [`clean_stale`]: remove a leftover `<dir>.new` / `<dir>.old` from an interrupted run,
//!   but only if it holds nothing except files we know.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// `ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`.
const SHARING_ERRORS: [i32; 2] = [32, 33];
/// `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: i32 = 5;

/// True if `path` is an existing file that a running program has mapped (a running exe or
/// a loaded DLL can't be opened for writing: sharing violation).
pub fn in_use(path: &Path) -> bool {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => false,
        Err(e) => e
            .raw_os_error()
            .is_some_and(|c| SHARING_ERRORS.contains(&c)),
    }
}

/// True if an I/O error means "something has this file or folder open".
pub fn is_busy(e: &io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|c| SHARING_ERRORS.contains(&c) || c == ACCESS_DENIED)
}

/// What [`remove_listed`] left behind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Removal {
    /// Files deleted.
    pub removed: usize,
    /// Listed files that could not be deleted (in use, access denied).
    pub failed: Vec<PathBuf>,
    /// True if `dir` itself is gone.
    pub dir_removed: bool,
}

/// Deletes `rel_paths` (`/`-separated, already validated) under `dir`: regular files
/// only (a link is removed as a link, never followed). Then removes the folders that held
/// them if they are empty, and `dir` itself if it is empty. Anything not listed stays.
pub fn remove_listed(dir: &Path, rel_paths: &[String]) -> Removal {
    let mut out = Removal::default();
    let mut parents: HashSet<PathBuf> = HashSet::new();
    for rel in rel_paths {
        let Some(path) = path_without_links(dir, rel) else {
            continue;
        };
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() && !meta.file_type().is_symlink() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => out.removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => out.failed.push(path.clone()),
        }
        let mut p = path.parent();
        while let Some(parent) = p {
            if parent == dir || !parent.starts_with(dir) {
                break;
            }
            parents.insert(parent.to_path_buf());
            p = parent.parent();
        }
    }
    // Deepest first.
    let mut parents: Vec<PathBuf> = parents.into_iter().collect();
    parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in parents {
        let _ = std::fs::remove_dir(&p);
    }
    out.dir_removed = match std::fs::remove_dir(dir) {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    };
    out
}

/// `dir/rel`, but only if no folder between `dir` and the file is a link (junction or
/// symlink): a manifest entry must never reach outside `dir` through one.
fn path_without_links(dir: &Path, rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split('/').collect();
    let (last, folders) = parts.split_last()?;
    let mut p = dir.to_path_buf();
    for f in folders {
        p.push(f);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            _ => return None,
        }
    }
    p.push(last);
    Some(p)
}

/// A leftover folder that holds something we don't know.
#[derive(Debug, thiserror::Error)]
#[error("{0} is in the way")]
pub struct InTheWay(pub PathBuf);

/// Removes a stale `<dir>.new` / `<dir>.old` if every file in it is in `known`
/// (lower-case `/`-separated relative paths). Missing is fine.
///
/// # Errors
///
/// [`InTheWay`] if it is a link, holds unknown files, or can't be fully removed.
pub fn clean_stale(path: &Path, known: &HashSet<String>) -> Result<(), InTheWay> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(InTheWay(path.to_path_buf())),
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(InTheWay(path.to_path_buf()));
    }
    let mut files = Vec::new();
    if !list_files(path, path, &mut files) {
        return Err(InTheWay(path.to_path_buf()));
    }
    if files.iter().any(|f| !known.contains(&f.to_lowercase())) {
        return Err(InTheWay(path.to_path_buf()));
    }
    let r = remove_listed(path, &files);
    if r.dir_removed {
        Ok(())
    } else {
        Err(InTheWay(path.to_path_buf()))
    }
}

/// Moves every file under `from` whose relative path (lower-cased) is **not** in `known`
/// to the same place under `to`, never replacing a file. Used at the end of an update so
/// files the user put in the install folder stay with the app. Does nothing if `from`
/// holds a link. Returns the moved relative paths.
pub fn carry_over(from: &Path, to: &Path, known: &HashSet<String>) -> Vec<String> {
    let mut files = Vec::new();
    if !list_files(from, from, &mut files) {
        return Vec::new();
    }
    let mut moved = Vec::new();
    for rel in files {
        if known.contains(&rel.to_lowercase()) {
            continue;
        }
        let src: PathBuf = rel.split('/').fold(from.to_path_buf(), |p, c| p.join(c));
        let dst: PathBuf = rel.split('/').fold(to.to_path_buf(), |p, c| p.join(c));
        if std::fs::symlink_metadata(&dst).is_ok() {
            continue;
        }
        if let Some(parent) = dst.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            continue;
        }
        if std::fs::rename(&src, &dst).is_ok() {
            moved.push(rel);
        }
    }
    moved
}

/// Removes empty folders under `dir` (deepest first) and `dir` itself if it ends up empty.
/// Links are never followed or removed. Returns true if `dir` is gone.
pub fn prune_empty_dirs(dir: &Path) -> bool {
    fn walk(d: &Path) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(m) = std::fs::symlink_metadata(&p)
                && m.is_dir()
                && !m.file_type().is_symlink()
            {
                walk(&p);
                let _ = std::fs::remove_dir(&p);
            }
        }
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
            walk(dir);
            std::fs::remove_dir(dir).is_ok()
        }
        Ok(_) => false,
        Err(_) => true,
    }
}

/// Collects relative file paths; false if a link or an unreadable entry is found.
fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in rd {
        let Ok(entry) = entry else { return false };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        if meta.file_type().is_symlink() {
            return false;
        }
        if meta.is_dir() {
            if !list_files(root, &path, out) {
                return false;
            }
        } else {
            let Ok(rel) = path.strip_prefix(root) else {
                return false;
            };
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            let Some(parts) = parts else { return false };
            out.push(parts.join("/"));
        }
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_listed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("app");
        std::fs::create_dir_all(d.join("licenses")).unwrap();
        std::fs::create_dir_all(d.join("mine")).unwrap();
        for f in ["a.exe", "licenses/x.txt", "mine/keep.txt", "notes.txt"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let r = remove_listed(
            &d,
            &[
                "a.exe".into(),
                "licenses/x.txt".into(),
                "missing.dll".into(),
            ],
        );
        assert_eq!(r.removed, 2);
        assert!(!r.dir_removed);
        assert!(!d.join("licenses").exists());
        assert!(d.join("mine/keep.txt").exists() && d.join("notes.txt").exists());
        let r = remove_listed(&d, &["mine/keep.txt".into(), "notes.txt".into()]);
        assert!(r.dir_removed && !d.exists());
    }

    #[test]
    fn never_follows_a_junction() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), b"keep").unwrap();
        let d = tmp.path().join("app");
        std::fs::create_dir_all(&d).unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(d.join("licenses"))
            .arg(&outside)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        // A manifest naming a file "inside" the junction must not reach through it…
        let r = remove_listed(&d, &["licenses/precious.txt".into(), "licenses".into()]);
        assert!(outside.join("precious.txt").exists(), "{r:?}");
        // …and a stale folder with a link in it is never cleaned.
        let known: HashSet<String> = ["licenses/precious.txt".to_owned()].into();
        assert!(clean_stale(&d, &known).is_err());
        assert!(outside.join("precious.txt").exists());
        let _ = std::fs::remove_dir(d.join("licenses"));
    }

    #[test]
    fn cleans_only_known_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let s = tmp.path().join("opensmarters.new");
        std::fs::create_dir_all(s.join("licenses")).unwrap();
        std::fs::write(s.join("opensmarters.exe"), b"x").unwrap();
        std::fs::write(s.join("licenses/OFL.txt"), b"x").unwrap();
        let known: HashSet<String> =
            ["opensmarters.exe".to_owned(), "licenses/ofl.txt".to_owned()].into();
        std::fs::write(s.join("theirs.txt"), b"x").unwrap();
        assert!(clean_stale(&s, &known).is_err());
        assert!(s.join("opensmarters.exe").exists());
        std::fs::remove_file(s.join("theirs.txt")).unwrap();
        clean_stale(&s, &known).unwrap();
        assert!(!s.exists());
        clean_stale(&s, &known).unwrap();
    }
}

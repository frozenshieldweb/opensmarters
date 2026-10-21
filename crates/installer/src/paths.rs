// SPDX-License-Identifier: GPL-3.0-or-later
//! Install-folder validation (LAUNCH_PROMPT §B7 "Paths").
//!
//! [`validate_install_dir`] accepts a folder only if it is a local, absolute drive path
//! (`X:\…`), not a drive root, not a network path (UNC, `\\?\UNC\`, mapped drives, which
//! canonicalise to UNC), not reached through a junction or symbolic link, not a system
//! folder or inside one, and not one of the user's special folders itself. The result is
//! the canonical path. [`folder_state`] then says whether the folder is missing, empty,
//! ours (a valid `install-manifest.txt`) or someone else's: we only install into the first
//! three.

use std::fmt;
use std::path::{Component, Path, PathBuf, Prefix};

use crate::brand;
use crate::manifest::InstallManifest;

/// Longest install folder path we accept (characters). Leaves room for the files inside
/// without needing long-path support anywhere else (Explorer, shortcuts).
pub const MAX_DIR_CHARS: usize = 200;

/// Folders the installer must never install into.
#[derive(Debug, Clone, Default)]
pub struct Protected {
    /// Never the folder itself nor anything inside it (Windows, Program Files, ProgramData).
    pub tree: Vec<PathBuf>,
    /// Never the folder itself, but a subfolder is fine (`%LOCALAPPDATA%`, Desktop…).
    pub exact: Vec<PathBuf>,
}

/// Why a folder can't be used. `Display` is the plain sentence shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProblem {
    /// Relative, empty, or contains `.`/`..`.
    NotAbsolute,
    /// A network or device path.
    Network,
    /// `C:\` and the like.
    DriveRoot,
    /// Windows, Program Files, ProgramData or inside them; or the app's data/recordings.
    SystemFolder,
    /// A special folder itself (Desktop, Documents, `%LOCALAPPDATA%`…).
    SpecialFolder,
    /// Reached through a junction or symbolic link.
    Link,
    /// Not valid Unicode.
    NotUnicode,
    /// Longer than [`MAX_DIR_CHARS`].
    TooLong,
    /// A file, not a folder.
    NotAFolder,
    /// A non-empty folder that isn't an opensmarters install.
    NotEmpty,
    /// The folder's state couldn't be read.
    Unreadable(String),
}

impl fmt::Display for PathProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::NotAbsolute => "Choose a full folder path, such as C:\\Apps\\opensmarters.",
            Self::Network => "Choose a folder on this PC. Network folders aren't supported.",
            Self::DriveRoot => "Choose a folder, not the top of a drive.",
            Self::SystemFolder => {
                "That folder belongs to Windows or to the app's data. Choose another one."
            }
            Self::SpecialFolder => {
                "Choose a folder inside it instead, so your own files stay separate."
            }
            Self::Link => "That folder is reached through a link. Choose the real folder instead.",
            Self::NotUnicode => "That folder name can't be used. Choose another one.",
            Self::TooLong => "That folder path is too long. Choose a shorter one.",
            Self::NotAFolder => "That is a file, not a folder. Choose a folder.",
            Self::NotEmpty => "That folder already has other files in it. Choose an empty folder.",
            Self::Unreadable(_) => "That folder can't be read. Choose another one.",
        };
        f.write_str(s)
    }
}

/// What is at the install folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderState {
    /// Nothing there yet.
    Missing,
    /// An empty folder.
    Empty,
    /// An opensmarters install (valid marker).
    Owned(InstallManifest),
    /// Something else.
    Foreign,
}

/// Lower-case, `\`-separated, without a trailing `\`: for comparisons only.
fn key(p: &Path) -> String {
    let s = strip_verbatim(p).to_string_lossy().replace('/', "\\");
    s.trim_end_matches('\\').to_lowercase()
}

/// `\\?\C:\x` → `C:\x` (leaves other forms alone).
fn strip_verbatim(p: &Path) -> PathBuf {
    if let Some(Component::Prefix(pre)) = p.components().next()
        && let Prefix::VerbatimDisk(_) = pre.kind()
        && let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix(r"\\?\")
    {
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

/// True if `a` is `b` or inside `b` (component-wise, case-insensitive).
fn is_within(a: &str, b: &str) -> bool {
    a == b || (a.starts_with(b) && a.as_bytes().get(b.len()) == Some(&b'\\'))
}

/// Checks `input` and returns its canonical form (see the module docs).
///
/// # Errors
///
/// The [`PathProblem`] that rules it out.
pub fn validate_install_dir(input: &Path, protected: &Protected) -> Result<PathBuf, PathProblem> {
    let s = input.to_str().ok_or(PathProblem::NotUnicode)?.trim();
    if s.is_empty() {
        return Err(PathProblem::NotAbsolute);
    }
    if s.starts_with(r"\\") || s.starts_with("//") {
        // UNC, \\?\, \\.\ device paths: only the verbatim drive form is fine.
        let verbatim_disk = matches!(
            input.components().next(),
            Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::VerbatimDisk(_))
        );
        if !verbatim_disk {
            return Err(PathProblem::Network);
        }
    }
    let path = strip_verbatim(Path::new(s));
    let mut comps = path.components();
    match comps.next() {
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)) => {}
        Some(Component::Prefix(_)) => return Err(PathProblem::Network),
        _ => return Err(PathProblem::NotAbsolute),
    }
    if comps.next() != Some(Component::RootDir) {
        return Err(PathProblem::NotAbsolute);
    }
    let mut normal = 0;
    for c in comps {
        match c {
            Component::Normal(n) => {
                let n = n.to_str().ok_or(PathProblem::NotUnicode)?;
                if n.ends_with(['.', ' ']) || n.contains(['*', '?', '"', '<', '>', '|', ':']) {
                    return Err(PathProblem::NotUnicode);
                }
                normal += 1;
            }
            _ => return Err(PathProblem::NotAbsolute),
        }
    }
    if normal == 0 {
        return Err(PathProblem::DriveRoot);
    }
    if s.chars().count() > MAX_DIR_CHARS {
        return Err(PathProblem::TooLong);
    }
    // Walk the existing part: no component may be a link (junction or symlink).
    let mut existing = PathBuf::new();
    let mut deepest: Option<PathBuf> = None;
    for c in path.components() {
        existing.push(c.as_os_str());
        if matches!(c, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match std::fs::symlink_metadata(&existing) {
            Ok(m) if m.file_type().is_symlink() => return Err(PathProblem::Link),
            Ok(m) => {
                if !m.is_dir() {
                    return Err(PathProblem::NotAFolder);
                }
                deepest = Some(existing.clone());
            }
            Err(_) => break,
        }
    }
    // The canonical form of the existing part must be the same path (catches subst and
    // mapped drives, which resolve to another drive or to UNC, and 8.3 short names).
    let canonical = match &deepest {
        Some(d) => {
            let c = std::fs::canonicalize(d).map_err(|e| PathProblem::Unreadable(e.to_string()))?;
            if let Some(Component::Prefix(p)) = c.components().next()
                && matches!(p.kind(), Prefix::VerbatimUNC(..) | Prefix::UNC(..))
            {
                return Err(PathProblem::Network);
            }
            let c = strip_verbatim(&c);
            if key(&c) != key(d) {
                return Err(PathProblem::Link);
            }
            match path.strip_prefix(d) {
                // `join("")` would add a trailing separator.
                Ok(rest) if !rest.as_os_str().is_empty() => c.join(rest),
                _ => c,
            }
        }
        None => path.clone(),
    };
    let k = key(&canonical);
    for p in &protected.tree {
        if is_within(&k, &key(p)) {
            return Err(PathProblem::SystemFolder);
        }
    }
    for p in &protected.exact {
        if k == key(p) {
            return Err(PathProblem::SpecialFolder);
        }
    }
    // Normalise the drive letter to upper case for display; never a trailing separator.
    let mut out = canonical
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_owned();
    if out.as_bytes().get(1) == Some(&b':') {
        out.replace_range(0..1, &out[0..1].to_ascii_uppercase());
    }
    Ok(PathBuf::from(out))
}

/// What is at `dir` (no links followed).
///
/// # Errors
///
/// [`PathProblem::NotAFolder`], [`PathProblem::Link`] or [`PathProblem::Unreadable`].
pub fn folder_state(dir: &Path) -> Result<FolderState, PathProblem> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FolderState::Missing),
        Err(e) => return Err(PathProblem::Unreadable(e.to_string())),
    };
    if meta.file_type().is_symlink() {
        return Err(PathProblem::Link);
    }
    if !meta.is_dir() {
        return Err(PathProblem::NotAFolder);
    }
    let mut entries = std::fs::read_dir(dir).map_err(|e| PathProblem::Unreadable(e.to_string()))?;
    if entries.next().is_none() {
        return Ok(FolderState::Empty);
    }
    match InstallManifest::read(dir) {
        Ok(Some(m)) => Ok(FolderState::Owned(m)),
        Ok(None) | Err(_) => Ok(FolderState::Foreign),
    }
}

/// The folder to install into for a user's choice in "Change…": the choice itself if it is
/// missing, empty or ours, otherwise `<choice>\opensmarters` (validated again).
///
/// # Errors
///
/// The [`PathProblem`] of the final candidate.
pub fn resolve_choice(chosen: &Path, protected: &Protected) -> Result<PathBuf, PathProblem> {
    let first = validate_install_dir(chosen, protected);
    match &first {
        Ok(dir) => match folder_state(dir)? {
            FolderState::Missing | FolderState::Empty | FolderState::Owned(_) => return first,
            FolderState::Foreign => {}
        },
        // A special folder (Desktop, Documents…) is fine as a parent.
        Err(PathProblem::SpecialFolder) => {}
        Err(_) => return first,
    }
    let inner = validate_install_dir(&chosen.join(brand::INSTALL_DIR_NAME), protected)?;
    match folder_state(&inner)? {
        FolderState::Missing | FolderState::Empty | FolderState::Owned(_) => Ok(inner),
        FolderState::Foreign => Err(PathProblem::NotEmpty),
    }
}

/// Checks that the install folder can receive an install: validated, and missing, empty or
/// ours.
///
/// # Errors
///
/// The [`PathProblem`].
pub fn check_target(
    dir: &Path,
    protected: &Protected,
) -> Result<(PathBuf, FolderState), PathProblem> {
    let dir = validate_install_dir(dir, protected)?;
    let state = folder_state(&dir)?;
    if state == FolderState::Foreign {
        return Err(PathProblem::NotEmpty);
    }
    Ok((dir, state))
}

/// `<dir>.new` / `<dir>.old`: siblings used by the atomic swap.
pub fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut s = dir.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn protected(tmp: &Path) -> Protected {
        Protected {
            tree: vec![tmp.join("Windows"), tmp.join("Program Files")],
            exact: vec![tmp.join("Desktop"), tmp.join("LocalAppData")],
        }
    }

    #[test]
    fn refuses_network_relative_and_roots() {
        let p = Protected::default();
        for (input, want) in [
            (r"\\server\share\opensmarters", PathProblem::Network),
            (r"\\?\UNC\server\share\x", PathProblem::Network),
            (r"\\.\PhysicalDrive0\x", PathProblem::Network),
            ("//server/share/x", PathProblem::Network),
            (r"relative\dir", PathProblem::NotAbsolute),
            (r"C:relative", PathProblem::NotAbsolute),
            (r"\no-drive", PathProblem::NotAbsolute),
            (r"C:\a\..\Windows", PathProblem::NotAbsolute),
            ("", PathProblem::NotAbsolute),
            (r"C:\", PathProblem::DriveRoot),
            (r"D:\", PathProblem::DriveRoot),
            (r"\\?\C:\", PathProblem::DriveRoot),
            (r"C:\bad|name", PathProblem::NotUnicode),
            (r"C:\trailing.", PathProblem::NotUnicode),
        ] {
            assert_eq!(
                validate_install_dir(Path::new(input), &p),
                Err(want.clone()),
                "{input:?}"
            );
        }
        let long = format!(r"C:\{}", "a".repeat(MAX_DIR_CHARS));
        assert_eq!(
            validate_install_dir(Path::new(&long), &p),
            Err(PathProblem::TooLong)
        );
    }

    #[test]
    fn refuses_system_and_special_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().canonicalize().unwrap();
        let t = strip_verbatim(&t);
        let p = protected(&t);
        for d in ["Windows", "Program Files", "Desktop", "LocalAppData"] {
            std::fs::create_dir_all(t.join(d)).unwrap();
        }
        assert_eq!(
            validate_install_dir(&t.join("Windows"), &p),
            Err(PathProblem::SystemFolder)
        );
        assert_eq!(
            validate_install_dir(&t.join(r"WINDOWS\System32\x"), &p),
            Err(PathProblem::SystemFolder)
        );
        assert_eq!(
            validate_install_dir(&t.join(r"Program Files\opensmarters"), &p),
            Err(PathProblem::SystemFolder)
        );
        assert_eq!(
            validate_install_dir(&t.join("Desktop"), &p),
            Err(PathProblem::SpecialFolder)
        );
        // Inside a special folder is fine; "Windows2" is not inside "Windows".
        assert!(validate_install_dir(&t.join(r"Desktop\opensmarters"), &p).is_ok());
        assert!(validate_install_dir(&t.join(r"Windows2\x"), &p).is_ok());
        // The real system folders, through the real environment's list.
        let real = crate::env::Environment::real_protected();
        if let Some(win) = std::env::var_os("SystemRoot") {
            assert_eq!(
                validate_install_dir(&PathBuf::from(win).join("opensmarters"), &real),
                Err(PathProblem::SystemFolder)
            );
        }
    }

    #[test]
    fn refuses_junctions_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let t = strip_verbatim(&tmp.path().canonicalize().unwrap());
        let real = t.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = t.join("link");
        // A junction needs no privileges (unlike a directory symlink).
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let p = Protected::default();
        assert_eq!(
            validate_install_dir(&link.join("opensmarters"), &p),
            Err(PathProblem::Link)
        );
        assert_eq!(validate_install_dir(&link, &p), Err(PathProblem::Link));
        std::fs::write(t.join("file"), b"x").unwrap();
        assert_eq!(
            validate_install_dir(&t.join(r"file\sub"), &p),
            Err(PathProblem::NotAFolder)
        );
        assert!(validate_install_dir(&real.join("opensmarters"), &p).is_ok());
        assert_eq!(folder_state(&link), Err(PathProblem::Link));
        std::fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn canonicalises_and_reports_state() {
        let tmp = tempfile::tempdir().unwrap();
        let t = strip_verbatim(&tmp.path().canonicalize().unwrap());
        let p = Protected::default();
        let dir = t.join("Apps").join("opensmarters");
        // Different case, forward slashes: same folder, canonical output.
        let upper = PathBuf::from(dir.to_string_lossy().to_uppercase().replace('\\', "/"));
        std::fs::create_dir_all(&dir).unwrap();
        let got = validate_install_dir(&upper, &p).unwrap();
        assert_eq!(key(&got), key(&dir));
        assert!(!got.to_string_lossy().ends_with('\\'), "{}", got.display());
        let with_slash = PathBuf::from(format!("{}\\", dir.display()));
        assert_eq!(validate_install_dir(&with_slash, &p).unwrap(), got);
        assert_eq!(folder_state(&dir).unwrap(), FolderState::Empty);
        assert_eq!(
            folder_state(&t.join("missing")).unwrap(),
            FolderState::Missing
        );
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        assert_eq!(folder_state(&dir).unwrap(), FolderState::Foreign);
        assert_eq!(check_target(&dir, &p), Err(PathProblem::NotEmpty));
        // Choosing a non-empty folder installs into a subfolder.
        assert_eq!(
            key(&resolve_choice(&dir, &p).unwrap()),
            key(&dir.join("opensmarters"))
        );
        let m = InstallManifest {
            product: "opensmarters".into(),
            version: "0.1.0".into(),
            files: vec![],
        };
        std::fs::write(dir.join(brand::INSTALL_MANIFEST), m.to_text()).unwrap();
        assert_eq!(folder_state(&dir).unwrap(), FolderState::Owned(m));
        assert_eq!(
            sibling(&dir, ".new"),
            t.join("Apps").join("opensmarters.new")
        );
    }
}

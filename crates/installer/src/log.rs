// SPDX-License-Identifier: GPL-3.0-or-later
//! The setup log: plain text, `%TEMP%\opensmarters-setup.log` (or `--log <file>`), paths
//! shown as they are. The installer handles no secrets (it never reads credential values),
//! so there is nothing to redact.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The log is trimmed back to nothing when it grows past this at start-up.
const MAX_LOG_BYTES: u64 = 1 << 20;

/// Starts logging to `path` (appending). Returns the path on success; logging is optional,
/// so a failure only means no log.
pub fn init(path: &Path) -> Option<PathBuf> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::remove_file(path);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    let _ = writeln!(file);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok()?;
    tracing::info!(
        version = crate::brand::VERSION,
        args = ?std::env::args().skip(1).collect::<Vec<_>>(),
        "opensmarters-setup starts"
    );
    Some(path.to_path_buf())
}

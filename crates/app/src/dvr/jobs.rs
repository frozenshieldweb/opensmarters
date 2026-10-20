// SPDX-License-Identifier: GPL-3.0-or-later
//! Pieces shared by download and recording jobs: the `.part` writer, the free-space
//! guard, speed smoothing, retry delays and control handling.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use opensmarters_core::Error as CoreError;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::sync::watch;

use super::engine::Control;
use crate::error::AppError;

/// Retry delays (seconds) after a lost connection; the last one repeats.
pub const BACKOFF: [u64; 5] = [2, 5, 10, 30, 30];

/// The message for a body that isn't video.
pub const NOT_VIDEO: &str = "The provider sent something that isn't a video, so nothing was saved.";

/// The retry delay for attempt `n` (0-based).
pub fn backoff(n: u32) -> Duration {
    let i = (n as usize).min(BACKOFF.len() - 1);
    Duration::from_secs(BACKOFF[i])
}

/// Waits for the next control message (a dropped sender counts as an interrupt).
pub async fn next_control(rx: &mut watch::Receiver<Control>) -> Control {
    loop {
        if rx.changed().await.is_err() {
            return Control::Interrupt;
        }
        let c = *rx.borrow_and_update();
        if c != Control::Run {
            return c;
        }
    }
}

/// Sleeps `d` unless a control message arrives first (returned).
pub async fn sleep_or_control(d: Duration, rx: &mut watch::Receiver<Control>) -> Option<Control> {
    tokio::select! {
        () = tokio::time::sleep(d) => None,
        c = next_control(rx) => Some(c),
    }
}

/// Errors worth retrying (the provider or the network may come back).
pub fn is_transient(e: &CoreError) -> bool {
    match e {
        CoreError::Timeout | CoreError::Network { .. } => true,
        CoreError::Http { status } => *status >= 500 || matches!(status, 408 | 429),
        _ => false,
    }
}

/// A plain-language message for a failed request.
pub fn failure_message(e: &CoreError) -> String {
    match e {
        CoreError::Http { status: 403 } => {
            "The provider refused it (HTTP 403). Its connection limit may be reached: try \
             again later."
                .to_owned()
        }
        CoreError::Http { status: 404 } => {
            "The provider doesn't have this any more (HTTP 404).".to_owned()
        }
        CoreError::InsecureRedirect => AppError::Provider(e.clone()).user_message(),
        CoreError::RangeMismatch | CoreError::RangeNotSatisfiable { .. } => {
            "The provider sent a different part of the file than asked for.".to_owned()
        }
        other => AppError::Provider(other.clone()).user_message(),
    }
}

/// "Paused: less than 2 GB is free…".
pub fn low_disk_message(keep_free: u64, paused: bool) -> String {
    let gb = keep_free / (1024 * 1024 * 1024);
    if paused {
        format!(
            "Paused: less than {gb} GB is free on the disk. Free some space (or change the \
             limit in Settings), then resume."
        )
    } else {
        format!("Stopped early: less than {gb} GB was free on the disk.")
    }
}

/// True when the disk holding `dir` has at least `keep_free` bytes free (unknown = yes).
pub async fn disk_ok(dir: &Path, keep_free: u64) -> bool {
    if keep_free == 0 {
        return true;
    }
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || super::files::free_space(&dir))
        .await
        .ok()
        .flatten()
        .is_none_or(|free| free >= keep_free)
}

/// The file a job writes (`<name>.part`): created new (never overwriting) or reopened
/// to append after a pause.
#[derive(Debug)]
pub struct PartWriter {
    file: BufWriter<tokio::fs::File>,
    path: PathBuf,
    len: u64,
}

impl PartWriter {
    /// Opens `path`: a new file when `fresh` (fails if it exists), else the existing one,
    /// positioned at its end.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub async fn open(path: &Path, fresh: bool) -> std::io::Result<Self> {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true);
        if fresh {
            opts.create_new(true);
        }
        let mut file = opts.open(path).await?;
        let len = file.seek(std::io::SeekFrom::End(0)).await?;
        Ok(Self {
            file: BufWriter::with_capacity(256 * 1024, file),
            path: path.to_path_buf(),
            len,
        })
    }

    /// Bytes in the file (written so far, buffered included).
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True while nothing was written.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The `.part` path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `data`.
    ///
    /// # Errors
    ///
    /// I/O errors (a full disk).
    pub async fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.file.write_all(data).await?;
        self.len += data.len() as u64;
        Ok(())
    }

    /// Empties the file (the server sent the whole file again).
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub async fn restart(&mut self) -> std::io::Result<()> {
        self.file.flush().await?;
        self.file.get_mut().set_len(0).await?;
        self.file
            .get_mut()
            .seek(std::io::SeekFrom::Start(0))
            .await?;
        self.len = 0;
        Ok(())
    }

    /// Flushes to the OS and the disk.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub async fn sync(&mut self) -> std::io::Result<()> {
        self.file.flush().await?;
        self.file.get_ref().sync_data().await
    }

    /// Flushes and closes the file (needed before renaming it on Windows).
    pub async fn close(mut self) -> u64 {
        if let Err(e) = self.sync().await {
            tracing::warn!(error = %e, "couldn't flush a recording file");
        }
        self.len
    }
}

/// Smoothed transfer speed.
#[derive(Debug)]
pub struct Meter {
    at: Instant,
    bytes: u64,
    rate: f64,
}

impl Meter {
    /// Starts measuring from `bytes`.
    pub fn new(bytes: u64) -> Self {
        Self {
            at: Instant::now(),
            bytes,
            rate: 0.0,
        }
    }

    /// Updates with the current byte count; returns bytes per second.
    pub fn update(&mut self, bytes: u64) -> f64 {
        let dt = self.at.elapsed().as_secs_f64();
        if dt >= 0.5 {
            let inst = bytes.saturating_sub(self.bytes) as f64 / dt;
            self.rate = if self.rate == 0.0 {
                inst
            } else {
                self.rate * 0.7 + inst * 0.3
            };
            self.at = Instant::now();
            self.bytes = bytes;
        }
        self.rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_and_messages() {
        assert_eq!(backoff(0), Duration::from_secs(2));
        assert_eq!(backoff(99), Duration::from_secs(30));
        assert!(is_transient(&CoreError::Timeout));
        assert!(is_transient(&CoreError::Http { status: 503 }));
        assert!(!is_transient(&CoreError::Http { status: 404 }));
        assert!(!is_transient(&CoreError::RangeMismatch));
        assert!(low_disk_message(2 * 1024 * 1024 * 1024, true).contains("less than 2 GB"));
        assert!(!failure_message(&CoreError::Http { status: 403 }).contains("http"));
    }

    #[tokio::test]
    async fn part_writer_appends_and_restarts() {
        let dir = std::env::temp_dir().join(format!("os-dvr-part-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("a.ts.part");
        let _ = std::fs::remove_file(&p);
        let mut w = PartWriter::open(&p, true)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(PartWriter::open(&p, true).await.is_err(), "never overwrite");
        w.write(b"hello").await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(w.close().await, 5);
        let mut w = PartWriter::open(&p, false)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(w.len(), 5);
        w.write(b" world").await.unwrap_or_else(|e| panic!("{e}"));
        w.sync().await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(std::fs::read(&p).unwrap_or_default(), b"hello world");
        w.restart().await.unwrap_or_else(|e| panic!("{e}"));
        w.write(b"x").await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(w.close().await, 1);
        assert_eq!(std::fs::read(&p).unwrap_or_default(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Logging: `tracing` to stderr (debug builds or `--log-stderr`) and to
//! `<data>/logs/opensmarters.log` (rotated at [`MAX_LOG_BYTES`], one old file kept).
//! **Every line goes through `RedactingWriter`** with the app-wide `Redactor`, which knows
//! the passwords and M3U URLs of opened profiles and structurally scrubs credentials in
//! URLs. mpv's own log lines are redacted again before they are logged (see
//! [`crate::playback`]).
//!
//! Filter: `OPENSMARTERS_LOG` (`tracing_subscriber` "targets" syntax, e.g.
//! `debug,mpv=info`), default `info` with noisy dependencies at `warn`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use opensmarters_core::Redactor;
use opensmarters_core::redact::RedactingWriter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Rotate the log file above this size.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Log file name.
pub const LOG_FILE: &str = "opensmarters.log";

struct Rotating {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

impl Rotating {
    fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            file: Some(file),
            size,
        })
    }

    fn rotate(&mut self) {
        self.file = None;
        let old = self.path.with_extension("1.log");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&self.path, &old);
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        self.size = 0;
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.size + buf.len() as u64 > MAX_LOG_BYTES {
            self.rotate();
        }
        if let Some(f) = &mut self.file {
            f.write_all(buf)?;
            self.size += buf.len() as u64;
        }
        Ok(())
    }
}

/// A shared handle to the rotating file.
#[derive(Clone)]
struct FileSink(Arc<Mutex<Rotating>>);

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = &mut self.0.lock().unwrap_or_else(PoisonError::into_inner).file {
            f.flush()?;
        }
        Ok(())
    }
}

struct RedactedFile {
    sink: FileSink,
    redactor: Redactor,
}

impl<'a> MakeWriter<'a> for RedactedFile {
    type Writer = RedactingWriter<FileSink>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(self.sink.clone(), self.redactor.clone())
    }
}

struct RedactedStderr {
    redactor: Redactor,
}

impl<'a> MakeWriter<'a> for RedactedStderr {
    type Writer = RedactingWriter<io::Stderr>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter::new(io::stderr(), self.redactor.clone())
    }
}

fn filter() -> Targets {
    let default = "info,mpv=info,fake_panel=warn,hyper=warn,hyper_util=warn,reqwest=warn,rustls=warn,eframe=warn,egui_glow=warn,winit=warn";
    std::env::var("OPENSMARTERS_LOG")
        .ok()
        .and_then(|s| s.parse::<Targets>().ok())
        .unwrap_or_else(|| default.parse().unwrap_or_default())
}

/// Installs the global subscriber. Returns the log file path, if any.
///
/// # Errors
///
/// A subscriber was already installed.
pub fn init(
    redactor: &Redactor,
    log_dir: Option<&Path>,
    stderr: bool,
) -> anyhow::Result<Option<PathBuf>> {
    let mut path_out = None;
    let file_layer = match log_dir {
        Some(dir) => match Rotating::open(dir.join(LOG_FILE)) {
            Ok(r) => {
                path_out = Some(r.path.clone());
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(RedactedFile {
                            sink: FileSink(Arc::new(Mutex::new(r))),
                            redactor: redactor.clone(),
                        })
                        .with_filter(filter()),
                )
            }
            Err(e) => {
                eprintln!("opensmarters: cannot open the log file: {e}");
                None
            }
        },
        None => None,
    };
    let stderr_layer = stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(RedactedStderr {
                redactor: redactor.clone(),
            })
            .with_filter(filter())
    });
    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init()?;
    Ok(path_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotating_file_rotates() {
        let dir = std::env::temp_dir().join(format!("os-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Rotating::open(dir.join(LOG_FILE)).unwrap_or_else(|e| panic!("{e}"));
        let line = vec![b'x'; 1024 * 1024];
        for _ in 0..6 {
            r.write_all(&line).unwrap_or_else(|e| panic!("{e}"));
        }
        assert!(dir.join("opensmarters.1.log").exists());
        assert!(r.size <= MAX_LOG_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_sink_redacts_credentials() {
        let dir = std::env::temp_dir().join(format!("os-log-red-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let r = Rotating::open(dir.join(LOG_FILE)).unwrap_or_else(|e| panic!("{e}"));
        let redactor = Redactor::new();
        redactor.add_secret("hunter22");
        let mk = RedactedFile {
            sink: FileSink(Arc::new(Mutex::new(r))),
            redactor,
        };
        {
            let mut w = mk.make_writer();
            let _ = writeln!(w, "GET http://h:80/live/bob/hunter22/1.ts and hunter22");
        }
        let text = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap_or_default();
        assert!(!text.contains("hunter22"), "{text}");
        assert!(text.contains("/live/***/***/1.ts"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Files the app saved itself: playback through `PlayableUrl::from_owned_media` (on a
//! player **without** `allow_local_files`), `stream-record` and `dump-cache` into a
//! `MediaOutput`. Headless (`ao=null`, no video output); **skips** (passes with a note on
//! stderr) when libmpv or the generated media is missing:
//!
//! ```text
//! powershell -ExecutionPolicy Bypass -File tools\fetch-libmpv.ps1
//! powershell -ExecutionPolicy Bypass -File tools\gen-test-media.ps1
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use opensmarters_player::{
    EndReason, Error, LoadOptions, MediaOutput, Op, PlayableUrl, Player, PlayerConfig, PlayerEvent,
    PlayerState, sniff_container,
};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn media(name: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/generated/player")
        .join(name);
    if p.is_file() {
        Some(std::fs::canonicalize(p).unwrap())
    } else {
        eprintln!(
            "SKIP: {} missing; run tools/gen-test-media.ps1",
            p.display()
        );
        None
    }
}

fn player() -> Option<Player> {
    player_with(PlayerConfig::default())
}

fn player_with(config: PlayerConfig) -> Option<Player> {
    match Player::new(config.audio_output(false).video_output(false)) {
        Ok(p) => Some(p),
        Err(Error::Load(e)) => {
            eprintln!("SKIP: {e}");
            None
        }
        Err(e) => panic!("Player::new failed: {e}"),
    }
}

fn wait_for(
    p: &Player,
    timeout: Duration,
    mut pred: impl FnMut(Option<&PlayerEvent>, &PlayerState) -> bool,
) -> (bool, Vec<PlayerEvent>) {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let ev = p.wait_event(Duration::from_millis(50));
        // Synthetic local paths/URLs only: safe to print for diagnosis.
        if let Some(PlayerEvent::Log(l)) = &ev
            && (l.level <= opensmarters_player::LogLevel::Warn
                || std::env::var_os("OWNED_MEDIA_DEBUG").is_some())
        {
            eprintln!("mpv[{}] {}", l.prefix, l.text);
        }
        let hit = pred(ev.as_ref(), &p.state());
        if let Some(ev) = ev {
            seen.push(ev);
        }
        if hit {
            return (true, seen);
        }
    }
    (false, seen)
}

/// A scratch folder (removed on drop).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "opensmarters-owned-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(std::fs::canonicalize(p).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn head(path: &Path) -> Vec<u8> {
    let mut b = std::fs::read(path).unwrap();
    b.truncate(4096);
    b
}

#[test]
fn owned_media_plays_without_allow_local_files_and_dev_files_do_not() {
    let _g = serial();
    let (Some(src), Some(p)) = (media("test.ts"), player()) else {
        return;
    };
    let root = Scratch::new("play");
    let copy = root.0.join("Recording.ts");
    std::fs::copy(&src, &copy).unwrap();

    // The development-only constructor stays refused on a production player.
    let dev = PlayableUrl::from_local_file(&copy).unwrap();
    assert!(matches!(
        p.load(&dev, &LoadOptions::default()),
        Err(Error::LocalFilesDisabled)
    ));

    let owned = PlayableUrl::from_owned_media(&root.0, &copy).unwrap();
    p.load(
        &owned,
        &LoadOptions {
            start: Some(17.0),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(15), |ev, _| {
        matches!(ev, Some(PlayerEvent::EndFile(_)))
    });
    assert!(ok, "no end of file; events: {events:#?}");
    assert!(
        events.contains(&PlayerEvent::FileLoaded),
        "the owned file did not open: {events:#?}"
    );
    assert!(
        events.contains(&PlayerEvent::EndFile(EndReason::Eof)),
        "expected a normal end: {events:#?}"
    );
}

#[test]
fn stream_record_writes_what_is_played() {
    let _g = serial();
    let (Some(src), Some(p)) = (media("test.ts"), player()) else {
        return;
    };
    let root = Scratch::new("record");
    let copy = root.0.join("Source.ts");
    std::fs::copy(&src, &copy).unwrap();
    let out = MediaOutput::new(&root.0, "Recorded.ts").unwrap();
    let out_path = out.path().to_path_buf();

    p.pause().unwrap();
    p.set_stream_record(Some(&out)).unwrap();
    p.load(
        &PlayableUrl::from_owned_media(&root.0, &copy).unwrap(),
        &LoadOptions::default(),
    )
    .unwrap();
    p.set_speed(4.0).unwrap();
    p.play().unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(15), |ev, _| {
        matches!(ev, Some(PlayerEvent::EndFile(_)))
    });
    assert!(ok, "no end of file; events: {events:#?}");
    p.set_stream_record(None).unwrap();
    // Give mpv a moment to finalise the file.
    let _ = wait_for(&p, Duration::from_millis(500), |_, _| false);
    let len = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    assert!(
        len > 10_000,
        "recording too small ({len} bytes); events: {events:#?}"
    );
    assert!(sniff_container(&head(&out_path)).is_some());
    // Outputs are never overwritten.
    assert!(MediaOutput::new(&root.0, "Recorded.ts").is_err());
}

/// Serves one file over HTTP (any path), so mpv caches it like a network stream.
fn serve(file: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let file = file.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let body = std::fs::read(&file).unwrap();
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            });
        }
    });
    format!("http://{addr}/live/stream.ts")
}

#[test]
fn dump_cache_saves_the_buffer_and_reports_done() {
    let _g = serial();
    let (Some(src), Some(p)) = (
        media("test.ts"),
        player_with(PlayerConfig::default().seekable_cache(true)),
    ) else {
        return;
    };
    let root = Scratch::new("dump");
    let url = PlayableUrl::parse(&serve(src)).unwrap();
    if std::env::var_os("OWNED_MEDIA_DEBUG").is_some() {
        p.set_log_level(opensmarters_player::LogLevel::Debug)
            .unwrap();
    }
    p.load(&url, &LoadOptions::default()).unwrap();
    p.pause().unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(15), |_, st| {
        st.file_loaded && st.demuxer_cache_duration.is_some_and(|d| d > 1.0)
    });
    assert!(ok, "the stream did not buffer; events: {events:#?}");

    // Reversed and non-finite ranges are refused before reaching mpv.
    let out = MediaOutput::new(&root.0, "Clip.ts").unwrap();
    assert!(p.dump_cache(5.0, 1.0, &out).is_err());
    assert!(p.dump_cache(f64::NAN, 1.0, &out).is_err());

    // "The last few seconds" of what was received, like the app's "save the last N
    // minutes".
    let st = p.state();
    let end = st.time_pos.unwrap_or(0.0) + st.demuxer_cache_duration.unwrap_or(0.0);
    p.dump_cache((end - 10.0).max(0.0), end, &out).unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(15), |ev, _| {
        matches!(
            ev,
            Some(
                PlayerEvent::CommandDone { op: Op::DumpCache }
                    | PlayerEvent::CommandFailed {
                        op: Op::DumpCache,
                        ..
                    }
            )
        )
    });
    assert!(ok, "no dump-cache reply; events: {events:#?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PlayerEvent::CommandDone { op: Op::DumpCache })),
        "dump-cache failed: {events:#?}"
    );
    let len = std::fs::metadata(out.path()).map(|m| m.len()).unwrap_or(0);
    assert!(len > 10_000, "clip too small ({len} bytes)");
    assert!(sniff_container(&head(out.path())).is_some());
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests for per-stream tuning ([`StreamTuning`]), runtime setters and the
//! network options, against the real libmpv (headless). They **skip** (pass with a note
//! on stderr) when libmpv or the generated media is missing:
//!
//! ```text
//! powershell -ExecutionPolicy Bypass -File tools\fetch-libmpv.ps1
//! powershell -ExecutionPolicy Bypass -File tools\gen-test-media.ps1
//! ```
//!
//! The "live" server below streams a TS file without `Content-Length` or `Range`
//! support, like an IPTV panel's live channel, so mpv (libcurl) sees a non-seekable
//! stream.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use opensmarters_player::{
    EndReason, Error, Hwdec, LoadOptions, Op, PlayableUrl, Player, PlayerConfig, PlayerEvent,
    PlayerState, StreamTuning,
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
        Some(p)
    } else {
        eprintln!(
            "SKIP: {} missing; run tools/gen-test-media.ps1",
            p.display()
        );
        None
    }
}

fn player(config: PlayerConfig) -> Option<Player> {
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
    if pred(None, &p.state()) {
        return (true, seen);
    }
    while Instant::now() < deadline {
        let ev = p.wait_event(Duration::from_millis(50));
        let st = p.state();
        let hit = pred(ev.as_ref(), &st);
        if let Some(ev) = ev {
            seen.push(ev);
        }
        if hit {
            return (true, seen);
        }
    }
    (false, seen)
}

fn tuning_failures(events: &[PlayerEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            PlayerEvent::CommandFailed { op, error } if matches!(op, Op::Tuning | Op::Load) => {
                Some(format!("{op:?}: {error}"))
            }
            _ => None,
        })
        .collect()
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "opensmarters-tuning-{tag}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What the live server does after sending its share of the file.
#[derive(Clone, Copy)]
enum After {
    /// Close the connection (the stream ends).
    Close,
    /// Keep the connection open and send nothing (a stalled provider).
    Hang,
}

/// Serves `file` like a live channel: no `Content-Length`, no `Range`. Sends the first
/// `fraction` of the file at once, then closes or hangs.
fn live_server(file: PathBuf, fraction: f64, after: After) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let file = file.clone();
            std::thread::spawn(move || {
                let _ = serve_live(stream, &file, fraction, after);
            });
        }
    });
    port
}

fn serve_live(stream: TcpStream, file: &Path, fraction: f64, after: After) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
    }
    let body = std::fs::read(file)?;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let n = ((body.len() as f64) * fraction) as usize;
    let mut out = stream;
    out.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: video/mp2t\r\nConnection: close\r\n\r\n")?;
    out.write_all(&body[..n.min(body.len())])?;
    out.flush()?;
    if let After::Hang = after {
        std::thread::sleep(Duration::from_secs(60));
    }
    Ok(())
}

fn live_tuning(disk: bool) -> StreamTuning {
    StreamTuning {
        start_buffer: Duration::from_millis(500),
        forward_bytes: 64 << 20,
        back_bytes: 64 << 20,
        seekable_cache: true,
        disk_cache: disk,
    }
}

#[test]
fn network_and_cache_options_are_known_to_this_libmpv() {
    let _serial = serial();
    let dir = scratch_dir("known");
    let Some(p) = player(PlayerConfig::default().cache_dir(dir.clone())) else {
        return;
    };
    assert!(
        p.skipped_options().is_empty(),
        "options this libmpv does not know: {:?}",
        p.skipped_options()
    );
    drop(p);
    // A relative cache folder is refused before mpv is touched.
    assert!(matches!(
        Player::new(PlayerConfig::default().cache_dir(PathBuf::from("rel/dir"))),
        Err(Error::InvalidArgument(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seekable_cache_lets_a_live_stream_go_back_and_stays_on_disk_until_closed() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let port = live_server(ts, 1.0, After::Close);
    let url = PlayableUrl::parse(&format!("http://127.0.0.1:{port}/live/1.ts")).unwrap();
    let dir = scratch_dir("disk");
    let Some(p) = player(PlayerConfig::default().cache_dir(dir.clone())) else {
        return;
    };

    // Control: without tuning, a live stream cannot be seeked.
    p.load(&url, &LoadOptions::default()).unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(10), |_, s| {
        s.time_pos.is_some_and(|t| t > 1.0)
    });
    assert!(ok, "untuned live stream did not play: {events:?}");
    assert!(!p.state().seekable, "a live TS should not be seekable");
    p.stop().unwrap();
    let _ = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);

    // Tuned: seekable cache on disk.
    p.load_with(&url, &LoadOptions::default(), &live_tuning(true))
        .unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(10), |_, s| {
        s.time_pos.is_some_and(|t| t > 3.0)
    });
    assert!(ok, "tuned live stream did not play: {events:?}");
    assert!(tuning_failures(&events).is_empty(), "{events:?}");
    let st = p.state();
    assert!(st.seekable, "force-seekable did not apply");
    let range = st
        .cache
        .range_at(st.time_pos.unwrap_or(0.0))
        .expect("a seekable range");
    eprintln!("cache: {:?}", st.cache);
    assert!(range.start <= 0.5, "range {range:?}");
    assert!(
        st.cache.cache_end.is_some_and(|e| e > 10.0),
        "{:?}",
        st.cache
    );
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "expected one disk cache file in {dir:?}");
    assert!(
        st.cache.file_cache_bytes.is_some_and(|b| b > 0),
        "{:?}",
        st.cache
    );

    // Seek back inside what was received.
    p.seek_absolute(1.0).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        s.time_pos.is_some_and(|t| t < 2.5)
    });
    assert!(ok, "seek back failed: {:?}", p.state().time_pos);

    // The cache file goes away with the stream (`demuxer-cache-unlink-files=whendone`).
    p.stop().unwrap();
    let _ = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && std::fs::read_dir(&dir).unwrap().next().is_some() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "cache file left behind"
    );
    drop(p);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_cache_folder_the_cache_stays_in_memory() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let port = live_server(ts, 1.0, After::Close);
    let url = PlayableUrl::parse(&format!("http://127.0.0.1:{port}/live/2.ts")).unwrap();
    let Some(p) = player(PlayerConfig::default()) else {
        return;
    };
    p.load_with(&url, &LoadOptions::default(), &live_tuning(true))
        .unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(10), |_, s| {
        s.time_pos.is_some_and(|t| t > 2.0)
    });
    assert!(ok, "did not play: {events:?}");
    assert!(tuning_failures(&events).is_empty(), "{events:?}");
    let st = p.state();
    assert!(st.seekable);
    assert!(
        st.cache.file_cache_bytes.unwrap_or(0) == 0,
        "{:?}",
        st.cache
    );
}

#[test]
fn tuning_and_hwdec_apply_while_playing_and_while_idle() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let dir = scratch_dir("runtime");
    let Some(p) = player(
        PlayerConfig::default()
            .allow_local_files(true)
            .cache_dir(dir.clone()),
    ) else {
        return;
    };
    // Idle: changes the defaults.
    p.set_tuning(&StreamTuning::default()).unwrap();
    p.set_hwdec(Hwdec::Disabled).unwrap();
    let url = PlayableUrl::from_local_file(&ts).unwrap();
    p.load_with(
        &url,
        &LoadOptions {
            start: Some(2.0),
            ..LoadOptions::default()
        },
        &StreamTuning {
            start_buffer: Duration::from_secs(2),
            ..StreamTuning::default()
        },
    )
    .unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(10), |_, s| {
        s.time_pos.is_some_and(|t| t > 2.5)
    });
    assert!(ok, "did not play: {events:?}");
    let mut all = events;
    // Playing: bigger caches, a longer buffer after stalls, hardware decoding back on.
    p.set_tuning(&StreamTuning {
        start_buffer: Duration::from_secs(6),
        forward_bytes: 512 << 20,
        back_bytes: 256 << 20,
        seekable_cache: true,
        disk_cache: true,
    })
    .unwrap();
    p.set_hwdec(Hwdec::AutoSafe).unwrap();
    let (_, events) = wait_for(&p, Duration::from_millis(1500), |_, _| false);
    all.extend(events);
    assert!(
        tuning_failures(&all).is_empty(),
        "mpv refused: {:?}",
        tuning_failures(&all)
    );
    assert!(p.state().time_pos.is_some_and(|t| t > 2.5));
    drop(p);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Documents this build's libcurl stream: it has no read timeout (`network-timeout` does
/// not apply to it), so a provider that stops sending leaves mpv waiting. The app detects
/// such stalls itself (no progress for a few seconds) and reconnects. If this test starts
/// failing, libmpv gained a read timeout: update `docs/LIBMPV.md` and the app's stall
/// detection.
#[test]
fn a_stalled_live_stream_never_ends_by_itself() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let port = live_server(ts, 0.25, After::Hang);
    let url = PlayableUrl::parse(&format!("http://127.0.0.1:{port}/live/3.ts")).unwrap();
    let Some(p) = player(PlayerConfig::default().network_timeout(Duration::from_secs(2))) else {
        return;
    };
    p.load_with(&url, &LoadOptions::default(), &live_tuning(false))
        .unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(10), |_, s| {
        s.time_pos.is_some_and(|t| t > 1.0)
    });
    assert!(ok, "did not play: {events:?}");
    // About 5 s of media arrived; wait well past it and past the 2 s network timeout.
    let (ended, events) = wait_for(&p, Duration::from_secs(9), |e, _| {
        matches!(e, Some(PlayerEvent::EndFile(_)))
    });
    let st = p.state();
    eprintln!(
        "after the stall: pos {:?}, paused-for-cache {}, ended {ended}",
        st.time_pos, st.paused_for_cache
    );
    assert!(!ended, "the stream ended: {events:?}");
    assert!(st.time_pos.is_some_and(|t| t < 8.0), "{:?}", st.time_pos);
    // Stop still works on a hung connection.
    p.stop().unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |e, s| {
        s.idle || matches!(e, Some(PlayerEvent::EndFile(EndReason::Stop)))
    });
    assert!(ok, "stop did not close a hung stream");
}

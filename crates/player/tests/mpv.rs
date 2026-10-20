// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests against the real libmpv (headless: `ao=null`, no video
//! output). They **skip** (pass with a note on stderr) when libmpv or the
//! generated media is missing:
//!
//! ```text
//! powershell -ExecutionPolicy Bypass -File tools\fetch-libmpv.ps1
//! powershell -ExecutionPolicy Bypass -File tools\gen-test-media.ps1
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opensmarters_player::{
    Aspect, EndReason, Error, LoadOptions, LogLevel, PlayableUrl, Player, PlayerConfig,
    PlayerEvent, PlayerState, Referrer, TrackKind, TrackSelection, UrlPolicy, UserAgent, VideoFit,
};

fn media_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/generated/player")
}

fn media(name: &str) -> Option<PathBuf> {
    let p = media_dir().join(name);
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

/// libmpv routes ffmpeg's (process-global) log callback to only one mpv
/// core, so tests that look at ffmpeg log lines must not overlap with other
/// cores. Every test holds this lock for its whole duration.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A headless player, or `None` (skip) if libmpv is not installed.
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

/// Waits until `pred` holds for an event or the state, collecting events.
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

fn local(path: &Path) -> PlayableUrl {
    PlayableUrl::from_local_file(path).unwrap()
}

// ---------------------------------------------------------------------------
// Tiny static HTTP/1.0 server that records request headers.

#[derive(Debug, Clone)]
struct Request {
    path: String,
    user_agent: Option<String>,
    referer: Option<String>,
}

struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    fn start(root: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reqs = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let root = root.clone();
                let reqs = Arc::clone(&reqs);
                std::thread::spawn(move || {
                    let _ = serve(stream, &root, &reqs);
                });
            }
        });
        Self { port, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, root: &Path, reqs: &Mutex<Vec<Request>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let (mut ua, mut referer) = (None, None);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "user-agent" => ua = Some(v.trim().to_owned()),
                "referer" => referer = Some(v.trim().to_owned()),
                _ => {}
            }
        }
    }
    reqs.lock().unwrap().push(Request {
        path: path.clone(),
        user_agent: ua,
        referer,
    });
    let rel = path.trim_start_matches('/').split('?').next().unwrap_or("");
    let file = root.join(rel);
    let mut out = stream;
    // `/redirect/<target>` answers 302 to `<target>` (percent-decoding not
    // needed for the targets the tests use).
    if let Some(target) = rel.strip_prefix("redirect/") {
        write!(
            out,
            "HTTP/1.0 302 Found
Location: {target}
Content-Length: 0
Connection: close

"
        )?;
        return Ok(());
    }
    if rel.contains("..") || !file.is_file() {
        out.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    let mut body = Vec::new();
    std::fs::File::open(&file)?.read_to_end(&mut body)?;
    let ctype = if rel.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else {
        "video/mp2t"
    };
    write!(
        out,
        "HTTP/1.0 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    out.write_all(&body)?;
    Ok(())
}

// ---------------------------------------------------------------------------

#[test]
fn reports_versions_and_applies_all_hardening_options() {
    let _serial = serial();
    let Some(p) = player(PlayerConfig::default()) else {
        return;
    };
    eprintln!(
        "mpv: {} | ffmpeg: {} | api {:?} | {}",
        p.mpv_version(),
        p.ffmpeg_version(),
        p.client_api_version(),
        p.libmpv_path().display()
    );
    assert!(p.mpv_version().starts_with("mpv "), "{}", p.mpv_version());
    assert_eq!(p.client_api_version().0, 2);
    assert!(
        p.skipped_options().is_empty(),
        "best-effort options not applied by this libmpv: {:?}",
        p.skipped_options()
    );
    let st = p.state();
    assert!(st.idle);
    assert!(!st.file_loaded);
}

#[test]
fn local_ts_resumes_from_start_option_and_ends_with_eof() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let Some(p) = player(PlayerConfig::default().allow_local_files(true)) else {
        return;
    };
    p.load(
        &local(&ts),
        &LoadOptions {
            start: Some(17.0),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(10), |e, _| {
        matches!(e, Some(PlayerEvent::FileLoaded))
    });
    assert!(ok, "FileLoaded not seen");
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        s.time_pos.is_some_and(|t| t >= 16.5)
    });
    assert!(ok, "start=17 not honoured: {:?}", p.state().time_pos);
    let d = p.state().duration.unwrap_or(0.0);
    assert!((19.0..21.5).contains(&d), "duration {d}");
    let (ok, _) = wait_for(&p, Duration::from_secs(15), |e, _| {
        matches!(e, Some(PlayerEvent::EndFile(EndReason::Eof)))
    });
    assert!(ok, "EndFile(Eof) not seen; state {:?}", p.state().last_end);
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    assert!(ok, "core did not go idle after EOF");
    assert_eq!(p.state().last_end, Some(EndReason::Eof));
}

#[test]
fn mkv_lists_and_switches_audio_and_subtitle_tracks() {
    let _serial = serial();
    let Some(mkv) = media("test-tracks.mkv") else {
        return;
    };
    let Some(p) = player(PlayerConfig::default().allow_local_files(true)) else {
        return;
    };
    p.load(&local(&mkv), &LoadOptions::default()).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(10), |_, s| {
        s.file_loaded && s.audio_tracks().count() == 2
    });
    assert!(ok, "tracks: {:?}", p.state().tracks);
    let st = p.state();
    let audio: Vec<_> = st.audio_tracks().cloned().collect();
    assert_eq!(audio[0].lang.as_deref(), Some("eng"));
    assert_eq!(audio[1].lang.as_deref(), Some("fra"));
    assert_eq!(audio[1].title.as_deref(), Some("Tone 880 Hz"));
    let subs: Vec<_> = st.subtitle_tracks().cloned().collect();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].title.as_deref(), Some("Test subtitles"));

    p.select_audio(TrackSelection::Id(audio[1].id)).unwrap();
    p.select_subtitle(TrackSelection::Id(subs[0].id)).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        s.selected(TrackKind::Audio).map(|t| t.id) == Some(audio[1].id)
            && s.selected(TrackKind::Subtitle).map(|t| t.id) == Some(subs[0].id)
    });
    assert!(ok, "selection not applied: {:?}", p.state().tracks);

    p.select_subtitle(TrackSelection::Off).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        s.selected(TrackKind::Subtitle).is_none()
    });
    assert!(ok, "subtitles not turned off");
    assert!(p.select_audio(TrackSelection::Id(-3)).is_err());
}

#[test]
fn seek_pause_and_video_fit_persist_across_loads() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let Some(p) = player(PlayerConfig::default().allow_local_files(true)) else {
        return;
    };
    let four_three = VideoFit {
        aspect: Aspect::Ratio(4, 3),
        ..VideoFit::ORIGINAL
    };
    p.set_video_fit(four_three).unwrap();
    for round in 0..2 {
        p.load(&local(&ts), &LoadOptions::default()).unwrap();
        let (ok, _) = wait_for(&p, Duration::from_secs(10), |e, _| {
            matches!(e, Some(PlayerEvent::FileLoaded))
        });
        assert!(ok, "round {round}: not loaded");
        let (ok, _) = wait_for(&p, Duration::from_secs(5), |e, _| {
            matches!(e, Some(PlayerEvent::PlaybackRestart))
        });
        assert!(ok, "round {round}: playback did not start");
        let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
            (s.video_aspect_override - 4.0 / 3.0).abs() < 1e-3
        });
        assert!(
            ok,
            "round {round}: aspect {}",
            p.state().video_aspect_override
        );
    }
    assert_eq!(p.video_fit(), four_three);

    p.pause().unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| s.paused);
    assert!(ok);
    p.seek_absolute(10.0).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |e, _| {
        matches!(e, Some(PlayerEvent::PlaybackRestart))
    });
    assert!(ok, "seek did not complete");
    std::thread::sleep(Duration::from_millis(200));
    let t = p.state().time_pos;
    assert!(t.is_some_and(|t| (t - 10.0).abs() < 0.6), "seek: {t:?}");
    let t0 = p.state().time_pos.unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        (p.state().time_pos.unwrap() - t0).abs() < 0.05,
        "advanced while paused"
    );
    p.seek_relative(-5.0).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        s.time_pos.is_some_and(|t| (t - 5.0).abs() < 0.6)
    });
    assert!(ok, "relative seek: {:?}", p.state().time_pos);
    p.play().unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        !s.paused && s.time_pos.is_some_and(|t| t > t0 - 5.0 + 0.3)
    });
    assert!(ok, "did not resume");
    p.set_volume(250.0).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| {
        (s.volume - 130.0).abs() < 1e-6
    });
    assert!(ok, "volume clamp: {}", p.state().volume);
    assert!(p.seek_absolute(f64::NAN).is_err());
}

#[test]
fn hls_over_http_sends_user_agent_and_referrer() {
    let _serial = serial();
    let Some(_) = media("hls/index.m3u8") else {
        return;
    };
    let server = Server::start(media_dir());
    let Some(p) = player(
        PlayerConfig::default().user_agent(UserAgent::new("opensmarters-default/1").unwrap()),
    ) else {
        return;
    };
    let url = PlayableUrl::parse(&server.url("hls/index.m3u8")).unwrap();
    p.load(
        &url,
        &LoadOptions {
            user_agent: Some(UserAgent::new("opensmarters-test/1.0").unwrap()),
            referrer: Some(Referrer::parse("http://127.0.0.1/portal").unwrap()),
            start: None,
        },
    )
    .unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(15), |_, s| {
        s.file_loaded && s.time_pos.is_some_and(|t| t > 0.5)
    });
    let _ = events.len();
    assert!(ok, "HLS did not play; events: {events:?}");
    assert!(p.state().seekable, "HLS VOD should be seekable");
    let reqs = server.requests();
    assert!(reqs.iter().any(|r| r.path.ends_with("index.m3u8")));
    assert!(reqs.iter().any(|r| r.path.contains("seg_")), "{reqs:?}");
    for r in &reqs {
        assert_eq!(
            r.user_agent.as_deref(),
            Some("opensmarters-test/1.0"),
            "{r:?}"
        );
        assert_eq!(
            r.referer.as_deref(),
            Some("http://127.0.0.1/portal"),
            "{r:?}"
        );
    }

    // The next load without overrides falls back to the configured default
    // User-Agent and sends no Referer.
    p.stop().unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    assert!(ok, "stop did not idle the core");
    let before = server.requests().len();
    let ts = PlayableUrl::parse(&server.url("test.ts")).unwrap();
    p.load(&ts, &LoadOptions::default()).unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(10), |e, _| {
        matches!(e, Some(PlayerEvent::FileLoaded))
    });
    assert!(ok);
    let reqs = server.requests();
    let new: Vec<_> = reqs[before..].to_vec();
    assert!(!new.is_empty());
    for r in &new {
        assert_eq!(
            r.user_agent.as_deref(),
            Some("opensmarters-default/1"),
            "{r:?}"
        );
        assert_eq!(r.referer, None, "{r:?}");
    }
}

#[test]
fn remote_playlist_cannot_reach_local_files() {
    let _serial = serial();
    let Some(_) = media("evil-file-segment.m3u8") else {
        return;
    };
    let server = Server::start(media_dir());
    let Some(p) = player(PlayerConfig::default().log_level(LogLevel::Warn)) else {
        return;
    };
    let url = PlayableUrl::parse(&server.url("evil-file-segment.m3u8")).unwrap();
    p.load(&url, &LoadOptions::default()).unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(15), |e, _| {
        matches!(e, Some(PlayerEvent::EndFile(EndReason::Error(_))))
    });
    assert!(ok, "expected a load error; events: {events:?}");
    let st = p.state();
    assert!(!st.file_loaded);
    assert!(st.time_pos.is_none(), "something played: {:?}", st.time_pos);
    let whitelist_hit = events
        .iter()
        .any(|e| matches!(e, PlayerEvent::Log(l) if l.text.contains("not on whitelist")));
    assert!(
        whitelist_hit,
        "ffmpeg protocol whitelist did not trigger; events: {events:#?}"
    );
}

#[test]
fn http_redirect_to_a_local_file_is_refused() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let server = Server::start(media_dir());
    let Some(p) = player(PlayerConfig::default()) else {
        return;
    };
    // Control: an ordinary same-server redirect works.
    let ok_url =
        PlayableUrl::parse(&server.url(&format!("redirect/{}", server.url("test.ts")))).unwrap();
    p.load(&ok_url, &LoadOptions::default()).unwrap();
    let (ok, events) = wait_for(&p, Duration::from_secs(10), |e, _| {
        matches!(e, Some(PlayerEvent::FileLoaded))
    });
    assert!(ok, "plain redirect did not play: {events:?}");
    p.stop().unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    assert!(ok);

    // A provider answering 302 -> file:///… must not make mpv read the file.
    let file_url = url_for_file(&ts);
    let evil = PlayableUrl::parse(&server.url(&format!("redirect/{file_url}"))).unwrap();
    p.load(&evil, &LoadOptions::default()).unwrap();
    let (ended, events) = wait_for(&p, Duration::from_secs(10), |e, _| {
        matches!(e, Some(PlayerEvent::EndFile(EndReason::Error(_))))
    });
    assert!(ended, "expected a load error; events: {events:?}");
    assert!(
        !events.iter().any(|e| matches!(e, PlayerEvent::FileLoaded)),
        "redirect to file:// was followed"
    );
}

fn url_for_file(p: &Path) -> String {
    PlayableUrl::from_local_file(p).unwrap().as_str().to_owned()
}

#[test]
fn player_enforces_its_own_policy() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let Some(p) = player(PlayerConfig::default()) else {
        return;
    };
    // Local files need the explicit opt-in on the player.
    assert!(matches!(
        p.load(&local(&ts), &LoadOptions::default()),
        Err(Error::LocalFilesDisabled)
    ));
    // A URL parsed with a permissive policy is still refused by an
    // HTTP-only player.
    let rtmp = PlayableUrl::parse_with_policy("rtmp://127.0.0.1/live/x", UrlPolicy::ALL_STREAMING)
        .unwrap();
    assert!(matches!(
        p.load(&rtmp, &LoadOptions::default()),
        Err(Error::SchemeNotEnabled(s)) if s == "rtmp"
    ));
    // Dangerous inputs never become a PlayableUrl in the first place.
    for bad in [
        "file:///C:/Windows/win.ini",
        "lavfi://sine",
        "edl://x",
        "av://x",
    ] {
        assert!(PlayableUrl::parse(bad).is_err(), "{bad}");
    }
    assert!(p.state().idle);
}

#[test]
fn loads_wait_for_a_render_context_when_video_is_enabled() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let p = match Player::new(
        PlayerConfig::default()
            .audio_output(false)
            .allow_local_files(true),
    ) {
        Ok(p) => p,
        Err(Error::Load(e)) => {
            eprintln!("SKIP: {e}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    p.load(&local(&ts), &LoadOptions::default()).unwrap();
    assert!(p.has_pending_load());
    let (started, _) = wait_for(&p, Duration::from_millis(800), |e, _| {
        matches!(e, Some(PlayerEvent::StartFile))
    });
    assert!(!started, "load must wait for a render context");
    p.stop().unwrap();
    assert!(!p.has_pending_load());
}

// ---------------------------------------------------------------------------
// Hostile provider content (security review, docs/SECURITY.md).

/// A fresh scratch directory under the system temp dir.
fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "opensmarters-player-{tag}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Loads `url` and collects events for `secs` seconds (or until the core is
/// idle again after at least one `EndFile`).
fn run_for(p: &Player, url: &str, secs: u64) -> Vec<PlayerEvent> {
    let url = PlayableUrl::parse(url).unwrap();
    p.load(&url, &LoadOptions::default()).unwrap();
    let mut ended = false;
    let (_, events) = wait_for(p, Duration::from_secs(secs), |e, s| {
        if matches!(
            e,
            Some(PlayerEvent::EndFile(EndReason::Error(_) | EndReason::Eof))
        ) {
            ended = true;
        }
        ended && s.idle
    });
    events
}

fn played(events: &[PlayerEvent]) -> bool {
    events.iter().any(|e| matches!(e, PlayerEvent::FileLoaded))
}

fn log_text(events: &[PlayerEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            PlayerEvent::Log(l) => Some(format!("{}: {}\n", l.prefix, l.text.trim_end())),
            _ => None,
        })
        .collect()
}

#[test]
fn remote_m3u_playlist_entries_cannot_be_local_or_special() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let dir = scratch_dir("m3u");
    std::fs::copy(&ts, dir.join("test.ts")).unwrap();
    let file_url = url_for_file(&ts);
    let raw_path = ts.display().to_string();
    let server = Server::start(dir.clone());
    // mpv's `hex://`/`memory://` turn a URL into in-memory data: try to
    // smuggle an EDL (which can reference local files and lavfi graphs) or a
    // nested playlist through them.
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    let edl_local = hex(&format!("# mpv EDL v0\n{raw_path}\n"));
    let edl_lavfi = hex("# mpv EDL v0\nav://lavfi:sine\n");
    let m3u_local = hex(&format!("#EXTM3U\n{file_url}\n"));
    let entries = [
        file_url.clone(),
        raw_path.clone(),
        format!("edl://{raw_path}"),
        "av://lavfi:sine".to_owned(),
        "lavfi://sine".to_owned(),
        format!("hex://{edl_local}"),
        format!("hex://{edl_lavfi}"),
        format!("hex://{m3u_local}"),
        format!("memory://{raw_path}"),
        format!("archive://{raw_path}|/x"),
        format!("slice://0-1000@{raw_path}"),
        format!("appending://{raw_path}"),
        format!("lavf://file:{raw_path}"),
        "fd://0".to_owned(),
        format!("mf://{raw_path}"),
        "dvd://".to_owned(),
        "bd://".to_owned(),
        "cdda://".to_owned(),
        "smb://127.0.0.1/share/x.ts".to_owned(),
    ];
    let mut evil = String::from("#EXTM3U\n");
    for e in &entries {
        evil.push_str("#EXTINF:-1,x\n");
        evil.push_str(e);
        evil.push('\n');
    }
    std::fs::write(dir.join("evil.m3u"), evil).unwrap();
    std::fs::write(
        dir.join("good.m3u"),
        format!("#EXTM3U\n#EXTINF:-1,net\n{}\n", server.url("test.ts")),
    )
    .unwrap();
    let Some(p) = player(PlayerConfig::default().log_level(LogLevel::Verbose)) else {
        return;
    };

    // Control: a network playlist may point at other network URLs.
    let events = run_for(&p, &server.url("good.m3u"), 15);
    assert!(
        played(&events),
        "control playlist did not play: {events:#?}"
    );
    p.stop().unwrap();
    let (ok, _) = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    assert!(ok);

    let events = run_for(&p, &server.url("evil.m3u"), 20);
    let logs = log_text(&events);
    assert!(
        !played(&events),
        "a remote playlist opened a local/special entry:\n{logs}"
    );
    assert!(p.state().time_pos.is_none());
    eprintln!(
        "playlist entries refused: {} | opened: {}",
        logs.matches("Opening failed or was aborted").count(),
        logs.matches("Opening done").count()
    );
    for line in logs.lines().filter(|l| l.contains("Opening done")) {
        eprintln!("  {line}");
    }
    if std::env::var_os("OPENSMARTERS_TEST_DUMP").is_some() {
        eprintln!("{logs}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remote_edl_cannot_reference_local_files() {
    let _serial = serial();
    let Some(ts) = media("test.ts") else { return };
    let dir = scratch_dir("edl");
    let server = Server::start(dir.clone());
    std::fs::write(
        dir.join("evil.ts"),
        format!("# mpv EDL v0\n{}\n", ts.display()),
    )
    .unwrap();
    std::fs::write(
        dir.join("evil2.ts"),
        format!("# mpv EDL v0\n{}\n", url_for_file(&ts)),
    )
    .unwrap();
    let Some(p) = player(PlayerConfig::default().log_level(LogLevel::Verbose)) else {
        return;
    };
    for name in ["evil.ts", "evil2.ts"] {
        let events = run_for(&p, &server.url(name), 15);
        assert!(
            !played(&events),
            "{name}: a remote EDL file opened a local file:\n{}",
            log_text(&events)
        );
        p.stop().unwrap();
        let _ = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn redirects_to_non_http_protocols_are_not_followed() {
    let _serial = serial();
    let Some(_) = media("test.ts") else { return };
    let server = Server::start(media_dir());
    // Something listening where the redirects point, counting connections.
    let trap = TcpListener::bind("127.0.0.1:0").unwrap();
    let trap_port = trap.local_addr().unwrap().port();
    trap.set_nonblocking(true).unwrap();
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let hits = Arc::clone(&hits);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                match trap.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut buf = [0u8; 256];
                        let n = s.read(&mut buf).unwrap_or(0);
                        hits.lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
    }
    let Some(p) = player(PlayerConfig::default().log_level(LogLevel::Verbose)) else {
        return;
    };
    let mut report = String::new();
    let mut followed = Vec::new();
    // `ftp` is last: libcurl (mpv's HTTP backend in this build) follows
    // redirects to ftp/ftps by default (documented in docs/SECURITY.md); every
    // other scheme must be refused without a connection.
    for scheme in [
        "gopher", "dict", "telnet", "smb", "ldap", "tftp", "scp", "sftp", "imap", "smtp", "mqtt",
        "rtsp", "rtmp", "ftp",
    ] {
        let before = hits.lock().unwrap().len();
        let target = format!("{scheme}://127.0.0.1:{trap_port}/x.ts");
        let events = run_for(&p, &server.url(&format!("redirect/{target}")), 10);
        let after = hits.lock().unwrap().len();
        report.push_str(&format!(
            "{scheme}: connections={} played={}\n",
            after - before,
            played(&events)
        ));
        if after > before {
            followed.push(scheme);
        }
        p.stop().unwrap();
        let _ = wait_for(&p, Duration::from_secs(5), |_, s| s.idle);
    }
    eprintln!("{report}");
    assert!(
        !report.contains("played=true"),
        "a redirect to another protocol played:\n{report}"
    );
    assert!(
        followed.iter().all(|s| *s == "ftp"),
        "mpv followed a redirect to a non-HTTP protocol:\n{report}"
    );
}

#[cfg(windows)]
#[test]
fn dll_search_path_is_restricted() {
    assert!(opensmarters_player::harden_dll_search_path());
    // Idempotent.
    assert!(opensmarters_player::harden_dll_search_path());
    // Release builds never search PATH or the working directory.
    let cwd = std::env::current_dir().unwrap();
    for p in opensmarters_player::libmpv_search_paths(None) {
        assert!(p.is_absolute(), "{}", p.display());
        assert_ne!(p.parent(), Some(cwd.as_path()), "{}", p.display());
    }
}

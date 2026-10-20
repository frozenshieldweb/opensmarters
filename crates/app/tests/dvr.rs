// SPDX-License-Identifier: GPL-3.0-or-later
//! The recording/download engine against the in-process fake panel (real HTTP, real
//! files in a temporary folder): a film download, interrupt + resume with `Range`,
//! preemption by viewing, a scheduled live recording (TS and HLS), and bodies that aren't
//! video being refused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use opensmarters_app::dvr::engine::{Engine, INTERRUPTED, MISSED, PREEMPTED};
use opensmarters_app::dvr::{self, ProgrammeToRecord, source};
use opensmarters_app::error::AppError;
use opensmarters_app::playback::MediaKind;
use opensmarters_app::services::connections::Purpose;
use opensmarters_app::services::{Paths, Services, Session};
use opensmarters_core::Redactor;
use opensmarters_core::chrono::Utc;
use opensmarters_core::secrecy::SecretString;
use opensmarters_player::{Container, sniff_container};
use opensmarters_store::{
    CategoryLock, ContentKind, MemorySecretStore, NewProfile, NewProfileSource, NewRecording,
    ProfileId, ProfileSettings, Recording, RecordingId, RecordingKind, RecordingState, Store,
    StoreOptions,
};

struct Env {
    rt: tokio::runtime::Runtime,
    services: Services,
    panel: fake_panel::Handle,
    dir: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env(name: &str, quirks: &str) -> Option<Env> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let q: fake_panel::Quirks = quirks.parse().unwrap();
    let panel = rt
        .block_on(fake_panel::spawn(
            fake_panel::Config::default().with_quirks(q),
        ))
        .unwrap();
    if panel.media_status() != fake_panel::MediaStatus::Ready {
        eprintln!("test media unavailable (ffmpeg missing?): skipping");
        return None;
    }
    let store = Arc::new(
        Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    );
    let dir = std::env::temp_dir().join(format!(
        "opensmarters-dvr-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let st = store.settings();
    st.set(&dvr::RECORDINGS_DIR, &Some(dir.clone())).unwrap();
    st.set(&dvr::KEEP_FREE_GB, &0).unwrap();
    st.set(&dvr::PAD_BEFORE, &0).unwrap();
    st.set(&dvr::PAD_AFTER, &0).unwrap();
    let services = Services::new(
        rt.handle().clone(),
        store,
        Redactor::new(),
        Paths::default(),
        true,
        egui::Context::default(),
    );
    Some(Env {
        rt,
        services,
        panel,
        dir,
    })
}

fn xtream(env: &Env) -> Arc<Session> {
    let id = env
        .services
        .store
        .profiles()
        .create(NewProfile {
            name: "Test".into(),
            source: NewProfileSource::Xtream {
                server_url: env.panel.base_url().to_owned(),
                username: "demo".into(),
                password: SecretString::from("demo".to_owned()),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id;
    env.rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error))
}

fn start(env: &Env, session: &Arc<Session>) -> Engine {
    let _guard = env.rt.enter();
    Engine::start(Arc::clone(session)).unwrap()
}

fn get(env: &Env, id: RecordingId) -> Recording {
    env.services.store.recordings().get(id).unwrap().unwrap()
}

/// Polls the row until `done(row)` or the timeout.
fn wait(
    env: &Env,
    id: RecordingId,
    secs: u64,
    what: &str,
    done: impl Fn(&Recording) -> bool,
) -> Recording {
    let until = Instant::now() + Duration::from_secs(secs);
    loop {
        let r = get(env, id);
        if done(&r) {
            return r;
        }
        assert!(
            Instant::now() < until,
            "timed out waiting for {what}: state {:?}, error {:?}, {} bytes",
            r.state,
            r.error,
            r.bytes_done
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn mkv_movie(env: &Env) -> (u64, String) {
    let m = env
        .panel
        .catalogue()
        .movies
        .iter()
        .find(|m| m.container_extension == "mkv")
        .unwrap();
    (m.stream_id, m.title.clone())
}

fn queue_movie(env: &Env, profile: ProfileId, stream_id: u64, title: &str) -> Recording {
    let mut new = NewRecording::download(RecordingKind::Movie, stream_id.to_string(), title);
    new.container_extension = Some("mkv".into());
    dvr::queue_download(&env.services.store, profile, &new).unwrap()
}

fn media_file(name: &str) -> PathBuf {
    fake_panel::default_media_dir().join(name)
}

fn part_len(dir: &Path, r: &Recording) -> u64 {
    r.file_name
        .as_ref()
        .and_then(|n| std::fs::metadata(dir.join(format!("{n}.part"))).ok())
        .map_or(0, |m| m.len())
}

fn head(path: &Path) -> Vec<u8> {
    let mut b = std::fs::read(path).unwrap();
    b.truncate(4096);
    b
}

#[test]
fn downloads_a_movie_and_plays_it_from_disk() {
    let Some(env) = env("movie", "") else { return };
    let session = xtream(&env);
    let engine = start(&env, &session);
    let (id, title) = mkv_movie(&env);
    let row = queue_movie(&env, session.profile_id(), id, &title);
    engine.wake();
    let done = wait(&env, row.id, 30, "the download", |r| {
        r.state == RecordingState::Done
    });
    let name = done.file_name.clone().unwrap();
    assert!(name.ends_with(".mkv"), "{name}");
    let path = env.dir.join(&name);
    let source_len = std::fs::metadata(media_file("movie-multitrack.mkv"))
        .unwrap()
        .len();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), source_len);
    assert_eq!(done.bytes_done, source_len);
    assert_eq!(sniff_container(&head(&path)), Some(Container::Matroska));
    assert!(!env.dir.join(format!("{name}.part")).exists());
    // Plays as the film itself (resume point shared with streaming), from the folder.
    let req = source::playback_request(&env.dir, &done, None).unwrap();
    assert_eq!(req.kind, MediaKind::Movie);
    assert!(req.url.is_owned_media());
    // The row never holds a URL.
    assert!(!format!("{done:?}").contains("demo/demo"));
    engine.shutdown();
}

#[test]
fn an_interrupted_download_resumes_with_range() {
    let Some(env) = env("resume", "slow-media") else {
        return;
    };
    let session = xtream(&env);
    let engine = start(&env, &session);
    let (id, title) = mkv_movie(&env);
    let row = queue_movie(&env, session.profile_id(), id, &title);
    engine.wake();
    // Let it get part of the way (the panel sends ~256 KiB/s).
    let started = wait(&env, row.id, 20, "a file name", |r| r.file_name.is_some());
    let until = Instant::now() + Duration::from_secs(20);
    while part_len(&env.dir, &started) < 300_000 {
        assert!(Instant::now() < until, "the download didn't progress");
        std::thread::sleep(Duration::from_millis(100));
    }
    engine.shutdown();
    let paused = wait(&env, row.id, 5, "the pause", |r| {
        r.state == RecordingState::Paused
    });
    assert_eq!(paused.error.as_deref(), Some(INTERRUPTED));
    std::thread::sleep(Duration::from_millis(500));
    let partial = part_len(&env.dir, &paused);
    let total = std::fs::metadata(media_file("movie-multitrack.mkv"))
        .unwrap()
        .len();
    assert!(partial > 0 && partial < total, "{partial} of {total}");

    // A new engine (the app started again) continues from the part file.
    env.panel.clear_requests();
    let engine = start(&env, &session);
    let done = wait(&env, row.id, 30, "the resumed download", |r| {
        r.state == RecordingState::Done
    });
    let path = env.dir.join(done.file_name.clone().unwrap());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), total);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(media_file("movie-multitrack.mkv")).unwrap()
    );
    let ranged = env
        .panel
        .requests()
        .into_iter()
        .filter(|r| r.path.starts_with("/movie/"))
        .filter_map(|r| r.range)
        .collect::<Vec<_>>();
    assert!(
        ranged
            .iter()
            .any(|r| r.starts_with("bytes=") && r != "bytes=0-"),
        "resumed with a Range request: {ranged:?}"
    );
    engine.shutdown();
}

#[test]
fn viewing_preempts_a_download_which_then_continues() {
    let Some(env) = env("preempt", "slow-media") else {
        return;
    };
    let session = xtream(&env);
    session.connections().set_override(Some(1));
    let engine = start(&env, &session);
    let (id, title) = mkv_movie(&env);
    let row = queue_movie(&env, session.profile_id(), id, &title);
    engine.wake();
    wait(&env, row.id, 20, "the download to start", |r| {
        r.state == RecordingState::Active && r.file_name.is_some()
    });
    std::thread::sleep(Duration::from_millis(600));
    // The user starts watching: the only connection goes to the player.
    let watch = session
        .connections()
        .acquire_for_viewing(Purpose::Watch)
        .expect("viewing takes the download's slot");
    let queued = wait(&env, row.id, 10, "the preemption", |r| {
        r.state == RecordingState::Queued
    });
    assert_eq!(queued.error.as_deref(), Some(PREEMPTED));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        get(&env, row.id).state,
        RecordingState::Queued,
        "waits while viewing"
    );
    // Viewing stops: the download continues by itself and finishes.
    drop(watch);
    let done = wait(&env, row.id, 40, "the download to finish", |r| {
        r.state == RecordingState::Done
    });
    let total = std::fs::metadata(media_file("movie-multitrack.mkv"))
        .unwrap()
        .len();
    assert_eq!(done.bytes_done, total);
    engine.shutdown();
}

#[test]
fn records_a_scheduled_live_programme() {
    let Some(env) = env("live", "live-endless") else {
        return;
    };
    let session = xtream(&env);
    let engine = start(&env, &session);
    let ch = env.panel.catalogue().live[0].clone();
    let now = Utc::now();
    let r = dvr::schedule_programme(
        &env.services.store,
        session.profile_id(),
        &ProgrammeToRecord {
            stream_id: ch.stream_id.to_string(),
            channel_name: ch.name.clone(),
            channel_logo: None,
            title: "Synthetic Evening News".into(),
            start: now - opensmarters_core::chrono::Duration::seconds(1),
            end: now + opensmarters_core::chrono::Duration::seconds(5),
        },
    )
    .unwrap();
    engine.wake();
    wait(&env, r.id, 15, "the recording to start", |r| {
        r.state == RecordingState::Active
    });
    assert!(engine.recording_keys().contains(&ch.stream_id.to_string()));
    let done = wait(&env, r.id, 30, "the recording to end", |r| {
        r.state == RecordingState::Done
    });
    let name = done.file_name.clone().unwrap();
    assert!(
        name.starts_with("Synthetic Evening News ") && name.ends_with(".ts"),
        "{name}"
    );
    let path = env.dir.join(&name);
    let len = std::fs::metadata(&path).unwrap().len();
    // About 5 s of a looping clip sent at its real bitrate (the smallest is ~22 KB/s).
    let clip = std::fs::metadata(media_file(&format!("live-{}.ts", ch.clip)))
        .unwrap()
        .len();
    assert!(len > clip / 20 * 3, "only {len} bytes recorded");
    assert_eq!(done.bytes_done, len);
    assert_eq!(sniff_container(&head(&path)), Some(Container::MpegTs));
    // Recorded from the .ts container (the account allows it).
    assert!(
        env.panel
            .requests()
            .iter()
            .any(|q| q.path == format!("/live/demo/demo/{}.ts", ch.stream_id))
    );
    // Plays as a recording (its own resume point).
    let req = source::playback_request(&env.dir, &done, None).unwrap();
    assert_eq!(req.kind, MediaKind::Recording);
    engine.shutdown();
}

#[test]
fn records_an_hls_only_channel() {
    let Some(env) = env("hls", "") else { return };
    // An M3U profile whose live entries are HLS playlists.
    let url = format!(
        "{}/get.php?username=demo&password=demo&type=m3u_plus&output=m3u8",
        env.panel.base_url()
    );
    let id = env
        .services
        .store
        .profiles()
        .create(NewProfile {
            name: "Playlist".into(),
            source: NewProfileSource::M3uUrl {
                url: SecretString::from(url),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id;
    let session = env
        .rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("{}", f.error));
    let catalog = env.rt.block_on(session.live_catalog()).unwrap();
    let ch = catalog.channels[0].clone();
    let engine = start(&env, &session);
    let now = Utc::now();
    let r = dvr::schedule_programme(
        &env.services.store,
        session.profile_id(),
        &ProgrammeToRecord {
            stream_id: ch.key.clone(),
            channel_name: ch.name.clone(),
            channel_logo: None,
            title: "Synthetic HLS".into(),
            start: now - opensmarters_core::chrono::Duration::seconds(1),
            end: now + opensmarters_core::chrono::Duration::seconds(6),
        },
    )
    .unwrap();
    engine.wake();
    let done = wait(&env, r.id, 40, "the HLS recording", |r| {
        r.state == RecordingState::Done
    });
    let path = env.dir.join(done.file_name.clone().unwrap());
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > 100_000, "only {len} bytes recorded");
    assert_eq!(sniff_container(&head(&path)), Some(Container::MpegTs));
    assert!(
        env.panel
            .requests()
            .iter()
            .any(|q| q.path.starts_with("/hls/")),
        "segments were fetched"
    );
    engine.shutdown();
}

#[test]
fn bodies_that_are_not_video_are_refused() {
    for quirk in ["media-html", "media-playlist"] {
        let Some(env) = env(quirk, quirk) else { return };
        let session = xtream(&env);
        let engine = start(&env, &session);
        let (id, title) = mkv_movie(&env);
        let row = queue_movie(&env, session.profile_id(), id, &title);
        engine.wake();
        let failed = wait(&env, row.id, 20, "the refusal", |r| {
            r.state == RecordingState::Failed
        });
        assert!(
            failed
                .error
                .as_deref()
                .is_some_and(|e| e.contains("isn't a video")),
            "{quirk}: {:?}",
            failed.error
        );
        let left: Vec<_> = std::fs::read_dir(&env.dir).unwrap().flatten().collect();
        assert!(left.is_empty(), "{quirk}: nothing is kept: {left:?}");
        engine.shutdown();
    }
    // A live channel answering an HTML page fails the same way.
    let Some(env) = env("live-html", "media-html") else {
        return;
    };
    let session = xtream(&env);
    let engine = start(&env, &session);
    let ch = env.panel.catalogue().live[1].clone();
    let now = Utc::now();
    let r = dvr::schedule_programme(
        &env.services.store,
        session.profile_id(),
        &ProgrammeToRecord {
            stream_id: ch.stream_id.to_string(),
            channel_name: ch.name.clone(),
            channel_logo: None,
            title: "Not video".into(),
            start: now,
            end: now + opensmarters_core::chrono::Duration::seconds(30),
        },
    )
    .unwrap();
    engine.wake();
    let failed = wait(&env, r.id, 20, "the refusal", |r| {
        r.state == RecordingState::Failed
    });
    assert!(
        failed
            .error
            .as_deref()
            .is_some_and(|e| e.contains("isn't a video"))
    );
    assert!(std::fs::read_dir(&env.dir).unwrap().next().is_none());
    engine.shutdown();
}

#[test]
fn a_recording_without_a_connection_waits_for_one() {
    let Some(env) = env("waiting", "live-endless") else {
        return;
    };
    let session = xtream(&env);
    session.connections().set_override(Some(1));
    // The user watches channel 1: the only connection is taken.
    let watch = session
        .connections()
        .acquire_for_viewing(Purpose::Watch)
        .expect("a free slot");
    let engine = start(&env, &session);
    let ch = env.panel.catalogue().live[0].clone();
    let key = ch.stream_id.to_string();
    let now = Utc::now();
    let r = dvr::schedule_programme(
        &env.services.store,
        session.profile_id(),
        &ProgrammeToRecord {
            stream_id: key.clone(),
            channel_name: ch.name.clone(),
            channel_logo: None,
            title: "Synthetic Waiting".into(),
            start: now - opensmarters_core::chrono::Duration::seconds(1),
            end: now + opensmarters_core::chrono::Duration::seconds(6),
        },
    )
    .unwrap();
    engine.wake();
    let waiting = wait(&env, r.id, 10, "the waiting message", |r| {
        r.state == RecordingState::Queued
            && r.error
                .as_deref()
                .is_some_and(|e| e.starts_with("Waiting for a free connection"))
    });
    assert!(
        waiting
            .error
            .as_deref()
            .is_some_and(|e| e.contains("Your only connection is in use"))
    );
    // Offered to the player (it can record what it plays without another connection).
    assert!(
        engine
            .waiting()
            .iter()
            .any(|(id, k)| *id == r.id && *k == key)
    );
    // The viewer stops: the recording takes the connection and runs to the end.
    drop(watch);
    let done = wait(&env, r.id, 30, "the recording", |r| r.state.is_final());
    assert_eq!(done.state, RecordingState::Done, "{:?}", done.error);
    // A programme whose whole window passed before it could start is reported as missed.
    let past = dvr::schedule_programme(
        &env.services.store,
        session.profile_id(),
        &ProgrammeToRecord {
            stream_id: key,
            channel_name: ch.name.clone(),
            channel_logo: None,
            title: "Synthetic Past".into(),
            start: now - opensmarters_core::chrono::Duration::hours(2),
            end: now - opensmarters_core::chrono::Duration::hours(1),
        },
    )
    .unwrap();
    engine.wake();
    let missed = wait(&env, past.id, 10, "the missed state", |r| {
        r.state.is_final()
    });
    assert_eq!(missed.state, RecordingState::Failed);
    assert_eq!(missed.error.as_deref(), Some(MISSED));
    engine.shutdown();
}

/// A profile (demo account) with the parental PIN set and these categories locked.
fn locked_xtream(env: &Env, movie_cat: u32, live_cat: u32) -> Arc<Session> {
    let store = &env.services.store;
    let id = store
        .profiles()
        .create(NewProfile {
            name: "Locked".into(),
            source: NewProfileSource::Xtream {
                server_url: env.panel.base_url().to_owned(),
                username: "demo".into(),
                password: SecretString::from("demo".to_owned()),
            },
            settings: ProfileSettings::default(),
        })
        .unwrap()
        .id;
    if !store.parental().is_pin_set().unwrap() {
        store
            .parental()
            .set_pin(&SecretString::from("1234".to_owned()))
            .unwrap();
    }
    for (kind, cat) in [
        (ContentKind::Movie, movie_cat),
        (ContentKind::Live, live_cat),
    ] {
        store
            .parental()
            .set_category_lock(id, kind, &cat.to_string(), Some(CategoryLock::Locked))
            .unwrap();
    }
    env.rt
        .block_on(Session::open(env.services.clone(), id))
        .unwrap_or_else(|f| panic!("open failed: {}", f.error))
}

#[test]
fn locked_items_still_download_and_record_but_need_the_pin_to_play() {
    let Some(env) = env("locked", "live-endless") else {
        return;
    };
    let movie = env
        .panel
        .catalogue()
        .movies
        .iter()
        .find(|m| m.container_extension == "mkv")
        .unwrap()
        .clone();
    let ch = env.panel.catalogue().live[0].clone();
    let session = locked_xtream(&env, movie.category_id, ch.category_id);
    // Parental control applies: playing either needs the PIN (unknown categories count
    // as locked too).
    assert!(matches!(
        session.movie_request(movie.stream_id, Some("mkv"), &movie.title, None, None, None),
        Err(AppError::Locked(_))
    ));

    // A download queued earlier (by someone who had the PIN) still runs in the
    // background, and so does a scheduled recording of a locked channel.
    let engine = start(&env, &session);
    let row = queue_movie(&env, session.profile_id(), movie.stream_id, &movie.title);
    let now = Utc::now();
    let rec = dvr::schedule_programme(
        &env.services.store,
        session.profile_id(),
        &ProgrammeToRecord {
            stream_id: ch.stream_id.to_string(),
            channel_name: ch.name.clone(),
            channel_logo: None,
            title: "Locked News".into(),
            start: now - opensmarters_core::chrono::Duration::seconds(1),
            end: now + opensmarters_core::chrono::Duration::seconds(4),
        },
    )
    .unwrap();
    engine.wake();
    let done = wait(&env, row.id, 30, "the locked download", |r| {
        r.state.is_final()
    });
    assert_eq!(done.state, RecordingState::Done, "{:?}", done.error);
    let recorded = wait(&env, rec.id, 30, "the locked recording", |r| {
        r.state.is_final()
    });
    assert_eq!(recorded.state, RecordingState::Done, "{:?}", recorded.error);

    // The saved files are locked like their items: no title in lists, the PIN to play.
    for r in [&done, &recorded] {
        assert!(
            dvr::is_row_locked(&session, r),
            "{:?} must be locked",
            r.kind
        );
        let (kind, item) = dvr::lock_target(r).unwrap();
        // What `UnlockFlow` does: look the item up, then the PIN unlocks its category.
        let locked = match env.rt.block_on(session.verify_item(kind, &item)) {
            Err(AppError::Locked(l)) => l,
            other => panic!("{:?} should stay locked: {other:?}", r.kind),
        };
        match &locked.category {
            Some(c) => session.unlock_category(kind, c),
            None => session.unlock_item(kind, &item),
        }
        assert!(
            !dvr::is_row_locked(&session, r),
            "{:?} after the PIN",
            r.kind
        );
        assert!(session.check_unlocked(kind, &item, None).is_ok());
    }
    assert_eq!(
        dvr::lock_target(&done),
        Some((ContentKind::Movie, movie.stream_id.to_string()))
    );
    assert_eq!(
        dvr::lock_target(&recorded),
        Some((ContentKind::Live, ch.stream_id.to_string()))
    );
    engine.shutdown();
}

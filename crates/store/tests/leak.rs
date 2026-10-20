//! Leak test: secrets handed to the public API must never reach the database files.
//!
//! Plants distinctive passwords, M3U/EPG URLs, a TMDB key and PINs through every API that
//! accepts them (including credential-bearing cache keys and values), then scans the raw
//! bytes of the SQLite file and its `-wal`/`-shm` siblings — once while the store is open
//! (WAL populated) and once after it is closed — for each secret as UTF-8 and UTF-16LE.
//! A non-secret marker must be found, proving the scan reads the right bytes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Env, m3u_url, pin, xtream};
use opensmarters_store::{
    CacheScope, ContentKind, ItemSnapshot, PinOutcome, PlaybackKind, PlaybackSnapshot, SecretKey,
};
use secrecy::{ExposeSecret, SecretString};

const MARKER: &str = "PlainMarkerProfileName-q8v";
const XTREAM_PW: &str = "Zq7LeakPw-91fd3c";
const XTREAM_PW_2: &str = "Rk2LeakPw-0b77e4";
const DELETED_PW: &str = "Dx5LeakPw-deleted-3a";
const M3U_PW: &str = "M3uLeak-55e1b0";
const EPG_PW: &str = "EpgLeak-7c21aa";
const TMDB_KEY: &str = "tmdbLeakKey0123456789abcdef";
const PIN_1: &str = "73914826";
const PIN_WRONG: &str = "61829374";
const PIN_2: &str = "58203716";

fn m3u_full_url() -> String {
    format!("http://m3u.example.invalid/get.php?username=leak_user&password={M3U_PW}&type=m3u_plus")
}

fn secrets() -> Vec<String> {
    vec![
        XTREAM_PW.into(),
        XTREAM_PW_2.into(),
        DELETED_PW.into(),
        M3U_PW.into(),
        m3u_full_url(),
        EPG_PW.into(),
        TMDB_KEY.into(),
        PIN_1.into(),
        PIN_WRONG.into(),
        PIN_2.into(),
    ]
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Every regular file in the store directory (db, -wal, -shm, any journal).
fn read_all(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            out.push((path.clone(), std::fs::read(&path).unwrap()));
        }
    }
    out.sort();
    out
}

fn assert_no_secrets(files: &[(PathBuf, Vec<u8>)], phase: &str) {
    for (path, bytes) in files {
        for s in secrets() {
            assert!(
                !contains(bytes, s.as_bytes()),
                "{phase}: secret {s:?} found as UTF-8 in {}",
                path.display()
            );
            assert!(
                !contains(bytes, &utf16le(&s)),
                "{phase}: secret {s:?} found as UTF-16LE in {}",
                path.display()
            );
        }
    }
}

fn assert_marker_present(files: &[(PathBuf, Vec<u8>)], phase: &str) {
    assert!(
        files.iter().any(|(_, b)| contains(b, MARKER.as_bytes())),
        "{phase}: positive control failed — the non-secret marker was not found, so the scan \
         is not reading the database bytes"
    );
}

#[test]
fn secrets_never_reach_the_database_files() {
    let env = Env::new();
    let store = env.open();
    let profiles = store.profiles();

    // Profiles with secrets.
    let x = profiles.create(xtream(MARKER, XTREAM_PW)).unwrap();
    let m = profiles
        .create(m3u_url("Leak M3U", &m3u_full_url()))
        .unwrap();
    let doomed = profiles.create(xtream("Doomed", DELETED_PW)).unwrap();
    profiles
        .set_epg_url_override(
            x.id,
            Some(&SecretString::from(format!(
                "http://epg.example.invalid/xmltv.php?username=leak_user&password={EPG_PW}"
            ))),
        )
        .unwrap();
    profiles
        .set_password(x.id, &SecretString::from(XTREAM_PW_2))
        .unwrap();
    store
        .settings()
        .set_tmdb_api_key(Some(&SecretString::from(TMDB_KEY)))
        .unwrap();

    // PIN: set, fail once, change.
    let par = store.parental();
    par.set_pin(&pin(PIN_1)).unwrap();
    assert!(matches!(
        par.verify_pin(&pin(PIN_WRONG)).unwrap(),
        PinOutcome::Wrong { .. }
    ));
    assert_eq!(
        par.change_pin(&pin(PIN_1), &pin(PIN_2)).unwrap(),
        PinOutcome::Ok
    );
    assert_eq!(par.verify_pin(&pin(PIN_2)).unwrap(), PinOutcome::Ok);

    // Cache: credential-bearing keys and values (as a careless caller might store them).
    let cache = store.cache();
    let api_key =
        format!("player_api.php?username=leak_user&password={XTREAM_PW_2}&action=get_live_streams");
    let api_body = format!(
        r#"{{"user_info":{{"username":"leak_user","password":"{XTREAM_PW_2}"}},
            "direct_source":"http://panel.example.invalid:8080/live/leak_user/{XTREAM_PW_2}/1.ts"}}"#
    );
    cache
        .put(
            CacheScope::Profile(x.id),
            &api_key,
            api_body.as_bytes(),
            Duration::from_secs(3600),
        )
        .unwrap();
    let playlist = format!(
        "#EXTM3U\n#EXTINF:-1,Synthetic One\nhttp://m3u.example.invalid/live/leak_user/{M3U_PW}/1.ts\n"
    );
    cache
        .put(
            CacheScope::Profile(m.id),
            &m3u_full_url(),
            playlist.as_bytes(),
            Duration::from_secs(3600),
        )
        .unwrap();
    cache
        .put(
            CacheScope::Global,
            &format!("https://api.themoviedb.example.invalid/3/movie/1?api_key={TMDB_KEY}"),
            format!("{{\"echo\":\"{TMDB_KEY}\"}}").as_bytes(),
            Duration::from_secs(3600),
        )
        .unwrap();
    assert_eq!(
        cache
            .get_fresh(CacheScope::Profile(x.id), &api_key)
            .unwrap()
            .unwrap(),
        api_body.as_bytes(),
        "cache round-trips"
    );

    // Ordinary data alongside.
    store
        .favourites()
        .add(
            x.id,
            ContentKind::Live,
            "101",
            &ItemSnapshot::named("Synthetic News"),
        )
        .unwrap();
    store
        .playback()
        .record_progress(
            x.id,
            PlaybackKind::Movie,
            "202",
            321.0,
            Some(4000.0),
            &PlaybackSnapshot {
                title: "Synthetic Movie".into(),
                ..PlaybackSnapshot::default()
            },
        )
        .unwrap();
    store
        .recent_live()
        .touch(x.id, "101", &ItemSnapshot::named("Synthetic News"))
        .unwrap();
    assert!(profiles.delete(doomed.id).unwrap());

    // Sanity: the secrets really went to the secret store.
    let sec = &env.secrets;
    assert_eq!(
        profiles.password(x.id).unwrap().unwrap().expose_secret(),
        XTREAM_PW_2
    );
    assert_eq!(
        profiles.m3u_url(m.id).unwrap().unwrap().expose_secret(),
        m3u_full_url()
    );
    assert!(sec.contains(&SecretKey::profile_epg_url(x.id)));
    assert!(sec.contains(&SecretKey::tmdb_api_key()));
    assert!(!sec.contains(&SecretKey::profile_password(doomed.id)));

    // Scan while open: the WAL holds the recent pages.
    let open_files = read_all(env.dir.path());
    let names: Vec<String> = open_files
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("-wal")),
        "WAL present: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with("-shm")),
        "SHM present: {names:?}"
    );
    assert_marker_present(&open_files, "open");
    let wal = &open_files
        .iter()
        .find(|(p, _)| p.to_string_lossy().ends_with("-wal"))
        .unwrap()
        .1;
    assert!(
        contains(wal, b"Synthetic News"),
        "positive control: recent plaintext writes are visible in the WAL scan"
    );
    assert_no_secrets(&open_files, "open");

    // Close (checkpoint) and scan the final file.
    store.close().unwrap();
    let closed_files = read_all(env.dir.path());
    assert!(!closed_files.is_empty());
    assert_marker_present(&closed_files, "closed");
    assert_no_secrets(&closed_files, "closed");

    // And after reopening, everything is still usable.
    let store = env.open();
    assert_eq!(
        store.parental().verify_pin(&pin(PIN_2)).unwrap(),
        PinOutcome::Ok
    );
    assert!(
        store
            .cache()
            .get_fresh(CacheScope::Profile(m.id), &m3u_full_url())
            .unwrap()
            .is_some()
    );
    drop(store);
    assert_no_secrets(&read_all(env.dir.path()), "reopened");
}

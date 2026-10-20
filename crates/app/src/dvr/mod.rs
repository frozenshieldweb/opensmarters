// SPDX-License-Identifier: GPL-3.0-or-later
//! Recordings and downloads ("DVR"): films and episodes saved for offline viewing, live
//! channels recorded now or on a schedule, and clips saved from the player's buffer.
//!
//! **Phase 7 foundation.** This module holds the shared pieces other features rely on;
//! the engine (download/record tasks, the scheduler, the Recordings screen) is built on
//! top of it.
//!
//! | Piece | Where |
//! |---|---|
//! | Job list, progress, file names (never URLs) | store: `store.recordings()` ([`opensmarters_store::Recordings`]) |
//! | Where files go | [`RECORDINGS_DIR`] (user setting) else [`default_recordings_dir`] |
//! | Safe file names | [`file_name`] (always passes the store's and the player's checks) |
//! | Scheduling a programme from the Guide | [`schedule_programme`] (a store insert; the engine starts it) |
//! | Connections | `session.connections()`: downloads are preemptible, recordings are not |
//! | Playing a saved file | `PlayableUrl::from_owned_media(recordings_dir, path)` |
//! | Clips / recording what plays | `Player::dump_cache` / `Player::set_stream_record` into a `MediaOutput` |
//!
//! Scheduled recordings store the **programme's** start and end; the engine starts
//! [`PAD_BEFORE`] early and stops [`PAD_AFTER`] late (user settings).

use std::path::{Path, PathBuf};
use std::time::Duration;

use opensmarters_core::chrono::{DateTime, Local, Utc};
use opensmarters_store::settings::Setting;
use opensmarters_store::{
    NewRecording, ProfileId, Recording, RecordingKind, RecordingState, Store, StoreError, Timestamp,
};

/// The recordings folder chosen by the user (`None` = [`default_recordings_dir`]).
pub const RECORDINGS_DIR: Setting<Option<PathBuf>> = Setting::new("app.recordings_dir", || None);

/// Seconds a scheduled recording starts before the programme.
pub const PAD_BEFORE: Setting<u32> = Setting::new("app.recording_pad_before_secs", || 60);

/// Seconds a scheduled recording continues after the programme.
pub const PAD_AFTER: Setting<u32> = Setting::new("app.recording_pad_after_secs", || 180);

/// Longest live recording accepted (a guard against bad EPG data).
pub const MAX_RECORDING: Duration = Duration::from_secs(8 * 3600);

/// `<Videos>/opensmarters` (e.g. `C:\Users\<you>\Videos\opensmarters`), else
/// `<data>/recordings` when the system has no Videos folder.
pub fn default_recordings_dir(data_dir: Option<&Path>) -> Option<PathBuf> {
    directories::UserDirs::new()
        .and_then(|d| d.video_dir().map(|v| v.join("opensmarters")))
        .or_else(|| data_dir.map(|d| d.join("recordings")))
}

/// Max characters of the title part of a file name (the whole name stays well under the
/// store's 180-character limit).
const MAX_TITLE_PART: usize = 100;

fn is_device_name(word: &str) -> bool {
    let w = word.to_ascii_uppercase();
    matches!(w.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((w.starts_with("COM") || w.starts_with("LPT"))
            && w.len() == 4
            && w.as_bytes()[3].is_ascii_digit())
}

/// A safe, readable file name: `"<title> <yyyy-mm-dd HH-MM>.<ext>"` (local time), e.g.
/// `"Evening News 2026-10-20 20-00.ts"`. Provider text is reduced to letters, digits,
/// spaces and a few punctuation marks (`- _ ( ) ' , &`); a title that is a reserved
/// Windows device name gets a `"Recording "` prefix. `ext` is one of `ts`, `mkv`, `mp4`
/// (anything else becomes `ts`).
///
/// The result always passes `opensmarters_store::recordings::validate_file_name` and
/// `MediaOutput::new`'s name checks; add a suffix (`" (2)"`) before the extension yourself
/// if the name is taken.
pub fn file_name(title: &str, when: DateTime<Utc>, ext: &str) -> String {
    let mut t = String::new();
    let mut last_space = true;
    for c in title.chars() {
        if t.chars().count() >= MAX_TITLE_PART {
            break;
        }
        let keep = c.is_alphanumeric() || matches!(c, '-' | '_' | '(' | ')' | '\'' | ',' | '&');
        if keep {
            t.push(c);
            last_space = false;
        } else if !last_space {
            t.push(' ');
            last_space = true;
        }
    }
    let mut t = t.trim().to_owned();
    if t.is_empty() {
        t = "Recording".to_owned();
    } else if is_device_name(t.split(' ').next().unwrap_or_default()) {
        t.insert_str(0, "Recording ");
    }
    let ext = match ext.to_ascii_lowercase().as_str() {
        "mkv" => "mkv",
        "mp4" => "mp4",
        _ => "ts",
    };
    let stamp = when.with_timezone(&Local).format("%Y-%m-%d %H-%M");
    format!("{t} {stamp}.{ext}")
}

/// A programme to record (from the Guide or the player).
#[derive(Debug, Clone, PartialEq)]
pub struct ProgrammeToRecord {
    /// Live channel key (Xtream `stream_id` or the app's M3U id). Never a URL.
    pub stream_id: String,
    /// Channel name.
    pub channel_name: String,
    /// Channel logo URL.
    pub channel_logo: Option<String>,
    /// Programme title.
    pub title: String,
    /// Programme start.
    pub start: DateTime<Utc>,
    /// Programme end.
    pub end: DateTime<Utc>,
}

/// Schedules a recording of a programme (blocking store call: run it off the UI thread).
/// Returns the existing recording if this programme is already scheduled or recording.
///
/// # Errors
///
/// [`StoreError::Invalid`] for a URL-like channel id, an end before the start or a
/// programme longer than [`MAX_RECORDING`]; other store errors.
pub fn schedule_programme(
    store: &Store,
    profile: ProfileId,
    p: &ProgrammeToRecord,
) -> Result<Recording, StoreError> {
    let start = Timestamp::from_unix_millis(p.start.timestamp_millis());
    let end = Timestamp::from_unix_millis(p.end.timestamp_millis());
    if end.duration_since(start) > MAX_RECORDING {
        return Err(StoreError::Invalid {
            field: "end_at",
            reason: "programme is longer than the longest recording allowed",
        });
    }
    if let Some(existing) = store.recordings().find_live(profile, &p.stream_id, start)? {
        return Ok(existing);
    }
    store.recordings().add(
        profile,
        &NewRecording {
            kind: RecordingKind::Live,
            state: RecordingState::Scheduled,
            item_id: p.stream_id.clone(),
            series_id: None,
            season: None,
            episode: None,
            container_extension: None,
            title: p.title.clone(),
            subtitle: Some(p.channel_name.clone()),
            icon_url: p.channel_logo.clone(),
            start_at: Some(start),
            end_at: Some(end),
        },
    )
}

/// App-wide recording/download state (`cx.dvr`). **Stub**: Phase 7 fills it in (engine,
/// job tasks, progress shown by the Recordings screen and the player).
#[derive(Debug, Default)]
pub struct Dvr {
    _todo: (),
}

impl Dvr {
    /// Stops every running job (app exit). Must not block for long.
    pub fn shutdown(&mut self) {}
}

/// Called once per frame after the player update (start due recordings, collect progress).
/// **Stub.**
pub fn on_frame(_cx: &mut crate::screens::AppCtx<'_>) {}

/// Called when a profile session opens (resume its queued jobs). **Stub.**
pub fn on_session_opened(
    _cx: &mut crate::screens::AppCtx<'_>,
    _session: &std::sync::Arc<crate::services::Session>,
) {
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use opensmarters_core::chrono::TimeZone;
    use opensmarters_store::recordings::validate_file_name;
    use opensmarters_store::{
        MemorySecretStore, NewProfile, NewProfileSource, ProfileSettings, StoreOptions,
    };

    use super::*;

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, h, 0, 0)
            .single()
            .unwrap_or_default()
    }

    #[test]
    fn file_names_are_safe_and_readable() {
        let long = "x".repeat(500);
        for title in [
            "Evening News",
            "FR | Le Film (2003)",
            "../../Windows/System32/evil",
            "C:\\boot.ini",
            "CON",
            "nul",
            "COM1 live",
            "  ...  ",
            "100% Live: «Match» ★★★ 20:00?",
            long.as_str(),
            "\u{0}\u{1}\n",
            "日本語のタイトル",
        ] {
            let name = file_name(title, at(20), "ts");
            assert!(validate_file_name(&name).is_ok(), "{title:?} -> {name:?}");
            assert!(!name.contains('%') && !name.contains(".."), "{name:?}");
            assert!(name.ends_with(".ts"), "{name:?}");
        }
        assert!(file_name("Evening News", at(20), "mkv").starts_with("Evening News 2026-10-"));
        assert!(file_name("x", at(20), "exe").ends_with(".ts"));
        assert!(file_name("CON", at(20), "ts").starts_with("Recording CON"));
        assert!(file_name("", at(20), "ts").starts_with("Recording "));
    }

    #[test]
    fn scheduling_twice_returns_the_same_recording() {
        let store = Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let profile = store
            .profiles()
            .create(NewProfile {
                name: "Test".into(),
                source: NewProfileSource::M3uFile {
                    path: PathBuf::from("synthetic.m3u"),
                },
                settings: ProfileSettings::default(),
            })
            .unwrap_or_else(|e| panic!("{e}"));
        let p = ProgrammeToRecord {
            stream_id: "1001".into(),
            channel_name: "Synthetic One".into(),
            channel_logo: None,
            title: "News".into(),
            start: at(20),
            end: at(21),
        };
        let a = schedule_programme(&store, profile.id, &p).unwrap_or_else(|e| panic!("{e}"));
        let b = schedule_programme(&store, profile.id, &p).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.id, b.id);
        assert_eq!(a.state, RecordingState::Scheduled);
        let too_long = ProgrammeToRecord {
            end: at(20) + opensmarters_core::chrono::Duration::hours(9),
            ..p
        };
        assert!(schedule_programme(&store, profile.id, &too_long).is_err());
    }
}

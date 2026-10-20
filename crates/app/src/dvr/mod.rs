// SPDX-License-Identifier: GPL-3.0-or-later
//! Recordings and downloads ("DVR"): films and episodes saved for offline viewing, live
//! channels recorded now or on a schedule, and clips saved from the player's buffer.
//!
//! | Piece | Where |
//! |---|---|
//! | Job list, progress, file names (never URLs) | store: `store.recordings()` ([`opensmarters_store::Recordings`]) |
//! | Where files go | [`RECORDINGS_DIR`] (user setting) else [`default_recordings_dir`]; see [`DvrSettings`] |
//! | Safe file names | [`file_name`] (recordings), [`files::download_name`] (films, episodes) |
//! | The engine (scheduler, download and recording jobs) | [`engine`], [`download`], [`record`] |
//! | Stream sources and playing saved files | [`source`] |
//! | Clips, recording through the player, resume points of recordings | [`player`] |
//! | Screens' hooks (detail pages, player OSD, Live TV badges) | [`ui`] |
//! | Scheduling a programme from the Guide | [`schedule_programme`] (a store insert; the engine starts it) |
//! | Connections | `session.connections()`: downloads are preemptible, recordings are not |
//!
//! Scheduled recordings store the **programme's** start and end; the engine starts
//! [`PAD_BEFORE`] early and stops [`PAD_AFTER`] late (user settings). A recording "until
//! I stop" stores `end = start + MAX_RECORDING` ([`is_until_stopped`]) and gets no
//! padding.
//!
//! `cx.dvr` ([`Dvr`]) is the UI handle: [`on_session_opened`] starts the engine of the
//! profile (stopping the previous one), [`on_frame`] keeps a snapshot of the job list for
//! the screens, handles the player's results, and [`Dvr::shutdown`] stops every job at
//! exit (their rows are marked paused and continue next time).

pub mod download;
pub mod engine;
pub mod files;
pub mod jobs;
pub mod player;
pub mod record;
pub mod source;
pub mod ui;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opensmarters_core::chrono::{DateTime, Local, Utc};
use opensmarters_store::settings::Setting;
use opensmarters_store::{
    NewRecording, ProfileId, Recording, RecordingId, RecordingKind, RecordingState, Store,
    StoreError, Timestamp,
};

use crate::screens::AppCtx;
use crate::services::{Paths, Session};
use crate::task::Loader;

/// The recordings folder chosen by the user (`None` = [`default_recordings_dir`]).
pub const RECORDINGS_DIR: Setting<Option<PathBuf>> = Setting::new("app.recordings_dir", || None);

/// Seconds a scheduled recording starts before the programme.
pub const PAD_BEFORE: Setting<u32> = Setting::new("app.recording_pad_before_secs", || 60);

/// Seconds a scheduled recording continues after the programme.
pub const PAD_AFTER: Setting<u32> = Setting::new("app.recording_pad_after_secs", || 180);

/// How many films/episodes download at the same time (1 or 2).
pub const DOWNLOADS_AT_ONCE: Setting<u8> = Setting::new("app.downloads_at_once", || 1);

/// Jobs stop when less than this many GB are free on the recordings disk.
pub const KEEP_FREE_GB: Setting<u32> = Setting::new("app.recordings_keep_free_gb", || 2);

/// The clip length the player offers first ("Save last minutes"), in minutes.
pub const CLIP_MINUTES: Setting<u32> = Setting::new("app.clip_minutes", || 5);

/// Longest live recording accepted (a guard against bad EPG data).
pub const MAX_RECORDING: Duration = Duration::from_secs(8 * 3600);

/// Choices offered in Settings (minutes / GB / count).
pub const PAD_BEFORE_CHOICES: [u32; 4] = [0, 1, 2, 5];
/// Choices offered in Settings (minutes).
pub const PAD_AFTER_CHOICES: [u32; 4] = [0, 2, 5, 10];
/// Choices offered in Settings.
pub const AT_ONCE_CHOICES: [u8; 2] = [1, 2];
/// Choices offered in Settings (GB).
pub const KEEP_FREE_CHOICES: [u32; 4] = [1, 2, 5, 10];
/// Clip lengths (minutes), in the player and in Settings.
pub const CLIP_CHOICES: [u32; 4] = [1, 5, 15, 30];

/// `<Videos>/opensmarters` (e.g. `C:\Users\<you>\Videos\opensmarters`), else
/// `<data>/recordings` when the system has no Videos folder.
pub fn default_recordings_dir(data_dir: Option<&Path>) -> Option<PathBuf> {
    directories::UserDirs::new()
        .and_then(|d| d.video_dir().map(|v| v.join("opensmarters")))
        .or_else(|| data_dir.map(|d| d.join("recordings")))
}

/// Where `--ephemeral` runs without `--data-dir` save (a temporary folder: they must not
/// touch the user's Videos folder).
pub fn ephemeral_recordings_dir() -> PathBuf {
    std::env::temp_dir()
        .join("opensmarters-ephemeral")
        .join("recordings")
}

/// The recording/download settings, read from the store (blocking).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvrSettings {
    /// The folder in effect.
    pub dir: Option<PathBuf>,
    /// True when the user chose it (else the default).
    pub custom_dir: bool,
    /// [`PAD_BEFORE`] (seconds).
    pub pad_before: u32,
    /// [`PAD_AFTER`] (seconds).
    pub pad_after: u32,
    /// [`DOWNLOADS_AT_ONCE`].
    pub at_once: u8,
    /// [`KEEP_FREE_GB`].
    pub keep_free_gb: u32,
    /// [`CLIP_MINUTES`].
    pub clip_minutes: u32,
}

impl Default for DvrSettings {
    fn default() -> Self {
        Self {
            dir: None,
            custom_dir: false,
            pad_before: PAD_BEFORE.default_value(),
            pad_after: PAD_AFTER.default_value(),
            at_once: DOWNLOADS_AT_ONCE.default_value(),
            keep_free_gb: KEEP_FREE_GB.default_value(),
            clip_minutes: CLIP_MINUTES.default_value(),
        }
    }
}

impl DvrSettings {
    /// Reads every setting (missing or unreadable ones take their default). Blocking.
    pub fn load(store: &Store, paths: &Paths, ephemeral: bool) -> Self {
        let st = store.settings();
        let custom = st.get(&RECORDINGS_DIR).ok().flatten();
        let fallback = if ephemeral && paths.data_dir.is_none() {
            Some(ephemeral_recordings_dir())
        } else {
            default_recordings_dir(paths.data_dir.as_deref())
        };
        Self {
            custom_dir: custom.is_some(),
            dir: custom.or(fallback),
            pad_before: st.get(&PAD_BEFORE).unwrap_or(60).min(3600),
            pad_after: st.get(&PAD_AFTER).unwrap_or(180).min(3600),
            at_once: st.get(&DOWNLOADS_AT_ONCE).unwrap_or(1).clamp(1, 2),
            keep_free_gb: st.get(&KEEP_FREE_GB).unwrap_or(2).min(1024),
            clip_minutes: st.get(&CLIP_MINUTES).unwrap_or(5).clamp(1, 30),
        }
    }

    /// [`Self::keep_free_gb`] in bytes.
    pub fn keep_free_bytes(&self) -> u64 {
        u64::from(self.keep_free_gb) * 1024 * 1024 * 1024
    }
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

/// Provider text reduced to letters, digits, spaces and `- _ ( ) ' , &` (at most 100
/// characters; `"Recording"` when nothing is left; a reserved device name gets a
/// `"Recording "` prefix).
pub(crate) fn clean_title(title: &str) -> String {
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
    t
}

/// A safe, readable file name: `"<title> <yyyy-mm-dd HH-MM>.<ext>"` (local time), e.g.
/// `"Evening News 2026-10-20 20-00.ts"`. Provider text is reduced to letters, digits,
/// spaces and a few punctuation marks (`- _ ( ) ' , &`); a title that is a reserved
/// Windows device name gets a `"Recording "` prefix. `ext` is one of `ts`, `mkv`, `mp4`
/// (anything else becomes `ts`).
///
/// The result always passes `opensmarters_store::recordings::validate_file_name` and
/// `MediaOutput::new`'s name checks; add a suffix (`" (2)"`) before the extension yourself
/// if the name is taken ([`files::unique_name`]).
pub fn file_name(title: &str, when: DateTime<Utc>, ext: &str) -> String {
    let t = clean_title(title);
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
/// A programme already on air starts at once (the engine sees it is due).
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

/// Starts recording a channel now "until I stop" (when there is no guide), capped at
/// [`MAX_RECORDING`]. Blocking store call.
///
/// # Errors
///
/// Store errors (a URL-like channel id).
pub fn record_until_stopped(
    store: &Store,
    profile: ProfileId,
    stream_id: &str,
    channel_name: &str,
    channel_logo: Option<String>,
    now: DateTime<Utc>,
) -> Result<Recording, StoreError> {
    let start = Timestamp::from_unix_millis(now.timestamp_millis());
    store.recordings().add(
        profile,
        &NewRecording {
            kind: RecordingKind::Live,
            state: RecordingState::Queued,
            item_id: stream_id.to_owned(),
            series_id: None,
            season: None,
            episode: None,
            container_extension: None,
            title: channel_name.to_owned(),
            subtitle: Some(channel_name.to_owned()),
            icon_url: channel_logo,
            start_at: Some(start),
            end_at: Some(start.saturating_add(MAX_RECORDING)),
        },
    )
}

/// True for a live recording started "until I stop" (no programme, no padding).
pub fn is_until_stopped(r: &Recording) -> bool {
    match (r.start_at, r.end_at) {
        (Some(s), Some(e)) => e.duration_since(s) == MAX_RECORDING,
        _ => false,
    }
}

/// Queues a film or episode download (blocking). An existing download of the same item
/// is returned instead (a failed or paused one is queued again).
///
/// # Errors
///
/// Store errors.
pub fn queue_download(
    store: &Store,
    profile: ProfileId,
    new: &NewRecording,
) -> Result<Recording, StoreError> {
    let existing = store
        .recordings()
        .list(profile)?
        .into_iter()
        .find(|r| r.kind == new.kind && r.item_id == new.item_id);
    if let Some(r) = existing {
        if matches!(r.state, RecordingState::Failed | RecordingState::Paused) {
            store
                .recordings()
                .set_state(r.id, RecordingState::Queued, None)?;
        }
        return Ok(store.recordings().get(r.id)?.unwrap_or(r));
    }
    store.recordings().add(profile, new)
}

/// App-wide recording/download state (`cx.dvr`). See the [module docs](self).
#[derive(Default)]
pub struct Dvr {
    engine: Option<engine::Engine>,
    rows: Loader<Arc<Vec<Recording>>>,
    rows_epoch: Option<u64>,
    snapshot: Arc<Vec<Recording>>,
    pub(crate) player: player::PlayerSide,
    pub(crate) ui: ui::UiState,
}

impl std::fmt::Debug for Dvr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dvr")
            .field("engine", &self.engine)
            .field("rows", &self.snapshot.len())
            .finish_non_exhaustive()
    }
}

impl Dvr {
    /// Stops every running job (app exit). Rows are marked paused at once (a small store
    /// write each); the jobs continue the next time the profile opens.
    pub fn shutdown(&mut self) {
        if let Some(e) = self.engine.take() {
            e.shutdown();
        }
    }

    /// The engine of the open profile, if started.
    pub fn engine(&self) -> Option<&engine::Engine> {
        self.engine.as_ref()
    }

    /// The last snapshot of the profile's recordings and downloads (newest first).
    pub fn rows(&self) -> &Arc<Vec<Recording>> {
        &self.snapshot
    }

    /// True once the first snapshot arrived.
    pub fn is_loaded(&self) -> bool {
        self.rows_epoch.is_some()
    }

    /// Changes whenever a row changes state.
    pub fn epoch(&self) -> u64 {
        self.engine.as_ref().map_or(0, engine::Engine::epoch)
    }

    /// One row of the snapshot.
    pub fn row(&self, id: RecordingId) -> Option<&Recording> {
        self.snapshot.iter().find(|r| r.id == id)
    }

    /// The download of a film/episode (any state), if there is one.
    pub fn download_of(&self, kind: RecordingKind, item_id: &str) -> Option<&Recording> {
        self.snapshot.iter().find(|r| {
            r.kind == kind && r.item_id == item_id && r.state != RecordingState::Cancelled
        })
    }

    /// Live progress of a running job.
    pub fn progress(&self, id: RecordingId) -> Option<engine::Progress> {
        self.engine.as_ref().and_then(|e| e.progress(id))
    }

    /// Channel keys being recorded now (● REC badges).
    pub fn recording_keys(&self) -> HashSet<String> {
        self.engine
            .as_ref()
            .map(engine::Engine::recording_keys)
            .unwrap_or_default()
    }

    /// True while channel `key` is being recorded.
    pub fn is_recording(&self, key: &str) -> bool {
        self.recording_keys().contains(key)
    }

    /// The settings in effect (defaults until the engine read them).
    pub fn settings(&self) -> DvrSettings {
        self.engine
            .as_ref()
            .map(engine::Engine::settings)
            .unwrap_or_default()
    }

    /// The recordings folder in effect.
    pub fn dir(&self) -> Option<PathBuf> {
        self.settings().dir
    }

    /// Settings changed (Settings › Recordings & downloads): the engine reloads them.
    pub fn reload_settings(&self) {
        if let Some(e) = &self.engine {
            e.reload_settings();
        }
    }

    fn services(&self) -> Option<crate::services::Services> {
        self.engine.as_ref().map(|e| e.session().services().clone())
    }

    fn profile(&self) -> Option<ProfileId> {
        self.engine.as_ref().map(engine::Engine::profile)
    }

    /// Runs a store write for a user action, then wakes the engine.
    fn act(
        &self,
        what: &'static str,
        f: impl FnOnce(&Store) -> Result<(), StoreError> + Send + 'static,
    ) {
        let (Some(services), Some(e)) = (self.services(), &self.engine) else {
            return;
        };
        let wake = e.waker();
        services.blocking(what, move |s| {
            let r = f(s);
            wake();
            r
        });
    }

    /// Queues a download (see [`queue_download`]).
    pub fn queue(&self, new: NewRecording) {
        let Some(profile) = self.profile() else {
            return;
        };
        self.act("queue a download", move |s| {
            queue_download(s, profile, &new).map(|_| ())
        });
    }

    /// Queues several downloads at once (a season).
    pub fn queue_all(&self, list: Vec<NewRecording>) {
        let Some(profile) = self.profile() else {
            return;
        };
        self.act("queue downloads", move |s| {
            for new in &list {
                queue_download(s, profile, new)?;
            }
            Ok(())
        });
    }

    /// Pauses a download (running: it stops and keeps its part file).
    pub fn pause(&self, id: RecordingId) {
        if self
            .engine
            .as_ref()
            .is_some_and(|e| e.control(id, engine::Control::Pause))
        {
            return;
        }
        self.act("pause a download", move |s| {
            if let Some(r) = s.recordings().get(id)?
                && r.state == RecordingState::Queued
            {
                s.recordings().set_state(id, RecordingState::Paused, None)?;
            }
            Ok(())
        });
    }

    /// Resumes a paused download, or retries a failed one (queued again).
    pub fn resume(&self, id: RecordingId) {
        self.act("resume a download", move |s| {
            s.recordings()
                .set_state(id, RecordingState::Queued, None)
                .map(|_| ())
        });
    }

    /// Stops a live recording now and keeps what was recorded.
    pub fn stop_recording(&mut self, id: RecordingId) {
        if self.engine.as_ref().is_some_and(|e| e.is_player_owned(id)) {
            self.player.stop_requested = Some(id);
            return;
        }
        if self
            .engine
            .as_ref()
            .is_some_and(|e| e.control(id, engine::Control::Finish))
        {
            return;
        }
        // Not running (waiting for a connection): nothing recorded yet, so it's a cancel.
        self.cancel(id);
    }

    /// Cancels a job: stops it, deletes its partial file and the row. Scheduled
    /// recordings are simply removed.
    pub fn cancel(&self, id: RecordingId) {
        if self
            .engine
            .as_ref()
            .is_some_and(|e| e.control(id, engine::Control::Cancel))
        {
            return;
        }
        self.delete(id);
    }

    /// Deletes a recording or download: its file (and `.part`) and its row.
    pub fn delete(&self, id: RecordingId) {
        if self
            .engine
            .as_ref()
            .is_some_and(|e| e.control(id, engine::Control::Cancel))
        {
            return;
        }
        let dir = self.dir();
        self.act("delete a recording", move |s| {
            if let (Some(dir), Some(r)) = (dir, s.recordings().get(id)?)
                && let Some(name) = r.file_name
                && !files::delete(&dir, &name)
            {
                return Err(StoreError::Invalid {
                    field: "file_name",
                    reason: "the file couldn't be deleted",
                });
            }
            s.recordings().remove(id).map(|_| ())
        });
    }
}

/// Called once per frame before the screen: refreshes the job snapshot when something
/// changed, handles the player's clip/record results, records through the player when a
/// waiting recording's channel is playing, saves the resume point of a played recording,
/// and gives Esc to an open DVR dialog.
pub fn on_frame(cx: &mut AppCtx<'_>) {
    cx.dvr.rows.poll();
    if let Some(rows) = cx.dvr.rows.take() {
        cx.dvr.snapshot = rows;
    }
    if let Some(e) = &cx.dvr.engine {
        let epoch = e.epoch();
        if cx.dvr.rows_epoch != Some(epoch) && !cx.dvr.rows.is_loading() {
            cx.dvr.rows_epoch = Some(epoch);
            let store = Arc::clone(&cx.services.store);
            let profile = e.profile();
            cx.dvr
                .rows
                .start_blocking(cx.services.rt(), &cx.egui, move || {
                    Ok(Arc::new(store.recordings().list(profile)?))
                });
        }
    }
    player::on_frame(cx);
    ui::on_frame(cx);
}

/// Called when a profile session opens: stops the previous profile's jobs and starts this
/// profile's engine (recovery first: interrupted jobs continue).
pub fn on_session_opened(cx: &mut AppCtx<'_>, session: &Arc<Session>) {
    if let Some(old) = cx.dvr.engine.take() {
        old.shutdown();
    }
    cx.dvr.rows.cancel();
    cx.dvr.rows_epoch = None;
    cx.dvr.snapshot = Arc::new(Vec::new());
    cx.dvr.player = player::PlayerSide::default();
    match engine::Engine::start(Arc::clone(session)) {
        Ok(e) => cx.dvr.engine = Some(e),
        Err(e) => {
            tracing::warn!(error = %e, "recordings engine unavailable");
        }
    }
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

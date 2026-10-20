// SPDX-License-Identifier: GPL-3.0-or-later
//! Recordings and downloads: films and episodes saved for offline viewing, live channels
//! recorded now or on a schedule, and clips saved from the player's buffer.
//!
//! The store only keeps the job list and progress. The files live in the user's
//! recordings folder (an app setting); [`Recording::file_name`] is a bare file name inside
//! it, validated by [`validate_file_name`]. A row **never** holds a stream URL: the app
//! rebuilds it from [`Recording::item_id`] and the profile's credentials when it starts the
//! job, so nothing secret reaches the database.
//!
//! ## States
//!
//! ```text
//! scheduled ──(start time)──▶ queued ──▶ active ──▶ done
//!                               ▲  │        │
//!                               │  ▼        ├──▶ failed
//!                             paused ◀──────┘
//! any state ──▶ cancelled (user)
//! ```
//!
//! `active` rows left behind by a crash are moved back to `paused` by
//! [`Recordings::recover_interrupted`] at startup.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, StoreError, map_fk};
use crate::ids::{ProfileId, sql_text_enum};
use crate::text::{
    MAX_TITLE_CHARS, MAX_TOKEN_BYTES, MAX_URL_BYTES, has_control, sanitize_display, sanitize_opt,
    validate_item_id,
};
use crate::time::Timestamp;

/// Max length (chars) of a recording's file name.
pub const MAX_FILE_NAME_CHARS: usize = 180;
/// Max length (chars) of a stored error message.
const MAX_ERROR_CHARS: usize = 300;

/// Row id of a recording.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordingId(pub i64);

impl std::fmt::Display for RecordingId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What a recording is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingKind {
    /// A film downloaded for offline viewing (`item_id` = VOD `stream_id`).
    Movie,
    /// A series episode downloaded for offline viewing (`item_id` = episode id).
    Episode,
    /// A live channel recorded now or on a schedule (`item_id` = live `stream_id`).
    Live,
    /// The last minutes of a live channel saved from the player's buffer.
    Clip,
}

impl RecordingKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Episode => "episode",
            Self::Live => "live",
            Self::Clip => "clip",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "movie" => Some(Self::Movie),
            "episode" => Some(Self::Episode),
            "live" => Some(Self::Live),
            "clip" => Some(Self::Clip),
            _ => None,
        }
    }

    /// Films and episodes are downloads; live and clips are recordings.
    pub const fn is_download(self) -> bool {
        matches!(self, Self::Movie | Self::Episode)
    }
}
sql_text_enum!(RecordingKind);

/// Where a recording is in its life cycle (see the [module docs](self)).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingState {
    /// Waits for its start time ([`Recording::start_at`]).
    Scheduled,
    /// Waits for a free connection.
    Queued,
    /// Downloading / recording now.
    Active,
    /// Stopped part-way (by the user, a lost connection or another stream that needed the
    /// connection); can resume.
    Paused,
    /// Finished; the file is complete.
    Done,
    /// Gave up ([`Recording::error`] says why).
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

impl RecordingState {
    /// Every state.
    pub const ALL: [Self; 7] = [
        Self::Scheduled,
        Self::Queued,
        Self::Active,
        Self::Paused,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Queued => "queued",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Finished one way or another (done, failed, cancelled).
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}
sql_text_enum!(RecordingState);

/// Input for [`Recordings::add`].
#[derive(Clone, Debug, PartialEq)]
pub struct NewRecording {
    /// What it is.
    pub kind: RecordingKind,
    /// Initial state: [`RecordingState::Scheduled`] (needs `start_at` and `end_at`),
    /// [`RecordingState::Queued`] or [`RecordingState::Active`].
    pub state: RecordingState,
    /// Provider id (see [`RecordingKind`]). Never a URL.
    pub item_id: String,
    /// Series id, for episodes.
    pub series_id: Option<String>,
    /// Season number, for episodes.
    pub season: Option<u32>,
    /// Episode number, for episodes.
    pub episode: Option<u32>,
    /// `container_extension` of a film or episode (to rebuild its URL).
    pub container_extension: Option<String>,
    /// Title (programme, film or episode). Provider text: sanitised and capped.
    pub title: String,
    /// Channel or series name. Provider text: sanitised and capped.
    pub subtitle: Option<String>,
    /// Poster or channel logo URL (dropped if unusable).
    pub icon_url: Option<String>,
    /// Programme start (live).
    pub start_at: Option<Timestamp>,
    /// Programme end (live).
    pub end_at: Option<Timestamp>,
}

impl NewRecording {
    /// A download of a film or episode, queued.
    pub fn download(
        kind: RecordingKind,
        item_id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            state: RecordingState::Queued,
            item_id: item_id.into(),
            series_id: None,
            season: None,
            episode: None,
            container_extension: None,
            title: title.into(),
            subtitle: None,
            icon_url: None,
            start_at: None,
            end_at: None,
        }
    }
}

/// One recording or download.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// Row id.
    pub id: RecordingId,
    /// Owning profile.
    pub profile: ProfileId,
    /// What it is.
    pub kind: RecordingKind,
    /// Life-cycle state.
    pub state: RecordingState,
    /// Provider id (see [`RecordingKind`]).
    pub item_id: String,
    /// Series id, for episodes.
    pub series_id: Option<String>,
    /// Season number.
    pub season: Option<u32>,
    /// Episode number.
    pub episode: Option<u32>,
    /// Container extension of a film or episode.
    pub container_extension: Option<String>,
    /// Title.
    pub title: String,
    /// Channel or series name.
    pub subtitle: Option<String>,
    /// Poster or logo URL.
    pub icon_url: Option<String>,
    /// Programme start (live).
    pub start_at: Option<Timestamp>,
    /// Programme end (live).
    pub end_at: Option<Timestamp>,
    /// File name inside the recordings folder, once the job started.
    pub file_name: Option<String>,
    /// Bytes written so far.
    pub bytes_done: u64,
    /// Expected size, when the server said.
    pub bytes_total: Option<u64>,
    /// Media duration (seconds), once known.
    pub duration_secs: Option<f64>,
    /// Resume point of the local copy (seconds).
    pub position_secs: Option<f64>,
    /// Why it failed or paused (plain language, for the UI).
    pub error: Option<String>,
    /// Created.
    pub created_at: Timestamp,
    /// Last change.
    pub updated_at: Timestamp,
}

impl Recording {
    /// Download progress 0..=1, when the total size is known.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.bytes_total.filter(|t| *t > 0)?;
        Some((self.bytes_done as f64 / total as f64).clamp(0.0, 1.0))
    }
}

const COLUMNS: &str = "id, profile_id, kind, state, item_id, series_id, season, episode,
    container_extension, title, subtitle, icon_url, start_at, end_at, file_name, bytes_done,
    bytes_total, duration_secs, position_secs, error, created_at, updated_at";

fn opt_u32(v: Option<i64>) -> Option<u32> {
    v.and_then(|n| u32::try_from(n).ok())
}

fn opt_u64(v: Option<i64>) -> Option<u64> {
    v.and_then(|n| u64::try_from(n).ok())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Recording> {
    Ok(Recording {
        id: RecordingId(row.get(0)?),
        profile: row.get(1)?,
        kind: row.get(2)?,
        state: row.get(3)?,
        item_id: row.get(4)?,
        series_id: row.get(5)?,
        season: opt_u32(row.get(6)?),
        episode: opt_u32(row.get(7)?),
        container_extension: row.get(8)?,
        title: row.get(9)?,
        subtitle: row.get(10)?,
        icon_url: row.get(11)?,
        start_at: row.get(12)?,
        end_at: row.get(13)?,
        file_name: row.get(14)?,
        bytes_done: u64::try_from(row.get::<_, i64>(15)?).unwrap_or(0),
        bytes_total: opt_u64(row.get(16)?),
        duration_secs: row.get(17)?,
        position_secs: row.get(18)?,
        error: row.get(19)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
    })
}

/// Checks a recording's file name: a bare name (no folders), 1–180 characters, no
/// control characters, none of `< > : " / \ | ? *`, no leading/trailing dot or space,
/// not `.`/`..` and not a reserved Windows device name (`CON`, `NUL`, `COM1`…, with or
/// without an extension).
///
/// The app builds names with its own sanitiser; this is the last line of defence against
/// a provider title escaping the recordings folder.
pub fn validate_file_name(name: &str) -> Result<&str> {
    let bad = |reason| Err(StoreError::invalid("file_name", reason));
    let count = name.chars().count();
    if count == 0 || count > MAX_FILE_NAME_CHARS {
        return bad("must be 1 to 180 characters");
    }
    if has_control(name) || name.chars().any(|c| "<>:\"/\\|?*".contains(c)) {
        return bad("contains a character that is not allowed in file names");
    }
    if name.starts_with(['.', ' ']) || name.ends_with(['.', ' ']) {
        return bad("must not start or end with a dot or a space");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return bad("is a reserved device name");
    }
    Ok(name)
}

fn validate_new(r: &NewRecording) -> Result<()> {
    validate_item_id("item_id", &r.item_id)?;
    if let Some(s) = &r.series_id {
        validate_item_id("series_id", s)?;
    }
    if r.state == RecordingState::Scheduled && (r.start_at.is_none() || r.end_at.is_none()) {
        return Err(StoreError::invalid(
            "start_at",
            "a scheduled recording needs a start and an end",
        ));
    }
    if let (Some(s), Some(e)) = (r.start_at, r.end_at)
        && e <= s
    {
        return Err(StoreError::invalid("end_at", "must be after the start"));
    }
    if !matches!(
        r.state,
        RecordingState::Scheduled | RecordingState::Queued | RecordingState::Active
    ) {
        return Err(StoreError::invalid(
            "state",
            "a new recording starts scheduled, queued or active",
        ));
    }
    Ok(())
}

/// Recording operations. Obtain with [`Store::recordings`].
#[derive(Clone, Copy, Debug)]
pub struct Recordings<'a> {
    pub(crate) store: &'a Store,
}

impl Recordings<'_> {
    /// Adds a recording or download.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for a URL-like or empty id, a scheduled recording without
    /// times, an end before the start or an initial state other than scheduled / queued /
    /// active; [`StoreError::ProfileNotFound`].
    pub fn add(&self, profile: ProfileId, r: &NewRecording) -> Result<Recording> {
        validate_new(r)?;
        let ext = sanitize_opt(r.container_extension.as_deref(), MAX_TOKEN_BYTES)
            .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()));
        let now = self.store.now();
        let id = {
            let conn = self.store.conn();
            conn.execute(
                "INSERT INTO recordings (profile_id, kind, state, item_id, series_id, season,
                     episode, container_extension, title, subtitle, icon_url, start_at, end_at,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
                params![
                    profile,
                    r.kind,
                    r.state,
                    r.item_id,
                    r.series_id,
                    r.season,
                    r.episode,
                    ext,
                    sanitize_display(&r.title, MAX_TITLE_CHARS),
                    r.subtitle
                        .as_deref()
                        .map(|s| sanitize_display(s, MAX_TITLE_CHARS))
                        .filter(|s| !s.is_empty()),
                    sanitize_opt(r.icon_url.as_deref(), MAX_URL_BYTES),
                    r.start_at,
                    r.end_at,
                    now,
                ],
            )
            .map_err(map_fk(profile))?;
            conn.last_insert_rowid()
        };
        self.get(RecordingId(id))?
            .ok_or_else(|| StoreError::invalid("id", "recording vanished after insert"))
    }

    /// One recording.
    pub fn get(&self, id: RecordingId) -> Result<Option<Recording>> {
        Ok(self
            .store
            .conn()
            .prepare_cached(&format!("SELECT {COLUMNS} FROM recordings WHERE id = ?1"))?
            .query_row([id.0], from_row)
            .optional()?)
    }

    /// Every recording of a profile, newest first.
    pub fn list(&self, profile: ProfileId) -> Result<Vec<Recording>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM recordings WHERE profile_id = ?1
             ORDER BY created_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map([profile], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Recordings of a profile in one of `states`, oldest first (queue order).
    pub fn with_states(
        &self,
        profile: ProfileId,
        states: &[RecordingState],
    ) -> Result<Vec<Recording>> {
        Ok(self
            .list(profile)?
            .into_iter()
            .rev()
            .filter(|r| states.contains(&r.state))
            .collect())
    }

    /// Scheduled recordings of a profile whose start time is at or before `at` (use
    /// "now + lead time"), earliest first.
    pub fn due(&self, profile: ProfileId, at: Timestamp) -> Result<Vec<Recording>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM recordings
             WHERE profile_id = ?1 AND state = 'scheduled' AND start_at <= ?2
             ORDER BY start_at, id"
        ))?;
        let rows = stmt.query_map(params![profile, at], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The not-yet-final live recording of `stream_id` that covers the programme starting at
    /// `start_at`, if any (the Guide's "recording" marker).
    pub fn find_live(
        &self,
        profile: ProfileId,
        stream_id: &str,
        start_at: Timestamp,
    ) -> Result<Option<Recording>> {
        Ok(self
            .store
            .conn()
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM recordings
                 WHERE profile_id = ?1 AND kind = 'live' AND item_id = ?2 AND start_at = ?3
                   AND state NOT IN ('failed', 'cancelled')
                 ORDER BY id DESC LIMIT 1"
            ))?
            .query_row(params![profile, stream_id, start_at], from_row)
            .optional()?)
    }

    /// The not-yet-cancelled download of a film or episode, if any.
    pub fn find_download(
        &self,
        profile: ProfileId,
        kind: RecordingKind,
        item_id: &str,
    ) -> Result<Option<Recording>> {
        Ok(self
            .store
            .conn()
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM recordings
                 WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3
                   AND state NOT IN ('failed', 'cancelled')
                 ORDER BY id DESC LIMIT 1"
            ))?
            .query_row(params![profile, kind, item_id], from_row)
            .optional()?)
    }

    fn update(&self, id: RecordingId, set: &str, values: &[&dyn rusqlite::ToSql]) -> Result<bool> {
        let now = self.store.now();
        let mut p: Vec<&dyn rusqlite::ToSql> = values.to_vec();
        p.push(&now);
        p.push(&id.0);
        let n = self.store.conn().execute(
            &format!(
                "UPDATE recordings SET {set}, updated_at = ?{} WHERE id = ?{}",
                values.len() + 1,
                values.len() + 2
            ),
            p.as_slice(),
        )?;
        Ok(n > 0)
    }

    /// Sets the state and the user-facing message (`None` clears it). Returns `false` if the
    /// recording does not exist.
    pub fn set_state(
        &self,
        id: RecordingId,
        state: RecordingState,
        error: Option<&str>,
    ) -> Result<bool> {
        let error = error
            .map(|e| sanitize_display(e, MAX_ERROR_CHARS))
            .filter(|e| !e.is_empty());
        self.update(id, "state = ?1, error = ?2", &[&state, &error])
    }

    /// Records the file name (see [`validate_file_name`]).
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for an unsafe name.
    pub fn set_file_name(&self, id: RecordingId, file_name: &str) -> Result<bool> {
        let name = validate_file_name(file_name)?;
        self.update(id, "file_name = ?1", &[&name])
    }

    /// Updates the byte counters.
    pub fn set_progress(
        &self,
        id: RecordingId,
        bytes_done: u64,
        bytes_total: Option<u64>,
    ) -> Result<bool> {
        let done = i64::try_from(bytes_done).unwrap_or(i64::MAX);
        let total = bytes_total.map(|t| i64::try_from(t).unwrap_or(i64::MAX));
        self.update(id, "bytes_done = ?1, bytes_total = ?2", &[&done, &total])
    }

    /// Records the media duration (seconds; ignored unless finite and ≥ 0).
    pub fn set_duration(&self, id: RecordingId, secs: f64) -> Result<bool> {
        if !secs.is_finite() || secs < 0.0 {
            return Ok(false);
        }
        self.update(id, "duration_secs = ?1", &[&secs])
    }

    /// Saves the resume point of the local copy (`None` clears it).
    pub fn set_position(&self, id: RecordingId, secs: Option<f64>) -> Result<bool> {
        let secs = secs.filter(|s| s.is_finite() && *s >= 0.0);
        self.update(id, "position_secs = ?1", &[&secs])
    }

    /// Moves a scheduled recording (e.g. padding or an EPG change).
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] if `end_at` is not after `start_at`.
    pub fn reschedule(
        &self,
        id: RecordingId,
        start_at: Timestamp,
        end_at: Timestamp,
    ) -> Result<bool> {
        if end_at <= start_at {
            return Err(StoreError::invalid("end_at", "must be after the start"));
        }
        self.update(id, "start_at = ?1, end_at = ?2", &[&start_at, &end_at])
    }

    /// Deletes the row (the app deletes the file). Returns `false` if it did not exist.
    pub fn remove(&self, id: RecordingId) -> Result<bool> {
        let n = self
            .store
            .conn()
            .execute("DELETE FROM recordings WHERE id = ?1", [id.0])?;
        Ok(n > 0)
    }

    /// Startup recovery: `active` rows (the app stopped while they ran) become `paused`.
    /// Returns how many were changed.
    pub fn recover_interrupted(&self) -> Result<usize> {
        let now = self.store.now();
        Ok(self.store.conn().execute(
            "UPDATE recordings SET state = 'paused', updated_at = ?1 WHERE state = 'active'",
            [now],
        )?)
    }

    /// Every file name in use (any profile), so the app never reuses or deletes a name it
    /// still references.
    pub fn file_names(&self) -> Result<Vec<String>> {
        let conn = self.store.conn();
        let mut stmt =
            conn.prepare_cached("SELECT file_name FROM recordings WHERE file_name IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_validated() {
        for ok in [
            "News 2026-10-20 20-00.ts",
            "Film (2003).mkv",
            "Épisode 1.mp4",
            "a",
        ] {
            assert!(validate_file_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "..",
            ".hidden.ts",
            "trailing.",
            "trailing ",
            "a/b.ts",
            "a\\b.ts",
            "C:evil.ts",
            "x\u{0}.ts",
            "what?.ts",
            "CON",
            "con.ts",
            "COM1.mkv",
            "lpt9",
            "NUL .ts",
        ] {
            assert!(validate_file_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_file_name(&"a".repeat(MAX_FILE_NAME_CHARS)).is_ok());
        assert!(validate_file_name(&"a".repeat(MAX_FILE_NAME_CHARS + 1)).is_err());
        // COM10 is not a device name.
        assert!(validate_file_name("COM10.ts").is_ok());
    }

    #[test]
    fn states_round_trip() {
        for s in RecordingState::ALL {
            assert_eq!(RecordingState::parse(s.as_str()), Some(s));
        }
        for k in [
            RecordingKind::Movie,
            RecordingKind::Episode,
            RecordingKind::Live,
            RecordingKind::Clip,
        ] {
            assert_eq!(RecordingKind::parse(k.as_str()), Some(k));
        }
    }
}

//! Resume points and watch history for movies, series episodes and catch-up programmes.
//!
//! The app reports progress periodically (e.g. every 10 s and on stop) with
//! [`Playback::record_progress`]. An item counts as **finished** once its position reaches
//! [`ResumeThresholds::max_fraction`] of the duration (default 95 %), or when the player
//! hits end-of-file and the app calls [`Playback::mark_finished`]. Re-watching from the
//! start clears `finished` but keeps `ever_finished` (for "watched" ticks).
//!
//! *Continue watching* lists unfinished items past [`ResumeThresholds::min_position_secs`]
//! (default 30 s), most recent first, with episodes collapsed to one entry per series.
//! For *Up next*, [`Playback::up_next_candidates`] returns series whose latest watched
//! episode is finished; combine it with the series' episode list from the panel and
//! [`next_episode`] to pick what to offer.
//!
//! Item ids: `stream_id` (movie), episode `id` (episode), and an app-chosen stable id for
//! catch-up (e.g. `"<stream_id>@<start_timestamp>"`). URLs are rejected.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, StoreError, map_fk};
use crate::ids::{ProfileId, sql_text_enum};
use crate::text::{
    MAX_ITEM_ID_BYTES, MAX_TITLE_CHARS, MAX_TOKEN_BYTES, MAX_URL_BYTES, sanitize_display,
    sanitize_opt, validate_item_id,
};
use crate::time::Timestamp;

/// Upper bound on rows scanned when collapsing series in list queries.
const MAX_SCAN_ROWS: usize = 5000;

/// What kind of item a resume point belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackKind {
    /// A movie (VOD `stream_id`).
    Movie,
    /// A series episode (episode `id`).
    Episode,
    /// A catch-up (timeshift) programme.
    Catchup,
}

impl PlaybackKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Episode => "episode",
            Self::Catchup => "catchup",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "movie" => Some(Self::Movie),
            "episode" => Some(Self::Episode),
            "catchup" => Some(Self::Catchup),
            _ => None,
        }
    }
}
sql_text_enum!(PlaybackKind);

/// Display data captured with a resume point, so history rows render (and resume plays)
/// without refetching.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackSnapshot {
    /// Title (episode title, or "Series – S01E02", as the app prefers).
    pub title: String,
    /// Poster / cover URL.
    pub poster_url: Option<String>,
    /// Series id, for episodes.
    pub series_id: Option<String>,
    /// Season number, for episodes.
    pub season: Option<u32>,
    /// Episode number within the season, for episodes.
    pub episode: Option<u32>,
    /// `container_extension` needed to rebuild the stream URL (e.g. `"mkv"`).
    pub container_extension: Option<String>,
}

impl PlaybackSnapshot {
    fn sanitized(&self) -> Self {
        Self {
            title: sanitize_display(&self.title, MAX_TITLE_CHARS),
            poster_url: sanitize_opt(self.poster_url.as_deref(), MAX_URL_BYTES),
            series_id: sanitize_opt(self.series_id.as_deref(), MAX_ITEM_ID_BYTES),
            season: self.season,
            episode: self.episode,
            container_extension: sanitize_opt(self.container_extension.as_deref(), MAX_TOKEN_BYTES)
                .filter(|e| e.bytes().all(|b| b.is_ascii_alphanumeric())),
        }
    }
}

/// A stored resume point.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PlaybackProgress {
    /// Item kind.
    pub kind: PlaybackKind,
    /// Item id.
    pub item_id: String,
    /// Last known position in seconds.
    pub position_secs: f64,
    /// Duration in seconds, if known.
    pub duration_secs: Option<f64>,
    /// Watched to the end (≥ the finished threshold, or end-of-file).
    pub finished: bool,
    /// Has ever been finished (sticky).
    pub ever_finished: bool,
    /// Last update.
    pub updated_at: Timestamp,
    /// Display snapshot.
    pub snapshot: PlaybackSnapshot,
}

impl PlaybackProgress {
    /// `position / duration` in `0.0..=1.0`, if the duration is known.
    pub fn fraction(&self) -> Option<f64> {
        self.duration_secs
            .filter(|d| *d > 0.0)
            .map(|d| (self.position_secs / d).clamp(0.0, 1.0))
    }

    /// True if this point is worth offering "Resume from …" for.
    pub fn is_resumable(&self, t: &ResumeThresholds) -> bool {
        !self.finished
            && self.position_secs >= t.min_position_secs
            && self.fraction().is_none_or(|f| f < t.max_fraction)
    }
}

/// Thresholds for resume / continue-watching decisions.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ResumeThresholds {
    /// Positions below this are not worth resuming. Default: 30 s.
    pub min_position_secs: f64,
    /// At or above this fraction of the duration the item counts as finished.
    /// Default: 0.95.
    pub max_fraction: f64,
}

impl Default for ResumeThresholds {
    fn default() -> Self {
        Self {
            min_position_secs: 30.0,
            max_fraction: 0.95,
        }
    }
}

const COLUMNS: &str = "kind, item_id, position_secs, duration_secs, finished, ever_finished, \
     updated_at, title, poster_url, series_id, season, episode, container_extension";

fn progress_from_row(row: &Row<'_>) -> rusqlite::Result<PlaybackProgress> {
    Ok(PlaybackProgress {
        kind: row.get(0)?,
        item_id: row.get(1)?,
        position_secs: row.get(2)?,
        duration_secs: row.get(3)?,
        finished: row.get(4)?,
        ever_finished: row.get(5)?,
        updated_at: row.get(6)?,
        snapshot: PlaybackSnapshot {
            title: row.get(7)?,
            poster_url: row.get(8)?,
            series_id: row.get(9)?,
            season: row.get(10)?,
            episode: row.get(11)?,
            container_extension: row.get(12)?,
        },
    })
}

/// Picks the episode that follows `current` = `(season, episode)`: the smallest
/// `(season, episode)` strictly greater than it among `episodes`. Returns its payload.
///
/// ```
/// use opensmarters_store::next_episode;
/// let eps = [(1, 1, "a"), (1, 2, "b"), (2, 1, "c")];
/// assert_eq!(next_episode((1, 2), eps), Some("c"));
/// assert_eq!(next_episode((2, 1), eps), None);
/// ```
pub fn next_episode<T>(
    current: (u32, u32),
    episodes: impl IntoIterator<Item = (u32, u32, T)>,
) -> Option<T> {
    episodes
        .into_iter()
        .filter(|(s, e, _)| (*s, *e) > current)
        .min_by_key(|(s, e, _)| (*s, *e))
        .map(|(_, _, t)| t)
}

/// Resume-point operations. Obtain with [`Store::playback`].
#[derive(Clone, Copy, Debug)]
pub struct Playback<'a> {
    pub(crate) store: &'a Store,
}

impl Playback<'_> {
    /// The store's configured thresholds.
    pub fn thresholds(&self) -> ResumeThresholds {
        self.store.resume
    }

    /// Records the current position (creating or updating the resume point) and returns
    /// the stored state. `duration_secs` may be `None` while unknown; a known duration is
    /// kept if a later report omits it. The position is clamped to the duration.
    pub fn record_progress(
        &self,
        profile: ProfileId,
        kind: PlaybackKind,
        item_id: &str,
        position_secs: f64,
        duration_secs: Option<f64>,
        snapshot: &PlaybackSnapshot,
    ) -> Result<PlaybackProgress> {
        let item_id = validate_item_id("item_id", item_id)?;
        if !position_secs.is_finite() || position_secs < 0.0 {
            return Err(StoreError::invalid(
                "position_secs",
                "must be a finite, non-negative number",
            ));
        }
        let duration = match duration_secs {
            Some(d) if d.is_finite() && d > 0.0 => Some(d),
            Some(d) if !d.is_finite() || d < 0.0 => {
                return Err(StoreError::invalid(
                    "duration_secs",
                    "must be a finite, non-negative number",
                ));
            }
            _ => None, // None or 0 (unknown, e.g. live-like streams)
        };
        let snap = snapshot.sanitized();
        let t = self.store.resume;
        let now = self.store.now();

        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        let prev_duration: Option<Option<f64>> = tx
            .query_row(
                "SELECT duration_secs FROM playback
                 WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
                params![profile, kind, item_id],
                |r| r.get(0),
            )
            .optional()?;
        let duration = duration.or(prev_duration.flatten());
        let position = duration.map_or(position_secs, |d| position_secs.min(d));
        let finished = duration.is_some_and(|d| position / d >= t.max_fraction);

        tx.execute(
            "INSERT INTO playback (profile_id, kind, item_id, position_secs, duration_secs,
                                   finished, ever_finished, updated_at, seq, title, poster_url,
                                   series_id, season, episode, container_extension)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7,
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM playback),
                     ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (profile_id, kind, item_id) DO UPDATE SET
                 position_secs = excluded.position_secs,
                 duration_secs = excluded.duration_secs,
                 finished = excluded.finished,
                 ever_finished = MAX(ever_finished, excluded.finished),
                 updated_at = excluded.updated_at,
                 seq = excluded.seq,
                 title = excluded.title,
                 poster_url = excluded.poster_url,
                 series_id = excluded.series_id,
                 season = excluded.season,
                 episode = excluded.episode,
                 container_extension = excluded.container_extension",
            params![
                profile,
                kind,
                item_id,
                position,
                duration,
                finished,
                now,
                snap.title,
                snap.poster_url,
                snap.series_id,
                snap.season,
                snap.episode,
                snap.container_extension
            ],
        )
        .map_err(map_fk(profile))?;
        let sql = format!(
            "SELECT {COLUMNS} FROM playback WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3"
        );
        let stored = tx.query_row(&sql, params![profile, kind, item_id], progress_from_row)?;
        tx.commit()?;
        Ok(stored)
    }

    /// Marks an existing resume point finished (end-of-file reached). Moves the position
    /// to the end if the duration is known. Returns `false` if there is no such point —
    /// call [`record_progress`](Self::record_progress) first.
    pub fn mark_finished(
        &self,
        profile: ProfileId,
        kind: PlaybackKind,
        item_id: &str,
    ) -> Result<bool> {
        let n = self.store.conn().execute(
            "UPDATE playback SET finished = 1, ever_finished = 1,
                    position_secs = COALESCE(duration_secs, position_secs),
                    updated_at = ?4, seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM playback)
             WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![profile, kind, item_id, self.store.now()],
        )?;
        Ok(n > 0)
    }

    /// The stored resume point, if any.
    pub fn get(
        &self,
        profile: ProfileId,
        kind: PlaybackKind,
        item_id: &str,
    ) -> Result<Option<PlaybackProgress>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM playback WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3"
        );
        Ok(self
            .store
            .conn()
            .prepare_cached(&sql)?
            .query_row(params![profile, kind, item_id], progress_from_row)
            .optional()?)
    }

    /// Where to resume, if the stored point is resumable under the configured thresholds.
    pub fn resume_position(
        &self,
        profile: ProfileId,
        kind: PlaybackKind,
        item_id: &str,
    ) -> Result<Option<f64>> {
        let t = self.store.resume;
        Ok(self
            .get(profile, kind, item_id)?
            .filter(|p| p.is_resumable(&t))
            .map(|p| p.position_secs))
    }

    /// Continue-watching row with the configured thresholds.
    pub fn continue_watching(
        &self,
        profile: ProfileId,
        limit: usize,
    ) -> Result<Vec<PlaybackProgress>> {
        self.continue_watching_with(profile, limit, &self.store.resume)
    }

    /// Unfinished items, most recent first, ignoring positions below
    /// `t.min_position_secs` and at/above `t.max_fraction`. Episodes are collapsed to the
    /// latest one per series; a series whose latest episode is finished is left out (see
    /// [`up_next_candidates`](Self::up_next_candidates)).
    pub fn continue_watching_with(
        &self,
        profile: ProfileId,
        limit: usize,
        t: &ResumeThresholds,
    ) -> Result<Vec<PlaybackProgress>> {
        self.scan_latest_per_series(profile, limit, |p| p.is_resumable(t))
    }

    /// Series whose most recently watched episode is finished, most recent first — the
    /// inputs for an *Up next* row. Use the returned episode's `season`/`episode` with
    /// [`next_episode`] over the series' episode list.
    pub fn up_next_candidates(
        &self,
        profile: ProfileId,
        limit: usize,
    ) -> Result<Vec<PlaybackProgress>> {
        self.scan_latest_per_series(profile, limit, |p| {
            p.kind == PlaybackKind::Episode && p.snapshot.series_id.is_some() && p.finished
        })
    }

    fn scan_latest_per_series(
        &self,
        profile: ProfileId,
        limit: usize,
        keep: impl Fn(&PlaybackProgress) -> bool,
    ) -> Result<Vec<PlaybackProgress>> {
        let conn = self.store.conn();
        let sql = format!("SELECT {COLUMNS} FROM playback WHERE profile_id = ?1 ORDER BY seq DESC");
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([profile])?;
        let mut seen_series = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut scanned = 0usize;
        while let Some(row) = rows.next()? {
            scanned += 1;
            if out.len() >= limit || scanned > MAX_SCAN_ROWS {
                break;
            }
            let p = progress_from_row(row)?;
            if p.kind == PlaybackKind::Episode
                && let Some(series) = &p.snapshot.series_id
                && !seen_series.insert(series.clone())
            {
                continue; // an older episode of a series already represented
            }
            if keep(&p) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// All stored episodes of a series, ordered by season and episode.
    pub fn series_progress(
        &self,
        profile: ProfileId,
        series_id: &str,
    ) -> Result<Vec<PlaybackProgress>> {
        let conn = self.store.conn();
        let sql = format!(
            "SELECT {COLUMNS} FROM playback
             WHERE profile_id = ?1 AND kind = 'episode' AND series_id = ?2
             ORDER BY season, episode, item_id"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![profile, series_id], progress_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The most recently watched episode of a series.
    pub fn last_episode(
        &self,
        profile: ProfileId,
        series_id: &str,
    ) -> Result<Option<PlaybackProgress>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM playback
             WHERE profile_id = ?1 AND kind = 'episode' AND series_id = ?2
             ORDER BY seq DESC LIMIT 1"
        );
        Ok(self
            .store
            .conn()
            .prepare_cached(&sql)?
            .query_row(params![profile, series_id], progress_from_row)
            .optional()?)
    }

    /// Full history, most recent first.
    pub fn history(&self, profile: ProfileId, limit: usize) -> Result<Vec<PlaybackProgress>> {
        let conn = self.store.conn();
        let sql = format!(
            "SELECT {COLUMNS} FROM playback WHERE profile_id = ?1 ORDER BY seq DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            params![profile, i64::try_from(limit).unwrap_or(i64::MAX)],
            progress_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forgets one item. Returns `false` if there was nothing stored.
    pub fn remove(&self, profile: ProfileId, kind: PlaybackKind, item_id: &str) -> Result<bool> {
        let n = self.store.conn().execute(
            "DELETE FROM playback WHERE profile_id = ?1 AND kind = ?2 AND item_id = ?3",
            params![profile, kind, item_id],
        )?;
        Ok(n > 0)
    }

    /// Clears a profile's whole watch history. Returns the number of rows removed.
    pub fn clear_history(&self, profile: ProfileId) -> Result<usize> {
        Ok(self
            .store
            .conn()
            .execute("DELETE FROM playback WHERE profile_id = ?1", [profile])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_episode_picks_smallest_greater() {
        let eps = vec![
            (2, 1, "s2e1"),
            (1, 3, "s1e3"),
            (1, 2, "s1e2"),
            (1, 10, "s1e10"),
        ];
        assert_eq!(next_episode((1, 2), eps.clone()), Some("s1e3"));
        assert_eq!(next_episode((1, 3), eps.clone()), Some("s1e10"));
        assert_eq!(next_episode((1, 10), eps.clone()), Some("s2e1"));
        assert_eq!(next_episode((2, 1), eps), None);
    }

    #[test]
    fn resumable_rules() {
        let t = ResumeThresholds::default();
        let mut p = PlaybackProgress {
            kind: PlaybackKind::Movie,
            item_id: "1".into(),
            position_secs: 10.0,
            duration_secs: Some(1000.0),
            finished: false,
            ever_finished: false,
            updated_at: Timestamp::EPOCH,
            snapshot: PlaybackSnapshot::default(),
        };
        assert!(!p.is_resumable(&t), "below 30 s");
        p.position_secs = 500.0;
        assert!(p.is_resumable(&t));
        p.position_secs = 960.0;
        assert!(!p.is_resumable(&t), "above 95 %");
        p.duration_secs = None;
        assert!(p.is_resumable(&t), "unknown duration");
        p.finished = true;
        assert!(!p.is_resumable(&t));
    }

    #[test]
    fn container_extension_is_sanitised() {
        let s = PlaybackSnapshot {
            container_extension: Some("mkv".into()),
            ..Default::default()
        };
        assert_eq!(s.sanitized().container_extension.as_deref(), Some("mkv"));
        let bad = PlaybackSnapshot {
            container_extension: Some("../x".into()),
            ..Default::default()
        };
        assert_eq!(bad.sanitized().container_extension, None);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! The DVR's work with the shared player (UI thread, no extra connection):
//!
//! - **Clips** ("Save the last N minutes"): `Player::dump_cache(time_pos − N min,
//!   time_pos)` into a new [`MediaOutput`] in the recordings folder. The buffer can't
//!   hold more than what played since the stream (re)started, so the clip is cut to that
//!   and the toast says how long it is. A `Clip` row (done) is added when the player
//!   reports the file complete (`PlayerEvent::CommandDone`).
//! - **Recording through the player**: when a scheduled recording can't get a connection
//!   and the user is watching that very channel, the player records it
//!   (`Player::set_stream_record`). It shares the viewing connection, so changing channel
//!   stops it; the row says so.
//! - **Resume points** of played recordings and clips (`recordings.position_secs`),
//!   saved every 10 s and when playback switches away.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use opensmarters_core::chrono::{DateTime, Utc};
use opensmarters_player::{MediaOutput, Op, PlayerEvent, ValidationError};
use opensmarters_store::{NewRecording, RecordingId, RecordingKind, RecordingState, Timestamp};

use super::{engine, files};
use crate::playback::{MediaKind, PlaybackItem, PlaybackStatus};
use crate::screens::AppCtx;

/// Message of a recording made through the player.
pub const THROUGH_PLAYER: &str = "Recording through the player: it shares your connection, so \
                                  changing channel stops it.";
/// How often the position of a played recording is saved.
const SAVE_POSITION_EVERY: Duration = Duration::from_secs(10);
/// A clip the player hasn't confirmed by then is forgotten.
const CLIP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
struct ClipWindow {
    key: String,
    earliest: f64,
}

#[derive(Debug)]
struct PendingClip {
    path: PathBuf,
    file_name: String,
    key: String,
    title: String,
    channel: String,
    logo: Option<String>,
    secs: f64,
    asked: f64,
    at: Instant,
}

#[derive(Debug)]
struct PlayerRecording {
    id: RecordingId,
    key: String,
    path: PathBuf,
    end: DateTime<Utc>,
}

#[derive(Debug)]
struct Playing {
    id: RecordingId,
    pos: Option<f64>,
    duration_saved: bool,
    finished: bool,
    last_save: Instant,
}

/// The player side of the DVR (in `cx.dvr`).
#[derive(Debug, Default)]
pub struct PlayerSide {
    window: Option<ClipWindow>,
    clips: VecDeque<PendingClip>,
    recording: Option<PlayerRecording>,
    /// "Stop recording" for the player's recording (set by `Dvr::stop_recording`).
    pub(crate) stop_requested: Option<RecordingId>,
    playing: Option<Playing>,
}

impl PlayerSide {
    /// The recording the player makes now, if any.
    pub fn recording_id(&self) -> Option<RecordingId> {
        self.recording.as_ref().map(|r| r.id)
    }

    /// Seconds of the playing live channel the buffer can hold now (0 when unknown).
    pub fn buffered_secs(&self, key: &str, time_pos: Option<f64>) -> f64 {
        match (&self.window, time_pos) {
            (Some(w), Some(t)) if w.key == key => (t - w.earliest).max(0.0),
            _ => 0.0,
        }
    }
}

/// A free name in `dir` that also isn't in the job list (UI thread: one `stat` per try).
fn free_output(cx: &AppCtx<'_>, dir: &std::path::Path, base: &str) -> Option<MediaOutput> {
    let taken: std::collections::HashSet<&str> = cx
        .dvr
        .rows()
        .iter()
        .filter_map(|r| r.file_name.as_deref())
        .collect();
    for n in 1..200 {
        let name = if n == 1 {
            base.to_owned()
        } else {
            files::numbered(base, n)
        };
        if taken.contains(name.as_str()) {
            continue;
        }
        match MediaOutput::new(dir, &name) {
            Ok(out) => return Some(out),
            Err(ValidationError::AlreadyExists) => {}
            Err(_) => return None,
        }
    }
    None
}

fn live_now_title(cx: &AppCtx<'_>) -> Option<String> {
    let session = cx.session()?;
    let zap = cx.playback.request()?.zap.as_ref()?;
    let ch = zap.channels.get(zap.index)?;
    let (_, now, _) = session.now_next(ch, cx.now);
    now.map(|p| p.title)
}

/// "Save the last `minutes` minutes" of the live channel playing.
pub fn save_clip(cx: &mut AppCtx<'_>, minutes: u32) {
    let Some(req) = cx.playback.request().cloned() else {
        cx.toasts.info("Nothing is playing.");
        return;
    };
    let PlaybackItem::Live { key, .. } = &req.item else {
        cx.toasts.info("Clips can be saved from live channels.");
        return;
    };
    let st = cx.playback.state();
    let have = cx.dvr.player.buffered_secs(key, st.time_pos);
    let Some(t) = st.time_pos.filter(|_| have >= 2.0) else {
        cx.toasts
            .info("The player hasn't buffered anything to save yet. Try again in a moment.");
        return;
    };
    let asked = f64::from(minutes.clamp(1, 30)) * 60.0;
    let secs = asked.min(have);
    let Some(dir) = cx.dvr.dir() else {
        cx.toasts.error(
            "There's no recordings folder. Choose one in Settings › Recordings & downloads.",
        );
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        cx.toasts
            .error("The recordings folder can't be used. Choose another one in Settings.");
        return;
    }
    let programme = live_now_title(cx);
    let title = programme.clone().unwrap_or_else(|| req.title.clone());
    let base = super::file_name(&format!("{title} clip"), cx.now, "ts");
    let Some(out) = free_output(cx, &dir, &base) else {
        cx.toasts
            .error("No free file name was found in the recordings folder.");
        return;
    };
    let Some(player) = cx.playback.player() else {
        return;
    };
    if let Err(e) = player.dump_cache(t - secs, t, &out) {
        tracing::warn!(error = %e, "dump-cache refused");
        cx.toasts.error("The player couldn't save its buffer.");
        return;
    }
    let file_name = out
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    cx.dvr.player.clips.push_back(PendingClip {
        path: out.path().to_path_buf(),
        file_name,
        key: key.clone(),
        title,
        channel: req.title.clone(),
        logo: req.artwork.clone(),
        secs,
        asked,
        at: Instant::now(),
    });
    cx.toasts.info(format!(
        "Saving the last {}…",
        crate::ui::text::duration_short(secs)
    ));
}

fn finish_clip(cx: &mut AppCtx<'_>, c: PendingClip, ok: bool) {
    let Some(engine) = cx.dvr.engine() else {
        return;
    };
    let profile = engine.profile();
    let wake = engine.waker();
    if !ok {
        cx.toasts.error(
            "The clip couldn't be saved: the player's buffer isn't available for this channel.",
        );
        let path = c.path;
        drop(cx.services.rt().spawn_blocking(move || {
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                let _ = std::fs::remove_file(&path);
            }
        }));
        return;
    }
    let asked_more = c.secs + 1.0 < c.asked;
    cx.toasts.success(if asked_more {
        format!(
            "Saved a {} clip to Recordings (that's what the buffer held).",
            crate::ui::text::duration_short(c.secs)
        )
    } else {
        format!(
            "Saved the last {} to Recordings.",
            crate::ui::text::duration_short(c.secs)
        )
    });
    let now = Utc::now();
    cx.services.blocking("save a clip", move |s| {
        let len = std::fs::metadata(&c.path).map(|m| m.len()).unwrap_or(0);
        if len == 0 || files::sniff_file(&c.path).is_none() {
            let _ = std::fs::remove_file(&c.path);
            wake();
            return Ok(());
        }
        let start = Timestamp::from_unix_millis(now.timestamp_millis() - (c.secs * 1000.0) as i64);
        let r = s.recordings().add(
            profile,
            &NewRecording {
                kind: RecordingKind::Clip,
                state: RecordingState::Active,
                item_id: c.key.clone(),
                series_id: None,
                season: None,
                episode: None,
                container_extension: None,
                title: c.title.clone(),
                subtitle: Some(c.channel.clone()),
                icon_url: c.logo.clone(),
                start_at: Some(start),
                end_at: Some(Timestamp::from_unix_millis(now.timestamp_millis())),
            },
        )?;
        s.recordings().set_file_name(r.id, &c.file_name)?;
        s.recordings().set_progress(r.id, len, Some(len))?;
        s.recordings().set_duration(r.id, c.secs)?;
        s.recordings().set_state(r.id, RecordingState::Done, None)?;
        wake();
        Ok(())
    });
}

/// Starts recording waiting row `id` through the player.
fn start_player_recording(cx: &mut AppCtx<'_>, id: RecordingId, key: &str) {
    let Some(row) = cx.dvr.row(id).cloned() else {
        return;
    };
    let settings = cx.dvr.settings();
    let Some((_, end)) = engine::live_window(&row, &settings) else {
        return;
    };
    let Some(dir) = settings.dir else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let when = row
        .start_at
        .and_then(|t| DateTime::from_timestamp_millis(t.as_unix_millis()))
        .unwrap_or(cx.now);
    let base = super::file_name(&row.title, when, "ts");
    let Some(out) = free_output(cx, &dir, &base) else {
        return;
    };
    let Some(player) = cx.playback.player() else {
        return;
    };
    if let Err(e) = player.set_stream_record(Some(&out)) {
        tracing::warn!(error = %e, "stream-record refused");
        return;
    }
    let Some(engine) = cx.dvr.engine() else {
        return;
    };
    engine.claim_for_player(id, key);
    let wake = engine.waker();
    let name = out
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    cx.services
        .blocking("start recording through the player", move |s| {
            s.recordings().set_file_name(id, &name)?;
            s.recordings()
                .set_state(id, RecordingState::Active, Some(THROUGH_PLAYER))?;
            wake();
            Ok(())
        });
    cx.toasts.info(format!(
        "Recording \u{201c}{}\u{201d} through the player (no other connection is free). \
         Changing channel stops it.",
        crate::ui::text::cap(&row.title, 60)
    ));
    cx.dvr.player.recording = Some(PlayerRecording {
        id,
        key: key.to_owned(),
        path: out.path().to_path_buf(),
        end,
    });
}

/// Stops the player's recording and keeps what it wrote (`note` for the row).
fn stop_player_recording(cx: &mut AppCtx<'_>, note: Option<&'static str>) {
    let Some(rec) = cx.dvr.player.recording.take() else {
        return;
    };
    if let Some(player) = cx.playback.player() {
        let _ = player.set_stream_record(None);
    }
    let Some(engine) = cx.dvr.engine() else {
        return;
    };
    engine.release_from_player(rec.id);
    let wake = engine.waker();
    let id = rec.id;
    let path = rec.path;
    cx.services
        .blocking("finish recording through the player", move |s| {
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if len > 0 && files::sniff_file(&path).is_some() {
                s.recordings().set_progress(id, len, Some(len))?;
                s.recordings().set_state(id, RecordingState::Done, note)?;
            } else {
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                    let _ = std::fs::remove_file(&path);
                }
                s.recordings().set_state(
                    id,
                    RecordingState::Failed,
                    Some("Nothing was recorded through the player."),
                )?;
            }
            wake();
            Ok(())
        });
}

/// Per frame: player results, the clip window, the player's recording, resume points.
pub(crate) fn on_frame(cx: &mut AppCtx<'_>) {
    // 1. Results of dump-cache / stream-record.
    for ev in cx.playback.take_dvr_events() {
        match ev {
            PlayerEvent::CommandDone { op: Op::DumpCache } => {
                if let Some(c) = cx.dvr.player.clips.pop_front() {
                    finish_clip(cx, c, true);
                }
            }
            PlayerEvent::CommandFailed {
                op: Op::DumpCache, ..
            } => {
                if let Some(c) = cx.dvr.player.clips.pop_front() {
                    finish_clip(cx, c, false);
                }
            }
            PlayerEvent::CommandFailed { op: Op::Record, .. }
                if cx.dvr.player.recording.is_some() =>
            {
                cx.toasts.error("The player couldn't record this channel.");
                stop_player_recording(cx, None);
            }
            _ => {}
        }
    }
    while cx
        .dvr
        .player
        .clips
        .front()
        .is_some_and(|c| c.at.elapsed() > CLIP_TIMEOUT)
    {
        cx.dvr.player.clips.pop_front();
    }

    // 2. How far back the buffer of the playing live stream goes.
    let status = cx.playback.status();
    let streaming = matches!(
        status,
        PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Buffering
    );
    let key = cx.playback.live_key().map(str::to_owned);
    let time_pos = cx.playback.state().time_pos;
    match (&key, time_pos) {
        (Some(k), Some(t)) if streaming => match &mut cx.dvr.player.window {
            Some(w) if w.key == *k && t >= w.earliest - 1.0 => {}
            _ => {
                cx.dvr.player.window = Some(ClipWindow {
                    key: k.clone(),
                    earliest: t,
                });
            }
        },
        _ if !streaming => cx.dvr.player.window = None,
        _ => {}
    }

    // 3. Recording through the player: stop it when its channel stops playing…
    if let Some(rec) = &cx.dvr.player.recording {
        let note: Option<Option<&'static str>> =
            if cx.dvr.player.stop_requested.take() == Some(rec.id) || Utc::now() >= rec.end {
                Some(None)
            } else if key.as_deref() != Some(rec.key.as_str()) {
                Some(Some(
                    "Stopped early: you changed channel (it was recording through the player).",
                ))
            } else if matches!(
                status,
                PlaybackStatus::Failed(_) | PlaybackStatus::Idle | PlaybackStatus::Unavailable(_)
            ) {
                Some(Some("Stopped early: the channel stopped playing."))
            } else {
                None
            };
        if let Some(note) = note {
            stop_player_recording(cx, note);
        }
    } else if streaming && let Some(k) = &key {
        // …and start it for a waiting recording of the channel that plays.
        let waiting = cx
            .dvr
            .engine()
            .map(engine::Engine::waiting)
            .unwrap_or_default();
        if let Some((id, _)) = waiting.into_iter().find(|(_, wk)| wk == k) {
            start_player_recording(cx, id, &k.clone());
        }
    }

    // 4. Resume point of a played recording or clip.
    let current = match cx.playback.request() {
        Some(r) if r.kind == MediaKind::Recording => match r.item {
            PlaybackItem::Recording { id } => Some(id),
            _ => None,
        },
        _ => None,
    };
    let st = cx.playback.state();
    let switched = cx
        .dvr
        .player
        .playing
        .as_ref()
        .is_some_and(|p| Some(p.id) != current);
    if switched
        && let Some(p) = cx.dvr.player.playing.take()
        && !p.finished
        && let Some(pos) = p.pos
    {
        let id = p.id;
        cx.services.blocking("save the position", move |s| {
            s.recordings().set_position(id, Some(pos)).map(|_| ())
        });
    }
    let Some(id) = current else { return };
    let p = cx.dvr.player.playing.get_or_insert_with(|| Playing {
        id,
        pos: None,
        duration_saved: false,
        finished: false,
        last_save: Instant::now(),
    });
    if let Some(t) = st.time_pos {
        p.pos = Some(t);
    }
    if !p.duration_saved
        && let Some(d) = st.duration.filter(|d| *d > 1.0)
    {
        p.duration_saved = true;
        cx.services.blocking("save the duration", move |s| {
            s.recordings().set_duration(id, d).map(|_| ())
        });
    }
    if status == PlaybackStatus::Finished && !p.finished {
        p.finished = true;
        cx.services.blocking("save the position", move |s| {
            s.recordings().set_position(id, None).map(|_| ())
        });
    } else if !p.finished
        && p.last_save.elapsed() >= SAVE_POSITION_EVERY
        && let Some(pos) = p.pos
    {
        p.last_save = Instant::now();
        cx.services.blocking("save the position", move |s| {
            s.recordings().set_position(id, Some(pos)).map(|_| ())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_seconds_follow_the_stream() {
        let mut side = PlayerSide::default();
        assert_eq!(side.buffered_secs("1", Some(10.0)), 0.0);
        side.window = Some(ClipWindow {
            key: "1".into(),
            earliest: 4.0,
        });
        assert_eq!(side.buffered_secs("1", Some(64.0)), 60.0);
        assert_eq!(side.buffered_secs("2", Some(64.0)), 0.0);
        assert_eq!(side.buffered_secs("1", None), 0.0);
    }
}

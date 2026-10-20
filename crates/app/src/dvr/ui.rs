// SPDX-License-Identifier: GPL-3.0-or-later
//! The DVR's pieces inside other screens. Each screen calls one function per hook (a
//! line or two), and everything else lives here:
//!
//! | Screen | Hook |
//! |---|---|
//! | Film detail | [`movie_download`] (in the action row), [`movie_status`] (a caption line under it) |
//! | Series detail | [`episode_downloads`] → `Episodes::downloads`, [`episode_button`] per row, [`season_button`] by the tabs, [`episode_action`] for what they return |
//! | Player (live) | [`player_buttons`] (Record / Stop recording, Save clip) inside the OSD, [`player_overlay`] every frame (● REC, the clip dialog) |
//! | Live TV | [`rec_badge`] on channels being recorded |
//!
//! Dialogs opened here (clip length, delete a download, a downloaded episode) are drawn
//! by the hook of the screen that opened them; Esc/Backspace close them ([`on_frame`]).

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Key, Modifiers, Pos2, Rect, Ui, pos2, vec2,
};
use opensmarters_core::SeriesInfo;
use opensmarters_core::chrono::{DateTime, Local, Utc};
use opensmarters_store::{NewRecording, Recording, RecordingId, RecordingKind, RecordingState};
use tokio::sync::oneshot;

use super::engine::Progress;
use super::{CLIP_CHOICES, ProgrammeToRecord, source};
use crate::error::AppResult;
use crate::focus;
use crate::nav::{RecordingsTab, Route};
use crate::playback::{PlaybackItem, PlaybackRequest};
use crate::screens::AppCtx;
use crate::screens::settings::human_bytes;
use crate::services::vod;
use crate::ui::icons::{self, Style};
use crate::ui::text;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::{Button, Dialog, IconButton};

/// A dialog the DVR shows over a screen.
#[derive(Debug, Clone)]
enum DvrDialog {
    /// "Save the last…" (player).
    Clip { back: Id },
    /// "Delete download?"
    Delete {
        id: RecordingId,
        title: String,
        back: Id,
    },
    /// A downloaded episode: play it or delete it.
    Copy {
        id: RecordingId,
        title: String,
        back: Id,
    },
}

impl DvrDialog {
    fn back(&self) -> Id {
        match self {
            Self::Clip { back } | Self::Delete { back, .. } | Self::Copy { back, .. } => *back,
        }
    }
}

/// UI state of the DVR hooks (in `cx.dvr`).
#[derive(Debug, Default)]
pub struct UiState {
    dialog: Option<DvrDialog>,
    drawn_pass: u64,
    play: Option<oneshot::Receiver<AppResult<PlaybackRequest>>>,
    /// Where the focus goes back after a dialog (requested the next frame, once the
    /// dialog's focus trap is gone).
    refocus: Option<Id>,
}

/// Per frame: plays a saved file once its request is ready, closes a dialog on
/// Esc/Backspace (before the screen sees the key) or when its screen went away.
pub(crate) fn on_frame(cx: &mut AppCtx<'_>) {
    if let Some(id) = cx.dvr.ui.refocus.take() {
        focus::request(&cx.egui, id);
    }
    if let Some(rx) = &mut cx.dvr.ui.play {
        match rx.try_recv() {
            Ok(Ok(req)) => {
                cx.dvr.ui.play = None;
                cx.play(req, true);
            }
            Ok(Err(e)) => {
                cx.dvr.ui.play = None;
                cx.toasts.error(e.user_message());
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => cx.dvr.ui.play = None,
        }
    }
    let pass = cx.egui.cumulative_pass_nr();
    if cx.dvr.ui.dialog.is_some() && pass > cx.dvr.ui.drawn_pass + 3 {
        cx.dvr.ui.dialog = None;
    }
    if cx.dvr.ui.dialog.is_some() && !focus::text_input_focused(&cx.egui) {
        let back = cx.egui.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Escape)
                || i.consume_key(Modifiers::NONE, Key::Backspace)
        });
        if back {
            close_dialog(cx);
        }
    }
}

/// Opens a dialog (drawn by the hook of the screen that opened it).
fn open_dialog(cx: &mut AppCtx<'_>, d: DvrDialog) {
    cx.dvr.ui.drawn_pass = cx.egui.cumulative_pass_nr();
    cx.dvr.ui.dialog = Some(d);
}

fn close_dialog(cx: &mut AppCtx<'_>) {
    if let Some(d) = cx.dvr.ui.dialog.take() {
        cx.dvr.ui.refocus = Some(d.back());
    }
}

/// True while a DVR dialog is open (screens that handle Esc themselves can check it).
pub fn dialog_open(cx: &AppCtx<'_>) -> bool {
    cx.dvr.ui.dialog.is_some()
}

/// Plays the saved file of `id` (the request is built off the UI thread: it checks the
/// file and reads the resume point).
pub fn play_saved(cx: &mut AppCtx<'_>, id: RecordingId) {
    let (Some(r), Some(dir)) = (cx.dvr.row(id).cloned(), cx.dvr.dir()) else {
        cx.toasts.error("This recording isn't available.");
        return;
    };
    let store = Arc::clone(&cx.services.store);
    let (tx, rx) = oneshot::channel();
    cx.dvr.ui.play = Some(rx);
    let ctx = cx.egui.clone();
    drop(cx.services.rt().spawn_blocking(move || {
        let resume = source::resume_point(&store, &r);
        let _ = tx.send(source::playback_request(&dir, &r, resume));
        ctx.request_repaint();
    }));
}

// ---------------------------------------------------------------------------------------
// Status text
// ---------------------------------------------------------------------------------------

/// How a row reads in lists and on detail pages.
#[derive(Debug, Clone)]
pub struct Status {
    /// Leading icon.
    pub glyph: &'static str,
    /// Icon colour (never the only signal: there is always a word).
    pub color: Color32,
    /// The line ("Downloading · 42% · 3.1 MB/s").
    pub line: String,
    /// Progress 0..=1 to draw as a bar, when meaningful.
    pub fraction: Option<f32>,
}

/// A length for clips: "45 s", "2 min 13 s", "5 min", "1 h 05".
pub fn length_label(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0 s".to_owned();
    }
    let total = secs.round() as u64;
    let (m, s) = (total / 60, total % 60);
    match (m, s) {
        (0, s) => format!("{s} s"),
        (m, 0) if m < 60 => format!("{m} min"),
        (m, s) if m < 60 => format!("{m} min {s} s"),
        _ => text::duration_short(secs),
    }
}

fn rate_label(bps: f64) -> Option<String> {
    (bps >= 1.0).then(|| format!("{}/s", human_bytes(bps as u64)))
}

fn local_time(cx: &AppCtx<'_>, t: Option<opensmarters_store::Timestamp>) -> Option<String> {
    let t = DateTime::<Utc>::from_timestamp_millis(t?.as_unix_millis())?;
    Some(cx.prefs.format_time(&t.with_timezone(&Local)))
}

/// "Tue 20 Oct · 20:00–21:00".
pub fn when_label(cx: &AppCtx<'_>, r: &Recording) -> String {
    let day = r
        .start_at
        .and_then(|t| DateTime::<Utc>::from_timestamp_millis(t.as_unix_millis()))
        .map(|t| t.with_timezone(&Local).format("%a %-d %b").to_string());
    let span = match (local_time(cx, r.start_at), local_time(cx, r.end_at)) {
        (Some(a), Some(_)) if super::is_until_stopped(r) => Some(format!("from {a}")),
        (Some(a), Some(b)) => Some(format!("{a}–{b}")),
        (Some(a), None) => Some(a),
        _ => None,
    };
    vod::meta_line([day, span].into_iter().flatten())
}

/// The status of a row (with its live progress, if it runs).
pub fn status(cx: &AppCtx<'_>, r: &Recording, p: Option<&Progress>) -> Status {
    let msg = r.error.clone();
    let bytes = p.map_or(r.bytes_done, |p| p.bytes);
    let total = p.and_then(|p| p.total).or(r.bytes_total);
    let fraction = total
        .filter(|t| *t > 0)
        .map(|t| (bytes as f64 / t as f64).clamp(0.0, 1.0) as f32);
    let size = if bytes > 0 {
        Some(match total {
            Some(t) if t > bytes => format!("{} of {}", human_bytes(bytes), human_bytes(t)),
            _ => human_bytes(bytes),
        })
    } else {
        None
    };
    let pct = fraction.map(|f| format!("{}%", (f * 100.0).floor() as u32));
    match r.state {
        RecordingState::Scheduled => Status {
            glyph: icons::CALENDAR,
            color: color::TEXT_2,
            line: format!("Scheduled · {}", when_label(cx, r)),
            fraction: None,
        },
        RecordingState::Queued => Status {
            glyph: icons::CLOCK,
            color: color::TEXT_2,
            line: msg.unwrap_or_else(|| {
                if r.kind.is_download() {
                    "Queued: starts when a connection is free.".to_owned()
                } else {
                    "Starting…".to_owned()
                }
            }),
            fraction: fraction.filter(|f| *f > 0.0),
        },
        RecordingState::Active if r.kind.is_download() => Status {
            glyph: icons::DOWNLOAD_SIMPLE,
            color: color::ACCENT,
            line: match p.and_then(|p| p.note.clone()) {
                Some(note) => note,
                None => vod::meta_line(
                    [
                        Some("Downloading".to_owned()),
                        pct,
                        p.and_then(|p| rate_label(p.rate)),
                        size,
                    ]
                    .into_iter()
                    .flatten(),
                ),
            },
            fraction,
        },
        RecordingState::Active => Status {
            glyph: icons::RECORD,
            color: color::LIVE,
            line: match (p.and_then(|p| p.note.clone()), msg) {
                (Some(note), _) => format!("Recording · {note}"),
                (None, Some(m)) => m,
                (None, None) => vod::meta_line(
                    [
                        Some("Recording".to_owned()),
                        size,
                        if super::is_until_stopped(r) {
                            Some("until you stop it".to_owned())
                        } else {
                            local_time(cx, r.end_at).map(|e| format!("until {e}"))
                        },
                    ]
                    .into_iter()
                    .flatten(),
                ),
            },
            fraction: None,
        },
        RecordingState::Paused => Status {
            glyph: icons::PAUSE,
            color: if msg.as_deref().is_some_and(|m| m.contains("disk")) {
                color::WARNING
            } else {
                color::TEXT_2
            },
            line: msg.unwrap_or_else(|| {
                vod::meta_line([Some("Paused".to_owned()), pct, size].into_iter().flatten())
            }),
            fraction,
        },
        RecordingState::Done => {
            let dur = r.duration_secs.filter(|d| *d > 0.0).map(length_label);
            let word = if r.kind.is_download() {
                "Downloaded"
            } else {
                "Saved"
            };
            let line = vod::meta_line([Some(word.to_owned()), size, dur].into_iter().flatten());
            Status {
                glyph: icons::CHECK_CIRCLE,
                color: color::SUCCESS,
                line: match msg {
                    Some(m) => format!("{line} · {m}"),
                    None => line,
                },
                fraction: None,
            }
        }
        RecordingState::Failed => Status {
            glyph: icons::WARNING_CIRCLE,
            color: color::DANGER,
            line: msg.unwrap_or_else(|| "It didn't work.".to_owned()),
            fraction: None,
        },
        RecordingState::Cancelled => Status {
            glyph: icons::X_CIRCLE,
            color: color::TEXT_3,
            line: "Cancelled".to_owned(),
            fraction: None,
        },
    }
}

// ---------------------------------------------------------------------------------------
// Film detail
// ---------------------------------------------------------------------------------------

/// What the film detail page knows about the film.
#[derive(Debug, Clone)]
pub struct MovieTarget<'a> {
    /// VOD `stream_id`.
    pub stream_id: u64,
    /// Display title.
    pub title: &'a str,
    /// `container_extension`.
    pub extension: Option<&'a str>,
    /// Poster URL.
    pub poster: Option<&'a str>,
}

/// The film's download control, in the detail page's action row: "Download" →
/// "Queued" / "Downloading 42%" (opens Recordings) → "Play downloaded copy" + delete.
pub fn movie_download(cx: &mut AppCtx<'_>, ui: &mut Ui, t: &MovieTarget<'_>) {
    let Some(session) = cx.session() else { return };
    if !session.is_xtream() || cx.dvr.engine().is_none() {
        return;
    }
    let item = t.stream_id.to_string();
    let row = cx.dvr.download_of(RecordingKind::Movie, &item).cloned();
    let button_id = Id::unique("detail.download");
    match row {
        None => {
            if Button::secondary("Download")
                .icon(icons::DOWNLOAD_SIMPLE)
                .large()
                .id(button_id)
                .show(ui)
                .activated
            {
                let mut new = NewRecording::download(RecordingKind::Movie, item, t.title);
                new.container_extension = t.extension.map(str::to_owned);
                new.icon_url = session.safe_url(t.poster);
                cx.dvr.queue(new);
                cx.toasts.info(format!(
                    "\u{201c}{}\u{201d} will download in the background. It's in Recordings › Downloads.",
                    text::cap(t.title, 50)
                ));
            }
        }
        Some(r) if r.state == RecordingState::Done => {
            if Button::secondary("Play downloaded copy")
                .icon(icons::PLAY)
                .large()
                .id(button_id)
                .show(ui)
                .activated
            {
                play_saved(cx, r.id);
            }
            let del = Id::unique("detail.download.delete");
            if IconButton::new(icons::TRASH)
                .size(52.0, 22.0)
                .id(del)
                .show(ui)
                .activated
            {
                open_dialog(
                    cx,
                    DvrDialog::Delete {
                        id: r.id,
                        title: r.title.clone(),
                        back: del,
                    },
                );
            }
        }
        Some(r) if r.state == RecordingState::Failed => {
            if Button::secondary("Download failed · Retry")
                .icon(icons::ARROW_CLOCKWISE)
                .large()
                .id(button_id)
                .show(ui)
                .activated
            {
                cx.dvr.resume(r.id);
            }
        }
        Some(r) => {
            let p = cx.dvr.progress(r.id);
            let frac = p
                .as_ref()
                .and_then(Progress::fraction)
                .or_else(|| r.fraction());
            let label = match (r.state, frac) {
                (RecordingState::Active, Some(f)) => format!("Downloading {}%", (f * 100.0) as u32),
                (RecordingState::Active, None) => "Downloading…".to_owned(),
                (RecordingState::Paused, Some(f)) => format!("Paused {}%", (f * 100.0) as u32),
                (RecordingState::Paused, None) => "Paused".to_owned(),
                _ => "Queued".to_owned(),
            };
            if Button::secondary(label)
                .icon(icons::DOWNLOAD_SIMPLE)
                .large()
                .id(button_id)
                .show(ui)
                .activated
            {
                cx.nav.push(Route::Recordings {
                    tab: RecordingsTab::Downloads,
                });
            }
        }
    }
    dialogs(cx);
}

/// A caption line under the film's actions: the download's state, when there is one.
pub fn movie_status(cx: &mut AppCtx<'_>, ui: &mut Ui, stream_id: u64, x: f32) {
    let Some(r) = cx
        .dvr
        .download_of(RecordingKind::Movie, &stream_id.to_string())
        .cloned()
    else {
        return;
    };
    let p = cx.dvr.progress(r.id);
    let s = status(cx, &r, p.as_ref());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), egui::Sense::hover());
    let painter = ui.painter();
    let y = rect.bottom() - 8.0;
    icons::paint(
        painter,
        pos2(x + 9.0, y),
        s.glyph,
        18.0,
        Style::Regular,
        s.color,
    );
    let lr = text::line(
        painter,
        pos2(x + 26.0, y),
        Align2::LEFT_CENTER,
        &s.line,
        font::caption(),
        color::TEXT_2,
        620.0,
    );
    if let Some(f) = s.fraction {
        let bar = Rect::from_min_size(pos2(lr.right() + 12.0, y - 2.0), vec2(120.0, 4.0));
        crate::ui::paint::progress_bar(painter, bar, f, color::ACCENT, color::PROGRESS_TRACK);
    }
}

// ---------------------------------------------------------------------------------------
// Series detail
// ---------------------------------------------------------------------------------------

/// The download state of the episodes of a series (episode id → row id, state,
/// progress). Empty and disabled when downloads aren't available (no engine, M3U).
#[derive(Debug, Clone, Default)]
pub struct EpisodeDownloads {
    enabled: bool,
    map: HashMap<u64, (RecordingId, RecordingState, Option<f32>)>,
}

impl EpisodeDownloads {
    fn get(&self, episode_id: u64) -> Option<&(RecordingId, RecordingState, Option<f32>)> {
        self.map.get(&episode_id)
    }

    fn contains(&self, episode_id: u64) -> bool {
        self.map.contains_key(&episode_id)
    }
}

/// What an episode download control asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadAction {
    /// Download this episode.
    Episode(u64),
    /// Download every episode of this season that isn't downloaded yet.
    Season(u32),
    /// Show the downloads (queued / running / paused).
    Open,
    /// A downloaded episode: play or delete it.
    Copy(RecordingId),
    /// Try a failed download again.
    Retry(RecordingId),
}

/// The episodes of `series_id` that have a download.
pub fn episode_downloads(cx: &AppCtx<'_>, series_id: u64) -> EpisodeDownloads {
    let enabled =
        cx.dvr.engine().is_some() && cx.session().is_some_and(|s| s.is_xtream()) && series_id > 0;
    let sid = series_id.to_string();
    let map = cx
        .dvr
        .rows()
        .iter()
        .filter(|r| r.kind == RecordingKind::Episode && r.series_id.as_deref() == Some(&sid))
        .filter_map(|r| {
            let id = r.item_id.parse::<u64>().ok()?;
            let frac = cx
                .dvr
                .progress(r.id)
                .and_then(|p| p.fraction())
                .or_else(|| r.fraction())
                .map(|f| f as f32);
            Some((id, (r.id, r.state, frac)))
        })
        .collect();
    EpisodeDownloads { enabled, map }
}

/// Splits an episode row: the row itself, and room for its download control on the
/// right (nothing when downloads aren't available).
pub fn split_episode_row(d: &EpisodeDownloads, rect: Rect) -> (Rect, Rect) {
    if !d.enabled {
        return (rect, Rect::NOTHING);
    }
    let button =
        Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), vec2(44.0, 44.0));
    (rect.with_max_x(rect.right() - 60.0), button)
}

/// The small download control of an episode row (at `rect`, 44 pt).
pub fn episode_button(
    ui: &mut Ui,
    downloads: &EpisodeDownloads,
    episode_id: u64,
    rect: Rect,
) -> Option<DownloadAction> {
    if !downloads.enabled || !rect.is_positive() {
        return None;
    }
    let id = Id::unique(("detail.ep.download", episode_id));
    let (glyph, caption, tint, action) = match downloads.get(episode_id) {
        None => (
            icons::DOWNLOAD_SIMPLE,
            None,
            None,
            DownloadAction::Episode(episode_id),
        ),
        Some((rid, RecordingState::Done, _)) => (
            icons::CHECK_CIRCLE,
            None,
            Some(color::SAGE),
            DownloadAction::Copy(*rid),
        ),
        Some((rid, RecordingState::Failed, _)) => (
            icons::WARNING_CIRCLE,
            Some("Retry".to_owned()),
            Some(color::DANGER),
            DownloadAction::Retry(*rid),
        ),
        Some((_, state, frac)) => (
            if *state == RecordingState::Paused {
                icons::PAUSE
            } else {
                icons::DOWNLOAD_SIMPLE
            },
            Some(frac.map_or_else(|| "…".to_owned(), |f| format!("{}%", (f * 100.0) as u32))),
            Some(color::ACCENT),
            DownloadAction::Open,
        ),
    };
    let mut b = IconButton::new(glyph).id(id);
    if let Some(c) = caption {
        b = b.caption(c);
    }
    if let Some(t) = tint {
        b = b.tint(t);
    }
    b.show_at(ui, rect).activated.then_some(action)
}

/// "Download season" on the season header row (`head`), right-aligned left of the
/// episode count that ends at `right`.
pub fn season_button(
    ui: &mut Ui,
    downloads: &EpisodeDownloads,
    head: Rect,
    right: f32,
    season: u32,
    episodes: usize,
) -> Option<DownloadAction> {
    if !downloads.enabled || episodes == 0 {
        return None;
    }
    let count = format!("{episodes} episode{}", if episodes == 1 { "" } else { "s" });
    let count_w = text::width(ui.painter(), &count, font::caption());
    let b = Button::ghost("Download season")
        .icon(icons::DOWNLOAD_SIMPLE)
        .small()
        .id(Id::unique("detail.season.download"));
    let size = b.size(ui);
    let rect = Rect::from_min_size(
        pos2(
            right - count_w - 16.0 - size.x,
            head.center().y + 2.0 - size.y / 2.0,
        ),
        size,
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    b.show(&mut child)
        .activated
        .then_some(DownloadAction::Season(season))
}

fn episode_new(
    info: &SeriesInfo,
    series_id: u64,
    episode_id: u64,
    poster: Option<String>,
) -> Option<NewRecording> {
    let (season, number, ep) = vod::locate_episode(info, episode_id)?;
    let series = vod::series_name(info);
    let mut new = NewRecording::download(
        RecordingKind::Episode,
        episode_id.to_string(),
        vod::episode_title(&ep.title, &series, number),
    );
    new.series_id = Some(series_id.to_string());
    new.season = Some(season);
    new.episode = Some(number);
    new.container_extension = ep.container_extension.clone();
    new.subtitle = Some(series);
    new.icon_url = ep.info.movie_image.clone().or(poster);
    Some(new)
}

/// Does what an episode control asked for.
pub fn episode_action(
    cx: &mut AppCtx<'_>,
    info: &SeriesInfo,
    series_id: u64,
    poster: Option<&str>,
    action: DownloadAction,
) {
    let Some(session) = cx.session() else { return };
    let poster = session.safe_url(poster);
    match action {
        DownloadAction::Episode(id) => {
            if let Some(mut new) = episode_new(info, series_id, id, poster) {
                new.icon_url = session.safe_url(new.icon_url.as_deref());
                cx.toasts.info(format!(
                    "\u{201c}{}\u{201d} will download in the background.",
                    text::cap(&new.title, 50)
                ));
                cx.dvr.queue(new);
            }
        }
        DownloadAction::Season(season) => {
            let have = episode_downloads(cx, series_id);
            let list: Vec<NewRecording> = info
                .episodes
                .get(&season)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter(|e| !have.contains(e.id))
                .filter_map(|e| {
                    let mut n = episode_new(info, series_id, e.id, poster.clone())?;
                    n.icon_url = session.safe_url(n.icon_url.as_deref());
                    Some(n)
                })
                .collect();
            if list.is_empty() {
                cx.toasts
                    .info("Every episode of this season is already downloaded or queued.");
            } else {
                cx.toasts.info(format!(
                    "{} episode{} will download in the background, one after another.",
                    list.len(),
                    if list.len() == 1 { "" } else { "s" }
                ));
                cx.dvr.queue_all(list);
            }
        }
        DownloadAction::Open => cx.nav.push(Route::Recordings {
            tab: RecordingsTab::Downloads,
        }),
        DownloadAction::Retry(id) => cx.dvr.resume(id),
        DownloadAction::Copy(id) => {
            let title = cx.dvr.row(id).map(|r| r.title.clone()).unwrap_or_default();
            let back = cx
                .dvr
                .row(id)
                .and_then(|r| r.item_id.parse::<u64>().ok())
                .map_or(Id::NULL, |e| Id::unique(("detail.ep.download", e)));
            open_dialog(cx, DvrDialog::Copy { id, title, back });
        }
    }
}

/// Draws the episode/download dialogs of the detail page (call once per frame).
pub fn detail_dialogs(cx: &mut AppCtx<'_>) {
    dialogs(cx);
}

// ---------------------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------------------

fn dialogs(cx: &mut AppCtx<'_>) {
    let Some(d) = cx.dvr.ui.dialog.clone() else {
        return;
    };
    let pass = cx.egui.cumulative_pass_nr();
    if cx.dvr.ui.drawn_pass == pass {
        return;
    }
    cx.dvr.ui.drawn_pass = pass;
    let ctx = cx.egui.clone();
    match d {
        DvrDialog::Clip { .. } => {
            let default = cx.dvr.settings().clip_minutes;
            let mut chosen = None;
            let mut close = false;
            Dialog::new(Id::unique("dvr.clip"), "Save the last…")
                .body(
                    "Saved from the player's buffer, so it needs no extra connection. If less \
                     has played since the channel started, that's what is saved.",
                )
                .show(&ctx, |ui| {
                    for m in CLIP_CHOICES.iter().rev() {
                        let b = Button::secondary(format!("{m} min"))
                            .id(Id::unique(("dvr.clip.len", *m)));
                        let b = if *m == default { b.autofocus() } else { b };
                        if b.show(ui).activated {
                            chosen = Some(*m);
                        }
                    }
                    if Button::ghost("Cancel")
                        .id(Id::unique("dvr.clip.cancel"))
                        .show(ui)
                        .activated
                    {
                        close = true;
                    }
                });
            if let Some(m) = chosen {
                close_dialog(cx);
                super::player::save_clip(cx, m);
            } else if close {
                close_dialog(cx);
            }
        }
        DvrDialog::Delete { id, title, .. } => {
            let mut yes = false;
            let mut no = false;
            let heading = format!("Delete \u{201c}{}\u{201d}?", text::cap(&title, 60));
            Dialog::new(Id::unique("dvr.delete"), &heading)
                .body("The downloaded file is removed from this computer. You can still stream it.")
                .show(&ctx, |ui| {
                    yes = Button::danger("Delete download")
                        .icon(icons::TRASH)
                        .id(Id::unique("dvr.delete.yes"))
                        .show(ui)
                        .activated;
                    no = Button::secondary("Keep it")
                        .autofocus()
                        .id(Id::unique("dvr.delete.no"))
                        .show(ui)
                        .activated;
                });
            if yes {
                cx.dvr.delete(id);
                cx.dvr.ui.dialog = None;
                cx.dvr.ui.refocus = Some(Id::unique("detail.download"));
                cx.toasts.info("Download deleted.");
            } else if no {
                close_dialog(cx);
            }
        }
        DvrDialog::Copy { id, title, back } => {
            let mut play = false;
            let mut delete = false;
            let mut close = false;
            let heading = text::cap(&title, 70).into_owned();
            Dialog::new(Id::unique("dvr.copy"), &heading)
                .body("This episode is downloaded: it plays even when your provider can't be reached.")
                .show(&ctx, |ui| {
                    play = Button::primary("Play downloaded copy")
                        .icon(icons::PLAY)
                        .autofocus()
                        .id(Id::unique("dvr.copy.play"))
                        .show(ui)
                        .activated;
                    delete = Button::danger("Delete download")
                        .icon(icons::TRASH)
                        .id(Id::unique("dvr.copy.delete"))
                        .show(ui)
                        .activated;
                    close = Button::ghost("Close")
                        .id(Id::unique("dvr.copy.close"))
                        .show(ui)
                        .activated;
                });
            if play {
                close_dialog(cx);
                play_saved(cx, id);
            } else if delete {
                open_dialog(cx, DvrDialog::Delete { id, title, back });
            } else if close {
                close_dialog(cx);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------------------

/// The live channel playing in the shared player, with its guide entry for now.
fn playing_channel(
    cx: &AppCtx<'_>,
) -> Option<(String, String, Option<String>, Option<ProgrammeToRecord>)> {
    let req = cx.playback.request()?;
    let PlaybackItem::Live { key, .. } = &req.item else {
        return None;
    };
    let programme = cx.session().and_then(|session| {
        let zap = req.zap.as_ref()?;
        let ch = zap.channels.get(zap.index)?;
        let (_, now, _) = session.now_next(ch, cx.now);
        let p = now?;
        Some(ProgrammeToRecord {
            stream_id: key.clone(),
            channel_name: req.title.clone(),
            channel_logo: req.artwork.clone(),
            title: p.title,
            start: p.start,
            end: p.stop,
        })
    });
    Some((
        key.clone(),
        req.title.clone(),
        req.artwork.clone(),
        programme,
    ))
}

/// The row recording channel `key` now (running, waiting, or through the player).
fn recording_of(cx: &AppCtx<'_>, key: &str) -> Option<RecordingId> {
    cx.dvr
        .rows()
        .iter()
        .find(|r| {
            r.kind == RecordingKind::Live
                && r.item_id == key
                && matches!(r.state, RecordingState::Active | RecordingState::Queued)
        })
        .map(|r| r.id)
}

/// Starts recording what plays: the current programme from the guide (it starts at once
/// and stops at its end + padding), or "until you stop it" without a guide.
pub fn record_current(cx: &mut AppCtx<'_>) {
    let (Some((key, name, logo, programme)), Some(session), Some(engine)) =
        (playing_channel(cx), cx.session(), cx.dvr.engine())
    else {
        return;
    };
    let profile = session.profile_id();
    let wake = engine.waker();
    let now = cx.now;
    match programme {
        Some(p) => {
            cx.toasts.info(format!(
                "Recording \u{201c}{}\u{201d} until it ends.",
                text::cap(&p.title, 60)
            ));
            cx.services.blocking("start a recording", move |s| {
                super::schedule_programme(s, profile, &p)?;
                wake();
                Ok(())
            });
        }
        None => {
            cx.toasts.info(
                "Recording until you stop it (8 hours at most): there's no guide for this channel.",
            );
            cx.services.blocking("start a recording", move |s| {
                super::record_until_stopped(s, profile, &key, &name, logo, now)?;
                wake();
                Ok(())
            });
        }
    }
}

/// The player's DVR buttons for a live channel: "Record" / "Stop recording" and "Save
/// clip", laid out right to left ending at `right` (x), centred on `y`. Call inside the
/// OSD's focus scope.
pub fn player_buttons(cx: &mut AppCtx<'_>, ui: &mut Ui, right: f32, y: f32) {
    let Some((key, ..)) = playing_channel(cx) else {
        return;
    };
    if cx.dvr.engine().is_none() {
        return;
    }
    let recording = recording_of(cx, &key);
    let clip = Button::ghost("Save clip")
        .icon(icons::SCISSORS)
        .small()
        .id(Id::unique("player.dvr.clip"));
    let rec = if recording.is_some() {
        Button::ghost("Stop recording")
            .icon(icons::STOP)
            .small()
            .id(Id::unique("player.dvr.record"))
    } else {
        Button::ghost("Record")
            .icon(icons::RECORD)
            .small()
            .id(Id::unique("player.dvr.record"))
    };
    let (cs, rs) = (clip.size(ui), rec.size(ui));
    let clip_rect = Rect::from_min_size(pos2(right - cs.x, y - cs.y / 2.0), cs);
    let rec_rect = Rect::from_min_size(pos2(clip_rect.left() - 8.0 - rs.x, y - rs.y / 2.0), rs);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rec_rect));
    if rec.show(&mut c).activated {
        match recording {
            Some(id) => {
                cx.dvr.stop_recording(id);
                cx.toasts.info("Recording stopped. It's in Recordings.");
            }
            None => record_current(cx),
        }
    }
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(clip_rect));
    if clip.show(&mut c).activated {
        open_dialog(
            cx,
            DvrDialog::Clip {
                back: Id::unique("player.dvr.clip"),
            },
        );
    }
}

/// Every frame on the player screen: the ● REC badge while the playing channel is
/// recorded (top right, below the clock) and the clip dialog.
pub fn player_overlay(cx: &mut AppCtx<'_>, ui: &mut Ui) {
    if let Some((key, ..)) = playing_channel(cx)
        && cx.dvr.is_recording(&key)
    {
        let safe = cx.layout.safe_area();
        let pos = pos2(
            safe.right() - 24.0,
            safe.top() + if cx.layout.tv { 64.0 } else { 76.0 },
        );
        let painter = ui.painter().clone();
        rec_badge(&painter, pos, Align2::RIGHT_TOP);
    }
    dialogs(cx);
}

/// A "● REC" pill anchored at `pos`. Returns its rect.
pub fn rec_badge(painter: &egui::Painter, pos: Pos2, align: Align2) -> Rect {
    let g = painter.layout_job(text::overline_job("REC", color::TEXT));
    let size = vec2(g.size().x + 30.0, 22.0);
    let rect = align.anchor_size(pos, size);
    painter.rect_filled(
        rect,
        CornerRadius::same(radius::FULL),
        Color32::from_black_alpha(170),
    );
    painter.circle_filled(pos2(rect.left() + 12.0, rect.center().y), 4.0, color::LIVE);
    painter.galley(
        pos2(rect.left() + 22.0, rect.center().y - g.size().y / 2.0),
        g,
        color::TEXT,
    );
    rect
}

/// Live TV: the ● REC badge over a channel's logo plate when it is being recorded.
pub fn live_row_badge(painter: &egui::Painter, logo: Rect, recording: bool) {
    if recording {
        rec_badge(
            painter,
            pos2(logo.right() + 4.0, logo.bottom() + 6.0),
            Align2::RIGHT_BOTTOM,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_read_naturally() {
        assert_eq!(length_label(9.6), "10 s");
        assert_eq!(length_label(60.0), "1 min");
        assert_eq!(length_label(133.0), "2 min 13 s");
        assert_eq!(length_label(300.2), "5 min");
        assert_eq!(length_label(3900.0), "1 h 05");
        assert_eq!(length_label(f64::NAN), "0 s");
    }
}

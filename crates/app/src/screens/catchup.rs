// SPDX-License-Identifier: GPL-3.0-or-later
//! Catch-up: channels that keep an archive → day tabs (server-local dates, newest first)
//! → programmes (time, title, synopsis, duration, watched progress) → inline preview with
//! the programme card.
//!
//! Behaviour:
//! - Moving the focus through the channel list selects the channel after a short pause
//!   (no request per keystroke); Enter selects at once and moves into the programmes.
//! - Enter on a programme plays it in the preview; Enter on the playing programme (or the
//!   Fullscreen button) opens the player at the same position. Resume points are the
//!   player's (`"<stream_id>@<start>"`).
//! - If the provider refuses the timeshift stream, the card says "Catch-up isn't
//!   available for this programme right now" and offers the **other** timeshift format
//!   once ([`Attempt`]); when that one works, it is used for the rest of the session and
//!   the card offers to save it to the profile.
//! - Other screens open a channel/programme with [`open`] (Guide, Home's Continue
//!   watching).

mod days;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use opensmarters_core::ArchiveProgramme;
use opensmarters_core::catalog::{SearchQuery, normalize_for_search};
use opensmarters_core::chrono::Local;
use opensmarters_store::{ContentKind, PlaybackKind, PlaybackProgress, TimeshiftFormat};

use super::{AppCtx, Screen, shell};
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{PlayerRoute, Route, Section};
use crate::playback::{PlaybackItem, PlaybackStatus};
use crate::services::catalog::{LiveCatalog, LiveChannel, split_tag};
use crate::services::session::catchup::{Attempt, Fallback, alternate, format_label, progress_id};
use crate::services::session::guide::EpgRow;
use crate::services::{ImageSize, Session};
use crate::task::Loader;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::icons::{self, Style};
use crate::ui::layout::Breakpoint;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{
    Button, Callout, CalloutKind, ChannelLogo, EmptyState, SearchField, Tabs, VirtualList,
};
use crate::ui::{paint, text};
pub use days::{Day, archive_days, day_label};

const CHANNEL_ROW_H: f32 = 56.0;
const PROG_ROW_H: f32 = 72.0;
const PROG_GAP: f32 = 6.0;
/// Focus resting on a channel this long selects it.
const FOCUS_SELECT_AFTER: Duration = Duration::from_millis(450);

/// Opens Catch-up on a channel from another screen: optionally on the programme starting
/// at `start` (UTC epoch seconds), optionally playing it at once.
pub fn open(cx: &mut AppCtx<'_>, stream_id: u64, start: Option<i64>, autoplay: bool) {
    cx.egui.data_mut(|d| {
        d.insert_temp(
            handoff_id(),
            Handoff {
                stream_id,
                start,
                autoplay,
            },
        );
    });
    cx.nav.push(Route::Catchup {
        stream_id: Some(stream_id),
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Handoff {
    stream_id: u64,
    start: Option<i64>,
    autoplay: bool,
}

fn handoff_id() -> Id {
    Id::unique("catchup.handoff")
}

fn channel_row_id(key: &str) -> Id {
    Id::unique(("catchup.channel", key))
}

fn prog_row_id(pid: &str) -> Id {
    Id::unique(("catchup.prog", pid))
}

fn channels_list_id() -> Id {
    Id::unique("catchup.channels")
}

fn progs_list_id() -> Id {
    Id::unique("catchup.progs")
}

/// A playback attempt being watched for success/failure.
#[derive(Debug, Clone)]
struct Tracked {
    attempt: Attempt,
    channel: LiveChannel,
    programme: ArchiveProgramme,
    resolved: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct ListKey {
    generation: u64,
    parental: bool,
}

/// The Catch-up screen.
#[derive(Debug)]
pub struct CatchupScreen {
    pending: Option<Handoff>,
    catalog: Loader<Arc<LiveCatalog>>,
    list_key: Option<ListKey>,
    channels: Arc<Vec<LiveChannel>>,
    names: Arc<Vec<String>>,
    filter: String,
    filter_applied: Option<String>,
    filtered: Arc<Vec<usize>>,
    selected: Option<String>,
    focus_select: Option<(String, Instant)>,
    table: Loader<Arc<EpgRow>>,
    table_for: Option<String>,
    days: Vec<Day>,
    days_gen: u64,
    day: usize,
    progress: Loader<HashMap<String, PlaybackProgress>>,
    progress_key: Option<(String, usize, u64, u64)>,
    focused: Option<String>,
    attempt: Option<Tracked>,
    offer: Fallback,
    scroll_channels_to: Option<usize>,
    scroll_progs_to: Option<usize>,
    focus_programme: Option<String>,
}

impl CatchupScreen {
    /// Creates the screen, optionally on a channel (`stream_id`).
    pub fn new(stream_id: Option<u64>) -> Self {
        Self {
            pending: stream_id.map(|s| Handoff {
                stream_id: s,
                start: None,
                autoplay: false,
            }),
            catalog: Loader::new(),
            list_key: None,
            channels: Arc::new(Vec::new()),
            names: Arc::new(Vec::new()),
            filter: String::new(),
            filter_applied: None,
            filtered: Arc::new(Vec::new()),
            selected: None,
            focus_select: None,
            table: Loader::new(),
            table_for: None,
            days: Vec::new(),
            days_gen: 0,
            day: 0,
            progress: Loader::new(),
            progress_key: None,
            focused: None,
            attempt: None,
            offer: Fallback::None,
            scroll_channels_to: None,
            scroll_progs_to: None,
            focus_programme: None,
        }
    }

    fn load_catalog(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        self.catalog.start(cx.rt(), &cx.egui, move |_| async move {
            session.live_catalog().await
        });
    }

    /// Archive channels (parental locks applied), rebuilt when the catalogue changes.
    fn refresh_channels(&mut self, session: &Session, cat: &LiveCatalog) {
        let key = ListKey {
            generation: self.catalog.generation(),
            parental: session.parental_enabled(),
        };
        if self.list_key.as_ref() == Some(&key) {
            return;
        }
        let parental = key.parental;
        let channels: Vec<LiveChannel> = cat
            .channels
            .iter()
            .filter(|c| c.archive && c.stream_id().is_some())
            .filter(|c| {
                !parental
                    || !c.categories.iter().any(|id| {
                        session.is_category_locked(
                            ContentKind::Live,
                            id,
                            cat.category_name(id).unwrap_or(""),
                        )
                    })
            })
            .cloned()
            .collect();
        self.names = Arc::new(
            channels
                .iter()
                .map(|c| normalize_for_search(&c.name))
                .collect(),
        );
        self.channels = Arc::new(channels);
        self.filter_applied = None;
        self.list_key = Some(key);
    }

    fn refresh_filter(&mut self) {
        if self.filter_applied.as_deref() == Some(self.filter.as_str()) {
            return;
        }
        let q = SearchQuery::new(&self.filter);
        self.filtered = Arc::new(
            self.names
                .iter()
                .enumerate()
                .filter(|(_, n)| q.matches_normalized(n))
                .map(|(i, _)| i)
                .collect(),
        );
        self.filter_applied = Some(self.filter.clone());
    }

    fn channel(&self, key: &str) -> Option<&LiveChannel> {
        self.channels.iter().find(|c| c.key == key)
    }

    fn select_channel(&mut self, cx: &AppCtx<'_>, key: &str) {
        if self.selected.as_deref() == Some(key) && !self.table.is_idle() {
            return;
        }
        let Some(session) = cx.session() else { return };
        let Some(ch) = self.channel(key).cloned() else {
            return;
        };
        self.selected = Some(key.to_owned());
        self.table_for = Some(key.to_owned());
        self.days.clear();
        self.day = 0;
        self.focused = None;
        self.progress_key = None;
        // A fresh loader: the previous channel's schedule must not show under this one.
        self.table = Loader::new();
        self.days_gen = u64::MAX;
        self.table.start(cx.rt(), &cx.egui, move |_| async move {
            session.full_row(&ch).await
        });
    }

    fn rebuild_days(
        &mut self,
        session: &Session,
        now: opensmarters_core::chrono::DateTime<opensmarters_core::chrono::Utc>,
    ) {
        if self.days_gen == self.table.generation() {
            return;
        }
        let Some(row) = self.table.value() else {
            return;
        };
        let tz = session.server_time_zone();
        self.days = archive_days(row, &tz, now);
        self.days_gen = self.table.generation();
        if self.day >= self.days.len() {
            self.day = 0;
        }
    }

    fn load_progress(&mut self, cx: &AppCtx<'_>) {
        let (Some(session), Some(key), Some(day)) =
            (cx.session(), self.selected.clone(), self.days.get(self.day))
        else {
            return;
        };
        let Some(stream_id) = self.channel(&key).and_then(LiveChannel::stream_id) else {
            return;
        };
        let pk = (
            key.clone(),
            self.day,
            self.days_gen,
            cx.playback.progress_epoch(),
        );
        if self.progress_key.as_ref() == Some(&pk) {
            return;
        }
        self.progress_key = Some(pk);
        let ids: Vec<String> = day
            .programmes
            .iter()
            .map(|p| progress_id(stream_id, p))
            .collect();
        let store = cx.services.store.clone();
        let profile = session.profile_id();
        self.progress.start_blocking(cx.rt(), &cx.egui, move || {
            let mut out = HashMap::new();
            for id in ids {
                if let Some(p) = store
                    .playback()
                    .get(profile, PlaybackKind::Catchup, &id)
                    .map_err(AppError::from)?
                {
                    out.insert(id, p);
                }
            }
            Ok(out)
        });
    }

    fn resume_point(&self, pid: &str) -> Option<&PlaybackProgress> {
        self.progress.value().and_then(|m| m.get(pid))
    }

    /// Plays `programme` of `channel` in the preview with `format`.
    fn play(
        &mut self,
        cx: &mut AppCtx<'_>,
        channel: &LiveChannel,
        programme: &ArchiveProgramme,
        format: TimeshiftFormat,
        is_fallback: bool,
        resume: Option<f64>,
    ) {
        let Some(session) = cx.session() else { return };
        let Some(stream_id) = channel.stream_id() else {
            return;
        };
        match session.catchup_request_with(channel, programme, format, resume) {
            Ok(req) => {
                self.attempt = Some(Tracked {
                    attempt: Attempt {
                        item: progress_id(stream_id, programme),
                        format,
                        is_fallback,
                    },
                    channel: channel.clone(),
                    programme: programme.clone(),
                    resolved: false,
                });
                self.offer = Fallback::None;
                cx.playback.play(session, req);
            }
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    fn play_programme(&mut self, cx: &mut AppCtx<'_>, pid: &str, from_start: bool) {
        let Some(session) = cx.session() else { return };
        let Some((ch, prog)) = self.find_programme(pid) else {
            return;
        };
        let t = session.store().playback().thresholds();
        let resume = if from_start {
            None
        } else {
            self.resume_point(pid)
                .filter(|p| p.is_resumable(&t))
                .map(|p| p.position_secs)
        };
        let format = session.timeshift_format();
        self.play(cx, &ch, &prog, format, false, resume);
    }

    fn find_programme(&self, pid: &str) -> Option<(LiveChannel, ArchiveProgramme)> {
        let ch = self.channel(self.selected.as_deref()?)?.clone();
        let sid = ch.stream_id()?;
        let prog = self
            .days
            .iter()
            .flat_map(|d| d.programmes.iter())
            .find(|p| progress_id(sid, p) == pid)?
            .clone();
        Some((ch, prog))
    }

    /// The `(stream_id, start)` of the catch-up item the shared player holds.
    fn playing(cx: &AppCtx<'_>) -> Option<String> {
        match cx.playback.request().map(|r| &r.item) {
            Some(PlaybackItem::Catchup {
                stream_id, start, ..
            }) => Some(format!("{stream_id}@{start}")),
            _ => None,
        }
    }

    fn open_fullscreen(&mut self, cx: &mut AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let Some(t) = &self.attempt else { return };
        let pos = cx.playback.state().time_pos.filter(|p| *p > 1.0);
        match session.catchup_request_with(&t.channel, &t.programme, t.attempt.format, pos) {
            Ok(req) => cx
                .nav
                .push(Route::Player(PlayerRoute::Request(Arc::new(req)))),
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    /// Watches the current attempt and decides what to offer.
    fn track_attempt(&mut self, cx: &AppCtx<'_>, session: &Session) {
        let Some(t) = &mut self.attempt else { return };
        if t.resolved {
            return;
        }
        if Self::playing(cx).as_deref() != Some(t.attempt.item.as_str()) {
            return;
        }
        match cx.playback.status() {
            PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Finished => {
                t.resolved = true;
                self.offer = t.attempt.on_success(session.saved_timeshift_format());
                if t.attempt.is_fallback {
                    // It works: use it for the rest of the session.
                    session.set_timeshift_format(t.attempt.format, false);
                }
            }
            PlaybackStatus::Failed(_) => {
                t.resolved = true;
                self.offer = t.attempt.on_failure();
            }
            _ => {}
        }
        // Move the focus to the decision when the user is still where they started it.
        let still_here = focus::current(&cx.egui).is_some_and(|id| {
            id == prog_row_id(&t.attempt.item)
                || id == Id::unique("catchup.play")
                || id == Id::unique("catchup.try-alt")
        });
        if t.resolved && still_here {
            match self.offer {
                Fallback::TryAlternate(_) => {
                    focus::request(&cx.egui, Id::unique("catchup.try-alt"));
                }
                Fallback::OfferSave(_) => {
                    focus::request(&cx.egui, Id::unique("catchup.save-format"));
                }
                Fallback::GiveUp => {
                    focus::request(&cx.egui, prog_row_id(&t.attempt.item));
                }
                Fallback::None => {}
            }
        }
    }

    fn apply_handoff(&mut self, cx: &mut AppCtx<'_>) {
        let Some(h) = self.pending else { return };
        if self.catalog.value().is_none() {
            return;
        }
        let key = h.stream_id.to_string();
        if self.channel(&key).is_none() {
            self.pending = None;
            cx.toasts
                .info("This channel doesn't keep an archive you can replay.");
            return;
        }
        if self.selected.as_deref() != Some(key.as_str()) {
            self.select_channel(cx, &key);
            if let Some(i) = self
                .filtered
                .iter()
                .position(|&i| self.channels.get(i).is_some_and(|c| c.key == key))
            {
                self.scroll_channels_to = Some(i);
            }
        }
        let Some(start) = h.start else {
            self.pending = None;
            self.focus_programme = Some(String::new());
            return;
        };
        if self.table.is_loading() || self.days.is_empty() {
            if self.table.error().is_some()
                || (!self.table.is_loading() && self.table.value().is_some())
            {
                self.pending = None;
            }
            return;
        }
        self.pending = None;
        let pid = format!("{}@{start}", h.stream_id);
        let Some(day) = self
            .days
            .iter()
            .position(|d| d.programmes.iter().any(|p| p.start.timestamp() == start))
        else {
            cx.toasts
                .info("That programme is no longer in the archive.");
            return;
        };
        self.day = day;
        self.focus_programme = Some(pid.clone());
        if h.autoplay {
            self.play_programme(cx, &pid, false);
        }
    }
}

impl Screen for CatchupScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if self.catalog.is_idle() || self.catalog.error().is_some() {
            self.load_catalog(cx);
        }
        if let Some(h) = cx.egui.data_mut(|d| d.remove_temp::<Handoff>(handoff_id())) {
            self.pending = Some(h);
        }
        // Coming back from the player: progress may have changed.
        self.progress_key = None;
        if Self::playing(cx).is_none() {
            self.attempt = self.attempt.take().filter(|t| !t.resolved);
        }
    }

    fn on_leave(&mut self, cx: &mut AppCtx<'_>) {
        if !matches!(cx.next_route, Some(Route::Player(_))) && Self::playing(cx).is_some() {
            cx.playback.stop();
        }
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        if matches!(
            self.offer,
            Fallback::TryAlternate(_) | Fallback::OfferSave(_)
        ) {
            self.offer = Fallback::None;
            return true;
        }
        false
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Catchup)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.catalog.poll();
        if self.table.poll() {
            self.days_gen = 0;
        }
        self.progress.poll();
        if let Some(h) = cx.egui.data_mut(|d| d.remove_temp::<Handoff>(handoff_id())) {
            self.pending = Some(h);
        }
        let Some(session) = cx.session() else { return };
        shell::status_cluster(cx, ui);
        let content = cx.layout.content;
        let m = cx.layout.margin;
        let wide = cx.layout.breakpoint == Breakpoint::Wide;

        if !session.is_xtream() {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content));
            shell::page_title(cx, &child, "Catch-up");
            let f = EmptyState::new(
                Id::unique("catchup.m3u"),
                Illustration::EmptyGuide,
                "Catch-up needs an Xtream account",
                "Playlists (M3U) don't give opensmarters a way to replay past programmes yet.",
            )
            .action("Open the guide", false)
            .show(&mut child);
            if f.is_some_and(|f| f.activated) {
                cx.nav.section(Route::Guide);
            }
            return;
        }

        let catalog = self.catalog.value().cloned();
        if let Some(cat) = &catalog {
            self.refresh_channels(&session, cat);
            self.refresh_filter();
            if self.selected.is_none()
                && self.pending.is_none()
                && let Some(first) = self.filtered.first().and_then(|&i| self.channels.get(i))
            {
                let key = first.key.clone();
                self.select_channel(cx, &key);
            }
        }
        self.rebuild_days(&session, cx.now);
        self.apply_handoff(cx);
        self.load_progress(cx);
        self.track_attempt(cx, &session);
        // Debounced focus selection.
        if let Some((key, at)) = &self.focus_select {
            if at.elapsed() >= FOCUS_SELECT_AFTER {
                let key = key.clone();
                self.focus_select = None;
                self.select_channel(cx, &key);
            } else {
                ui.ctx()
                    .request_repaint_after(FOCUS_SELECT_AFTER.saturating_sub(at.elapsed()));
            }
        }

        // Columns.
        let chan_w = if wide { 320.0 } else { 248.0 };
        let chan_rect = Rect::from_min_size(content.min, vec2(chan_w, content.height()));
        let right = Rect::from_min_max(pos2(chan_rect.right(), content.top()), content.max);
        let preview_frac = if wide { 0.40 } else { 0.38 };
        let preview_w = ((right.width() - m * 2.0 - 32.0) * preview_frac).round();
        let list_rect = Rect::from_min_max(
            pos2(right.left() + m, content.top() + 96.0),
            pos2(
                right.right() - m - preview_w - 32.0,
                content.bottom() - 24.0,
            ),
        );
        let preview_rect = Rect::from_min_max(
            pos2(list_rect.right() + 32.0, content.top() + 104.0),
            pos2(right.right() - m, content.bottom() - 24.0),
        );

        // Channel column.
        let painter = ui.painter().clone();
        painter.rect_filled(chan_rect, 0.0, color::SURFACE_1);
        painter.vline(
            chan_rect.right() - 0.5,
            chan_rect.y_range(),
            Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        painter.text(
            pos2(chan_rect.left() + 24.0, content.top() + 56.0),
            Align2::LEFT_CENTER,
            "Catch-up",
            font::heading(),
            color::TEXT,
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
            pos2(chan_rect.left() + 16.0, content.top() + 96.0),
            pos2(chan_rect.right() - 16.0, chan_rect.bottom() - 16.0),
        )));
        focus::scope(&mut child, Id::unique("catchup.chan-col"), |ui| {
            self.channel_column(cx, ui, catalog.is_some())
        });

        // Programme column.
        self.programme_column(cx, ui, &session, list_rect, catalog.is_some());

        // Preview column.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(preview_rect));
        focus::scope(&mut child, Id::unique("catchup.preview"), |ui| {
            self.preview_column(cx, ui, &session, preview_rect)
        });
    }
}

impl CatchupScreen {
    fn channel_column(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, loaded: bool) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let w = ui.available_width();
        SearchField::new(Id::unique("catchup.filter"), "Find a channel")
            .size(w - 16.0, 48.0)
            .show(ui, &mut self.filter);
        ui.add_space(12.0);
        if !loaded {
            if let Some(err) = self.catalog.error().cloned() {
                text::block(
                    ui.painter(),
                    ui.cursor().min + vec2(8.0, 8.0),
                    &err.user_message(),
                    font::caption(),
                    color::TEXT_3,
                    w - 24.0,
                    4,
                );
                return;
            }
            for i in 0..8 {
                let (r, _) = ui.allocate_exact_size(vec2(w, CHANNEL_ROW_H), Sense::hover());
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(r.min + vec2(12.0, 13.0), vec2(48.0, 30.0)),
                    radius::SM,
                );
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(
                        r.min + vec2(72.0, 21.0),
                        vec2(90.0 + (i % 3) as f32 * 30.0, 14.0),
                    ),
                    radius::XS,
                );
            }
            return;
        }
        let count = self.filtered.len();
        text::overline(
            ui.painter(),
            ui.cursor().min + vec2(8.0, 8.0),
            Align2::LEFT_CENTER,
            &format!(
                "{count} channel{} with archive",
                if count == 1 { "" } else { "s" }
            ),
            color::TEXT_3,
        );
        ui.add_space(20.0);
        if count == 0 {
            let msg = if self.channels.is_empty() {
                "Your provider doesn't keep an archive for any channel."
            } else {
                "No channel matches that name."
            };
            text::block(
                ui.painter(),
                ui.cursor().min + vec2(8.0, 4.0),
                msg,
                font::body(),
                color::TEXT_2,
                w - 24.0,
                3,
            );
            return;
        }
        let filtered = Arc::clone(&self.filtered);
        let channels = Arc::clone(&self.channels);
        let images = cx.services.images.clone();
        let selected = self.selected.clone();
        let mut activated: Option<String> = None;
        let mut focused_key: Option<String> = None;
        VirtualList::new(channels_list_id(), CHANNEL_ROW_H, filtered.len())
            .gap(4.0)
            .scroll_to(self.scroll_channels_to.take())
            .show(
                ui,
                |i| {
                    filtered
                        .get(i)
                        .and_then(|&ix| channels.get(ix))
                        .map_or(Id::NULL, |c| channel_row_id(&c.key))
                },
                |ui, i, rect| {
                    let Some(ch) = filtered.get(i).and_then(|&ix| channels.get(ix)) else {
                        return;
                    };
                    let is_sel = selected.as_deref() == Some(ch.key.as_str());
                    let (_, f) = focus::focusable(
                        ui,
                        channel_row_id(&ch.key),
                        rect,
                        FocusOpts::default().autofocus_if(is_sel || (selected.is_none() && i == 0)),
                    );
                    let painter = ui.painter().clone();
                    if f.focused {
                        painter.rect_filled(rect, CornerRadius::same(radius::SM), color::SURFACE_3);
                        focused_key = Some(ch.key.clone());
                    } else if is_sel {
                        painter.rect_filled(rect, CornerRadius::same(radius::SM), color::SURFACE_2);
                    }
                    if is_sel {
                        let bar = Rect::from_min_size(
                            pos2(rect.left(), rect.center().y - 12.0),
                            vec2(3.0, 24.0),
                        );
                        painter.rect_filled(bar, CornerRadius::same(2), color::CREAM);
                    }
                    let logo = Rect::from_min_size(
                        pos2(rect.left() + 12.0, rect.center().y - 15.0),
                        vec2(48.0, 30.0),
                    );
                    let slot = images.get(ch.logo.as_deref(), ImageSize::Logo);
                    ChannelLogo::paint(ui, &painter, logo, &ch.name, &slot);
                    let tx = logo.right() + 12.0;
                    let tw = rect.right() - 12.0 - tx;
                    let fg = if f.focused || is_sel {
                        color::TEXT
                    } else {
                        color::TEXT_2
                    };
                    text::line(
                        &painter,
                        pos2(tx, rect.center().y - 9.0),
                        Align2::LEFT_CENTER,
                        split_tag(&ch.name).1,
                        font::body_strong(),
                        fg,
                        tw,
                    );
                    let days = ch.archive_days.filter(|d| *d > 0).map_or_else(
                        || "Archive".to_owned(),
                        |d| format!("{d} day{}", if d == 1 { "" } else { "s" }),
                    );
                    let sub = match ch.number {
                        Some(n) => format!("{n:03} · {days}"),
                        None => days,
                    };
                    text::line(
                        &painter,
                        pos2(tx, rect.center().y + 11.0),
                        Align2::LEFT_CENTER,
                        &sub,
                        font::caption(),
                        color::TEXT_3,
                        tw,
                    );
                    if f.focused {
                        paint::focus_ring(&painter, rect, radius::SM, 1.0);
                    }
                    if f.activated {
                        activated = Some(ch.key.clone());
                    }
                },
            );
        if let Some(key) = activated {
            self.focus_select = None;
            self.select_channel(cx, &key);
            self.focus_programme = Some(String::new());
        } else if let Some(key) = focused_key {
            if self.selected.as_deref() != Some(key.as_str())
                && self.focus_select.as_ref().is_none_or(|(k, _)| *k != key)
            {
                self.focus_select = Some((key, Instant::now()));
            }
        } else {
            self.focus_select = None;
        }
    }

    fn programme_column(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        rect: Rect,
        loaded: bool,
    ) {
        let painter = ui.painter().clone();
        let channel = self
            .selected
            .as_deref()
            .and_then(|k| self.channel(k))
            .cloned();
        let head_y = cx.layout.content.top() + 56.0;
        let title = channel
            .as_ref()
            .map_or("Catch-up", |c| split_tag(&c.name).1)
            .to_owned();
        let tr = text::line(
            &painter,
            pos2(rect.left(), head_y),
            Align2::LEFT_CENTER,
            &title,
            font::heading(),
            color::TEXT,
            rect.width() - 140.0,
        );
        if let Some(ch) = &channel
            && let Some(d) = ch.archive_days.filter(|d| *d > 0)
        {
            painter.text(
                pos2(tr.right() + 12.0, head_y + 4.0),
                Align2::LEFT_CENTER,
                format!("{d} day{} of archive", if d == 1 { "" } else { "s" }),
                font::caption(),
                color::TEXT_3,
            );
        }
        if !loaded || channel.is_none() {
            if loaded && self.channels.is_empty() {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                let _ = EmptyState::new(
                    Id::unique("catchup.none"),
                    Illustration::EmptyGuide,
                    "No catch-up here",
                    "Your provider doesn't keep an archive for any channel on this account.",
                )
                .illustration_size(160.0)
                .show(&mut child);
            }
            return;
        }

        // Day tabs.
        let tabs_rect = Rect::from_min_size(rect.min, vec2(rect.width(), 48.0));
        let list_top = rect.top() + 64.0;
        if !self.days.is_empty() {
            let labels: Vec<String> = self.days.iter().map(|d| d.label.clone()).collect();
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(tabs_rect));
            let mut sel = self.day;
            let changed = focus::scope(&mut child, Id::unique("catchup.days"), |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("catchup.days.scroll")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .content_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        Tabs::new(Id::unique("catchup.day-tab"), &labels).show(ui, &mut sel)
                    })
                    .inner
            });
            focus::forward_scroll(&child);
            if changed {
                self.day = sel;
                self.scroll_progs_to = Some(0);
                self.focused = None;
            }
        }
        painter.hline(
            rect.x_range(),
            list_top - 8.0,
            Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        // Days are the provider's; say so when its clock differs from ours.
        if !self.days.is_empty() {
            let server_off =
                (session.server_time_zone().to_local(cx.now) - cx.now.naive_utc()).num_minutes();
            let local_off = i64::from(
                opensmarters_core::chrono::Offset::fix(cx.now.with_timezone(&Local).offset())
                    .local_minus_utc(),
            ) / 60;
            if server_off != local_off && rect.width() > 560.0 {
                text::line(
                    &painter,
                    pos2(rect.right(), tabs_rect.center().y),
                    Align2::RIGHT_CENTER,
                    "Days follow your provider's clock",
                    font::caption(),
                    color::TEXT_3,
                    rect.width() * 0.4,
                );
            }
        }
        let rows_rect = Rect::from_min_max(
            pos2(rect.left() - 8.0, list_top),
            pos2(rect.right() + 8.0, rect.bottom()),
        );

        match (self.table.value().cloned(), self.table.error().cloned()) {
            (None, Some(err)) => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                let f = EmptyState::new(
                    Id::unique("catchup.table-error"),
                    Illustration::ErrorOffline,
                    err.title(),
                    &err.user_message(),
                )
                .action("Retry", true)
                .illustration_size(160.0)
                .show(&mut child);
                if f.is_some_and(|f| f.activated)
                    && let Some(k) = self.selected.clone()
                {
                    self.table = Loader::new();
                    self.select_channel(cx, &k);
                }
            }
            (None, None) => {
                if self.table.show_skeleton() {
                    let mut y = rows_rect.top() + 8.0;
                    while y + PROG_ROW_H < rows_rect.bottom() {
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(
                                pos2(rows_rect.left() + 24.0, y + 18.0),
                                vec2(52.0, 14.0),
                            ),
                            radius::XS,
                        );
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(
                                pos2(rows_rect.left() + 112.0, y + 18.0),
                                vec2(220.0, 14.0),
                            ),
                            radius::XS,
                        );
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(
                                pos2(rows_rect.left() + 112.0, y + 42.0),
                                vec2(300.0, 10.0),
                            ),
                            radius::XS,
                        );
                        y += PROG_ROW_H + PROG_GAP;
                    }
                }
            }
            (Some(_), _) if self.days.is_empty() => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                let _ = EmptyState::new(
                    Id::unique("catchup.empty-channel"),
                    Illustration::EmptyGuide,
                    "Nothing to replay yet",
                    "This channel has no archived programmes right now. Try another channel or come back later.",
                )
                .illustration_size(160.0)
                .show(&mut child);
            }
            (Some(_), _) => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                self.programme_list(cx, &mut child, session);
            }
        }
    }

    fn programme_list(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>) {
        let Some(ch) = self
            .selected
            .as_deref()
            .and_then(|k| self.channel(k))
            .cloned()
        else {
            return;
        };
        let Some(sid) = ch.stream_id() else { return };
        let Some(day) = self.days.get(self.day).cloned() else {
            return;
        };
        let progs = day.programmes;
        let pids: Vec<String> = progs.iter().map(|p| progress_id(sid, p)).collect();
        // Focus a programme after a selection/hand-off (latest one for "Today").
        if let Some(want) = self.focus_programme.take() {
            let idx = if want.is_empty() {
                if self.day == 0 {
                    progs.len().saturating_sub(1)
                } else {
                    0
                }
            } else {
                pids.iter().position(|p| *p == want).unwrap_or(0)
            };
            if let Some(pid) = pids.get(idx) {
                focus::request(&cx.egui, prog_row_id(pid));
                self.scroll_progs_to = Some(idx);
            }
        }
        let playing = Self::playing(cx);
        let failed = matches!(
            cx.playback.status(),
            PlaybackStatus::Failed(_) | PlaybackStatus::Finished
        );
        let thresholds = session.store().playback().thresholds();
        let progress = self.progress.value().cloned().unwrap_or_default();
        let mut activated: Option<usize> = None;
        let mut focused: Option<String> = None;
        let now = cx.now;
        VirtualList::new(progs_list_id(), PROG_ROW_H, progs.len())
            .gap(PROG_GAP)
            .scroll_to(self.scroll_progs_to.take())
            .show(
                ui,
                |i| pids.get(i).map_or(Id::NULL, |p| prog_row_id(p)),
                |ui, i, rect| {
                    let (Some(p), Some(pid)) = (progs.get(i), pids.get(i)) else {
                        return;
                    };
                    let is_playing = playing.as_deref() == Some(pid.as_str());
                    let (_, f) = focus::focusable(ui, prog_row_id(pid), rect, FocusOpts::default());
                    let painter = ui.painter().clone();
                    if f.focused {
                        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_3);
                        focused = Some(pid.clone());
                    } else if is_playing {
                        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_2);
                    }
                    if is_playing {
                        let bar = Rect::from_min_size(
                            pos2(rect.left(), rect.top() + 14.0),
                            vec2(3.0, rect.height() - 28.0),
                        );
                        painter.rect_filled(bar, CornerRadius::same(2), color::ACCENT);
                    }
                    let start = p.start.with_timezone(&Local);
                    painter.text(
                        pos2(rect.left() + 20.0, rect.top() + 24.0),
                        Align2::LEFT_CENTER,
                        cx.prefs.format_time(&start),
                        font::body_strong(),
                        color::TEXT,
                    );
                    painter.text(
                        pos2(rect.left() + 20.0, rect.top() + 48.0),
                        Align2::LEFT_CENTER,
                        text::duration_short(f64::from(p.duration_minutes()) * 60.0),
                        font::caption(),
                        color::TEXT_3,
                    );
                    let tx = rect.left() + 108.0;
                    // Room for the status on the right only when there is one.
                    let has_status = (p.stop > now && p.start <= now)
                        || progress.get(pid.as_str()).is_some_and(|pp| {
                            pp.finished || pp.ever_finished || pp.is_resumable(&thresholds)
                        });
                    let right_w = if has_status {
                        132.0
                    } else if f.focused {
                        48.0
                    } else {
                        16.0
                    };
                    let tw = (rect.right() - right_w - tx).max(60.0);
                    text::line(
                        &painter,
                        pos2(tx, rect.top() + 24.0),
                        Align2::LEFT_CENTER,
                        &p.title,
                        font::body_strong(),
                        if f.focused || is_playing {
                            color::TEXT
                        } else {
                            color::TEXT_2
                        },
                        tw,
                    );
                    if !p.description.trim().is_empty() {
                        text::line(
                            &painter,
                            pos2(tx, rect.top() + 47.0),
                            Align2::LEFT_CENTER,
                            &p.description,
                            font::caption(),
                            color::TEXT_3,
                            tw,
                        );
                    }
                    // Right: watched state / on air.
                    let rx = rect.right() - 16.0;
                    let cy = rect.center().y;
                    let point = progress.get(pid.as_str());
                    if p.stop > now && p.start <= now {
                        crate::ui::widgets::Badge::live(
                            &painter,
                            pos2(rx, cy),
                            Align2::RIGHT_CENTER,
                        );
                    } else if let Some(pp) = point {
                        if pp.finished || pp.ever_finished && !pp.is_resumable(&thresholds) {
                            let r = painter.text(
                                pos2(rx, cy),
                                Align2::RIGHT_CENTER,
                                "Watched",
                                font::caption(),
                                color::TEXT_3,
                            );
                            icons::paint(
                                &painter,
                                pos2(r.left() - 12.0, cy),
                                icons::CHECK,
                                16.0,
                                Style::Regular,
                                color::SAGE,
                            );
                        } else if pp.is_resumable(&thresholds) {
                            painter.text(
                                pos2(rx, cy - 8.0),
                                Align2::RIGHT_CENTER,
                                format!("Resume {}", text::timecode(pp.position_secs)),
                                font::caption(),
                                color::TEXT_2,
                            );
                            let bar =
                                Rect::from_min_size(pos2(rx - 96.0, cy + 8.0), vec2(96.0, 3.0));
                            paint::progress_bar(
                                &painter,
                                bar,
                                pp.fraction().unwrap_or(0.0) as f32,
                                color::ACCENT,
                                color::PROGRESS_TRACK,
                            );
                        }
                    } else if f.focused {
                        icons::paint(
                            &painter,
                            pos2(rx - 10.0, cy),
                            if is_playing && !failed {
                                icons::CORNERS_OUT
                            } else {
                                icons::PLAY
                            },
                            20.0,
                            Style::Regular,
                            color::TEXT,
                        );
                    }
                    if f.focused {
                        paint::focus_ring(&painter, rect, radius::MD, 1.0);
                    }
                    if f.activated {
                        activated = Some(i);
                    }
                },
            );
        if let Some(pid) = focused {
            self.focused = Some(pid);
        }
        if let Some(i) = activated
            && let Some(pid) = pids.get(i).cloned()
        {
            let playing_ok = playing.as_deref() == Some(pid.as_str()) && !failed;
            if playing_ok {
                self.open_fullscreen(cx);
            } else {
                self.play_programme(cx, &pid, false);
            }
        }
    }

    fn preview_column(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        rect: Rect,
    ) {
        let painter = ui.painter().clone();
        let playing = Self::playing(cx);
        // Which programme the card describes: the playing one, else the focused one.
        let shown_pid = playing
            .clone()
            .filter(|p| self.find_programme(p).is_some())
            .or_else(|| self.focused.clone());
        let shown = shown_pid.as_deref().and_then(|p| self.find_programme(p));
        let is_playing = shown_pid.is_some() && shown_pid == playing;

        // Header.
        let hy = rect.top() + 8.0;
        if is_playing {
            painter.circle_filled(pos2(rect.left() + 3.0, hy), 3.0, color::ACCENT);
            text::overline(
                &painter,
                pos2(rect.left() + 12.0, hy),
                Align2::LEFT_CENTER,
                "Playing in preview",
                color::TEXT_2,
            );
        } else {
            text::overline(
                &painter,
                pos2(rect.left(), hy),
                Align2::LEFT_CENTER,
                "Preview",
                color::TEXT_3,
            );
        }

        // Video (16:9).
        let video = Rect::from_min_size(
            pos2(rect.left(), rect.top() + 28.0),
            vec2(rect.width(), (rect.width() * 9.0 / 16.0).round()),
        );
        let (vresp, vf) =
            focus::focusable(ui, Id::unique("catchup.video"), video, FocusOpts::default());
        if vf.anim > 0.0 {
            paint::focus_glow(&painter, video, radius::MD, vf.anim);
        }
        painter.rect_filled(video, CornerRadius::same(radius::MD), color::VIDEO);
        let status = cx.playback.status();
        let unavailable_msg = "Catch-up isn't available for this programme right now";
        if is_playing {
            cx.playback.paint_video(ui, video);
            match &status {
                PlaybackStatus::Opening | PlaybackStatus::Buffering => {
                    paint::spinner(&painter, video.center(), 22.0, ui.input(|i| i.time), 3.0);
                    ui.ctx().request_repaint();
                }
                PlaybackStatus::Reconnecting { attempt } => {
                    paint::spinner(
                        &painter,
                        video.center() - vec2(0.0, 12.0),
                        22.0,
                        ui.input(|i| i.time),
                        3.0,
                    );
                    painter.text(
                        video.center() + vec2(0.0, 30.0),
                        Align2::CENTER_CENTER,
                        format!("Reconnecting… ({attempt})"),
                        font::caption(),
                        color::TEXT_2,
                    );
                    ui.ctx().request_repaint();
                }
                PlaybackStatus::Failed(msg) => {
                    painter.rect_filled(video, CornerRadius::same(radius::MD), color::BG_DEEP);
                    let c = video.center();
                    icons::paint(
                        &painter,
                        c - vec2(0.0, 44.0),
                        icons::CLOCK_COUNTER_CLOCKWISE,
                        32.0,
                        Style::Regular,
                        color::TEXT_3,
                    );
                    let notice = cx.playback.notice();
                    let (head, body): (&str, String) = match (notice, self.offer) {
                        (Some(n), _) => ("Playback stopped", n),
                        (None, Fallback::TryAlternate(_)) => (
                            unavailable_msg,
                            "Some providers expect a different link format for catch-up. You can try the other one once.".to_owned(),
                        ),
                        (None, Fallback::GiveUp) => (
                            unavailable_msg,
                            "Your provider didn't play it with either link format. It may not keep this archive, or it may be busy. Try again later.".to_owned(),
                        ),
                        (None, _) => (unavailable_msg, msg.clone()),
                    };
                    let g = text::galley(
                        &painter,
                        head,
                        font::title_sm(),
                        color::TEXT,
                        video.width() - 64.0,
                        2,
                    );
                    let hr = Rect::from_center_size(c + vec2(0.0, -4.0), g.size());
                    painter.galley(hr.min, g, color::TEXT);
                    let mut job = text::job(
                        &body,
                        font::caption(),
                        color::TEXT_2,
                        video.width() - 80.0,
                        3,
                    );
                    job.halign = egui::Align::Center;
                    let bg = painter.layout_job(job);
                    painter.galley(pos2(c.x, hr.bottom() + 8.0), bg, color::TEXT_2);
                }
                PlaybackStatus::Unavailable(msg) => {
                    painter.rect_filled(video, CornerRadius::same(radius::MD), color::BG_DEEP);
                    text::block(
                        &painter,
                        video.left_top() + vec2(24.0, video.height() / 2.0 - 20.0),
                        msg,
                        font::caption(),
                        color::TEXT_2,
                        video.width() - 48.0,
                        4,
                    );
                }
                PlaybackStatus::Finished => {
                    painter.rect_filled(video, CornerRadius::same(radius::MD), color::BG_DEEP);
                    icons::paint(
                        &painter,
                        video.center() - vec2(0.0, 18.0),
                        icons::CHECK_CIRCLE,
                        32.0,
                        Style::Regular,
                        color::SAGE,
                    );
                    painter.text(
                        video.center() + vec2(0.0, 20.0),
                        Align2::CENTER_CENTER,
                        "Finished · Enter to watch it again",
                        font::body(),
                        color::TEXT_2,
                    );
                }
                _ => {}
            }
        } else {
            if !assets::paint_cover(
                ui,
                &painter,
                Asset::PlayerIdle,
                video,
                vec2(0.5, 0.5),
                radius::MD,
                Color32::from_gray(170),
            ) {
                painter.rect_filled(video, CornerRadius::same(radius::MD), color::BG_DEEP);
            }
            if let Some((ch, _)) = &shown {
                let logo =
                    Rect::from_center_size(video.center() - vec2(0.0, 18.0), vec2(112.0, 70.0));
                let slot = cx.services.images.get(ch.logo.as_deref(), ImageSize::Logo);
                ChannelLogo::paint(ui, &painter, logo, &ch.name, &slot);
                painter.text(
                    pos2(video.center().x, logo.bottom() + 24.0),
                    Align2::CENTER_CENTER,
                    "Press Enter to watch from the start",
                    font::caption(),
                    color::TEXT_2,
                );
            } else {
                let sym =
                    Rect::from_center_size(video.center() - vec2(0.0, 18.0), vec2(48.0, 48.0));
                assets::paint_contain(
                    ui,
                    &painter,
                    Asset::Symbol,
                    sym,
                    Color32::WHITE.gamma_multiply(0.6),
                );
                painter.text(
                    video.center() + vec2(0.0, 30.0),
                    Align2::CENTER_CENTER,
                    "Choose a programme to replay",
                    font::body(),
                    color::TEXT_2,
                );
            }
        }
        paint::round_corners_mask(&painter, video, f32::from(radius::MD), color::BG);
        painter.rect_stroke(
            video,
            CornerRadius::same(radius::MD),
            Stroke::new(1.0, color::STROKE_SUBTLE),
            StrokeKind::Inside,
        );
        if vf.focused {
            paint::focus_ring(&painter, video, radius::MD, 1.0);
        }
        let failed = matches!(status, PlaybackStatus::Failed(_));
        let finished = matches!(status, PlaybackStatus::Finished);
        if (vf.activated || vresp.double_clicked()) && is_playing && !failed {
            self.open_fullscreen(cx);
        }

        // Programme card.
        let mut y = video.bottom() + 24.0;
        let w = rect.width();
        if let Some((ch, p)) = &shown {
            let day = self
                .days
                .iter()
                .find(|d| d.programmes.iter().any(|q| q.start == p.start))
                .map_or("", |d| d.label.as_str());
            let range = format!(
                "{} · {}–{}",
                day,
                cx.prefs.format_time(&p.start.with_timezone(&Local)),
                cx.prefs.format_time(&p.stop.with_timezone(&Local))
            );
            text::overline(
                &painter,
                pos2(rect.left(), y),
                Align2::LEFT_CENTER,
                &range,
                color::TEXT_3,
            );
            y += 18.0;
            let tr = text::block(
                &painter,
                pos2(rect.left(), y),
                &p.title,
                font::title(),
                color::TEXT,
                w,
                2,
            );
            y = tr.bottom() + 6.0;
            let meta = format!(
                "{} · {}",
                split_tag(&ch.name).1,
                text::duration_short(f64::from(p.duration_minutes()) * 60.0)
            );
            let mr = text::line(
                &painter,
                pos2(rect.left(), y + 9.0),
                Align2::LEFT_CENTER,
                &meta,
                font::caption(),
                color::TEXT_3,
                w,
            );
            y = mr.bottom() + 12.0;
            let pid = progress_id(ch.stream_id().unwrap_or_default(), p);
            let thresholds = session.store().playback().thresholds();
            if let Some(pp) = self
                .resume_point(&pid)
                .filter(|pp| pp.is_resumable(&thresholds))
            {
                paint::progress_bar(
                    &painter,
                    Rect::from_min_size(pos2(rect.left(), y), vec2(w, 4.0)),
                    pp.fraction().unwrap_or(0.0) as f32,
                    color::ACCENT,
                    color::PROGRESS_TRACK,
                );
                y += 16.0;
            }
            // Synopsis: as many lines as fit above the buttons (max 4).
            let rows = ((rect.bottom() - 140.0 - y) / 24.0).floor().clamp(0.0, 4.0) as usize;
            if rows > 0 && !p.description.trim().is_empty() {
                let r = text::block(
                    &painter,
                    pos2(rect.left(), y),
                    &p.description,
                    font::body(),
                    color::TEXT_2,
                    w,
                    rows,
                );
                y = r.bottom() + 8.0;
            }
        }

        // Actions.
        let by = (y + 16.0)
            .min(rect.bottom() - 52.0 - 72.0)
            .max(video.bottom() + 24.0);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
            pos2(rect.left(), by),
            pos2(rect.right(), rect.bottom()),
        )));
        child.spacing_mut().item_spacing.y = 12.0;
        let shown_pid2 = shown_pid.clone();
        match self.offer {
            Fallback::TryAlternate(fmt) if is_playing && failed => {
                let mut go = false;
                let mut dismiss = false;
                child.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    go = Button::primary("Try the other link format")
                        .icon(icons::ARROW_CLOCKWISE)
                        .id(Id::unique("catchup.try-alt"))
                        .show(ui)
                        .activated;
                    dismiss = Button::ghost("Not now")
                        .id(Id::unique("catchup.try-alt.cancel"))
                        .show(ui)
                        .activated;
                });
                if go && let Some(t) = self.attempt.clone() {
                    let resume = None;
                    self.play(cx, &t.channel, &t.programme, fmt, true, resume);
                }
                if dismiss {
                    self.offer = Fallback::None;
                }
            }
            Fallback::OfferSave(fmt) => {
                Callout::new(
                    CalloutKind::Success,
                    "The other link format works.",
                    format!(
                        "Your provider plays catch-up with the {} format. Use it for this account from now on?",
                        format_label(fmt)
                    ),
                )
                .width(w)
                .show(&mut child);
                let mut save = false;
                let mut later = false;
                child.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    save = Button::primary("Use it from now on")
                        .icon(icons::CHECK)
                        .id(Id::unique("catchup.save-format"))
                        .show(ui)
                        .activated;
                    later = Button::ghost("Not now")
                        .id(Id::unique("catchup.save-format.later"))
                        .show(ui)
                        .activated;
                });
                if save {
                    session.set_timeshift_format(fmt, true);
                    cx.toasts
                        .success("Saved. Catch-up will use this link format.");
                    self.offer = Fallback::None;
                    focus::request(&cx.egui, Id::unique("catchup.fullscreen"));
                }
                if later {
                    self.offer = Fallback::None;
                    focus::request(&cx.egui, Id::unique("catchup.fullscreen"));
                }
            }
            _ => {
                let Some(pid) = shown_pid2 else { return };
                let thresholds = session.store().playback().thresholds();
                let resume = self
                    .resume_point(&pid)
                    .filter(|p| p.is_resumable(&thresholds))
                    .map(|p| p.position_secs);
                let mut play = false;
                let mut restart = false;
                let mut full = false;
                child.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if is_playing && !failed && !finished {
                        full = Button::primary("Fullscreen")
                            .icon(icons::CORNERS_OUT)
                            .id(Id::unique("catchup.fullscreen"))
                            .show(ui)
                            .activated;
                    } else {
                        let label = match resume {
                            Some(s) => format!("Resume {}", text::timecode(s)),
                            None if failed && is_playing => "Try again".to_owned(),
                            None if finished && is_playing => "Watch again".to_owned(),
                            None => "Play".to_owned(),
                        };
                        play = Button::primary(label)
                            .icon(icons::PLAY)
                            .id(Id::unique("catchup.play"))
                            .show(ui)
                            .activated;
                    }
                    if resume.is_some() {
                        restart = Button::secondary("Start over")
                            .icon(icons::ARROW_COUNTER_CLOCKWISE)
                            .id(Id::unique("catchup.restart"))
                            .show(ui)
                            .activated;
                    }
                });
                if full {
                    self.open_fullscreen(cx);
                } else if play {
                    self.play_programme(cx, &pid, false);
                } else if restart {
                    self.play_programme(cx, &pid, true);
                }
            }
        }
        // Which format is in use (small print, helps support).
        let f = session.timeshift_format();
        let note = if f == session.saved_timeshift_format() {
            format!("Link format: {}", format_label(f))
        } else {
            format!("Link format: {} (this session)", format_label(f))
        };
        text::line(
            &painter,
            pos2(rect.left(), rect.bottom() - 10.0),
            Align2::LEFT_CENTER,
            &note,
            font::caption(),
            color::TEXT_3,
            w,
        );
        let _ = alternate;
    }
}

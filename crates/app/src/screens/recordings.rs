// SPDX-License-Identifier: GPL-3.0-or-later
//! Recordings & downloads (`Route::Recordings { tab }`): tabs Downloads / Recordings /
//! Scheduled, drawn from the DVR's job list (`cx.dvr`, no provider request), so the page
//! works offline and saved files play even when the provider can't be reached.
//!
//! - **Header:** "Recordings", the free space and folder, "Open folder".
//! - **Rows:** poster or channel logo, title, a subtitle (channel / series, when), the
//!   state with a progress bar, size and duration. Actions on the row: Play, Pause /
//!   Resume, Stop (live), Cancel, Delete (confirmed), Retry. Enter plays a saved file;
//!   Delete asks to delete it.
//! - **Scheduled** also lists programme reminders (`store.reminders()`), read-only with
//!   Cancel (the reminders themselves fire elsewhere).

use std::sync::Arc;

use eframe::egui::{self, Align2, CornerRadius, Id, Key, Modifiers, Rect, Ui, pos2, vec2};
use opensmarters_core::chrono::{DateTime, Local, Utc};
use opensmarters_store::{
    Recording, RecordingId, RecordingKind, RecordingState, Reminder, ReminderId,
};

use super::{AppCtx, Screen, shell};
use crate::dvr::ui as dvr_ui;
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{RecordingsTab, Route, Section};
use crate::screens::settings::human_bytes;
use crate::services::ImageSize;
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{Button, ChannelLogo, Dialog, EmptyState, Tabs, VirtualList};
use crate::ui::{paint, text};

const ROW_H: f32 = 104.0;

/// One line of a tab.
#[derive(Debug, Clone)]
enum Row {
    Job(Recording),
    Reminder(Reminder),
}

impl Row {
    fn key(&self) -> (u8, i64) {
        match self {
            Row::Job(r) => (0, r.id.0),
            Row::Reminder(r) => (1, r.id.0),
        }
    }
}

fn row_id(key: (u8, i64)) -> Id {
    Id::unique(("rec.row", key))
}

fn action_id(key: (u8, i64), action: &str) -> Id {
    Id::unique(("rec.action", key, action))
}

/// What the user asked for on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Play,
    Pause,
    Resume,
    Stop,
    Cancel,
    Delete,
    Retry,
    CancelReminder,
}

impl Action {
    fn slug(self) -> &'static str {
        match self {
            Action::Play => "play",
            Action::Pause => "pause",
            Action::Resume => "resume",
            Action::Stop => "stop",
            Action::Cancel => "cancel",
            Action::Delete => "delete",
            Action::Retry => "retry",
            Action::CancelReminder => "cancel-reminder",
        }
    }

    fn button(self) -> Button {
        let (label, glyph, danger) = match self {
            Action::Play => ("Play", icons::PLAY, false),
            Action::Pause => ("Pause", icons::PAUSE, false),
            Action::Resume => ("Resume", icons::DOWNLOAD_SIMPLE, false),
            Action::Stop => ("Stop", icons::STOP, false),
            Action::Cancel | Action::CancelReminder => ("Cancel", icons::X, false),
            Action::Delete => ("Delete", icons::TRASH, true),
            Action::Retry => ("Retry", icons::ARROW_CLOCKWISE, false),
        };
        let b = if danger {
            Button::danger(label)
        } else {
            Button::ghost(label)
        };
        b.icon(glyph).small()
    }
}

/// The actions offered for a job, primary first.
fn actions_for(r: &Recording) -> Vec<Action> {
    use RecordingState as S;
    match (r.kind.is_download(), r.state) {
        (_, S::Done) => vec![Action::Play, Action::Delete],
        (_, S::Failed) if r.kind.is_download() => vec![Action::Retry, Action::Delete],
        (_, S::Failed) => vec![Action::Delete],
        (true, S::Active | S::Queued) => vec![Action::Pause, Action::Cancel],
        (true, S::Paused) => vec![Action::Resume, Action::Cancel],
        (false, S::Active) => vec![Action::Stop],
        (false, S::Queued | S::Paused | S::Scheduled) => vec![Action::Cancel],
        (_, S::Scheduled | S::Cancelled) => vec![Action::Cancel],
    }
}

/// Running and waiting jobs first, in queue order (oldest first); then paused, failed
/// and finished ones, newest first.
fn queue_order(mut v: Vec<Recording>) -> Vec<Recording> {
    let rank = |s: RecordingState| match s {
        RecordingState::Active => 0,
        RecordingState::Queued => 1,
        RecordingState::Paused => 2,
        RecordingState::Failed => 3,
        _ => 4,
    };
    v.sort_by(|a, b| {
        rank(a.state).cmp(&rank(b.state)).then_with(|| {
            if rank(a.state) <= 1 {
                (a.created_at, a.id).cmp(&(b.created_at, b.id))
            } else {
                (b.created_at, b.id).cmp(&(a.created_at, a.id))
            }
        })
    });
    v
}

fn tab_rows(
    tab: RecordingsTab,
    jobs: &[Recording],
    reminders: &[Reminder],
    now: DateTime<Utc>,
) -> Vec<Row> {
    match tab {
        RecordingsTab::Downloads => queue_order(
            jobs.iter()
                .filter(|r| r.kind.is_download() && r.state != RecordingState::Cancelled)
                .cloned()
                .collect(),
        )
        .into_iter()
        .map(Row::Job)
        .collect(),
        RecordingsTab::Recordings => queue_order(
            jobs.iter()
                .filter(|r| {
                    !r.kind.is_download()
                        && !matches!(
                            r.state,
                            RecordingState::Scheduled | RecordingState::Cancelled
                        )
                })
                .cloned()
                .collect(),
        )
        .into_iter()
        .map(Row::Job)
        .collect(),
        RecordingsTab::Scheduled => {
            let mut v: Vec<(i64, Row)> = jobs
                .iter()
                .filter(|r| r.kind == RecordingKind::Live && r.state == RecordingState::Scheduled)
                .map(|r| {
                    (
                        r.start_at.map_or(i64::MAX, |t| t.as_unix_millis()),
                        Row::Job(r.clone()),
                    )
                })
                .collect();
            let now_ms = now.timestamp_millis();
            v.extend(
                reminders
                    .iter()
                    .filter(|r| r.fired_at.is_none() && r.end_at.as_unix_millis() > now_ms)
                    .map(|r| (r.start_at.as_unix_millis(), Row::Reminder(r.clone()))),
            );
            v.sort_by_key(|(t, _)| *t);
            v.into_iter().map(|(_, r)| r).collect()
        }
    }
}

/// The folder's last two parts (`…\Videos\opensmarters`): the full path is in Settings.
fn short_path(p: &std::path::Path) -> String {
    let parts: Vec<String> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.len() <= 3 {
        return p.display().to_string();
    }
    let sep = std::path::MAIN_SEPARATOR;
    format!(
        "…{sep}{}{sep}{}",
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

/// The Recordings screen.
#[derive(Debug)]
pub struct RecordingsScreen {
    tab: RecordingsTab,
    reminders: Loader<Arc<Vec<Reminder>>>,
    space: Loader<Option<u64>>,
    space_epoch: Option<u64>,
    confirm: Option<(RecordingId, String)>,
    focus_after: Option<Id>,
}

impl RecordingsScreen {
    /// Creates the screen on `tab`.
    pub fn new(tab: RecordingsTab) -> Self {
        Self {
            tab,
            reminders: Loader::new(),
            space: Loader::new(),
            space_epoch: None,
            confirm: None,
            focus_after: None,
        }
    }

    fn load_reminders(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let store = Arc::clone(&cx.services.store);
        let profile = session.profile_id();
        self.reminders.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(Arc::new(
                store.reminders().list(profile).map_err(AppError::from)?,
            ))
        });
    }

    fn load_space(&mut self, cx: &AppCtx<'_>) {
        self.space_epoch = Some(cx.dvr.epoch());
        let Some(dir) = cx.dvr.dir() else { return };
        self.space.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(crate::dvr::files::free_space(&dir)
                .or_else(|| dir.parent().and_then(crate::dvr::files::free_space)))
        });
    }

    fn act(&mut self, cx: &mut AppCtx<'_>, row: &Row, action: Action) {
        match (row, action) {
            (Row::Job(r), Action::Play) => dvr_ui::play_saved(cx, r.id),
            (Row::Job(r), Action::Pause) => cx.dvr.pause(r.id),
            (Row::Job(r), Action::Resume | Action::Retry) => cx.dvr.resume(r.id),
            (Row::Job(r), Action::Stop) => {
                cx.dvr.stop_recording(r.id);
                cx.toasts
                    .info("Recording stopped. What was recorded is kept.");
            }
            (Row::Job(r), Action::Cancel) => {
                cx.dvr.cancel(r.id);
                cx.toasts.info(if r.state == RecordingState::Scheduled {
                    "Scheduled recording cancelled."
                } else {
                    "Cancelled."
                });
            }
            (Row::Job(r), Action::Delete) => {
                self.confirm = Some((r.id, r.title.clone()));
            }
            (Row::Reminder(r), _) => {
                let id: ReminderId = r.id;
                cx.services.blocking("cancel a reminder", move |s| {
                    s.reminders().remove(id).map(|_| ())
                });
                if let Some(list) = self.reminders.value_mut() {
                    let mut v = (**list).clone();
                    v.retain(|x| x.id != id);
                    *list = Arc::new(v);
                }
                cx.toasts.info("Reminder cancelled.");
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn paint_row(
        &self,
        cx: &AppCtx<'_>,
        ui: &mut Ui,
        row: &Row,
        rect: Rect,
        first: bool,
    ) -> Option<Action> {
        let key = row.key();
        let actions: Vec<Action> = match row {
            Row::Job(r) => actions_for(r),
            Row::Reminder(_) => vec![Action::CancelReminder],
        };
        // Buttons (right), measured first.
        let buttons: Vec<(Action, Button, egui::Vec2)> = actions
            .iter()
            .map(|a| {
                let b = a.button().id(action_id(key, a.slug()));
                let s = b.size(ui);
                (*a, b, s)
            })
            .collect();
        let buttons_w: f32 = buttons.iter().map(|(_, _, s)| s.x + 8.0).sum::<f32>() + 8.0;
        let row_rect = Rect::from_min_max(rect.min, pos2(rect.right() - buttons_w, rect.bottom()));
        let (_, f) = focus::focusable(
            ui,
            row_id(key),
            row_rect,
            FocusOpts::default().autofocus_if(first),
        );
        let painter = ui.painter().clone();
        let any_focus = f.focused
            || actions
                .iter()
                .any(|a| focus::is_focused(ui.ctx(), action_id(key, a.slug())));
        painter.rect_filled(
            rect,
            CornerRadius::same(radius::MD),
            if any_focus {
                color::SURFACE_3
            } else {
                color::SURFACE_1
            },
        );
        // Art.
        let images = &cx.services.images;
        let (icon_url, name, is_live) = match row {
            Row::Job(r) => (
                r.icon_url.as_deref(),
                r.subtitle.as_deref().unwrap_or(&r.title),
                !r.kind.is_download(),
            ),
            Row::Reminder(r) => (r.icon_url.as_deref(), r.channel_name.as_str(), true),
        };
        let tx = if is_live {
            let art = Rect::from_min_size(
                pos2(rect.left() + 16.0, rect.center().y - 24.0),
                vec2(76.0, 48.0),
            );
            let slot = images.get(icon_url, ImageSize::Logo);
            ChannelLogo::paint(ui, &painter, art, name, &slot);
            art.right() + 20.0
        } else {
            let art = Rect::from_min_size(
                pos2(rect.left() + 16.0, rect.top() + 10.0),
                vec2(56.0, 84.0),
            );
            let slot = images.get(icon_url, ImageSize::Poster);
            match slot.texture() {
                Some(t) => paint::image_rounded(
                    &painter,
                    t.id(),
                    art,
                    paint::cover_uv(t.size_vec2(), art.size(), vec2(0.5, 0.5)),
                    radius::SM,
                    egui::Color32::WHITE,
                ),
                None => {
                    painter.rect_filled(art, CornerRadius::same(radius::SM), color::SURFACE_2);
                    icons::paint(
                        &painter,
                        art.center(),
                        icons::FILM_STRIP,
                        20.0,
                        Style::Regular,
                        color::TEXT_3,
                    );
                }
            }
            art.right() + 20.0
        };
        let tw = (row_rect.right() - tx - 16.0).max(80.0);
        let (title, sub, status) = match row {
            Row::Job(r) => {
                let p = cx.dvr.progress(r.id);
                let s = dvr_ui::status(cx, r, p.as_ref());
                let sub = match r.kind {
                    RecordingKind::Movie => "Film".to_owned(),
                    RecordingKind::Episode => {
                        let se = match (r.season, r.episode) {
                            (Some(s), Some(e)) => format!("S{s} · E{e}"),
                            _ => String::new(),
                        };
                        crate::services::vod::meta_line([
                            r.subtitle.clone().unwrap_or_default(),
                            se,
                        ])
                    }
                    RecordingKind::Live | RecordingKind::Clip => crate::services::vod::meta_line([
                        r.subtitle.clone().unwrap_or_default(),
                        if r.kind == RecordingKind::Clip {
                            "Clip".to_owned()
                        } else {
                            String::new()
                        },
                        dvr_ui::when_label(cx, r),
                    ]),
                };
                (r.title.clone(), sub, Some(s))
            }
            Row::Reminder(r) => {
                let when = DateTime::<Utc>::from_timestamp_millis(r.start_at.as_unix_millis())
                    .map(|t| {
                        let l = t.with_timezone(&Local);
                        format!("{} · {}", l.format("%a %-d %b"), cx.prefs.format_time(&l))
                    })
                    .unwrap_or_default();
                let what = match r.action {
                    opensmarters_store::ReminderAction::Switch => "Switches to it when it starts",
                    opensmarters_store::ReminderAction::Notify => "Reminds you when it starts",
                };
                (
                    r.title.clone(),
                    crate::services::vod::meta_line([r.channel_name.clone(), when]),
                    Some(dvr_ui::Status {
                        glyph: icons::BELL_SIMPLE,
                        color: color::INFO,
                        line: what.to_owned(),
                        fraction: None,
                    }),
                )
            }
        };
        text::line(
            &painter,
            pos2(tx, rect.top() + 24.0),
            Align2::LEFT_CENTER,
            &text::cap(&title, 160),
            font::body_strong(),
            color::TEXT,
            tw,
        );
        text::line(
            &painter,
            pos2(tx, rect.top() + 48.0),
            Align2::LEFT_CENTER,
            &text::cap(&sub, 200),
            font::caption(),
            color::TEXT_3,
            tw,
        );
        if let Some(s) = status {
            let y = rect.top() + 76.0;
            icons::paint(
                &painter,
                pos2(tx + 9.0, y),
                s.glyph,
                18.0,
                Style::Regular,
                s.color,
            );
            let bar_w = if s.fraction.is_some() { 140.0 } else { 0.0 };
            let lr = text::line(
                &painter,
                pos2(tx + 26.0, y),
                Align2::LEFT_CENTER,
                &s.line,
                font::caption(),
                color::TEXT_2,
                (tw - 26.0 - bar_w - 16.0).max(60.0),
            );
            if let Some(frac) = s.fraction {
                let bar = Rect::from_min_size(pos2(lr.right() + 16.0, y - 2.0), vec2(bar_w, 4.0));
                paint::progress_bar(&painter, bar, frac, color::ACCENT, color::PROGRESS_TRACK);
            }
        }
        if f.focused {
            paint::focus_ring(&painter, rect, radius::MD, 1.0);
        }
        // Buttons.
        let mut out = None;
        if f.activated
            && let Some(a) = actions.first()
        {
            out = Some(*a);
        }
        let mut x = rect.right() - 12.0;
        let cy = rect.center().y;
        for (a, b, s) in buttons.into_iter().rev() {
            let br = Rect::from_min_size(pos2(x - s.x, cy - s.y / 2.0), s);
            x -= s.x + 8.0;
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(br));
            if b.show(&mut child).activated {
                out = Some(a);
            }
        }
        out
    }
}

impl Screen for RecordingsScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.load_reminders(cx);
        self.load_space(cx);
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if let Some((id, _)) = self.confirm.take() {
            focus::request(&cx.egui, row_id((0, id.0)));
            return true;
        }
        false
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if self.confirm.is_some() || focus::text_input_focused(ctx) {
            return;
        }
        let Some(cur) = focus::current(ctx) else {
            return;
        };
        let del = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete));
        if !del {
            return;
        }
        let row = cx
            .dvr
            .rows()
            .iter()
            .find(|r| cur == row_id((0, r.id.0)))
            .cloned();
        if let Some(r) = row {
            if r.state == RecordingState::Done || r.state == RecordingState::Failed {
                self.confirm = Some((r.id, r.title));
            } else {
                cx.dvr.cancel(r.id);
            }
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Recordings)
    }

    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reminders.poll();
        self.space.poll();
        if self.space_epoch != Some(cx.dvr.epoch()) && !self.space.is_loading() {
            self.load_space(cx);
        }
        shell::status_cluster(cx, ui);
        shell::page_title(cx, ui, "Recordings");
        let content = cx.layout.content;
        let m = cx.layout.margin;
        let width = (content.width() - 2.0 * m).min(1240.0);
        let painter = ui.painter().clone();

        // Free space and folder, "Open folder".
        let dir = cx.dvr.dir();
        let open = Button::ghost("Open folder")
            .icon(icons::FOLDER_OPEN)
            .small()
            .id(Id::unique("rec.open-folder"));
        let os = open.size(ui);
        let right = content.left() + m + width;
        let open_rect = Rect::from_min_size(pos2(right - os.x, content.top() + 98.0), os);
        let space = match (self.space.value().copied().flatten(), &dir) {
            (Some(free), Some(d)) => format!("{} free · {}", human_bytes(free), short_path(d)),
            (None, Some(d)) => short_path(d),
            _ => "No recordings folder".to_owned(),
        };
        text::line(
            &painter,
            pos2(open_rect.left() - 12.0, open_rect.center().y),
            Align2::RIGHT_CENTER,
            &space,
            font::caption(),
            color::TEXT_3,
            (width * 0.45).max(120.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(open_rect));
        if focus::scope(&mut child, Id::unique("rec.header"), |ui| open.show(ui)).activated {
            match &dir {
                Some(d) => {
                    let _ = std::fs::create_dir_all(d);
                    if crate::platform::open_folder(d).is_err() {
                        cx.toasts.error("The recordings folder couldn't be opened.");
                    }
                }
                None => cx
                    .toasts
                    .info("Choose a recordings folder in Settings › Recordings & downloads."),
            }
        }

        // Tabs.
        let now = cx.now;
        let jobs = Arc::clone(cx.dvr.rows());
        let reminders = self.reminders.value().cloned().unwrap_or_default();
        let labels: Vec<String> = RecordingsTab::ALL
            .iter()
            .map(|t| {
                let n = tab_rows(*t, &jobs, &reminders, now).len();
                if n > 0 {
                    format!("{} {n}", t.label())
                } else {
                    t.label().to_owned()
                }
            })
            .collect();
        let tabs = Rect::from_min_size(
            pos2(content.left() + m - 4.0, content.top() + 92.0),
            vec2(width * 0.5, 48.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(tabs));
        let mut idx = RecordingsTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        focus::scope(&mut child, Id::unique("rec.tabs"), |ui| {
            Tabs::new(Id::unique("rec.tab"), &labels).show(ui, &mut idx)
        });
        if let Some(t) = RecordingsTab::ALL.get(idx)
            && *t != self.tab
        {
            self.tab = *t;
            self.confirm = None;
        }

        let top = tabs.bottom() + 20.0;
        let rows_rect = Rect::from_min_max(
            pos2(content.left() + m - 8.0, top),
            pos2(content.left() + m + width + 8.0, content.bottom() - 44.0),
        );
        let rows = tab_rows(self.tab, &jobs, &reminders, now);
        if !cx.dvr.is_loaded() {
            if cx.dvr.engine().is_some() {
                let mut y = rows_rect.top() + 8.0;
                while y + ROW_H < rows_rect.bottom() {
                    skeleton(
                        &cx.egui,
                        &painter,
                        Rect::from_min_size(pos2(rows_rect.left() + 8.0, y), vec2(width, ROW_H)),
                        radius::MD,
                    );
                    y += ROW_H + 8.0;
                }
            }
            return;
        }
        if rows.is_empty() {
            let mut c = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(content.left(), top), content.max)),
            );
            let (ill, headline, body, action, route) = match self.tab {
                RecordingsTab::Downloads => (
                    Illustration::EmptyFavourites,
                    "No downloads yet",
                    "Press Download on a film, or on an episode, to watch it later without \
                     streaming.",
                    "Browse films",
                    Route::Movies { category: None },
                ),
                RecordingsTab::Recordings => (
                    Illustration::EmptyGuide,
                    "No recordings yet",
                    "Press Record in the player, or Record on a programme in the Guide. Save \
                     clip keeps the last minutes of a live channel.",
                    "Open the Guide",
                    Route::Guide,
                ),
                RecordingsTab::Scheduled => (
                    Illustration::EmptyGuide,
                    "Nothing scheduled",
                    "In the Guide, pick a programme that hasn't started and choose Record or \
                     Remind me.",
                    "Open the Guide",
                    Route::Guide,
                ),
            };
            let f = EmptyState::new(
                Id::unique(("rec.empty", self.tab.slug())),
                ill,
                headline,
                body,
            )
            .action(action, true)
            .show(&mut c);
            if f.is_some_and(|f| f.activated) {
                cx.nav.section(route);
            }
            return;
        }

        if let Some(id) = self.focus_after.take() {
            focus::request(&cx.egui, id);
        }
        let mut acted: Option<(Row, Action)> = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
        let n = rows.len();
        VirtualList::new(Id::unique(("rec.list", self.tab.slug())), ROW_H, n)
            .gap(8.0)
            .show(
                &mut child,
                |i| rows.get(i).map_or(Id::NULL, |r| row_id(r.key())),
                |ui, i, rect| {
                    let Some(row) = rows.get(i) else { return };
                    if let Some(a) = self.paint_row(cx, ui, row, rect, i == 0) {
                        acted = Some((row.clone(), a));
                    }
                },
            );
        if let Some((row, a)) = acted {
            self.act(cx, &row, a);
        }
        // Footer hint.
        let hint = if cx.layout.tv {
            "Enter plays a saved file · the buttons on each row pause, stop or delete it"
        } else {
            "Enter plays a saved file · Delete removes it"
        };
        painter.text(
            pos2(right, content.bottom() - 22.0),
            Align2::RIGHT_CENTER,
            hint,
            font::caption(),
            color::TEXT_3,
        );

        // Delete confirmation.
        if let Some((id, title)) = self.confirm.clone() {
            let mut yes = false;
            let mut no = false;
            let heading = format!("Delete \u{201c}{}\u{201d}?", text::cap(&title, 60));
            Dialog::new(Id::unique("rec.delete"), &heading)
                .body("The file is removed from this computer. This can't be undone.")
                .show(ui.ctx(), |ui| {
                    yes = Button::danger("Delete")
                        .icon(icons::TRASH)
                        .id(Id::unique("rec.delete.yes"))
                        .show(ui)
                        .activated;
                    no = Button::secondary("Keep it")
                        .autofocus()
                        .id(Id::unique("rec.delete.no"))
                        .show(ui)
                        .activated;
                });
            if yes {
                cx.dvr.delete(id);
                self.confirm = None;
                cx.toasts.info("Deleted.");
                // Keep the focus on the list: the next row, else the previous one.
                let pos = rows.iter().position(|r| r.key() == (0, id.0)).unwrap_or(0);
                self.focus_after = rows
                    .get(pos + 1)
                    .or_else(|| pos.checked_sub(1).and_then(|p| rows.get(p)))
                    .map(|r| row_id(r.key()));
            } else if no {
                self.confirm = None;
                focus::request(&cx.egui, row_id((0, id.0)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use opensmarters_store::{ProfileId, Timestamp};

    use super::*;

    fn job(id: i64, kind: RecordingKind, state: RecordingState) -> Recording {
        Recording {
            id: RecordingId(id),
            profile: ProfileId::new_random(),
            kind,
            state,
            item_id: "1".into(),
            series_id: None,
            season: None,
            episode: None,
            container_extension: None,
            title: "T".into(),
            subtitle: None,
            icon_url: None,
            start_at: Some(Timestamp::from_unix_secs(100 + id)),
            end_at: Some(Timestamp::from_unix_secs(200 + id)),
            file_name: None,
            bytes_done: 0,
            bytes_total: None,
            duration_secs: None,
            position_secs: None,
            error: None,
            created_at: Timestamp::EPOCH,
            updated_at: Timestamp::EPOCH,
        }
    }

    #[test]
    fn tabs_split_the_jobs() {
        let jobs = vec![
            job(1, RecordingKind::Movie, RecordingState::Active),
            job(2, RecordingKind::Episode, RecordingState::Done),
            job(3, RecordingKind::Live, RecordingState::Scheduled),
            job(4, RecordingKind::Live, RecordingState::Done),
            job(5, RecordingKind::Clip, RecordingState::Done),
            job(6, RecordingKind::Movie, RecordingState::Cancelled),
        ];
        let now = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default();
        let keys = |t| {
            tab_rows(t, &jobs, &[], now)
                .iter()
                .map(|r| r.key().1)
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(RecordingsTab::Downloads), [1, 2]);
        assert_eq!(
            keys(RecordingsTab::Recordings),
            [5, 4],
            "newest finished first"
        );
        assert_eq!(keys(RecordingsTab::Scheduled), [3]);
    }

    #[test]
    fn queues_read_in_order() {
        // Newest first from the store: 4 (queued), 3 (done), 2 (queued), 1 (active).
        let jobs = vec![
            job(4, RecordingKind::Movie, RecordingState::Queued),
            job(3, RecordingKind::Movie, RecordingState::Done),
            job(2, RecordingKind::Movie, RecordingState::Queued),
            job(1, RecordingKind::Movie, RecordingState::Active),
        ];
        let ids: Vec<i64> = queue_order(jobs).iter().map(|r| r.id.0).collect();
        assert_eq!(ids, [1, 2, 4, 3]);
    }

    #[test]
    fn actions_match_the_state() {
        use RecordingKind as K;
        use RecordingState as S;
        assert_eq!(
            actions_for(&job(1, K::Movie, S::Done)),
            [Action::Play, Action::Delete]
        );
        assert_eq!(
            actions_for(&job(1, K::Movie, S::Active)),
            [Action::Pause, Action::Cancel]
        );
        assert_eq!(
            actions_for(&job(1, K::Episode, S::Paused)),
            [Action::Resume, Action::Cancel]
        );
        assert_eq!(
            actions_for(&job(1, K::Movie, S::Failed)),
            [Action::Retry, Action::Delete]
        );
        assert_eq!(actions_for(&job(1, K::Live, S::Active)), [Action::Stop]);
        assert_eq!(
            actions_for(&job(1, K::Live, S::Scheduled)),
            [Action::Cancel]
        );
        assert_eq!(
            actions_for(&job(1, K::Clip, S::Done)),
            [Action::Play, Action::Delete]
        );
    }
}

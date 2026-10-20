// SPDX-License-Identifier: GPL-3.0-or-later
//! Programme reminders and the Guide's programme actions.
//!
//! - **Setting them:** [`programme_menu`] (Guide and Search: Enter on a programme that is
//!   on now or coming up) offers "Remind me", "Switch to it when it starts", "Record"
//!   (a [`crate::dvr::schedule_programme`] store insert; the recording engine starts it)
//!   and, when set, "Cancel reminder" / "Cancel recording". On the programme on now:
//!   "Watch" and "Record until it ends". Every write runs off the UI thread; the markers
//!   update at once (optimistically) and again when the write lands.
//! - **Markers:** [`Scheduler::mark`] answers "is there a reminder / a recording for this
//!   programme?" from one cached list per profile (reloaded every [`RELOAD_EVERY`] and after
//!   our own writes); [`paint_marks`] draws a bell and a red dot.
//! - **Firing** ([`on_frame`], run by [`crate::services::alerts::on_frame`]): the pending
//!   reminders of the open profile fire [`REMINDER_LEAD_SECS`] before the start ("Starting
//!   in 1 min: … on …", Watch / Dismiss). A "switch" reminder counts down
//!   [`SWITCH_COUNTDOWN_SECS`] ("Switching to … in 10 s", Switch now / Cancel) so the switch
//!   lands on the start, and then opens the channel. A fired reminder is marked in the
//!   store. When the window isn't focused the taskbar button flashes; the focus is never
//!   stolen from another app.
//! - **Profiles:** only the open profile's reminders fire. When a profile opens, a reminder
//!   that should have fired while it was closed and whose programme is still on shows once
//!   ("Started 5 min ago"); reminders whose programme ended are pruned.
//!
//! The decision itself is the pure [`due`] (time injected, unit-tested).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, Id, Painter, Pos2, Rect, Sense, Ui, pos2, vec2};
use opensmarters_core::chrono::{DateTime, Local, Utc};
use opensmarters_store::settings::Setting;
use opensmarters_store::{
    NewReminder, ProfileId, Recording, RecordingKind, RecordingState, Reminder, ReminderAction,
    ReminderId, Store, StoreError, Timestamp,
};

use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{PlayerRoute, Route};
use crate::screens::AppCtx;
use crate::services::alerts::{Countdown, Prompt};
use crate::services::catalog::{LiveChannel, Programme, split_tag};
use crate::task::Loader;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::{Button, Dialog};
use crate::ui::{paint, text};

/// Seconds before the start a reminder appears (0, 60 or 300). Default 1 min.
pub const REMINDER_LEAD_SECS: Setting<u32> = Setting::new("app.reminder_lead_secs", || 60);

/// The lead times offered in Settings.
pub const LEAD_CHOICES: [u32; 3] = [0, 60, 300];

/// The reminder list is reloaded this often (and after every change made here).
pub const RELOAD_EVERY: Duration = Duration::from_secs(30);

/// A "switch" reminder counts down this long; it appears this long before the start so
/// the channel changes on time.
pub const SWITCH_COUNTDOWN_SECS: i64 = 10;

/// A reminder found more than this late (the profile or the app was closed) is "missed".
pub const MISSED_AFTER_SECS: i64 = 90;

/// A "Starting …" prompt goes away this long after the programme started.
const NOTIFY_FOR_SECS: i64 = 300;

/// A "Started … ago" prompt goes away after this long.
const MISSED_FOR_SECS: i64 = 300;

const KEY_PREFIX: &str = "reminder:";

/// What to do with a pending reminder now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Not yet.
    Wait,
    /// "Starting in 1 min" (or "Starting now").
    Notify,
    /// Count down, then switch to the channel.
    Switch,
    /// Its time passed while it couldn't fire and the programme is still on: "Started 5 min
    /// ago" (shown once, never switches on its own).
    Missed,
    /// The programme is over.
    Over,
}

/// Decides what a reminder does at `now` (all times in Unix seconds; `lead` from
/// [`REMINDER_LEAD_SECS`]).
pub fn due(action: ReminderAction, start: i64, end: i64, now: i64, lead: i64) -> Due {
    if now >= end {
        return Due::Over;
    }
    let fire_at = match action {
        ReminderAction::Notify => start - lead.max(0),
        ReminderAction::Switch => start - SWITCH_COUNTDOWN_SECS,
    };
    if now < fire_at {
        return Due::Wait;
    }
    let on_time = now - fire_at <= MISSED_AFTER_SECS;
    match action {
        // Late but before the start (a 5 min lead and the profile opened 3 min before):
        // still "Starting in 2 min".
        ReminderAction::Notify if now < start || on_time => Due::Notify,
        ReminderAction::Switch if on_time => Due::Switch,
        _ => Due::Missed,
    }
}

/// "Starting in 1 min" / "Starting now".
pub fn starting_in(start: i64, now: i64) -> String {
    let secs = start - now;
    if secs > 30 {
        format!("Starting in {} min", (secs + 59) / 60)
    } else {
        "Starting now".to_owned()
    }
}

/// "Started 5 min ago" / "Started just now" / "Started 1 h 05 ago".
pub fn started_ago(start: i64, now: i64) -> String {
    let mins = (now - start).max(0) / 60;
    match mins {
        0 => "Started just now".to_owned(),
        1..=59 => format!("Started {mins} min ago"),
        _ => format!("Started {} h {:02} ago", mins / 60, mins % 60),
    }
}

/// A programme's markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mark {
    /// A reminder is set.
    pub reminder: Option<ReminderAction>,
    /// A recording is scheduled, waiting, running or paused.
    pub recording: Option<RecordingState>,
}

impl Mark {
    fn is_empty(self) -> bool {
        self.reminder.is_none() && self.recording.is_none()
    }
}

/// What one load brings back.
#[derive(Debug)]
struct Snapshot {
    profile: ProfileId,
    /// The write counter when the load started (older than a later write = stale).
    writes: u64,
    reminders: Vec<Reminder>,
    recordings: Vec<Recording>,
    lead: u32,
}

/// A prompt on screen for a reminder.
#[derive(Debug, Clone)]
struct Shown {
    key: String,
    reminder: Reminder,
    due: Due,
    /// When a "switch" countdown ends.
    until: Option<Instant>,
}

/// The reminder scheduler and the markers cache (`cx.alerts.reminders`).
#[derive(Debug, Default)]
pub struct Scheduler {
    profile: Option<ProfileId>,
    load: Loader<Snapshot>,
    started_at: Option<Instant>,
    reload: bool,
    /// Not fired yet, programme not over.
    pending: Vec<Reminder>,
    marks: HashMap<(String, i64), Mark>,
    /// Fired this session (guards against a reload racing the store update).
    fired: HashSet<ReminderId>,
    lead: Option<u32>,
    /// Completed writes / writes still running.
    writes: Arc<AtomicU64>,
    inflight: Arc<AtomicU64>,
    seen_writes: u64,
    shown: Vec<Shown>,
    /// Where the focus goes back to next frame after the programme menu closed (the
    /// menu's focus trap still holds it during the frame it closes).
    refocus: Option<Id>,
    /// The programme menu drawn at this pass, and where the focus was when it opened
    /// (restored when the screen closes it, e.g. with Esc).
    menu: Option<(u64, Option<Id>)>,
    #[cfg(feature = "demo")]
    demo_seeded: bool,
}

impl Scheduler {
    /// The markers of a channel's programme (`start` in Unix seconds).
    pub fn mark(&self, stream_id: &str, start: i64) -> Option<Mark> {
        self.marks.get(&(stream_id.to_owned(), start)).copied()
    }

    /// Reminders of the open profile that haven't fired yet.
    pub fn pending(&self) -> &[Reminder] {
        &self.pending
    }

    /// [`REMINDER_LEAD_SECS`] (the default until it is read).
    pub fn lead_secs(&self) -> u32 {
        self.lead.unwrap_or(60)
    }

    /// Changes [`REMINDER_LEAD_SECS`] and saves it.
    pub fn set_lead_secs(&mut self, services: &crate::services::Services, secs: u32) {
        self.lead = Some(secs);
        services.blocking("save the reminder setting", move |s| {
            s.settings().set(&REMINDER_LEAD_SECS, &secs)
        });
    }

    fn set_mark(&mut self, stream_id: &str, start: i64, f: impl FnOnce(&mut Mark)) {
        let key = (stream_id.to_owned(), start);
        let mut m = self.marks.get(&key).copied().unwrap_or_default();
        f(&mut m);
        if m.is_empty() {
            self.marks.remove(&key);
        } else {
            self.marks.insert(key, m);
        }
    }

    fn apply(&mut self, snap: Snapshot) {
        self.lead = Some(snap.lead);
        let fresh = snap.writes == self.writes.load(Ordering::Acquire)
            && self.inflight.load(Ordering::Acquire) == 0;
        if fresh {
            let mut marks: HashMap<(String, i64), Mark> = HashMap::new();
            for r in &snap.reminders {
                marks
                    .entry((r.stream_id.clone(), r.start_at.as_unix_secs()))
                    .or_default()
                    .reminder = Some(r.action);
            }
            for r in &snap.recordings {
                if let Some(start) = r.start_at {
                    marks
                        .entry((r.item_id.clone(), start.as_unix_secs()))
                        .or_default()
                        .recording = Some(r.state);
                }
            }
            self.marks = marks;
        }
        self.pending = snap
            .reminders
            .into_iter()
            .filter(|r| r.fired_at.is_none() && !self.fired.contains(&r.id))
            .collect();
    }
}

fn load_snapshot(store: &Store, profile: ProfileId, writes: u64) -> Result<Snapshot, AppError> {
    let now = Timestamp::now();
    // Reminders whose programme ended are of no use any more.
    store.reminders().prune(profile, now)?;
    let reminders = store.reminders().list(profile)?;
    let recordings = store
        .recordings()
        .with_states(
            profile,
            &[
                RecordingState::Scheduled,
                RecordingState::Queued,
                RecordingState::Active,
                RecordingState::Paused,
            ],
        )?
        .into_iter()
        .filter(|r| r.kind == RecordingKind::Live)
        .collect();
    let lead = store.settings().get(&REMINDER_LEAD_SECS)?;
    Ok(Snapshot {
        profile,
        writes,
        reminders,
        recordings,
        lead: if LEAD_CHOICES.contains(&lead) {
            lead
        } else {
            60
        },
    })
}

/// Runs a store write off the UI thread, then reloads the list. A failure shows the usual
/// "Couldn't …" toast.
fn write(
    cx: &mut AppCtx<'_>,
    what: &'static str,
    f: impl FnOnce(&Store) -> Result<(), StoreError> + Send + 'static,
) {
    let services = cx.services.clone();
    let writes = Arc::clone(&cx.alerts.reminders.writes);
    let inflight = Arc::clone(&cx.alerts.reminders.inflight);
    inflight.fetch_add(1, Ordering::AcqRel);
    cx.rt().spawn_blocking(move || {
        if let Err(e) = f(&services.store) {
            tracing::warn!(error = %e, "{what} failed");
            services.report_background_error(what);
        }
        writes.fetch_add(1, Ordering::AcqRel);
        inflight.fetch_sub(1, Ordering::AcqRel);
        services.egui().request_repaint();
    });
}

/// Opens a live channel in the player (replacing the player if it is on top).
pub fn watch_channel(cx: &mut AppCtx<'_>, key: &str) {
    let route = Route::Player(PlayerRoute::LiveChannel {
        key: key.to_owned(),
    });
    if cx.sleep.player_on_top(&cx.egui) {
        if cx.playback.live_key() == Some(key) {
            return;
        }
        cx.nav.replace(route);
    } else {
        cx.nav.push(route);
    }
}

/// Flashes the taskbar button when the window isn't focused (never takes the focus).
fn request_attention(ctx: &egui::Context) {
    if ctx.input(|i| i.viewport().focused) == Some(false) {
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
    }
}

fn what_on(r: &Reminder) -> String {
    if r.title.trim().is_empty() {
        r.channel_name.clone()
    } else {
        format!("{} on {}", r.title, r.channel_name)
    }
}

fn prompt_for(s: &Shown, now: i64) -> Prompt {
    let r = &s.reminder;
    let start = r.start_at.as_unix_secs();
    let end = r.end_at.as_unix_secs();
    let now_i = Instant::now();
    let after = |secs: i64| now_i + Duration::from_secs(u64::try_from(secs.max(1)).unwrap_or(1));
    match s.due {
        Due::Switch => {
            let until = s.until.unwrap_or(now_i);
            let left = until.saturating_duration_since(now_i).as_secs()
                + u64::from(
                    until > now_i && until.saturating_duration_since(now_i).subsec_nanos() > 0,
                );
            Prompt::new(
                s.key.clone(),
                icons::TELEVISION_SIMPLE,
                format!("Switching to {} in {left} s", r.channel_name),
            )
            .body(r.title.clone())
            .action("cancel", "Cancel", false)
            .action("switch", "Switch now", true)
            .cancel("cancel")
            .countdown(Countdown {
                until,
                total: Duration::from_secs(SWITCH_COUNTDOWN_SECS as u64),
                label: "",
            })
            .expires(until, "switch")
            .modal()
            .priority(6)
        }
        Due::Missed => Prompt::new(s.key.clone(), icons::BELL_RINGING, started_ago(start, now))
            .body(what_on(r))
            .action("dismiss", "Dismiss", false)
            .action("watch", "Watch", true)
            .expires(after(MISSED_FOR_SECS.min(end - now)), "dismiss")
            .modal()
            .priority(5),
        _ => Prompt::new(s.key.clone(), icons::BELL_RINGING, starting_in(start, now))
            .body(what_on(r))
            .action("dismiss", "Dismiss", false)
            .action("watch", "Watch", true)
            .expires(after((start + NOTIFY_FOR_SECS).min(end) - now), "dismiss")
            .modal()
            .priority(5),
    }
}

/// Called once per frame by [`crate::services::alerts::on_frame`].
pub(crate) fn on_frame(cx: &mut AppCtx<'_>, now: Instant) {
    if let Some(id) = cx.alerts.reminders.refocus.take() {
        focus::request(&cx.egui, id);
    }
    let pass = cx.egui.cumulative_pass_nr();
    if let Some((drawn, back)) = cx.alerts.reminders.menu
        && drawn + 1 < pass
    {
        // The screen closed the menu (Esc): back to the programme it was opened on.
        cx.alerts.reminders.menu = None;
        if let Some(id) = back {
            focus::request(&cx.egui, id);
        }
    }
    let session = cx.session();
    let profile = session.as_ref().map(|s| s.profile_id());
    if cx.alerts.reminders.profile != profile {
        // Another profile (or none): its reminders don't fire here.
        let s = &mut cx.alerts.reminders;
        *s = Scheduler {
            profile,
            reload: true,
            lead: s.lead,
            writes: Arc::clone(&s.writes),
            inflight: Arc::clone(&s.inflight),
            seen_writes: s.writes.load(Ordering::Acquire),
            ..Scheduler::default()
        };
        cx.alerts.dismiss_where(|k| k.starts_with(KEY_PREFIX));
    }
    let Some(profile) = profile else {
        return;
    };
    #[cfg(feature = "demo")]
    if let Some(session) = &session {
        demo_seed(cx, session);
    }

    // (Re)load the list.
    let ctx = cx.egui.clone();
    let s = &mut cx.alerts.reminders;
    let writes = s.writes.load(Ordering::Acquire);
    if writes != s.seen_writes {
        s.seen_writes = writes;
        s.reload = true;
    }
    if s.load.poll() {
        if let Some(snap) = s.load.take()
            && snap.profile == profile
        {
            s.apply(snap);
        }
        if let Some(e) = s.load.error() {
            tracing::warn!(error = %e, "couldn't load the reminders");
        }
    }
    let stale = s.started_at.is_none_or(|t| t.elapsed() >= RELOAD_EVERY);
    if (s.reload || stale) && !s.load.is_loading() {
        s.reload = false;
        s.started_at = Some(now);
        let store = cx.services.store.clone();
        let counter = Arc::clone(&s.writes);
        s.load.start_blocking(cx.services.rt(), &ctx, move || {
            let writes = counter.load(Ordering::Acquire);
            load_snapshot(&store, profile, writes)
        });
    }

    // Answers to the prompts on screen.
    let shown = std::mem::take(&mut cx.alerts.reminders.shown);
    let mut keep = Vec::with_capacity(shown.len());
    for sh in shown {
        match cx.alerts.take_answer(&sh.key) {
            Some("watch" | "switch") => watch_channel(cx, &sh.reminder.stream_id),
            Some(_) => {}
            None if cx.alerts.contains(&sh.key) => keep.push(sh),
            None => {}
        }
    }

    // Fire what is due.
    let now_s = cx.now.timestamp();
    let lead = i64::from(cx.alerts.reminders.lead_secs());
    let watching = cx
        .sleep
        .player_on_top(&ctx)
        .then(|| cx.playback.live_key().map(str::to_owned))
        .flatten();
    let s = &mut cx.alerts.reminders;
    let mut fire = Vec::new();
    for r in &s.pending {
        if s.fired.contains(&r.id) {
            continue;
        }
        let d = due(
            r.action,
            r.start_at.as_unix_secs(),
            r.end_at.as_unix_secs(),
            now_s,
            lead,
        );
        if d != Due::Wait {
            fire.push((r.clone(), d));
        }
    }
    for (r, d) in fire {
        cx.alerts.reminders.fired.insert(r.id);
        let id = r.id;
        cx.services.blocking("update a reminder", move |st| {
            st.reminders().mark_fired(id).map(|_| ())
        });
        // Already watching that channel in the player: nothing to remind.
        if d == Due::Over || watching.as_deref() == Some(r.stream_id.as_str()) {
            continue;
        }
        let start = r.start_at.as_unix_secs();
        let until = (d == Due::Switch).then(|| {
            let secs = (start - now_s).max(SWITCH_COUNTDOWN_SECS);
            now + Duration::from_secs(u64::try_from(secs).unwrap_or(10))
        });
        let sh = Shown {
            key: format!("{KEY_PREFIX}{}", r.id.0),
            reminder: r,
            due: d,
            until,
        };
        tracing::info!(due = ?d, "reminder fired");
        cx.alerts.push(prompt_for(&sh, now_s));
        request_attention(&ctx);
        keep.push(sh);
    }
    let s = &mut cx.alerts.reminders;
    s.pending.retain(|r| !s.fired.contains(&r.id));

    // Live titles ("Starting in 1 min" → "Starting now", "Switching … in 7 s").
    for sh in &keep {
        if matches!(sh.due, Due::Notify | Due::Switch) {
            let title = prompt_for(sh, now_s).title;
            cx.alerts.update(&sh.key, |p| p.title = title);
        }
    }
    cx.alerts.reminders.shown = keep;

    // Wake up for the next reminder (and the periodic reload).
    let next = cx
        .alerts
        .reminders
        .pending
        .iter()
        .map(|r| {
            let start = r.start_at.as_unix_secs();
            match r.action {
                ReminderAction::Notify => start - lead,
                ReminderAction::Switch => start - SWITCH_COUNTDOWN_SECS,
            }
        })
        .min()
        .map_or(RELOAD_EVERY, |at| {
            Duration::from_secs(u64::try_from((at - now_s).max(1)).unwrap_or(1))
        });
    ctx.request_repaint_after(next.min(RELOAD_EVERY));
}

/// Dev only (`demo` feature): `OPENSMARTERS_DEMO_REMINDERS=notify:70,switch:20,missed:300`
/// sets synthetic reminders on the first demo channels, starting that many seconds from
/// now (`missed:` = started that long ago), so harness runs can see them fire.
#[cfg(feature = "demo")]
fn demo_seed(cx: &mut AppCtx<'_>, session: &Arc<crate::services::Session>) {
    if std::mem::replace(&mut cx.alerts.reminders.demo_seeded, true) {
        return;
    }
    let Ok(spec) = std::env::var("OPENSMARTERS_DEMO_REMINDERS") else {
        return;
    };
    let session = Arc::clone(session);
    let store = cx.services.store.clone();
    let writes = Arc::clone(&cx.alerts.reminders.writes);
    let ctx = cx.egui.clone();
    cx.services.spawn(async move {
        let Ok(cat) = session.live_catalog().await else {
            return;
        };
        let profile = session.profile_id();
        let now = Utc::now().timestamp();
        let mut items = Vec::new();
        for (i, tok) in spec.split(',').enumerate() {
            let Some((kind, secs)) = tok.trim().split_once(':') else {
                continue;
            };
            let Ok(secs) = secs.trim().parse::<i64>() else {
                continue;
            };
            let Some(ch) = cat.channels.get(i) else {
                continue;
            };
            let (action, start) = match kind {
                "switch" => (ReminderAction::Switch, now + secs),
                "missed" => (ReminderAction::Notify, now - secs),
                _ => (ReminderAction::Notify, now + secs),
            };
            items.push(NewReminder {
                stream_id: ch.key.clone(),
                start_at: Timestamp::from_unix_secs(start),
                end_at: Timestamp::from_unix_secs(start.max(now) + 1800),
                title: "Synthetic Evening Show".to_owned(),
                channel_name: split_tag(&ch.name).1.to_owned(),
                icon_url: None,
                action,
            });
        }
        let _ = tokio::task::spawn_blocking(move || {
            for r in &items {
                if let Err(e) = store.reminders().set(profile, r) {
                    tracing::warn!(error = %e, "demo reminder");
                }
            }
            writes.fetch_add(1, Ordering::AcqRel);
        })
        .await;
        ctx.request_repaint();
    });
}

/// Paints a programme's markers right-aligned on `right_center` (a red dot for a recording,
/// a bell for a reminder). Returns the width used.
pub fn paint_marks(painter: &Painter, right_center: Pos2, mark: Mark) -> f32 {
    let y = right_center.y;
    let mut x = right_center.x;
    if mark.recording.is_some() {
        painter.circle_filled(pos2(x - 5.0, y), 5.0, color::LIVE);
        x -= 10.0 + 8.0;
    }
    if mark.reminder.is_some() {
        icons::paint(
            painter,
            pos2(x - 8.0, y),
            icons::BELL,
            16.0,
            Style::Fill,
            color::TEXT_2,
        );
        x -= 16.0 + 8.0;
    }
    right_center.x - x
}

/// A Guide cell's markers, top right of its visible part (`visible`), on a patch of the
/// cell's own fill so a long title ends under them. Left of the catch-up icon when the
/// cell has one. One call from the Guide's grid.
pub fn paint_cell_marks(
    painter: &Painter,
    scheduler: &Scheduler,
    stream_id: &str,
    p: &Programme,
    visible: Rect,
    focused: bool,
    now: DateTime<Utc>,
) {
    let Some(mark) = scheduler.mark(stream_id, p.start.timestamp()) else {
        return;
    };
    if visible.width() < 72.0 {
        return;
    }
    let live = p.start <= now && now < p.stop;
    let fill = if focused || live {
        color::SURFACE_3
    } else if p.stop <= now {
        color::SURFACE_1
    } else {
        color::SURFACE_2
    };
    let catchup = p.has_archive && p.start <= now;
    let right = visible.right() - if catchup { 34.0 } else { 10.0 };
    let y = visible.top() + 16.0;
    let w = marks_width(mark);
    painter.rect_filled(
        Rect::from_min_max(pos2(right - w - 6.0, y - 11.0), pos2(right + 2.0, y + 11.0)),
        CornerRadius::same(radius::XS),
        fill,
    );
    paint_marks(painter, pos2(right, y), mark);
}

/// A Search programme row's markers, in the row's right column left of the LIVE badge /
/// "Catch-up" label. One call from Search.
pub fn paint_row_marks(
    painter: &Painter,
    scheduler: &Scheduler,
    stream_id: Option<&str>,
    p: &Programme,
    row: Rect,
) {
    if let Some(mark) = stream_id.and_then(|k| scheduler.mark(k, p.start.timestamp())) {
        paint_marks(painter, pos2(row.right() - 130.0, row.center().y), mark);
    }
}

fn marks_width(mark: Mark) -> f32 {
    let mut w = 0.0;
    if mark.recording.is_some() {
        w += 18.0;
    }
    if mark.reminder.is_some() {
        w += 24.0;
    }
    w
}

/// The programme menu's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Watch,
    Remind(ReminderAction),
    CancelReminder,
    Record,
    CancelRecording,
    StopRecording,
}

/// What the user did in [`programme_menu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuOutcome {
    /// Close the menu (an action ran, or the user closed it).
    pub close: bool,
    /// Watch the channel now (the caller plays it with its own zap list).
    pub watch: bool,
}

const ROW_H: f32 = 52.0;

/// The actions of a programme that is on now or coming up, as a dialog (Esc belongs to
/// the caller's `on_back`, like every dialog). Returns what the caller must do.
pub fn programme_menu(
    cx: &mut AppCtx<'_>,
    id: Id,
    channel: &LiveChannel,
    p: &Programme,
) -> MenuOutcome {
    let now = cx.now;
    let live = p.start <= now && now < p.stop;
    let start = p.start.timestamp();
    let mark = cx
        .alerts
        .reminders
        .mark(&channel.key, start)
        .unwrap_or_default();
    let channel_name = split_tag(&channel.name).1.to_owned();
    let lead = cx.alerts.reminders.lead_secs();

    // (action, icon, label, detail, selected)
    let mut rows: Vec<(Act, &'static str, String, String, bool)> = Vec::new();
    let record_row = |live: bool| match mark.recording {
        None => (
            Act::Record,
            icons::RECORD,
            if live {
                "Record until it ends"
            } else {
                "Record"
            }
            .to_owned(),
            String::new(),
            false,
        ),
        Some(RecordingState::Active) => (
            Act::StopRecording,
            icons::STOP,
            "Stop recording".to_owned(),
            "Recording now".to_owned(),
            true,
        ),
        Some(_) => (
            Act::CancelRecording,
            icons::RECORD,
            "Cancel recording".to_owned(),
            "Scheduled".to_owned(),
            false,
        ),
    };
    if live {
        rows.push((
            Act::Watch,
            icons::PLAY,
            "Watch".to_owned(),
            String::new(),
            false,
        ));
        rows.push(record_row(true));
    } else {
        let when = match lead {
            0 => "When it starts".to_owned(),
            s => format!("{} min before", s / 60),
        };
        rows.push((
            Act::Remind(ReminderAction::Notify),
            icons::BELL,
            "Remind me".to_owned(),
            when,
            mark.reminder == Some(ReminderAction::Notify),
        ));
        rows.push((
            Act::Remind(ReminderAction::Switch),
            icons::TELEVISION_SIMPLE,
            "Switch to it when it starts".to_owned(),
            String::new(),
            mark.reminder == Some(ReminderAction::Switch),
        ));
        if mark.reminder.is_some() {
            rows.push((
                Act::CancelReminder,
                icons::BELL_SLASH,
                "Cancel reminder".to_owned(),
                String::new(),
                false,
            ));
        }
        rows.push(record_row(false));
        rows.push((
            Act::Watch,
            icons::PLAY,
            "Watch the channel now".to_owned(),
            String::new(),
            false,
        ));
    }

    let when = format!(
        "{} · {}–{} · {}",
        crate::screens::guide::relative_day(p.start, now),
        cx.prefs.format_time(&p.start.with_timezone(&Local)),
        cx.prefs.format_time(&p.stop.with_timezone(&Local)),
        channel_name,
    );
    let ctx = cx.egui.clone();
    // Remember what had the focus when the menu opened (the programme), to go back to it.
    let pass = ctx.cumulative_pass_nr();
    let rid = id.with("return");
    let back_to = ctx
        .data(|d| d.get_temp::<(u64, Option<Id>)>(rid))
        .filter(|(seen, _)| seen + 2 >= pass)
        .map_or_else(|| focus::current(&ctx), |(_, r)| r);
    ctx.data_mut(|d| d.insert_temp(rid, (pass, back_to)));
    cx.alerts.reminders.menu = Some((pass, back_to));
    let mut chosen: Option<Act> = None;
    let mut closed = false;
    Dialog::new(id, "").width(540.0).show_custom(&ctx, |ui| {
        let w = ui.available_width();
        text::overline(
            ui.painter(),
            ui.cursor().min + vec2(0.0, 8.0),
            Align2::LEFT_CENTER,
            if live { "On now" } else { "Coming up" },
            color::TEXT_3,
        );
        ui.add_space(22.0);
        let g = text::galley(ui.painter(), &p.title, font::title(), color::TEXT, w, 2);
        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(r.min, g, color::TEXT);
        ui.add_space(4.0);
        let g = text::galley(ui.painter(), &when, font::caption(), color::TEXT_3, w, 1);
        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(r.min, g, color::TEXT_3);
        if !p.description.trim().is_empty() {
            ui.add_space(12.0);
            let g = text::galley(
                ui.painter(),
                &p.description,
                font::body(),
                color::TEXT_2,
                w,
                3,
            );
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, color::TEXT_2);
        }
        ui.add_space(16.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        for (i, (act, glyph, label, detail, selected)) in rows.iter().enumerate() {
            if let Some(a) = menu_row(
                ui,
                id.with(("row", i)),
                i == 0,
                glyph,
                label,
                detail,
                *selected,
            )
            .then_some(*act)
            {
                chosen = Some(a);
            }
        }
        // For the mouse (Esc closes it too).
        ui.add_space(16.0);
        let w = ui.available_width();
        ui.allocate_ui_with_layout(
            vec2(w, 44.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                closed = Button::secondary("Close")
                    .id(id.with("close"))
                    .show(ui)
                    .activated;
            },
        );
    });

    if chosen.is_some() || closed {
        cx.alerts.reminders.menu = None;
    }
    if chosen.is_some_and(|a| a != Act::Watch) || closed {
        cx.alerts.reminders.refocus = back_to;
    }
    if closed {
        return MenuOutcome {
            close: true,
            watch: false,
        };
    }
    let Some(act) = chosen else {
        return MenuOutcome::default();
    };
    let mut out = MenuOutcome {
        close: true,
        watch: false,
    };
    let Some(session) = cx.session() else {
        return out;
    };
    let profile = session.profile_id();
    let stream_id = channel.key.clone();
    let start_at = Timestamp::from_unix_millis(p.start.timestamp_millis());
    let time = cx.prefs.format_time(&p.start.with_timezone(&Local));
    match act {
        Act::Watch => out.watch = true,
        Act::Remind(action) => {
            if mark.reminder == Some(action) {
                return out;
            }
            let r = NewReminder {
                stream_id: stream_id.clone(),
                start_at,
                end_at: Timestamp::from_unix_millis(p.stop.timestamp_millis()),
                title: p.title.clone(),
                channel_name: channel_name.clone(),
                icon_url: channel.logo.clone(),
                action,
            };
            cx.alerts
                .reminders
                .set_mark(&stream_id, start, |m| m.reminder = Some(action));
            cx.toasts.success(match action {
                ReminderAction::Notify => format!("Reminder set for {time}"),
                ReminderAction::Switch => format!("We'll switch to {channel_name} at {time}"),
            });
            write(cx, "set the reminder", move |s| {
                s.reminders().set(profile, &r).map(|_| ())
            });
        }
        Act::CancelReminder => {
            cx.alerts
                .reminders
                .set_mark(&stream_id, start, |m| m.reminder = None);
            cx.toasts.success("Reminder cancelled");
            write(cx, "cancel the reminder", move |s| {
                if let Some(r) = s.reminders().find(profile, &stream_id, start_at)? {
                    s.reminders().remove(r.id)?;
                }
                Ok(())
            });
        }
        Act::Record => {
            let prog = crate::dvr::ProgrammeToRecord {
                stream_id: stream_id.clone(),
                channel_name: channel_name.clone(),
                channel_logo: channel.logo.clone(),
                title: p.title.clone(),
                start: p.start,
                end: p.stop,
            };
            cx.alerts.reminders.set_mark(&stream_id, start, |m| {
                m.recording = Some(RecordingState::Scheduled);
            });
            cx.toasts.success(if live {
                format!(
                    "Recording until {}",
                    cx.prefs.format_time(&p.stop.with_timezone(&Local))
                )
            } else {
                format!("Recording scheduled for {time}")
            });
            write(cx, "schedule the recording", move |s| {
                crate::dvr::schedule_programme(s, profile, &prog).map(|_| ())
            });
        }
        Act::CancelRecording => {
            cx.alerts
                .reminders
                .set_mark(&stream_id, start, |m| m.recording = None);
            cx.toasts.success("Recording cancelled");
            write(cx, "cancel the recording", move |s| {
                if let Some(r) = s.recordings().find_live(profile, &stream_id, start_at)?
                    && r.state != RecordingState::Active
                {
                    s.recordings()
                        .set_state(r.id, RecordingState::Cancelled, None)?;
                }
                Ok(())
            });
        }
        // p7b: stop it here (keeps what was recorded).
        Act::StopRecording => {
            if let Some(id) = crate::dvr::ui::running_recording(cx, &stream_id, start_at) {
                crate::dvr::ui::stop_recording(cx, id);
            }
            cx.alerts
                .reminders
                .set_mark(&stream_id, start, |m| m.recording = None);
        }
    }
    out
}

/// One 52 pt action row: icon, label, detail on the right, a check when selected.
/// Returns true when activated.
fn menu_row(
    ui: &mut Ui,
    id: Id,
    autofocus: bool,
    glyph: &'static str,
    label: &str,
    detail: &str,
    selected: bool,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    let (_, f) = focus::focusable(ui, id, rect, FocusOpts::default().autofocus_if(autofocus));
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        CornerRadius::same(radius::SM),
        if f.focused {
            color::SURFACE_3
        } else {
            color::SURFACE_2
        },
    );
    let fg = if f.focused {
        color::TEXT
    } else {
        color::TEXT_2
    };
    icons::paint(
        &p,
        pos2(rect.left() + 28.0, rect.center().y),
        glyph,
        20.0,
        Style::Regular,
        if glyph == icons::RECORD {
            color::LIVE
        } else {
            fg
        },
    );
    text::line(
        &p,
        pos2(rect.left() + 52.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font::body_strong(),
        color::TEXT,
        rect.width() - 200.0,
    );
    let mut rx = rect.right() - 16.0;
    if selected {
        icons::paint(
            &p,
            pos2(rx - 10.0, rect.center().y),
            icons::CHECK,
            20.0,
            Style::Regular,
            color::ACCENT,
        );
        rx -= 32.0;
    }
    if !detail.is_empty() {
        p.text(
            pos2(rx, rect.center().y),
            Align2::RIGHT_CENTER,
            detail,
            font::caption(),
            color::TEXT_3,
        );
    }
    if f.focused {
        paint::focus_ring(&p, rect, radius::SM, 1.0);
    }
    f.activated
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: ReminderAction = ReminderAction::Notify;
    const S: ReminderAction = ReminderAction::Switch;

    // A programme from 20:00 to 21:00 (as seconds from an arbitrary origin).
    const START: i64 = 72_000;
    const END: i64 = 75_600;

    #[test]
    fn notify_fires_lead_before_the_start() {
        assert_eq!(due(N, START, END, START - 61, 60), Due::Wait);
        assert_eq!(due(N, START, END, START - 60, 60), Due::Notify);
        assert_eq!(due(N, START, END, START - 1, 300), Due::Notify);
        assert_eq!(due(N, START, END, START - 301, 300), Due::Wait);
        // Lead 0: at the start.
        assert_eq!(due(N, START, END, START - 1, 0), Due::Wait);
        assert_eq!(due(N, START, END, START, 0), Due::Notify);
        // A negative lead is treated as 0.
        assert_eq!(due(N, START, END, START - 1, -60), Due::Wait);
    }

    #[test]
    fn switch_counts_down_onto_the_start() {
        assert_eq!(due(S, START, END, START - 11, 300), Due::Wait);
        assert_eq!(due(S, START, END, START - 10, 300), Due::Switch);
        assert_eq!(due(S, START, END, START + 30, 300), Due::Switch);
    }

    #[test]
    fn late_reminders_are_missed_while_the_programme_is_on() {
        // The profile opened 5 min into the programme.
        assert_eq!(due(N, START, END, START + 300, 60), Due::Missed);
        assert_eq!(due(S, START, END, START + 300, 60), Due::Missed);
        // Slightly late (the list loaded a moment after the fire time): still on time.
        assert_eq!(due(N, START, END, START + 20, 60), Due::Notify);
        // A long lead and the profile opened before the start: not missed.
        assert_eq!(due(N, START, END, START - 120, 300), Due::Notify);
        // Over.
        assert_eq!(due(N, START, END, END, 60), Due::Over);
        assert_eq!(due(S, START, END, END + 1, 60), Due::Over);
    }

    #[test]
    fn texts_are_plain() {
        assert_eq!(starting_in(START, START - 60), "Starting in 1 min");
        assert_eq!(starting_in(START, START - 61), "Starting in 2 min");
        assert_eq!(starting_in(START, START - 300), "Starting in 5 min");
        assert_eq!(starting_in(START, START - 10), "Starting now");
        assert_eq!(started_ago(START, START + 30), "Started just now");
        assert_eq!(started_ago(START, START + 300), "Started 5 min ago");
        assert_eq!(started_ago(START, START + 3900), "Started 1 h 05 ago");
    }

    #[test]
    fn marks_come_from_reminders_and_live_recordings() {
        let mut s = Scheduler::default();
        let profile = ProfileId::new_random();
        let r = Reminder {
            id: ReminderId(1),
            profile,
            stream_id: "1001".into(),
            start_at: Timestamp::from_unix_secs(START),
            end_at: Timestamp::from_unix_secs(END),
            title: "News".into(),
            channel_name: "Synthetic One".into(),
            icon_url: None,
            action: S,
            fired_at: None,
            created_at: Timestamp::EPOCH,
        };
        let fired = Reminder {
            id: ReminderId(2),
            stream_id: "1002".into(),
            fired_at: Some(Timestamp::from_unix_secs(START)),
            action: N,
            ..r.clone()
        };
        s.apply(Snapshot {
            profile,
            writes: 0,
            reminders: vec![r, fired],
            recordings: Vec::new(),
            lead: 300,
        });
        assert_eq!(s.lead_secs(), 300);
        assert_eq!(
            s.mark("1001", START),
            Some(Mark {
                reminder: Some(S),
                recording: None
            })
        );
        // A fired reminder keeps its bell until the programme ends, but no longer fires.
        assert!(s.mark("1002", START).is_some());
        assert_eq!(s.pending().len(), 1);
        assert_eq!(s.mark("1001", START + 1), None);
        // Optimistic edits.
        s.set_mark("1003", START, |m| {
            m.recording = Some(RecordingState::Scheduled)
        });
        assert!(s.mark("1003", START).is_some());
        s.set_mark("1003", START, |m| m.recording = None);
        assert_eq!(s.mark("1003", START), None);
    }

    #[test]
    fn a_snapshot_older_than_a_write_keeps_the_optimistic_marks() {
        let mut s = Scheduler::default();
        s.set_mark("1001", START, |m| m.reminder = Some(N));
        s.writes.store(1, Ordering::Release);
        s.apply(Snapshot {
            profile: ProfileId::new_random(),
            writes: 0,
            reminders: Vec::new(),
            recordings: Vec::new(),
            lead: 60,
        });
        assert!(s.mark("1001", START).is_some(), "stale load ignored");
        s.apply(Snapshot {
            profile: ProfileId::new_random(),
            writes: 1,
            reminders: Vec::new(),
            recordings: Vec::new(),
            lead: 60,
        });
        assert_eq!(s.mark("1001", START), None, "fresh load wins");
    }
}

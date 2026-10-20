// SPDX-License-Identifier: GPL-3.0-or-later
//! The sleep timer (`cx.sleep`): stop playback after 15–90 minutes, at the end of this film
//! or episode, or at the end of the live programme (from the session's EPG now/next).
//!
//! - **Setting it:** the moon button in the player's OSD ([`osd_button`], a small menu) or
//!   the `Z` key ([`player_input`]): Off → 15 → 30 → 45 → 60 → 90 min → end of this … → Off,
//!   with a flash such as "Sleep in 30 min". While it is set, the moon button shows the
//!   time left.
//! - **Counting:** it runs while something plays in the main player (`cx.playback`) or in
//!   multiscreen (`cx.multi`). When nothing has played for [`IDLE_GRACE`] (playback stopped
//!   on its own, or the user stopped it) the timer is cancelled. The grace covers zapping
//!   and the next-episode countdown.
//! - **The last [`FADE`]:** the volume fades out (mpv's volume only: the saved volume in the
//!   preferences is never touched, and it is restored afterwards; Settings:
//!   [`SLEEP_FADE_OUT`]) and the "Still watching?" prompt ([`crate::services::alerts`])
//!   counts down with Keep watching / Stop now. Any key, Esc or Keep watching restores the
//!   volume and turns the timer off.
//! - **At zero:** it stops the player and every multiscreen tile (which frees their provider
//!   connections), leaves window fullscreen and closes the player screen if it is on top.
//!
//! The timing is the pure [`Machine`] (time injected, unit-tested); [`on_frame`] feeds it
//! the player's state and applies what it decides.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, CornerRadius, Event, Frame, Id, Key, Margin, Modifiers, Order, Rect, Sense, Ui,
    pos2, vec2,
};
use opensmarters_core::chrono::{DateTime, Local, Utc};
use opensmarters_store::settings::Setting;

use crate::focus::{self, FocusOpts};
use crate::playback::{MediaKind, PlaybackStatus};
use crate::screens::AppCtx;
use crate::services::alerts::{Countdown, Prompt};
use crate::task::Loader;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::IconButton;
use crate::ui::{paint, text};

/// "Fade out before stopping" (Settings › Recordings & downloads). Default on.
pub const SLEEP_FADE_OUT: Setting<bool> = Setting::new("app.sleep_fade_out", || true);

/// The volume fades out and "Still watching?" counts down over this last stretch.
pub const FADE: Duration = Duration::from_secs(30);

/// Nothing playing for this long cancels the timer (zapping and the 10 s next-episode
/// countdown fit inside it).
pub const IDLE_GRACE: Duration = Duration::from_secs(15);

/// The fixed durations offered, in minutes.
pub const MINUTES: [u32; 5] = [15, 30, 45, 60, 90];

/// The alert key of the "Still watching?" prompt.
pub const PROMPT_KEY: &str = "sleep";

/// What the timer is set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// Not set.
    #[default]
    Off,
    /// After this many minutes.
    Minutes(u32),
    /// At the end of the film, episode or recording that plays (VOD end of file).
    EndOfItem,
    /// At the end of the live programme on air when it was set (EPG).
    EndOfProgramme,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    At(Instant),
    EndOfItem,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Armed {
    choice: Choice,
    target: Target,
    idle_since: Option<Instant>,
}

/// What plays this frame (fed to [`Machine::tick`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Playing {
    /// Something plays (or opens, buffers, reconnects, is paused) in the main player or in
    /// multiscreen.
    pub active: bool,
    /// Time left in the film/episode (VOD), when known.
    pub item_left: Option<Duration>,
    /// The film/episode reached its end.
    pub item_finished: bool,
}

/// One step of the [`Machine`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tick {
    /// Not set.
    Off,
    /// Counting; `left` is unknown while a film's duration is unknown.
    Counting {
        /// Time left.
        left: Option<Duration>,
    },
    /// The last [`FADE`]: fade the volume to `volume` (1 → 0) and ask "Still watching?".
    Warning {
        /// Time left.
        left: Duration,
        /// Volume factor 0..=1 (smooth).
        volume: f32,
    },
    /// Time is up: stop everything. The machine is off afterwards.
    Expired,
    /// Nothing played for [`IDLE_GRACE`]: the timer turned itself off.
    Cancelled,
}

/// The sleep timer's state machine. Pure: the caller passes the time and what plays.
#[derive(Debug, Default)]
pub struct Machine {
    armed: Option<Armed>,
}

impl Machine {
    /// The current choice ([`Choice::Off`] when not set).
    pub fn choice(&self) -> Choice {
        self.armed.map_or(Choice::Off, |a| a.choice)
    }

    /// True while set.
    pub fn is_set(&self) -> bool {
        self.armed.is_some()
    }

    /// Sets (or restarts) the timer. `length` is required for [`Choice::Minutes`] and
    /// [`Choice::EndOfProgramme`] (time until the programme ends) and ignored otherwise.
    /// Returns false (and leaves the timer unchanged) when a needed length is missing.
    pub fn set(&mut self, choice: Choice, now: Instant, length: Option<Duration>) -> bool {
        let target = match choice {
            Choice::Off => {
                self.armed = None;
                return true;
            }
            Choice::EndOfItem => Target::EndOfItem,
            Choice::Minutes(_) | Choice::EndOfProgramme => match length {
                Some(l) => Target::At(now + l),
                None => return false,
            },
        };
        self.armed = Some(Armed {
            choice,
            target,
            idle_since: None,
        });
        true
    }

    /// Turns the timer off (Keep watching, a key, the menu).
    pub fn cancel(&mut self) {
        self.armed = None;
    }

    /// Advances the machine to `now`.
    pub fn tick(&mut self, now: Instant, p: Playing) -> Tick {
        let Some(a) = &mut self.armed else {
            return Tick::Off;
        };
        let left = match a.target {
            Target::At(t) => Some(t.saturating_duration_since(now)),
            Target::EndOfItem if p.item_finished => Some(Duration::ZERO),
            Target::EndOfItem => p.item_left,
        };
        if left == Some(Duration::ZERO) {
            self.armed = None;
            return Tick::Expired;
        }
        if p.active {
            a.idle_since = None;
        } else {
            let since = *a.idle_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= IDLE_GRACE {
                self.armed = None;
                return Tick::Cancelled;
            }
        }
        match left {
            Some(l) if l <= FADE && p.active => Tick::Warning {
                left: l,
                volume: fade_volume(l),
            },
            left => Tick::Counting { left },
        }
    }
}

/// The volume factor with `left` of the fade remaining: 1 at [`FADE`], 0 at the end, eased
/// (smoothstep) so the start and the end of the fade are gentle.
pub fn fade_volume(left: Duration) -> f32 {
    let t = (left.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// "28 min" / "1 h 59" / "45 s" (rounded up: the timer never shows 0 while it runs).
pub fn left_label(left: Duration) -> String {
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    let mins = secs.div_ceil(60);
    match (secs, mins) {
        (0..60, _) => format!("{secs} s"),
        (_, 0..60) => format!("{mins} min"),
        (_, m) if m % 60 == 0 => format!("{} h", m / 60),
        (_, m) => format!("{} h {:02}", m / 60, m % 60),
    }
}

/// The choice after `current` in `options` (the `Z` key), wrapping to the first.
pub fn next_choice(current: Choice, options: &[Choice]) -> Choice {
    match options.iter().position(|c| *c == current) {
        Some(i) => options
            .get(i + 1)
            .or_else(|| options.first())
            .copied()
            .unwrap_or(Choice::Off),
        // The current choice is not offered here (e.g. the programme end became unknown):
        // start the cycle again after "Off".
        None => options.get(1).copied().unwrap_or(Choice::Off),
    }
}

/// How long a "minute" lasts. Dev only (`demo` feature):
/// `OPENSMARTERS_DEMO_SLEEP_SECS_PER_MIN=2` makes 15 min last 30 s for harness runs.
fn minute() -> Duration {
    #[cfg(feature = "demo")]
    {
        static SCALE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
        let scale = SCALE.get_or_init(|| {
            std::env::var("OPENSMARTERS_DEMO_SLEEP_SECS_PER_MIN")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|s| (1..=60).contains(s))
        });
        if let Some(s) = scale {
            return Duration::from_secs(*s);
        }
    }
    Duration::from_secs(60)
}

/// The sleep timer (`cx.sleep`). See the [module docs](self).
#[derive(Debug, Default)]
pub struct SleepTimer {
    machine: Machine,
    /// Time left at the last frame (for the OSD).
    left: Option<Duration>,
    /// The mpv volume before the fade started (restored afterwards; never saved).
    faded_from: Option<f64>,
    /// "Still watching?" is up.
    warning: bool,
    /// [`SLEEP_FADE_OUT`], once read from the store (`None` = default).
    fade_out: Option<bool>,
    settings: Loader<bool>,
    menu_open: bool,
    /// The menu was drawn this frame (the OSD may hide it).
    menu_seen: bool,
    refocus: bool,
    /// The last frame (`cumulative_pass_nr`) the player screen was on top.
    player_pass: Option<u64>,
}

impl SleepTimer {
    /// What the timer is set to.
    pub fn choice(&self) -> Choice {
        self.machine.choice()
    }

    /// True while the timer is set.
    pub fn is_set(&self) -> bool {
        self.machine.is_set()
    }

    /// Time left, when known (the OSD shows it).
    pub fn left(&self) -> Option<Duration> {
        self.left.filter(|_| self.is_set())
    }

    /// True if the player screen was on top during the last frame (it reports itself
    /// through [`player_input`]).
    pub fn player_on_top(&self, ctx: &egui::Context) -> bool {
        self.player_pass
            .is_some_and(|p| p + 2 >= ctx.cumulative_pass_nr())
    }

    /// The "fade out" setting.
    pub fn fade_out(&self) -> bool {
        self.fade_out.unwrap_or(true)
    }

    /// Changes the "fade out" setting and saves it.
    pub fn set_fade_out(&mut self, services: &crate::services::Services, on: bool) {
        self.fade_out = Some(on);
        services.blocking("save the sleep timer setting", move |s| {
            s.settings().set(&SLEEP_FADE_OUT, &on)
        });
    }
}

/// What plays now, for the machine.
fn playing(cx: &AppCtx<'_>) -> Playing {
    let status = cx.playback.status();
    let main = cx.playback.request().is_some()
        && matches!(
            status,
            PlaybackStatus::Opening
                | PlaybackStatus::Playing
                | PlaybackStatus::Paused
                | PlaybackStatus::Buffering
                | PlaybackStatus::Reconnecting { .. }
        );
    let vod = cx.playback.request().is_some_and(|r| !r.kind.is_live());
    let item_left = if vod && main {
        let st = cx.playback.state();
        let duration = st
            .duration
            .or_else(|| cx.playback.request().and_then(|r| r.duration_hint));
        match (duration, st.time_pos) {
            (Some(d), Some(p)) if d.is_finite() && p.is_finite() && d > 0.0 => {
                Duration::try_from_secs_f64((d - p).max(0.0)).ok()
            }
            _ => None,
        }
    } else {
        None
    };
    Playing {
        active: main || cx.multi.is_active(),
        item_left,
        item_finished: vod && status == PlaybackStatus::Finished,
    }
}

/// The live programme's end, from the session's EPG now/next (`None` when unknown).
fn programme_end(cx: &AppCtx<'_>) -> Option<DateTime<Utc>> {
    let req = cx.playback.request()?;
    if req.kind != MediaKind::Live {
        return None;
    }
    let session = cx.playback.session()?;
    let zap = req.zap.as_ref()?;
    let ch = zap.channels.get(zap.index)?;
    let (_, now_p, _) = session.now_next(ch, cx.now);
    now_p.map(|p| p.stop).filter(|stop| *stop > cx.now)
}

/// The "end of this …" choice for what plays, with its label.
fn end_option(cx: &AppCtx<'_>) -> Option<(Choice, String)> {
    let kind = cx.playback.request()?.kind;
    let label = match kind {
        MediaKind::Live => {
            let end = programme_end(cx)?;
            return Some((
                Choice::EndOfProgramme,
                format!(
                    "End of this programme ({})",
                    cx.prefs.format_time(&end.with_timezone(&Local))
                ),
            ));
        }
        MediaKind::Movie => "End of this film",
        MediaKind::Episode => "End of this episode",
        MediaKind::Catchup => "End of this programme",
        MediaKind::Recording => "End of this recording",
    };
    Some((Choice::EndOfItem, label.to_owned()))
}

/// Every choice offered now, with menu labels.
fn options(cx: &AppCtx<'_>) -> Vec<(Choice, String)> {
    let mut v = vec![(Choice::Off, "Off".to_owned())];
    v.extend(
        MINUTES
            .iter()
            .map(|m| (Choice::Minutes(*m), format!("{m} min"))),
    );
    v.extend(end_option(cx));
    v
}

/// Restores the volume if a fade lowered it.
fn restore_volume(cx: &mut AppCtx<'_>) {
    if let Some(v) = cx.sleep.faded_from.take() {
        tracing::debug!(volume = v, "sleep timer: volume restored");
        cx.playback.set_volume(v);
    }
}

/// Removes the prompt and restores the volume (the timer left its last stretch).
fn end_warning(cx: &mut AppCtx<'_>) {
    restore_volume(cx);
    if cx.sleep.warning {
        cx.sleep.warning = false;
        cx.alerts.dismiss(PROMPT_KEY);
    }
}

/// Sets the timer to `choice`; returns the message to flash.
pub fn set(cx: &mut AppCtx<'_>, choice: Choice) -> String {
    end_warning(cx);
    let now = Instant::now();
    let msg = match choice {
        Choice::Off => {
            cx.sleep.machine.cancel();
            "Sleep timer off".to_owned()
        }
        Choice::Minutes(m) => {
            cx.sleep.machine.set(choice, now, Some(minute() * m));
            format!("Sleep in {m} min")
        }
        Choice::EndOfItem => {
            cx.sleep.machine.set(choice, now, None);
            let label = end_option(cx).map_or_else(|| "End of this film".to_owned(), |(_, l)| l);
            format!("Sleep at the {}", label.to_lowercase())
        }
        Choice::EndOfProgramme => {
            let Some(end) = programme_end(cx) else {
                cx.sleep.machine.cancel();
                return "This programme's end time isn't known".to_owned();
            };
            let left = (end - cx.now).to_std().unwrap_or_default();
            cx.sleep.machine.set(choice, now, Some(left));
            format!(
                "Sleep at the end of this programme ({})",
                cx.prefs.format_time(&end.with_timezone(&Local))
            )
        }
    };
    cx.sleep.left = None;
    tracing::info!(choice = ?choice, "sleep timer set");
    cx.egui.request_repaint();
    msg
}

/// Keep watching: the timer turns off and the volume comes back.
fn keep_watching(cx: &mut AppCtx<'_>) {
    end_warning(cx);
    cx.sleep.machine.cancel();
    cx.toasts.push(
        crate::ui::widgets::ToastKind::Info,
        "Sleep timer off",
        Some(PROMPT_KEY),
    );
}

/// Time is up: stop everything that plays, leave fullscreen, close the player.
fn stop_everything(cx: &mut AppCtx<'_>) {
    // The volume first, so the next stream isn't silent.
    end_warning(cx);
    cx.sleep.machine.cancel();
    cx.sleep.menu_open = false;
    tracing::info!("sleep timer: stopping playback");
    cx.playback.stop();
    cx.multi.stop_all();
    let fs = cx.egui.input(|i| i.viewport().fullscreen.unwrap_or(false));
    if fs {
        cx.egui
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }
    if cx.sleep.player_on_top(&cx.egui) {
        cx.nav.back();
    }
    cx.toasts.push(
        crate::ui::widgets::ToastKind::Info,
        "The sleep timer stopped playback",
        Some(PROMPT_KEY),
    );
}

fn is_prompt_key(k: Key) -> bool {
    matches!(
        k,
        Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::Tab
            | Key::Enter
            | Key::Space
            | Key::Escape
            | Key::Backspace
    )
}

/// During "Still watching?": any key other than the ones that operate the prompt means
/// "I'm here". The key is swallowed so it doesn't also zap, mute or leave the player.
fn take_wake_key(ctx: &egui::Context) -> bool {
    ctx.input_mut(|i| {
        let wake = i
            .events
            .iter()
            .any(|e| matches!(e, Event::Key { pressed: true, key, .. } if !is_prompt_key(*key)));
        if wake {
            i.events
                .retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_)));
        }
        wake
    })
}

/// Called once per frame before the screen.
pub fn on_frame(cx: &mut AppCtx<'_>) {
    let ctx = cx.egui.clone();
    // The fade setting, once.
    if cx.sleep.settings.is_idle() {
        let store = cx.services.store.clone();
        cx.sleep
            .settings
            .start_blocking(cx.services.rt(), &ctx, move || {
                Ok(store.settings().get(&SLEEP_FADE_OUT)?)
            });
    }
    if cx.sleep.settings.poll()
        && cx.sleep.fade_out.is_none()
        && let Some(on) = cx.sleep.settings.value()
    {
        cx.sleep.fade_out = Some(*on);
    }

    // The OSD menu: closes when the OSD hid it; Esc closes it.
    if cx.sleep.menu_open && !cx.sleep.menu_seen {
        cx.sleep.menu_open = false;
    }
    cx.sleep.menu_seen = false;
    if cx.sleep.menu_open
        && ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Escape)
                | i.consume_key(Modifiers::NONE, Key::Backspace)
        })
    {
        cx.sleep.menu_open = false;
        cx.sleep.refocus = true;
    }

    if !cx.sleep.machine.is_set() {
        cx.sleep.left = None;
        end_warning(cx);
        return;
    }
    // "End of this film" makes no sense once a live channel plays.
    if cx.sleep.choice() == Choice::EndOfItem
        && cx.playback.request().is_some_and(|r| r.kind.is_live())
    {
        end_warning(cx);
        cx.sleep.machine.cancel();
        cx.toasts.info("Sleep timer off");
        return;
    }

    // Answers to "Still watching?".
    if cx.sleep.warning && take_wake_key(&ctx) {
        keep_watching(cx);
        return;
    }
    match cx.alerts.take_answer(PROMPT_KEY) {
        Some("stop") => {
            stop_everything(cx);
            return;
        }
        Some(_) => {
            keep_watching(cx);
            return;
        }
        None => {}
    }

    let now = Instant::now();
    let p = playing(cx);
    match cx.sleep.machine.tick(now, p) {
        Tick::Off => end_warning(cx),
        Tick::Counting { left } => {
            // Back above the fade (e.g. seeking back in a film): undo it.
            end_warning(cx);
            cx.sleep.left = left;
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        Tick::Warning { left, volume } => {
            cx.sleep.left = Some(left);
            let until = now + left;
            if !cx.sleep.warning {
                cx.sleep.warning = true;
                let body = if cx.sleep.fade_out() {
                    "The sound is fading out. Playback stops when the countdown ends."
                } else {
                    "Playback stops when the countdown ends."
                };
                cx.alerts.push(
                    Prompt::new(PROMPT_KEY, icons::MOON, "Still watching?")
                        .body(body)
                        .action("stop", "Stop now", false)
                        .action("keep", "Keep watching", true)
                        .cancel("keep")
                        .countdown(Countdown {
                            until,
                            total: FADE,
                            label: "Stopping in",
                        })
                        .modal()
                        .priority(10),
                );
            } else {
                cx.alerts.update(PROMPT_KEY, |p| {
                    if let Some(c) = &mut p.countdown {
                        c.until = until;
                    }
                });
            }
            if cx.sleep.fade_out() {
                let base = *cx.sleep.faded_from.get_or_insert_with(|| {
                    let v = cx.playback.state().volume.clamp(0.0, 100.0);
                    tracing::debug!(volume = v, "sleep timer: fading out");
                    v
                });
                cx.playback.set_volume(base * f64::from(volume));
            } else {
                restore_volume(cx);
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        Tick::Expired => stop_everything(cx),
        Tick::Cancelled => {
            end_warning(cx);
            cx.sleep.left = None;
            tracing::info!("sleep timer: nothing playing, turned off");
        }
    }
}

/// The player's key hook (from its `on_input`, which runs only while it is on top): `Z`
/// cycles the timer. Returns the message for the player to flash.
pub fn player_input(cx: &mut AppCtx<'_>, ctx: &egui::Context) -> Option<String> {
    cx.sleep.player_pass = Some(ctx.cumulative_pass_nr());
    if !ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Z)) {
        return None;
    }
    let choices: Vec<Choice> = options(cx).into_iter().map(|(c, _)| c).collect();
    let next = next_choice(cx.sleep.choice(), &choices);
    Some(set(cx, next))
}

fn button_id() -> Id {
    Id::unique("player.sleep")
}

fn option_id(i: usize) -> Id {
    Id::unique(("player.sleep.option", i))
}

const ROW_H: f32 = 44.0;

/// The moon button of the player's OSD, drawn at `rect` (48 pt circle), with its menu.
/// Returns the message for the player to flash when the user picks a choice.
pub fn osd_button(cx: &mut AppCtx<'_>, ui: &mut Ui, rect: Rect) -> Option<String> {
    let ctx = cx.egui.clone();
    if std::mem::take(&mut cx.sleep.refocus) {
        focus::request(&ctx, button_id());
    }
    let left = cx.sleep.left();
    let mut button = IconButton::new(icons::MOON)
        .size(48.0, 24.0)
        .id(button_id());
    if cx.sleep.is_set() {
        button = button
            .selected(true)
            .caption(left.map_or_else(|| "on".to_owned(), left_label));
    }
    let options = options(cx);
    let current = options
        .iter()
        .position(|(c, _)| *c == cx.sleep.choice())
        .unwrap_or(0);
    if button.show_at(ui, rect).activated {
        cx.sleep.menu_open = !cx.sleep.menu_open;
        if cx.sleep.menu_open {
            focus::request(&ctx, option_id(current));
        }
    }
    if !cx.sleep.menu_open {
        return None;
    }
    cx.sleep.menu_seen = true;

    // The menu opens above the button, right-aligned with it.
    let width = 340.0;
    let header_h = 36.0;
    let menu_h = header_h + options.len() as f32 * ROW_H + 16.0;
    let screen = ctx.content_rect();
    let pos = pos2(
        (rect.right() - width).max(screen.left() + 8.0),
        (rect.top() - 12.0 - menu_h).max(screen.top() + 8.0),
    );
    let scope = Id::unique("player.sleep.menu");
    focus::trap(&ctx, scope);
    let mut chosen = None;
    let menu = egui::Area::new(Id::unique("player.sleep.area"))
        .order(Order::Foreground)
        .fixed_pos(pos)
        .show(&ctx, |ui| {
            Frame::new()
                .fill(color::SURFACE_1)
                .corner_radius(CornerRadius::same(radius::LG))
                .shadow(elevation::E2)
                .stroke(egui::Stroke::new(1.0, color::STROKE_SUBTLE))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let (hr, _) = ui
                        .allocate_exact_size(vec2(ui.available_width(), header_h), Sense::hover());
                    text::overline(
                        ui.painter(),
                        pos2(hr.left() + 14.0, hr.center().y),
                        Align2::LEFT_CENTER,
                        "Sleep timer",
                        color::TEXT_3,
                    );
                    focus::scope(ui, scope, |ui| {
                        for (i, (choice, label)) in options.iter().enumerate() {
                            let (r, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), ROW_H),
                                Sense::hover(),
                            );
                            let (_, f) = focus::focusable(
                                ui,
                                option_id(i),
                                r,
                                FocusOpts::default().autofocus_if(i == current),
                            );
                            let p = ui.painter().clone();
                            if f.focused {
                                p.rect_filled(r, CornerRadius::same(radius::SM), color::SURFACE_3);
                            }
                            let selected = i == current;
                            text::line(
                                &p,
                                pos2(r.left() + 14.0, r.center().y),
                                Align2::LEFT_CENTER,
                                label,
                                font::body_strong(),
                                if f.focused || selected {
                                    color::TEXT
                                } else {
                                    color::TEXT_2
                                },
                                r.width() - if selected { 124.0 } else { 28.0 },
                            );
                            let mut rx = r.right() - 14.0;
                            if selected {
                                icons::paint(
                                    &p,
                                    pos2(rx - 9.0, r.center().y),
                                    icons::CHECK,
                                    18.0,
                                    Style::Regular,
                                    color::ACCENT,
                                );
                                rx -= 28.0;
                                if *choice != Choice::Off
                                    && let Some(l) = left
                                {
                                    p.text(
                                        pos2(rx, r.center().y),
                                        Align2::RIGHT_CENTER,
                                        format!("{} left", left_label(l)),
                                        font::caption(),
                                        color::TEXT_3,
                                    );
                                }
                            }
                            if f.focused {
                                paint::focus_ring(&p, r, radius::SM, 1.0);
                            }
                            if f.activated {
                                chosen = Some(*choice);
                            }
                        }
                    });
                });
        });
    let clicked_outside = ctx.input(|i| {
        i.pointer.any_click()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !menu.response.rect.contains(p) && !rect.contains(p))
    });
    if clicked_outside {
        cx.sleep.menu_open = false;
    }
    let choice = chosen?;
    cx.sleep.menu_open = false;
    cx.sleep.refocus = true;
    Some(set(cx, choice))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    const PLAYING: Playing = Playing {
        active: true,
        item_left: None,
        item_finished: false,
    };

    #[test]
    fn minutes_count_down_fade_then_expire() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert_eq!(m.tick(t0, PLAYING), Tick::Off);
        assert!(m.set(Choice::Minutes(15), t0, Some(secs(15 * 60))));
        assert_eq!(m.choice(), Choice::Minutes(15));
        assert_eq!(
            m.tick(t0 + secs(60), PLAYING),
            Tick::Counting {
                left: Some(secs(14 * 60))
            }
        );
        // Last 30 s: warning, volume going down.
        match m.tick(t0 + secs(15 * 60 - 30), PLAYING) {
            Tick::Warning { left, volume } => {
                assert_eq!(left, secs(30));
                assert!((volume - 1.0).abs() < 1e-6);
            }
            other => panic!("{other:?}"),
        }
        match m.tick(t0 + secs(15 * 60 - 10), PLAYING) {
            Tick::Warning { volume, .. } => assert!(volume > 0.0 && volume < 0.5),
            other => panic!("{other:?}"),
        }
        assert_eq!(m.tick(t0 + secs(15 * 60), PLAYING), Tick::Expired);
        assert!(!m.is_set());
        assert_eq!(m.tick(t0 + secs(15 * 60 + 1), PLAYING), Tick::Off);
    }

    #[test]
    fn a_missing_length_does_not_arm() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        assert!(!m.set(Choice::EndOfProgramme, t0, None));
        assert!(!m.is_set());
        assert!(m.set(Choice::EndOfProgramme, t0, Some(secs(600))));
        assert!(m.set(Choice::Off, t0, None));
        assert!(!m.is_set());
    }

    #[test]
    fn end_of_item_follows_the_film() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.set(Choice::EndOfItem, t0, None);
        // Duration not known yet: keeps waiting.
        assert_eq!(m.tick(t0, PLAYING), Tick::Counting { left: None });
        let at = |left: u64| Playing {
            item_left: Some(secs(left)),
            ..PLAYING
        };
        assert_eq!(
            m.tick(t0 + secs(5), at(600)),
            Tick::Counting {
                left: Some(secs(600))
            }
        );
        // Paused for an hour: the film hasn't moved, neither has the timer.
        assert_eq!(
            m.tick(t0 + secs(3600), at(600)),
            Tick::Counting {
                left: Some(secs(600))
            }
        );
        assert!(matches!(
            m.tick(t0 + secs(4000), at(20)),
            Tick::Warning { .. }
        ));
        // Seeking back leaves the warning.
        assert!(matches!(
            m.tick(t0 + secs(4001), at(300)),
            Tick::Counting { .. }
        ));
        // End of file: stop (the player reports "finished", which isn't "active").
        let finished = Playing {
            active: false,
            item_left: None,
            item_finished: true,
        };
        assert_eq!(m.tick(t0 + secs(4300), finished), Tick::Expired);
    }

    #[test]
    fn nothing_playing_cancels_after_the_grace() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.set(Choice::Minutes(30), t0, Some(secs(1800)));
        let idle = Playing::default();
        assert!(matches!(m.tick(t0 + secs(1), idle), Tick::Counting { .. }));
        // Zapping: a short gap, then playing again resets the grace.
        assert!(matches!(
            m.tick(t0 + secs(1) + IDLE_GRACE - secs(1), idle),
            Tick::Counting { .. }
        ));
        assert!(matches!(
            m.tick(t0 + secs(20), PLAYING),
            Tick::Counting { .. }
        ));
        assert!(matches!(m.tick(t0 + secs(21), idle), Tick::Counting { .. }));
        assert_eq!(m.tick(t0 + secs(21) + IDLE_GRACE, idle), Tick::Cancelled);
        assert!(!m.is_set());
    }

    #[test]
    fn no_warning_while_nothing_plays() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.set(Choice::Minutes(15), t0, Some(secs(40)));
        let idle = Playing::default();
        // Within the fade window but idle: no prompt, no fade.
        assert!(matches!(m.tick(t0 + secs(20), idle), Tick::Counting { .. }));
        // Playing again inside the window: the warning appears.
        assert!(matches!(
            m.tick(t0 + secs(21), PLAYING),
            Tick::Warning { .. }
        ));
    }

    #[test]
    fn cancel_turns_it_off() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.set(Choice::Minutes(60), t0, Some(secs(3600)));
        m.cancel();
        assert_eq!(m.choice(), Choice::Off);
        assert_eq!(m.tick(t0 + secs(4000), PLAYING), Tick::Off);
    }

    #[test]
    fn setting_again_restarts_the_countdown() {
        let t0 = Instant::now();
        let mut m = Machine::default();
        m.set(Choice::Minutes(15), t0, Some(secs(900)));
        m.set(Choice::Minutes(15), t0 + secs(800), Some(secs(900)));
        assert_eq!(
            m.tick(t0 + secs(900), PLAYING),
            Tick::Counting {
                left: Some(secs(800))
            }
        );
    }

    #[test]
    fn fade_is_smooth_and_bounded() {
        assert!((fade_volume(FADE) - 1.0).abs() < 1e-6);
        assert!(fade_volume(Duration::ZERO).abs() < 1e-6);
        assert!((fade_volume(secs(60)) - 1.0).abs() < 1e-6);
        let mut last = 0.0;
        for s in 0..=30 {
            let v = fade_volume(secs(s));
            assert!(v >= last, "fade must rise with time left");
            last = v;
        }
    }

    #[test]
    fn labels_round_up() {
        assert_eq!(left_label(secs(28 * 60)), "28 min");
        assert_eq!(left_label(secs(28 * 60 - 1)), "28 min");
        assert_eq!(left_label(secs(61)), "2 min");
        assert_eq!(left_label(secs(60)), "1 min");
        assert_eq!(left_label(Duration::from_millis(44_200)), "45 s");
        assert_eq!(left_label(secs(119 * 60)), "1 h 59");
        assert_eq!(left_label(secs(90 * 60)), "1 h 30");
        assert_eq!(left_label(secs(60 * 60)), "1 h");
    }

    #[test]
    fn z_cycles_through_the_choices() {
        let opts = [
            Choice::Off,
            Choice::Minutes(15),
            Choice::Minutes(30),
            Choice::EndOfItem,
        ];
        assert_eq!(next_choice(Choice::Off, &opts), Choice::Minutes(15));
        assert_eq!(next_choice(Choice::Minutes(30), &opts), Choice::EndOfItem);
        assert_eq!(next_choice(Choice::EndOfItem, &opts), Choice::Off);
        // A choice no longer offered starts the cycle again.
        assert_eq!(
            next_choice(Choice::EndOfProgramme, &opts),
            Choice::Minutes(15)
        );
        assert_eq!(next_choice(Choice::Off, &[]), Choice::Off);
    }
}

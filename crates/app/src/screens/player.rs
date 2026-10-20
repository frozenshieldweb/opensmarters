// SPDX-License-Identifier: GPL-3.0-or-later
//! The fullscreen player (§6.10, §8.5), used for live TV, movies, episodes and catch-up
//! (`Route::Player(PlayerRoute::Request(..))`).
//!
//! ## Keys
//!
//! Channel keys follow one convention everywhere (TV remotes, see
//! [`crate::playback::keys`]): **PageUp / Ctrl+↑ / media "next" = channel +1**,
//! **PageDown / Ctrl+↓ / media "previous" = channel −1**. Zapping stops at the ends of
//! the list ("First channel" / "Last channel" in the banner).
//!
//! | Key | OSD hidden | OSD visible |
//! |---|---|---|
//! | Space | play/pause | play/pause |
//! | Enter | VOD: play/pause (Up next card: play it) · live: show OSD | activate the focused control |
//! | ← / → | VOD: seek −/+10 s (Shift: 60 s) · live: channel −/+ | move focus (on the scrubber: seek) |
//! | ↑ / ↓ | live: channel +/− · VOD: show OSD | move focus |
//! | PageUp / PageDown, Ctrl+↑/↓ | channel +/− | channel +/− |
//! | 0–9 | channel number entry (zaps after 1.5 s or Enter; current list, else All) | same |
//! | M / F / I | mute / window fullscreen / show-hide OSD | same |
//! | A / S | next audio track / next subtitle (off → tracks → off) | same |
//! | + / − | volume ±5 | same |
//! | Esc, Backspace | leave the player (hides the Up next card first) | hide the OSD (closes the track sheet first) |
//!
//! Entering the player shows the OSD for 3 s. A channel change shows the zap banner
//! instead of the OSD; while the next channel (or a reconnect) opens, the last frame stays
//! up under a scrim with a spinner.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Event, Id, Key, Modifiers, Rect, Sense, Ui,
    pos2, vec2,
};
use opensmarters_core::chrono::Local;
use opensmarters_player::{TrackKind, TrackSelection, VideoFit};

use super::live::{LockFilter, all_by_number};
use super::{AppCtx, Chrome, Screen};
use crate::focus::{self, FocusOpts};
use crate::nav::{PlayerRoute, Route};
use crate::playback::keys::{self, Surface};
use crate::playback::next::UP_NEXT_EARLY_SECS;
use crate::playback::{
    MediaKind, PlaybackItem, PlaybackRequest, PlaybackStatus, ZapContext, ZapOutcome,
};
use crate::services::ImageSize;
use crate::services::catalog::LiveChannel;
use crate::task::Loader;
use crate::ui::assets::{self, Asset};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::callout::Badge;
use crate::ui::widgets::up_next::{self, UpNextAction, UpNextCard, UpNextCardAction, UpNextPrompt};
use crate::ui::widgets::{Button, ChannelLogo, IconButton, clock, zap_banner};
use crate::ui::{motion, paint, text};

const OSD_HIDE_AFTER: Duration = Duration::from_secs(3);
const NUMBER_ENTRY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Scrim over the held last frame while the next stream opens.
const SWITCH_SCRIM: Color32 = Color32::from_black_alpha(150);

fn scrubber_id() -> Id {
    Id::unique("player.scrubber")
}

fn up_next_id() -> Id {
    Id::unique("player.up-next")
}

/// Where a typed channel number leads (player and Live TV).
#[derive(Debug, Clone)]
pub(crate) enum NumberMatch {
    /// In the current zap list.
    Current(LiveChannel),
    /// Only in All (index into the All list).
    All(usize, LiveChannel),
    /// Nowhere.
    Missing,
    /// Not known yet (All is loading).
    Unknown,
}

/// The player screen.
#[derive(Debug)]
pub struct PlayerScreen {
    route: PlayerRoute,
    resolve: Loader<PlaybackRequest>,
    /// Every unlocked channel by number (channel-number fallback outside the zap list).
    all: Loader<Arc<Vec<LiveChannel>>>,
    last_activity: Instant,
    osd_forced: Option<bool>,
    sheet: bool,
    number_entry: Option<(String, Instant)>,
    /// A number typed while All was still loading.
    pending_number: Option<u64>,
    seek_preview: Option<f64>,
    flash: Option<(String, Instant)>,
    /// "+10 s" above the scrubber thumb.
    seek_flash: Option<(String, Instant)>,
}

impl PlayerScreen {
    /// Creates the screen for a player route.
    pub fn new(route: PlayerRoute) -> Self {
        Self {
            route,
            resolve: Loader::new(),
            all: Loader::new(),
            last_activity: Instant::now(),
            osd_forced: None,
            sheet: false,
            number_entry: None,
            pending_number: None,
            seek_preview: None,
            flash: None,
            seek_flash: None,
        }
    }

    fn osd_visible(&self, cx: &AppCtx<'_>) -> bool {
        if let Some(f) = self.osd_forced {
            return f;
        }
        if self.sheet {
            return true;
        }
        let st = cx.playback.status();
        if !matches!(
            st,
            PlaybackStatus::Playing
                | PlaybackStatus::Buffering
                | PlaybackStatus::Opening
                | PlaybackStatus::Reconnecting { .. }
        ) {
            return true;
        }
        self.last_activity.elapsed() < OSD_HIDE_AFTER
    }

    fn poke(&mut self) {
        self.last_activity = Instant::now();
        self.osd_forced = None;
    }

    fn is_live(cx: &AppCtx<'_>) -> bool {
        cx.playback
            .request()
            .is_some_and(|r| r.kind == MediaKind::Live)
    }

    fn flash(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
    }

    /// Zaps by `delta` from a key: the banner shows the channel, the OSD stays down so the
    /// next key zaps again.
    fn zap(&mut self, cx: &mut AppCtx<'_>, delta: isize) {
        match cx.playback.zap(delta) {
            ZapOutcome::Zapped(_) | ZapOutcome::Edge(_) => {
                self.osd_forced = Some(false);
                self.sheet = false;
            }
            ZapOutcome::Unavailable => {}
        }
    }

    fn toggle_fullscreen(cx: &AppCtx<'_>) {
        let fs = cx.egui.input(|i| i.viewport().fullscreen.unwrap_or(false));
        cx.egui
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }

    fn change_volume(cx: &mut AppCtx<'_>, delta: f64) {
        let v = (cx.playback.state().volume + delta).clamp(0.0, 100.0);
        cx.playback.set_volume(v);
        cx.prefs.volume = v.round() as u8;
        cx.save_prefs();
    }

    /// Loads every unlocked channel sorted by number (once), for numbers that aren't in
    /// the zap list.
    fn ensure_all(&mut self, cx: &AppCtx<'_>) {
        if !self.all.is_idle() && self.all.error().is_none() {
            return;
        }
        let Some(session) = cx.session() else { return };
        self.all.start(cx.rt(), &cx.egui, move |_| async move {
            let cat = session.live_catalog().await?;
            Ok(Arc::new(all_by_number(&session, &cat)))
        });
    }

    fn number_match(&self, cx: &AppCtx<'_>, n: u64) -> NumberMatch {
        if let Some(zap) = cx.playback.request().and_then(|r| r.zap.as_ref())
            && let Some((_, c)) = zap.find_number(n)
        {
            return NumberMatch::Current(c.clone());
        }
        match self.all.value() {
            Some(all) => all
                .iter()
                .position(|c| c.number == Some(n))
                .map_or(NumberMatch::Missing, |i| {
                    NumberMatch::All(i, all[i].clone())
                }),
            None if self.all.is_loading() => NumberMatch::Unknown,
            None => NumberMatch::Missing,
        }
    }

    fn zap_number(&mut self, cx: &mut AppCtx<'_>, n: u64) {
        match self.number_match(cx, n) {
            NumberMatch::Current(_) => {
                if cx.playback.zap_to_number(n).is_some() {
                    self.osd_forced = Some(false);
                }
            }
            NumberMatch::All(i, _) => {
                if let Some(all) = self.all.value().cloned()
                    && matches!(cx.playback.zap_to_index(all, i), ZapOutcome::Zapped(_))
                {
                    self.osd_forced = Some(false);
                }
            }
            NumberMatch::Unknown => self.pending_number = Some(n),
            NumberMatch::Missing => cx.toasts.info(format!("There's no channel {n}.")),
        }
    }

    /// The title for the OSD: movies without the provider's tag and "(1981)".
    fn display_title(req: &PlaybackRequest) -> (String, Option<String>) {
        if req.kind == MediaKind::Movie {
            let d = crate::services::vod::display_title(&req.title);
            let sub = req
                .subtitle
                .clone()
                .or_else(|| d.year.map(|y| y.to_string()));
            (d.title.to_owned(), sub)
        } else {
            (req.title.clone(), req.subtitle.clone())
        }
    }
}

impl Screen for PlayerScreen {
    fn chrome(&self) -> Chrome {
        Chrome::Fullscreen
    }

    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        // The OSD shows for a moment on entry.
        self.poke();
        let Some(session) = cx.session() else { return };
        match &self.route {
            PlayerRoute::Request(req) => {
                // Already playing it (Live or Catch-up preview → fullscreen): keep the
                // stream; reopening it would cost a connection and a rebuffer.
                let same = cx
                    .playback
                    .request()
                    .is_some_and(|cur| cur.item.same_item(&req.item));
                let stale = matches!(
                    cx.playback.status(),
                    PlaybackStatus::Failed(_) | PlaybackStatus::Idle | PlaybackStatus::Finished
                );
                if !same || stale {
                    cx.playback.play(session, (**req).clone());
                }
            }
            PlayerRoute::LiveChannel { key } => {
                if self.resolve.is_idle() {
                    let key = key.clone();
                    self.resolve.start(cx.rt(), &cx.egui, move |_| async move {
                        let cat = session.live_catalog().await?;
                        let index = cat
                            .index_of(&key)
                            .or_else(|| {
                                key.parse::<u64>().ok().and_then(|n| {
                                    cat.channels.iter().position(|c| c.number == Some(n))
                                })
                            })
                            .ok_or_else(|| {
                                crate::error::AppError::msg(format!("There's no channel {key}."))
                            })?;
                        // Parental control: never open (or zap into) a locked category.
                        let mut locks = LockFilter::new(&session, &cat);
                        let target = cat.channels[index].clone();
                        if locks.is_locked(&target) {
                            return Err(crate::error::AppError::msg(
                                "This channel is in a locked category. Open it from Live TV \
                                 and enter the PIN.",
                            ));
                        }
                        let channels: Vec<LiveChannel> = cat
                            .channels
                            .iter()
                            .filter(|c| !locks.is_locked(c))
                            .cloned()
                            .collect();
                        let index = channels
                            .iter()
                            .position(|c| c.key == target.key)
                            .unwrap_or(0);
                        let channels = Arc::new(channels);
                        session.live_request(&target, Some(ZapContext { channels, index }))
                    });
                }
            }
        }
    }

    fn on_leave(&mut self, cx: &mut AppCtx<'_>) {
        // Back to the Live preview keeps the stream; anything else stops it.
        let to_live = matches!(cx.next_route, Some(Route::Live { .. }));
        let live_playing = Self::is_live(cx);
        if !(to_live && live_playing) {
            cx.playback.stop();
        }
        let fs = cx.egui.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if fs {
            cx.egui
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if self.number_entry.take().is_some() {
            return true;
        }
        if self.sheet {
            self.sheet = false;
            focus::request(&cx.egui, Id::unique("player.tracks"));
            return true;
        }
        // Esc first stops the next-episode countdown (card or prompt).
        if let Some(next) = cx.playback.up_next_soon()
            && !up_next::is_cancelled(&cx.egui, up_next_id(), next.key)
        {
            up_next::dismiss(&cx.egui, up_next_id(), next.key);
            return true;
        }
        if cx.playback.up_next().is_some() && up_next::cancel(&cx.egui, up_next_id()) {
            return true;
        }
        let paused = cx.playback.state().paused;
        let ended = matches!(
            cx.playback.status(),
            PlaybackStatus::Finished | PlaybackStatus::Failed(_)
        );
        if self.osd_visible(cx) && !paused && !ended {
            self.osd_forced = Some(false);
            return true;
        }
        false
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        // Esc/Backspace are "back", not activity: counting them would re-show the OSD
        // that `on_back` then hides, so the player could never be left while playing.
        let any_input = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, Event::Key { pressed: true, key, .. }
                    if !matches!(key, Key::Escape | Key::Backspace))
            }) || i.pointer.delta() != egui::Vec2::ZERO
        });
        let osd = self.osd_visible(cx);
        let live = Self::is_live(cx);
        let consume = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let shift = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, k));
        let mut zapped = false;

        // Channel keys (PageUp/PageDown, Ctrl+↑/↓, media keys): one convention everywhere.
        if let Some(step) = keys::take_channel_step(ctx, Surface::Player)
            && step != 0
            && live
        {
            self.zap(cx, step.signum());
            zapped = true;
        }

        // Global keys.
        if consume(Key::Space) {
            cx.playback.toggle_pause();
        }
        if consume(Key::M) {
            cx.playback.toggle_mute();
            self.flash(if cx.playback.state().muted {
                "Sound on"
            } else {
                "Muted"
            });
        }
        if consume(Key::F) {
            Self::toggle_fullscreen(cx);
        }
        if consume(Key::I) {
            if osd {
                self.osd_forced = Some(false);
            } else {
                self.poke();
            }
            // Not "activity": that would undo hiding the OSD with I.
            zapped = true;
        }
        if consume(Key::A) {
            cx.playback.cycle_audio();
            self.flash("Audio track changed");
        }
        if consume(Key::S) {
            cx.playback.cycle_subtitles();
            self.flash("Subtitles changed");
        }
        if consume(Key::Plus) || consume(Key::Equals) {
            Self::change_volume(cx, 5.0);
        }
        if consume(Key::Minus) {
            Self::change_volume(cx, -5.0);
        }
        // Digits: channel number entry (live).
        if live {
            let mut typed = false;
            for (k, c) in [
                (Key::Num0, '0'),
                (Key::Num1, '1'),
                (Key::Num2, '2'),
                (Key::Num3, '3'),
                (Key::Num4, '4'),
                (Key::Num5, '5'),
                (Key::Num6, '6'),
                (Key::Num7, '7'),
                (Key::Num8, '8'),
                (Key::Num9, '9'),
            ] {
                if consume(k) {
                    let e = self
                        .number_entry
                        .get_or_insert_with(|| (String::new(), Instant::now()));
                    if e.0.len() < 5 {
                        e.0.push(c);
                    }
                    e.1 = Instant::now();
                    typed = true;
                }
            }
            if typed {
                self.ensure_all(cx);
            }
            if self.number_entry.is_some()
                && consume(Key::Enter)
                && let Some((buf, _)) = self.number_entry.take()
                && let Ok(n) = buf.parse::<u64>()
            {
                self.zap_number(cx, n);
                zapped = true;
            }
        }

        if !osd && !self.sheet {
            // Direct transport with the OSD hidden.
            if consume(Key::Enter) {
                if let Some(next) = cx.playback.up_next_soon()
                    && !up_next::is_cancelled(ctx, up_next_id(), next.key)
                {
                    if cx.playback.play_up_next() {
                        self.poke();
                    }
                } else if live {
                    self.poke();
                } else {
                    cx.playback.toggle_pause();
                    self.poke();
                }
                return;
            }
            let (left, right) = (consume(Key::ArrowLeft), consume(Key::ArrowRight));
            let (sleft, sright) = (shift(Key::ArrowLeft), shift(Key::ArrowRight));
            if live {
                let mut step = 0;
                if left {
                    step -= 1;
                }
                if right {
                    step += 1;
                }
                if consume(Key::ArrowUp) {
                    step += 1;
                }
                if consume(Key::ArrowDown) {
                    step -= 1;
                }
                if step != 0 {
                    self.zap(cx, step);
                    zapped = true;
                }
            } else {
                let step = if left {
                    -10.0
                } else if right {
                    10.0
                } else if sleft {
                    -60.0
                } else if sright {
                    60.0
                } else {
                    0.0
                };
                if step != 0.0 {
                    cx.playback.seek_relative(step);
                    // The OSD appears with the scrubber focused: more arrows keep seeking.
                    focus::request(ctx, scrubber_id());
                    self.seek_flash = Some((seek_label(step), Instant::now()));
                    self.poke();
                }
                if consume(Key::ArrowUp) || consume(Key::ArrowDown) {
                    self.poke();
                }
            }
        } else if focus::is_focused(ctx, scrubber_id()) && !live {
            let step = if consume(Key::ArrowLeft) {
                -10.0
            } else if consume(Key::ArrowRight) {
                10.0
            } else if shift(Key::ArrowLeft) {
                -60.0
            } else if shift(Key::ArrowRight) {
                60.0
            } else {
                0.0
            };
            if step != 0.0 {
                cx.playback.seek_relative(step);
                self.seek_flash = Some((seek_label(step), Instant::now()));
            }
        }
        if any_input && !zapped {
            self.last_activity = Instant::now();
            if self.osd_forced == Some(false) {
                self.osd_forced = None;
            }
        }
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.resolve.poll()
            && let (Some(req), Some(session)) = (self.resolve.take(), cx.session())
        {
            cx.playback.play(session, req);
        }
        if self.all.poll()
            && let Some(n) = self.pending_number.take()
        {
            self.zap_number(cx, n);
        }
        if let Some((buf, at)) = &self.number_entry
            && at.elapsed() >= NUMBER_ENTRY_TIMEOUT
        {
            let n = buf.parse::<u64>().ok();
            self.number_entry = None;
            if let Some(n) = n {
                self.zap_number(cx, n);
            }
        }
        let screen = cx.layout.screen;
        let painter = ui.painter().clone();
        painter.rect_filled(screen, 0.0, color::VIDEO);
        let status = cx.playback.status();
        let req = cx.playback.request().cloned();
        let show_video = req.is_some()
            && !matches!(
                status,
                PlaybackStatus::Finished
                    | PlaybackStatus::Failed(_)
                    | PlaybackStatus::Unavailable(_)
            );
        if show_video {
            cx.playback.paint_video(ui, screen);
        }
        // Click toggles the OSD; double-click toggles window fullscreen.
        let bg = ui.interact(screen, Id::unique("player.bg"), Sense::click());
        if bg.double_clicked() {
            Self::toggle_fullscreen(cx);
        } else if bg.clicked() {
            self.osd_forced = Some(!self.osd_visible(cx));
        }

        // Idle / finished / failed states; the last frame under a scrim while switching.
        let resolve_err = self.resolve.error().cloned();
        let spinner_at = screen.center();
        match (&status, req.as_ref(), resolve_err) {
            (_, None, Some(err)) => {
                self.idle_state(cx, ui, err.title(), &err.user_message(), false)
            }
            (PlaybackStatus::Unavailable(msg), ..) => {
                self.idle_state(cx, ui, "Video unavailable", msg, false)
            }
            (PlaybackStatus::Finished, Some(_), _) => self.idle_state(cx, ui, "Finished", "", true),
            (PlaybackStatus::Failed(msg), Some(_), _) => {
                self.idle_state(cx, ui, "Playback stopped", msg, false)
            }
            (PlaybackStatus::Idle, None, None) if !self.resolve.is_loading() => {
                self.idle_state(cx, ui, "Nothing playing", "", false);
            }
            (PlaybackStatus::Opening | PlaybackStatus::Buffering, ..) | (_, None, None) => {
                painter.rect_filled(screen, 0.0, SWITCH_SCRIM);
                paint::spinner(&painter, spinner_at, 24.0, ui.input(|i| i.time), 3.0);
                painter.text(
                    spinner_at + vec2(0.0, 48.0),
                    Align2::CENTER_CENTER,
                    "Buffering\u{2026}",
                    font::caption(),
                    color::TEXT_2,
                );
                ui.ctx().request_repaint();
            }
            (PlaybackStatus::Reconnecting { .. }, ..) => {
                painter.rect_filled(screen, 0.0, SWITCH_SCRIM);
                paint::spinner(&painter, spinner_at, 24.0, ui.input(|i| i.time), 3.0);
                painter.text(
                    spinner_at + vec2(0.0, 48.0),
                    Align2::CENTER_CENTER,
                    cx.playback
                        .reconnect_message()
                        .unwrap_or_else(|| "Reconnecting\u{2026}".to_owned()),
                    font::caption(),
                    color::TEXT_2,
                );
                if let Some(note) = cx.playback.notice() {
                    text::line(
                        &painter,
                        spinner_at + vec2(0.0, 76.0),
                        Align2::CENTER_CENTER,
                        &note,
                        font::body(),
                        color::WARNING,
                        screen.width() - 96.0,
                    );
                }
                ui.ctx().request_repaint();
            }
            _ => {}
        }

        // The next-episode prompt has its own controls.
        let visible = self.osd_visible(cx) && cx.playback.up_next().is_none();
        let alpha = motion::animate_bool(
            &cx.egui,
            Id::unique("player.osd"),
            visible,
            crate::ui::theme::motion::OSD,
        );
        if alpha > 0.01
            && let Some(req) = &req
        {
            self.osd(cx, ui, req, alpha, &status);
        } else if bg.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::None);
        }
        if visible {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        self.up_next_card(cx, ui, visible);
        self.overlays(cx, ui, visible);
    }
}

fn seek_label(step: f64) -> String {
    if step > 0.0 {
        format!("+{step:.0} s")
    } else {
        format!("\u{2212}{:.0} s", -step)
    }
}

impl PlayerScreen {
    fn idle_state(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        title: &str,
        body: &str,
        finished: bool,
    ) {
        let screen = cx.layout.screen;
        let painter = ui.painter().clone();
        if !assets::paint_cover(
            ui,
            &painter,
            Asset::PlayerIdle,
            screen,
            vec2(0.5, 0.5),
            0,
            Color32::WHITE,
        ) {
            painter.rect_filled(screen, 0.0, color::BG);
        }
        // A finished episode offers the next one, with a countdown.
        if finished && let Some(next) = cx.playback.up_next() {
            let img = cx
                .services
                .images
                .get(next.artwork.as_deref(), ImageSize::Card);
            let action = UpNextPrompt::new(
                up_next_id(),
                next.key,
                &next.code,
                &next.title,
                &next.series,
                &img,
            )
            .show(ui, screen);
            match action {
                UpNextAction::Play => {
                    if cx.playback.play_up_next() {
                        self.poke();
                    }
                }
                UpNextAction::Replay => {
                    cx.playback.replay();
                    self.poke();
                }
                UpNextAction::Back => cx.nav.back(),
                UpNextAction::None => {}
            }
            return;
        }
        let c = screen.center();
        let sym = Rect::from_center_size(c - vec2(0.0, 72.0), vec2(64.0, 64.0));
        assets::paint_contain(
            ui,
            &painter,
            Asset::Symbol,
            sym,
            Color32::WHITE.gamma_multiply(0.6),
        );
        painter.text(
            c,
            Align2::CENTER_CENTER,
            title,
            font::display_sized(28.0),
            color::TEXT,
        );
        if !body.is_empty() {
            text::block(
                &painter,
                c + vec2(-280.0, 28.0),
                body,
                font::body(),
                color::TEXT_2,
                560.0,
                3,
            );
        }
        let row = Rect::from_center_size(c + vec2(0.0, 120.0), vec2(360.0, 52.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        focus::scope(&mut child, Id::unique("player.idle"), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let has_req = cx.playback.request().is_some();
            if finished {
                if Button::primary("Replay")
                    .icon(icons::ARROW_COUNTER_CLOCKWISE)
                    .large()
                    .autofocus()
                    .show(ui)
                    .activated
                {
                    cx.playback.replay();
                    self.poke();
                }
            } else if has_req
                && Button::primary("Try again")
                    .icon(icons::ARROWS_CLOCKWISE)
                    .large()
                    .autofocus()
                    .show(ui)
                    .activated
            {
                cx.playback.retry();
                self.poke();
            }
            if Button::secondary("Back")
                .icon(icons::CARET_LEFT)
                .large()
                .autofocus_if(!has_req)
                .show(ui)
                .activated
            {
                cx.nav.back();
            }
        });
    }

    fn osd(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        req: &PlaybackRequest,
        alpha: f32,
        status: &PlaybackStatus,
    ) {
        let screen = cx.layout.screen;
        let safe = cx.layout.safe_area();
        let tv = cx.layout.tv;
        let painter = ui.painter().clone().with_layer_id(ui.layer_id());
        let mut p = painter.clone();
        p.set_opacity(alpha);
        let mx = if tv {
            safe.left() - screen.left() + 24.0
        } else {
            48.0
        };
        // Finished / failed: the idle state has its own buttons; only the top bar stays.
        let ended = matches!(
            status,
            PlaybackStatus::Finished | PlaybackStatus::Failed(_) | PlaybackStatus::Unavailable(_)
        );
        paint::scrim_osd_top(&p, screen, if tv { 200.0 } else { 160.0 }, 1.0);
        if !ended {
            paint::scrim_osd_bottom(&p, screen, 260.0, 1.0);
        }
        let interactive = alpha > 0.5;
        let st = cx.playback.state();
        let live = req.kind == MediaKind::Live;
        let (title, subtitle) = Self::display_title(req);

        // Top bar.
        let top_y = safe.top() + if tv { 28.0 } else { 40.0 };
        let back_rect = Rect::from_center_size(
            pos2(screen.left() + mx + 24.0, top_y + 4.0),
            vec2(48.0, 48.0),
        );
        let tx = back_rect.right() + 20.0;
        text::line(
            &p,
            pos2(tx, top_y - 6.0),
            Align2::LEFT_CENTER,
            &title,
            font::title(),
            color::TEXT,
            screen.width() * 0.6,
        );
        let sub = match (subtitle, live) {
            (Some(s), _) => Some(s),
            (None, true) => Some(self.live_subtitle(cx, req)),
            _ => None,
        };
        if let Some(s) = sub {
            text::line(
                &p,
                pos2(tx, top_y + 20.0),
                Align2::LEFT_CENTER,
                &s,
                font::caption(),
                color::TEXT_2,
                screen.width() * 0.6,
            );
        }
        let sheet_w = if self.sheet { 360.0 + 24.0 } else { 0.0 };
        let right = screen.right() - mx - sheet_w;
        clock::clock(
            &cx.egui,
            &p,
            pos2(right, top_y),
            Align2::RIGHT_CENTER,
            cx.prefs,
            font::title_sm(),
            color::TEXT,
        );

        // Bottom block.
        let controls_y = safe.bottom() - if tv { 56.0 } else { 80.0 };
        let bar_y = controls_y - 64.0;
        let mut bar = Rect::from_min_max(
            pos2(screen.left() + mx, bar_y - 3.0),
            pos2(right, bar_y + 3.0),
        );
        let dur = st.duration.or(req.duration_hint).unwrap_or(0.0);
        let pos = self.seek_preview.or(st.time_pos).unwrap_or(0.0);
        if ended {
            // Nothing below: no stale pause button or 0:00 scrubber under Finished.
        } else if live {
            self.live_info(cx, &p, req, bar);
        } else {
            // Elapsed and remaining at the ends of the bar (§6.10).
            let el = p.text(
                pos2(bar.left(), bar.center().y),
                Align2::LEFT_CENTER,
                text::timecode(pos),
                font::body(),
                color::TEXT,
            );
            let rem = if dur > 0.0 {
                format!("\u{2212}{}", text::timecode((dur - pos).max(0.0)))
            } else {
                "\u{2212}0:00".to_owned()
            };
            let rr = p.text(
                pos2(bar.right(), bar.center().y),
                Align2::RIGHT_CENTER,
                rem,
                font::body(),
                if dur > 0.0 {
                    color::TEXT
                } else {
                    Color32::TRANSPARENT
                },
            );
            bar.min.x = el.right() + 20.0;
            bar.max.x = rr.left() - 20.0;
        }

        if !interactive {
            return;
        }
        focus::scope(ui, Id::unique("player.osd"), |ui| {
            // Back.
            if IconButton::new(icons::CARET_LEFT)
                .size(48.0, 24.0)
                .id(Id::unique("player.back"))
                .show_at(ui, back_rect)
                .activated
            {
                cx.nav.back();
            }
            if ended {
                return;
            }
            // Scrubber (VOD).
            if !live {
                let hit = bar.expand2(vec2(0.0, 12.0));
                let (resp, f) = focus::focusable(ui, scrubber_id(), hit, FocusOpts::default());
                let h = if f.focused { 10.0 } else { 6.0 };
                let track = Rect::from_center_size(bar.center(), vec2(bar.width(), h));
                let frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
                p.rect_filled(
                    track,
                    CornerRadius::same(radius::XS),
                    Color32::from_white_alpha(51),
                );
                if let Some(cache) = st.demuxer_cache_duration
                    && dur > 0.0
                {
                    let bf = (((pos + cache) / dur) as f32).clamp(0.0, 1.0);
                    p.rect_filled(
                        Rect::from_min_size(track.min, vec2(track.width() * bf, h)),
                        CornerRadius::same(radius::XS),
                        Color32::from_white_alpha(89),
                    );
                }
                p.rect_filled(
                    Rect::from_min_size(track.min, vec2(track.width() * frac.clamp(0.0, 1.0), h)),
                    CornerRadius::same(radius::XS),
                    color::ACCENT,
                );
                let thumb = pos2(
                    track.left() + track.width() * frac.clamp(0.0, 1.0),
                    track.center().y,
                );
                if f.focused {
                    p.circle_filled(thumb, 10.0, color::CREAM);
                    paint::focus_ring(&p, hit, radius::SM, 1.0);
                }
                // "+10 s" just above the thumb.
                if let Some((label, at)) = &self.seek_flash
                    && at.elapsed() < Duration::from_millis(1200)
                {
                    let g = p.layout_no_wrap(label.clone(), font::caption_strong(), color::TEXT);
                    let tip = Rect::from_center_size(
                        pos2(
                            thumb.x.clamp(track.left() + 30.0, track.right() - 30.0),
                            hit.top() - 22.0,
                        ),
                        g.size() + vec2(20.0, 10.0),
                    );
                    p.rect_filled(tip, CornerRadius::same(radius::SM), color::SURFACE_3);
                    p.galley(tip.center() - g.size() / 2.0, g, color::TEXT);
                }
                if (resp.dragged() || resp.clicked())
                    && dur > 0.0
                    && let Some(ptr) = resp.interact_pointer_pos()
                {
                    let t = ((ptr.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * dur;
                    if resp.dragged() {
                        self.seek_preview = Some(t);
                    } else {
                        cx.playback.seek_absolute(t);
                    }
                }
                if resp.drag_stopped()
                    && let Some(t) = self.seek_preview.take()
                {
                    cx.playback.seek_absolute(t);
                }
            }

            // Transport (left).
            let mut x = screen.left() + mx;
            let cy = controls_y;
            if live {
                for (glyph, delta, id) in [
                    (icons::CARET_UP, 1, "player.ch-up"),
                    (icons::CARET_DOWN, -1, "player.ch-down"),
                ] {
                    let r = Rect::from_center_size(pos2(x + 24.0, cy), vec2(48.0, 48.0));
                    if IconButton::new(glyph)
                        .size(48.0, 24.0)
                        .id(Id::unique(id))
                        .show_at(ui, r)
                        .activated
                    {
                        // From the OSD buttons the OSD stays up.
                        let _ = cx.playback.zap(delta);
                    }
                    x += 48.0 + 16.0;
                }
            } else {
                let r = Rect::from_center_size(pos2(x + 24.0, cy), vec2(48.0, 48.0));
                if IconButton::new(icons::ARROW_COUNTER_CLOCKWISE)
                    .size(48.0, 24.0)
                    .caption("10")
                    .id(Id::unique("player.back10"))
                    .show_at(ui, r)
                    .activated
                {
                    cx.playback.seek_relative(-10.0);
                    self.seek_flash = Some((seek_label(-10.0), Instant::now()));
                }
                x += 48.0 + 24.0;
            }
            let pp = Rect::from_center_size(pos2(x + 32.0, cy), vec2(64.0, 64.0));
            let glyph = if st.paused { icons::PLAY } else { icons::PAUSE };
            if IconButton::new(glyph)
                .size(64.0, 28.0)
                .fill(true)
                .primary()
                .autofocus()
                .id(Id::unique("player.playpause"))
                .show_at(ui, pp)
                .activated
            {
                cx.playback.toggle_pause();
            }
            x += 64.0 + 24.0;
            if !live {
                let r = Rect::from_center_size(pos2(x + 24.0, cy), vec2(48.0, 48.0));
                if IconButton::new(icons::ARROW_CLOCKWISE)
                    .size(48.0, 24.0)
                    .caption("10")
                    .id(Id::unique("player.fwd10"))
                    .show_at(ui, r)
                    .activated
                {
                    cx.playback.seek_relative(10.0);
                    self.seek_flash = Some((seek_label(10.0), Instant::now()));
                }
            }

            // Right cluster: tracks, aspect, volume, fullscreen.
            let mut rx = right - 24.0;
            let fs = cx.egui.input(|i| i.viewport().fullscreen.unwrap_or(false));
            let items: [(&str, &str); 4] = [
                (
                    if fs {
                        icons::CORNERS_IN
                    } else {
                        icons::CORNERS_OUT
                    },
                    "player.fullscreen",
                ),
                (
                    if st.muted {
                        icons::SPEAKER_SLASH
                    } else {
                        icons::SPEAKER_HIGH
                    },
                    "player.volume",
                ),
                (icons::FRAME_CORNERS, "player.aspect"),
                (icons::SUBTITLES, "player.tracks"),
            ];
            for (glyph, id) in items {
                let r = Rect::from_center_size(pos2(rx, cy), vec2(48.0, 48.0));
                let mut b = IconButton::new(glyph).size(48.0, 24.0).id(Id::unique(id));
                if id == "player.tracks" && self.sheet {
                    b = b.surface();
                }
                if b.show_at(ui, r).activated {
                    match id {
                        "player.fullscreen" => Self::toggle_fullscreen(cx),
                        "player.volume" => cx.playback.toggle_mute(),
                        "player.aspect" => {
                            let name = cx.playback.cycle_fit();
                            cx.prefs.video_fit = cx.playback.fit_index() as u8;
                            cx.save_prefs();
                            self.flash(name);
                        }
                        _ => {
                            self.sheet = !self.sheet;
                            if self.sheet {
                                focus::request(&cx.egui, Id::unique("player.sheet.first"));
                            }
                        }
                    }
                }
                rx -= 48.0 + 16.0;
            }
        });
        if self.sheet && !ended {
            self.track_sheet(cx, ui);
        }
    }

    /// The live top-bar subtitle: "003 · <programme on now>" (else "003 · Live TV").
    fn live_subtitle(&self, cx: &AppCtx<'_>, req: &PlaybackRequest) -> String {
        let number = match &req.item {
            PlaybackItem::Live {
                number: Some(n), ..
            } => Some(format!("{n:03}")),
            _ => None,
        };
        let programme = match (cx.session(), cx.playback.live_channel()) {
            (Some(s), Some(ch)) => s.now_next(ch, cx.now).1.map(|p| p.title),
            _ => None,
        };
        match (number, programme) {
            (Some(n), Some(p)) => format!("{n} \u{b7} {p}"),
            (Some(n), None) => format!("{n} \u{b7} Live TV"),
            (None, Some(p)) => p,
            (None, None) => "Live TV".to_owned(),
        }
    }

    fn live_info(&self, cx: &AppCtx<'_>, p: &egui::Painter, req: &PlaybackRequest, bar: Rect) {
        // Line 1 (above the bar): LIVE · number + channel · now title; right: next.
        let y = bar.top() - 26.0;
        let badge = Badge::live(p, pos2(bar.left(), y), Align2::LEFT_CENTER);
        let number = match &req.item {
            PlaybackItem::Live {
                number: Some(n), ..
            } => format!("{n:03}  "),
            _ => String::new(),
        };
        let r = text::line(
            p,
            pos2(badge.right() + 12.0, y),
            Align2::LEFT_CENTER,
            &format!("{number}{}", req.title),
            font::body_strong(),
            color::TEXT,
            520.0,
        );
        let mut progress = 0.0;
        if let (Some(session), Some(ch)) = (cx.session(), cx.playback.live_channel()) {
            let (_, now_p, next_p) = session.now_next(ch, cx.now);
            let mut right_edge = bar.right();
            if let Some(n) = next_p {
                let line = format!(
                    "Next \u{b7} {} {}",
                    cx.prefs.format_time(&n.start.with_timezone(&Local)),
                    n.title
                );
                let nr = text::line(
                    p,
                    pos2(bar.right(), y),
                    Align2::RIGHT_CENTER,
                    &line,
                    font::caption(),
                    color::TEXT_2,
                    bar.width() * 0.3,
                );
                right_edge = nr.left() - 24.0;
            }
            if let Some(n) = now_p {
                let line = format!(
                    "{} \u{b7} until {}",
                    n.title,
                    cx.prefs.format_time(&n.stop.with_timezone(&Local))
                );
                text::line(
                    p,
                    pos2(r.right() + 16.0, y),
                    Align2::LEFT_CENTER,
                    &line,
                    font::caption(),
                    color::TEXT_2,
                    (right_edge - r.right() - 16.0).max(40.0),
                );
                progress = n.progress(cx.now);
            }
        }
        let pb = Rect::from_center_size(bar.center(), vec2(bar.width(), 4.0));
        paint::progress_bar(p, pb, progress, color::ACCENT, color::PROGRESS_TRACK);
    }

    fn track_sheet(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let screen = cx.layout.screen;
        let sheet = Rect::from_min_max(
            pos2(screen.right() - 360.0 - 48.0, screen.top() + 48.0),
            pos2(screen.right() - 48.0, screen.bottom() - 48.0),
        );
        let painter = ui.painter().clone();
        paint::shadow(&painter, sheet, radius::LG, elevation::E3);
        painter.rect_filled(
            sheet,
            CornerRadius::same(radius::LG),
            color::SURFACE_1.gamma_multiply(0.96),
        );
        painter.text(
            pos2(sheet.left() + 28.0, sheet.top() + 44.0),
            Align2::LEFT_CENTER,
            "Playback options",
            font::title(),
            color::TEXT,
        );
        let st = cx.playback.state();
        let mut child = ui.new_child(
            egui::UiBuilder::new().max_rect(
                sheet
                    .shrink2(vec2(16.0, 0.0))
                    .with_min_y(sheet.top() + 80.0),
            ),
        );
        focus::scope(&mut child, Id::unique("player.sheet"), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("player.sheet.scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let mut first = true;
                    let mut row = |ui: &mut Ui,
                                   label: String,
                                   detail: String,
                                   selected: bool,
                                   id: Id|
                     -> bool {
                        let (rect, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
                        let fid = if first {
                            Id::unique("player.sheet.first")
                        } else {
                            id
                        };
                        first = false;
                        let (_, f) = focus::focusable(ui, fid, rect, FocusOpts::default());
                        let p = ui.painter().clone();
                        if selected || f.focused {
                            p.rect_filled(rect, CornerRadius::same(radius::SM), color::SURFACE_3);
                        }
                        text::line(
                            &p,
                            pos2(rect.left() + 16.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &label,
                            font::body_strong(),
                            color::TEXT,
                            rect.width() - 140.0,
                        );
                        let mut dx = rect.right() - 16.0;
                        if selected {
                            icons::paint(
                                &p,
                                pos2(dx - 10.0, rect.center().y),
                                icons::CHECK,
                                20.0,
                                Style::Regular,
                                color::ACCENT,
                            );
                            dx -= 32.0;
                        }
                        if !detail.is_empty() {
                            p.text(
                                pos2(dx, rect.center().y),
                                Align2::RIGHT_CENTER,
                                detail,
                                font::caption(),
                                color::TEXT_2,
                            );
                        }
                        if f.focused {
                            paint::focus_ring(&p, rect, radius::SM, 1.0);
                        }
                        f.activated
                    };
                    section(ui, "Audio");
                    let audio: Vec<_> = st.audio_tracks().cloned().collect();
                    if audio.is_empty() {
                        note(ui, "No audio tracks");
                    }
                    for t in &audio {
                        let label = t.lang.clone().unwrap_or_else(|| format!("Track {}", t.id));
                        let detail = t
                            .title
                            .clone()
                            .or_else(|| t.codec.clone())
                            .unwrap_or_default();
                        if row(
                            ui,
                            text::cap(&label, 40).into_owned(),
                            text::cap(&detail, 32).into_owned(),
                            t.selected,
                            Id::unique(("player.audio", t.id)),
                        ) {
                            cx.playback.choose_audio(TrackSelection::Id(t.id));
                        }
                    }
                    section(ui, "Subtitles");
                    let subs: Vec<_> = st.subtitle_tracks().cloned().collect();
                    let none_selected = st.selected(TrackKind::Subtitle).is_none();
                    if row(
                        ui,
                        "Off".into(),
                        String::new(),
                        none_selected,
                        Id::unique("player.sub.off"),
                    ) {
                        cx.playback.choose_subtitle(TrackSelection::Off);
                    }
                    for t in &subs {
                        let label = t.lang.clone().unwrap_or_else(|| format!("Track {}", t.id));
                        let detail = if t.forced {
                            "Forced".to_owned()
                        } else {
                            t.title.clone().unwrap_or_default()
                        };
                        if row(
                            ui,
                            text::cap(&label, 40).into_owned(),
                            text::cap(&detail, 32).into_owned(),
                            t.selected,
                            Id::unique(("player.sub", t.id)),
                        ) {
                            cx.playback.choose_subtitle(TrackSelection::Id(t.id));
                        }
                    }
                    section(ui, "Aspect");
                    let current = cx.playback.fit_index();
                    for (i, (name, _)) in VideoFit::PRESETS.iter().enumerate() {
                        if row(
                            ui,
                            (*name).to_owned(),
                            String::new(),
                            i == current,
                            Id::unique(("player.fit", i)),
                        ) {
                            cx.playback.set_fit_index(i);
                            cx.prefs.video_fit = i as u8;
                            cx.save_prefs();
                        }
                    }
                });
        });
    }

    /// The "Up next" card over the video during an episode's last seconds.
    fn up_next_card(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, osd: bool) {
        let Some(next) = cx.playback.up_next_soon() else {
            return;
        };
        let Some(remaining) = next.remaining else {
            return;
        };
        let safe = cx.layout.safe_area();
        let bottom = if osd {
            safe.bottom() - if cx.layout.tv { 56.0 } else { 80.0 } - 112.0
        } else {
            safe.bottom() - 48.0
        };
        let img = cx
            .services
            .images
            .get(next.artwork.as_deref(), ImageSize::Card);
        let action = UpNextCard::new(
            up_next_id(),
            next.key,
            &next.code,
            &next.title,
            &img,
            remaining,
            UP_NEXT_EARLY_SECS,
        )
        .show(ui, pos2(safe.right() - 48.0, bottom));
        match action {
            UpNextCardAction::Play => {
                if cx.playback.play_up_next() {
                    self.poke();
                }
            }
            UpNextCardAction::Dismiss | UpNextCardAction::None => {}
        }
    }

    fn overlays(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, osd: bool) {
        let screen = cx.layout.screen;
        let safe = cx.layout.safe_area();
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::unique("player.overlays"),
        ));
        // Zap banner (the OSD shows the same while it is up).
        if !osd {
            let mx = if cx.layout.tv { 24.0 } else { 48.0 };
            zap_banner::show(
                cx,
                ui,
                &painter,
                pos2(safe.left() + mx, safe.bottom() - mx),
                (screen.width() * 0.5).max(420.0),
                false,
            );
        }
        if let Some((buf, _)) = &self.number_entry {
            self.number_box(cx, ui, &painter, buf, screen);
        }
        if let Some((msg, at)) = &self.flash {
            let age = at.elapsed().as_secs_f32();
            if age < 1.5 {
                let a = (1.5 - age).min(0.3) / 0.3;
                let g = painter.layout_no_wrap(msg.clone(), font::title_sm(), color::TEXT);
                let r = Rect::from_center_size(
                    pos2(screen.center().x, screen.top() + 120.0),
                    g.size() + vec2(32.0, 20.0),
                );
                painter.rect_filled(
                    r,
                    CornerRadius::same(radius::MD),
                    color::SURFACE_3.gamma_multiply(0.9 * a),
                );
                painter.galley(
                    r.center() - g.size() / 2.0,
                    g,
                    color::TEXT.gamma_multiply(a),
                );
                ui.ctx().request_repaint();
            } else {
                self.flash = None;
            }
        }
        if self
            .seek_flash
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < Duration::from_millis(1200))
        {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn number_box(
        &self,
        cx: &AppCtx<'_>,
        ui: &Ui,
        painter: &egui::Painter,
        buf: &str,
        screen: Rect,
    ) {
        let target = buf.parse::<u64>().ok().map(|n| self.number_match(cx, n));
        paint_number_box(
            cx,
            ui,
            painter,
            pos2(screen.right() - 48.0, screen.top() + 96.0),
            buf,
            target.as_ref(),
        );
    }
}

/// The channel-number entry box (top-right corner at `top_right`): the digits, then the
/// channel they lead to (logo + "012 · Name") or "Channel not found".
pub(crate) fn paint_number_box(
    cx: &AppCtx<'_>,
    ui: &Ui,
    painter: &egui::Painter,
    top_right: egui::Pos2,
    buf: &str,
    target: Option<&NumberMatch>,
) {
    let g = painter.layout_no_wrap(buf.to_owned(), font::numeric_xl(), color::TEXT);
    let w = 320.0_f32.max(g.size().x + 48.0);
    let r = Rect::from_min_size(top_right - vec2(w, 0.0), vec2(w, 148.0));
    paint::shadow(painter, r, radius::LG, elevation::E2);
    painter.rect_filled(r, CornerRadius::same(radius::LG), color::SURFACE_1);
    painter.galley(
        pos2(r.center().x - g.size().x / 2.0, r.top() + 12.0),
        g,
        color::TEXT,
    );
    let y = r.bottom() - 32.0;
    match target {
        Some(NumberMatch::Current(ch) | NumberMatch::All(_, ch)) => {
            let logo = Rect::from_min_size(pos2(r.left() + 20.0, y - 15.0), vec2(48.0, 30.0));
            let slot = cx.services.images.get(ch.logo.as_deref(), ImageSize::Logo);
            ChannelLogo::paint(ui, painter, logo, &ch.name, &slot);
            let label = match ch.number {
                Some(n) => format!("{n:03} \u{b7} {}", ch.name),
                None => ch.name.clone(),
            };
            text::line(
                painter,
                pos2(logo.right() + 12.0, y),
                Align2::LEFT_CENTER,
                &label,
                font::body_strong(),
                color::TEXT,
                r.right() - logo.right() - 32.0,
            );
        }
        Some(NumberMatch::Missing) => {
            painter.text(
                pos2(r.center().x, y),
                Align2::CENTER_CENTER,
                "Channel not found",
                font::body(),
                color::TEXT_3,
            );
        }
        Some(NumberMatch::Unknown) | None => {}
    }
    ui.ctx().request_repaint_after(Duration::from_millis(200));
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    text::overline(
        ui.painter(),
        pos2(r.left() + 16.0, r.center().y),
        Align2::LEFT_CENTER,
        title,
        color::TEXT_2,
    );
}

fn note(ui: &mut Ui, s: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    ui.painter().text(
        pos2(r.left() + 16.0, r.center().y),
        Align2::LEFT_CENTER,
        s,
        font::caption(),
        color::TEXT_3,
    );
}

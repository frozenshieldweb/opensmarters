// SPDX-License-Identifier: GPL-3.0-or-later
//! Multiscreen (`Route::Multiscreen`): two to four live channels at once, as many as the
//! connection budget allows. The tile players and their rules (budget, staggered starts,
//! sound) live in [`crate::playback::multi`] (`cx.multi`); this screen draws them and
//! handles the keys. Layouts: 1×2 for two slots, 2×2 for three or four, and "enlarged"
//! (one tile big, the others in a column).
//!
//! Opened with [`open`] from Live TV ([`live_button`], next to Fullscreen/Guide) and the
//! player's OSD ([`osd_button`]): the main player stops, freeing its connection, and its
//! channel goes to tile 1. With a budget of one stream it doesn't open and explains why.
//!
//! ## Keys
//!
//! | Key | Action |
//! |---|---|
//! | Arrows | move between the tiles (the sound follows) and the header buttons |
//! | Enter | empty slot: choose a channel · playing tile: enlarge it / back to all tiles · stopped or failed tile: play it again |
//! | PageUp / PageDown | channel up / down in the focused tile |
//! | 0–9 | channel number for the focused tile (after 1.5 s or Enter); on the empty slot it adds that channel |
//! | M / F / I | mute / window fullscreen / show or hide the channel banners |
//! | Esc, Backspace | close the picker, then leave the enlarged view, then leave multiscreen (every tile stops and frees its connection) |

mod layout;
mod picker;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Event, Id, Key, Modifiers, Rect, Stroke, StrokeKind, Ui,
    pos2, vec2,
};
use opensmarters_core::chrono::Local;

use super::{AppCtx, Chrome, Screen, shell};
use crate::focus::{self, Focus, FocusOpts};
use crate::nav::{Route, Section};
use crate::playback::PlaybackStatus;
use crate::playback::multi::{self, TileSettings};
use crate::services::Session;
use crate::services::catalog::{LiveCatalog, LiveChannel};
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::{Button, EmptyState, IconButton, ToastKind};
use crate::ui::{motion, paint, text};

use picker::{NUMBER_ENTRY_TIMEOUT, Picker, push_digits, take_digits};

/// Channel banners stay this long after a key press or a channel change.
const BANNER_FOR: Duration = Duration::from_secs(3);

fn tile_id(slot: usize) -> Id {
    Id::unique(("multi.tile", slot))
}

/// Opens multiscreen: the main player stops (freeing its connection) and its live channel
/// goes to tile 1. When the connection budget allows fewer than two streams, it explains
/// why instead and stays where it is. `replace` replaces the current screen (the
/// fullscreen player) instead of pushing, so Esc returns to what was below it.
pub fn open(cx: &mut AppCtx<'_>, replace: bool) {
    let Some(session) = cx.session() else { return };
    let budget = session.connections();
    if let Err(refusal) = multi::capacity(budget.limit(), &budget.holders()) {
        tracing::info!(?refusal, "multiscreen not opened");
        cx.toasts.push(
            ToastKind::Warning,
            refusal.message(),
            Some("multiscreen.refused"),
        );
        return;
    }
    let carried = cx.playback.request().filter(|r| r.kind.is_live()).cloned();
    // Free the main player's connection before the tiles take theirs.
    cx.playback.stop();
    cx.multi.set_carried(carried.map(|r| (session, r)));
    if replace {
        cx.nav.replace(Route::Multiscreen);
    } else {
        cx.nav.push(Route::Multiscreen);
    }
}

/// The Live TV preview column's "Multiscreen" button: labelled on wide layouts, an icon
/// button where the column is narrow (medium, TV).
pub fn live_button(cx: &mut AppCtx<'_>, ui: &mut Ui) {
    let id = Id::unique("live.multiscreen");
    let f = if cx.layout.is_wide() {
        Button::secondary("Multiscreen")
            .icon(icons::SQUARES_FOUR)
            .id(id)
            .show(ui)
    } else {
        IconButton::new(icons::SQUARES_FOUR)
            .surface()
            .id(id)
            .show(ui)
    };
    if f.activated {
        open(cx, false);
    }
}

/// The player OSD's "Multiscreen" button (live only): opens it with this channel in
/// tile 1.
pub fn osd_button(cx: &mut AppCtx<'_>, ui: &mut Ui, rect: Rect) {
    if IconButton::new(icons::SQUARES_FOUR)
        .size(48.0, 24.0)
        .id(Id::unique("player.multiscreen"))
        .show_at(ui, rect)
        .activated
    {
        open(cx, true);
    }
}

/// What Esc/Backspace does, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Back {
    ClosePicker,
    CancelNumber,
    LeaveEnlarged,
    Leave,
}

fn back_step(picker: bool, number: bool, enlarged: bool) -> Back {
    if picker {
        Back::ClosePicker
    } else if number {
        Back::CancelNumber
    } else if enlarged {
        Back::LeaveEnlarged
    } else {
        Back::Leave
    }
}

/// What Enter on a slot does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activate {
    Pick,
    Restart,
    ToggleEnlarged,
}

fn activate_action(empty: bool, stopped: bool, status: Option<&PlaybackStatus>) -> Activate {
    if empty {
        Activate::Pick
    } else if stopped
        || matches!(
            status,
            Some(PlaybackStatus::Failed(_) | PlaybackStatus::Finished)
        )
    {
        Activate::Restart
    } else {
        Activate::ToggleEnlarged
    }
}

fn tile_settings(cx: &AppCtx<'_>) -> TileSettings {
    TileSettings {
        egui: cx.egui.clone(),
        redactor: cx.services.redactor.clone(),
        fit_index: u8::try_from(cx.playback.fit_index()).unwrap_or(0),
        volume: cx.prefs.volume,
        hw_decoding: cx.playback.tuning().hw_decoding,
        audio_language: cx.prefs.audio_language.clone(),
    }
}

fn toggle_fullscreen(cx: &AppCtx<'_>) {
    let fs = cx.egui.input(|i| i.viewport().fullscreen.unwrap_or(false));
    cx.egui
        .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
}

/// The multiscreen screen.
#[derive(Debug)]
pub struct MultiscreenScreen {
    catalog: Loader<Arc<LiveCatalog>>,
    /// All channels (number entry outside a tile's list), per catalogue generation.
    all: Option<(u64, Arc<Vec<LiveChannel>>)>,
    picker: Option<Picker>,
    enlarged: Option<usize>,
    /// The focused slot (kept while the focus is on a header button).
    focused: usize,
    focus_request: Option<usize>,
    number_entry: Option<(String, Instant)>,
    last_activity: Instant,
    banners_forced: Option<bool>,
    entered: bool,
}

impl Default for MultiscreenScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiscreenScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {
            catalog: Loader::new(),
            all: None,
            picker: None,
            enlarged: None,
            focused: 0,
            focus_request: None,
            number_entry: None,
            last_activity: Instant::now(),
            banners_forced: None,
            entered: false,
        }
    }

    fn load(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        self.catalog.start(cx.rt(), &cx.egui, move |_| async move {
            session.live_catalog().await
        });
    }

    fn all_channels(&mut self) -> Option<Arc<Vec<LiveChannel>>> {
        let cat = self.catalog.value()?;
        let generation = self.catalog.generation();
        if self.all.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.all = Some((generation, Arc::new(cat.channels.clone())));
        }
        self.all.as_ref().map(|(_, a)| Arc::clone(a))
    }

    /// Plays `channel` in `slot` (a new tile on the empty slot).
    fn place(
        &mut self,
        cx: &mut AppCtx<'_>,
        session: &Arc<Session>,
        slot: usize,
        channel: &LiveChannel,
        zap: crate::playback::ZapContext,
    ) {
        match session.live_request(channel, Some(zap)) {
            Ok(req) => match cx.multi.place(slot, Arc::clone(session), req) {
                Some(i) => {
                    self.focused = i;
                    self.focus_request = Some(i);
                    cx.multi.focus(i);
                }
                None => cx
                    .toasts
                    .info("Multiscreen shows at most four channels at once."),
            },
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    /// A typed channel number for the focused slot: the tile's own list first, else all
    /// channels.
    fn commit_number(&mut self, cx: &mut AppCtx<'_>, session: &Arc<Session>, number: u64) {
        let slot = self.focused;
        if slot < cx.multi.len() && cx.multi.zap_to_number(slot, number).is_some() {
            return;
        }
        let (Some(cat), Some(all)) = (self.catalog.value().cloned(), self.all_channels()) else {
            cx.toasts.info("The channel list is still loading.");
            return;
        };
        let Some(i) = all.iter().position(|c| c.number == Some(number)) else {
            cx.toasts.info(format!("There's no channel {number}."));
            return;
        };
        if picker::is_locked(session, &cat, &all[i]) {
            cx.toasts.info(format!(
                "Channel {number} is in a locked category. Open it in Live TV with your PIN \
                 first."
            ));
            return;
        }
        let ch = all[i].clone();
        self.place(
            cx,
            session,
            slot,
            &ch,
            crate::playback::ZapContext {
                channels: all,
                index: i,
            },
        );
    }

    fn open_picker(&mut self, cx: &AppCtx<'_>, slot: usize) {
        self.picker = Some(Picker::new(slot, cx.multi.channel_key(slot)));
        self.number_entry = None;
    }
}

impl Screen for MultiscreenScreen {
    fn chrome(&self) -> Chrome {
        Chrome::Fullscreen
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Live)
    }

    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if self.catalog.is_idle() || self.catalog.error().is_some() {
            self.load(cx);
        }
        cx.multi.configure(tile_settings(cx));
        self.last_activity = Instant::now();
        if self.entered {
            // Back from a screen that covered this one: play the tiles again.
            cx.multi.resume();
        } else {
            // A new multiscreen: forget old tiles, start with the handed-over channel.
            self.entered = true;
            let carried = cx.multi.take_carried();
            cx.multi.clear();
            if let (Some((session, req)), Some(current)) = (carried, cx.session())
                && Arc::ptr_eq(&session, &current)
            {
                cx.multi.place(0, session, req);
            }
            self.focused = 0;
        }
        self.focus_request = Some(self.focused);
    }

    fn on_leave(&mut self, cx: &mut AppCtx<'_>) {
        // Every tile stops and frees its connection; the idle players are kept for reuse.
        cx.multi.stop_all();
        if let Some(s) = cx.session() {
            let budget = s.connections();
            tracing::info!(
                in_use = budget.in_use(),
                limit = budget.limit(),
                "multiscreen left; its tiles stopped"
            );
        }
        self.picker = None;
        self.number_entry = None;
        let fs = cx.egui.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if fs {
            cx.egui
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        self.last_activity = Instant::now();
        match back_step(
            self.picker.is_some(),
            self.number_entry.is_some(),
            self.enlarged.is_some(),
        ) {
            Back::ClosePicker => {
                if let Some(p) = &mut self.picker
                    && p.on_back()
                {
                    return true;
                }
                if let Some(p) = self.picker.take() {
                    self.focus_request = Some(p.slot());
                }
                true
            }
            Back::CancelNumber => {
                self.number_entry = None;
                true
            }
            Back::LeaveEnlarged => {
                self.enlarged = None;
                focus::request(&cx.egui, tile_id(self.focused));
                true
            }
            Back::Leave => false,
        }
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        let any_key = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, Event::Key { pressed: true, key, .. }
                    if !matches!(key, Key::Escape | Key::Backspace))
            })
        });
        if any_key {
            self.last_activity = Instant::now();
            if self.banners_forced == Some(false) {
                self.banners_forced = None;
            }
        }
        if let Some(p) = &mut self.picker {
            p.on_input(ctx);
            return;
        }
        let consume = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        if consume(Key::F) {
            toggle_fullscreen(cx);
        }
        if consume(Key::M) {
            let muted = cx.multi.toggle_mute();
            cx.toasts.push(
                ToastKind::Info,
                if muted { "Muted" } else { "Sound on" },
                Some("multi.mute"),
            );
        }
        if consume(Key::I) {
            let shown = self.banners_forced.unwrap_or(true);
            self.banners_forced = Some(!shown);
        }
        let slot = self.focused;
        let playing = slot < cx.multi.len();
        // PageUp/PageDown zap the focused tile (channel up = next in its list).
        if consume(Key::PageUp) && playing {
            cx.multi.zap(slot, 1);
        }
        if consume(Key::PageDown) && playing {
            cx.multi.zap(slot, -1);
        }
        push_digits(&mut self.number_entry, &take_digits(ctx));
        if self.number_entry.is_some()
            && consume(Key::Enter)
            && let Some((buf, _)) = self.number_entry.take()
            && let Ok(n) = buf.parse::<u64>()
            && let Some(session) = cx.session()
        {
            self.commit_number(cx, &session, n);
        }
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.catalog.poll();
        let Some(session) = cx.session() else { return };
        cx.multi.update(Some(&session));
        let all = self.all_channels();
        if let (Some(cat), Some(all)) = (self.catalog.value(), &all) {
            cx.multi.resolve_channels(cat, all);
        }
        if let Some((buf, at)) = &self.number_entry
            && at.elapsed() >= NUMBER_ENTRY_TIMEOUT
        {
            let n = buf.parse::<u64>().ok();
            self.number_entry = None;
            if let Some(n) = n {
                self.commit_number(cx, &session, n);
            }
        }

        let budget = Arc::clone(session.connections());
        let capacity = multi::capacity(budget.limit(), &budget.holders());
        let tiles = cx.multi.len();
        let slots = multi::slot_count(tiles, capacity.as_ref().map_or(0, |c| *c));
        let screen = cx.layout.screen;
        let safe = cx.layout.safe_area();
        let m = cx.layout.margin;
        let left = safe.left().max(screen.left() + m);
        let right = safe.right().min(screen.right() - m);
        let bottom = safe.bottom().min(screen.bottom());
        let painter = ui.painter().clone();

        // Header: title, layout and connections; the clock and warnings on the right.
        let hy = screen.top() + 56.0;
        let cluster = shell::status_cluster(cx, ui);
        let tr = text::line(
            &painter,
            pos2(left, hy),
            Align2::LEFT_CENTER,
            "Multiscreen",
            font::title(),
            color::TEXT,
            320.0,
        );
        let mut meta_right = tr.right();
        if slots > 0 {
            let used = budget.in_use();
            let limit = budget.limit();
            meta_right = painter
                .text(
                    pos2(tr.right() + 16.0, hy + 1.0),
                    Align2::LEFT_CENTER,
                    format!(
                        "{} \u{b7} {used} of {limit} connection{} in use",
                        layout::label(slots),
                        if limit == 1 { "" } else { "s" }
                    ),
                    font::caption(),
                    color::TEXT_3,
                )
                .right();
        }

        if slots == 0 {
            // Opened directly (`--route multiscreen`) on a budget that doesn't allow it.
            let refusal = capacity.err().unwrap_or(multi::Refusal::OneStream);
            let area = Rect::from_min_max(pos2(left, screen.top() + 104.0), pos2(right, bottom));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            let back = EmptyState::new(
                Id::unique("multi.refused"),
                Illustration::EmptyGuide,
                refusal.title(),
                &refusal.message(),
            )
            .action("Back", true)
            .show(&mut child);
            if back.is_some_and(|f| f.activated) {
                cx.nav.back();
            }
            return;
        }
        self.focused = self.focused.min(slots - 1);
        if self.enlarged.is_some_and(|e| e >= tiles) {
            self.enlarged = None;
        }
        if let Some(s) = self.focus_request.take() {
            focus::request(&cx.egui, tile_id(s.min(slots - 1)));
        }

        // Header actions for the focused tile.
        let focused_tile = self.focused < tiles;
        let actions_right = cluster.left() - 24.0;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2((meta_right + 24.0).min(actions_right), hy - 22.0),
                    pos2(actions_right, hy + 22.0),
                ))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let (change, remove) = focus::scope(&mut child, Id::unique("multi.actions"), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let remove = Button::ghost("Remove")
                .icon(icons::X)
                .small()
                .enabled(focused_tile)
                .id(Id::unique("multi.remove"))
                .show(ui)
                .activated;
            let change = Button::secondary("Change channel")
                .icon(icons::LIST_BULLETS)
                .small()
                .id(Id::unique("multi.change"))
                .show(ui)
                .activated;
            (change, remove)
        });
        if change {
            self.open_picker(cx, self.focused);
        }
        if remove && focused_tile {
            let slot = self.focused;
            cx.multi.remove(slot);
            self.enlarged = None;
            self.focused = multi::after_remove(slot, slot, cx.multi.len());
            self.focus_request = Some(self.focused);
        }

        // Tiles.
        let area = Rect::from_min_max(pos2(left, screen.top() + 104.0), pos2(right, bottom - 56.0));
        let tiles = cx.multi.len();
        let slots = multi::slot_count(tiles, capacity.as_ref().map_or(0, |c| *c));
        let rects = layout::tile_rects(area, slots, self.enlarged);
        let recent = self.last_activity.elapsed() < BANNER_FOR;
        let banners = self.banners_forced.unwrap_or(recent);
        let header_focused = focus::is_in_scope(&cx.egui, Id::unique("multi.actions"));
        let mut activated: Option<usize> = None;
        focus::scope(ui, Id::unique("multi.tiles"), |ui| {
            for (i, rect) in rects.iter().enumerate() {
                let (resp, f) = focus::focusable(
                    ui,
                    tile_id(i),
                    *rect,
                    FocusOpts::default().autofocus_if(i == self.focused),
                );
                if f.focused {
                    if self.focused != i {
                        self.last_activity = Instant::now();
                    }
                    self.focused = i;
                    cx.multi.focus(i);
                }
                let shown = banners
                    || cx
                        .multi
                        .changed_at(i)
                        .is_some_and(|t| t.elapsed() < BANNER_FOR);
                let alpha = motion::animate_bool(
                    &cx.egui,
                    tile_id(i).with("banner"),
                    shown,
                    crate::ui::theme::motion::OSD,
                );
                let selected = header_focused && i == self.focused;
                paint_tile(cx, ui, &session, i, *rect, &f, alpha, selected);
                if f.activated || resp.double_clicked() {
                    activated = Some(i);
                }
            }
        });
        if let (Some(cell), Ok(&3)) = (
            layout::spare_cell(area, slots).filter(|_| self.enlarged.is_none()),
            capacity.as_ref(),
        ) {
            painter.rect_stroke(
                cell,
                CornerRadius::same(radius::MD),
                Stroke::new(1.0, color::STROKE_SUBTLE),
                StrokeKind::Inside,
            );
            let g = text::galley(
                &painter,
                "Your connections allow three streams at once.",
                font::body(),
                color::TEXT_3,
                cell.width() - 64.0,
                2,
            );
            painter.galley(cell.center() - g.size() / 2.0, g, color::TEXT_3);
        }
        if let Some(i) = activated {
            let status = cx.multi.status(i);
            match activate_action(i >= tiles, cx.multi.is_stopped(i), status.as_ref()) {
                Activate::Pick => self.open_picker(cx, i),
                Activate::Restart => cx.multi.restart(i),
                Activate::ToggleEnlarged => {
                    self.enlarged = if self.enlarged == Some(i) {
                        None
                    } else {
                        Some(i)
                    };
                    self.focus_request = Some(i);
                }
            }
        }
        if banners || recent {
            cx.egui.request_repaint_after(Duration::from_millis(250));
        }

        // Footer: what Enter does here, and which tile has the sound.
        let fy = bottom - 28.0;
        icons::paint(
            &painter,
            pos2(left + 9.0, fy),
            icons::INFO,
            16.0,
            Style::Regular,
            color::TEXT_3,
        );
        let hint = if self.focused >= tiles {
            "Enter: choose a channel \u{b7} Type a number to add it \u{b7} Esc: leave multiscreen"
        } else if self.enlarged.is_some() {
            "Enter: back to all tiles \u{b7} PageUp/PageDown or a number: change channel \u{b7} \
             M: mute \u{b7} F: full screen \u{b7} Esc: back to all tiles"
        } else {
            "Enter: enlarge \u{b7} PageUp/PageDown or a number: change channel \u{b7} M: mute \
             \u{b7} F: full screen \u{b7} Esc: leave multiscreen"
        };
        let sound = match cx.multi.audible() {
            _ if cx.multi.is_muted() => Some((icons::SPEAKER_SLASH, "Muted".to_owned())),
            Some(i) => cx
                .multi
                .request(i)
                .map(|r| (icons::SPEAKER_HIGH, format!("Sound: {}", r.title))),
            None => None,
        };
        let mut hint_w = right - left - 28.0;
        if let Some((glyph, label)) = sound {
            let r = text::line(
                &painter,
                pos2(right, fy),
                Align2::RIGHT_CENTER,
                &label,
                font::caption(),
                color::TEXT_2,
                (right - left) * 0.3,
            );
            icons::paint(
                &painter,
                pos2(r.left() - 14.0, fy),
                glyph,
                16.0,
                Style::Regular,
                color::TEXT_2,
            );
            hint_w = r.left() - 40.0 - left - 28.0;
        }
        text::line(
            &painter,
            pos2(left + 28.0, fy),
            Align2::LEFT_CENTER,
            hint,
            font::caption(),
            color::TEXT_3,
            hint_w,
        );

        // Channel-number entry (top-right, like Live TV).
        if let Some((buf, _)) = &self.number_entry {
            let p = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                Id::unique("multi.number"),
            ));
            let g = p.layout_no_wrap(buf.clone(), font::numeric_xl(), color::TEXT);
            let w = (g.size().x + 48.0).max(140.0);
            let r = Rect::from_min_size(pos2(right - w, screen.top() + 104.0), vec2(w, 100.0));
            paint::shadow(&p, r, radius::LG, crate::ui::theme::elevation::E2);
            p.rect_filled(r, CornerRadius::same(radius::LG), color::SURFACE_1);
            p.galley(r.center() - g.size() / 2.0, g, color::TEXT);
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }

        // The channel picker (modal).
        if let Some(mut p) = self.picker.take() {
            let keys: Vec<Option<String>> = (0..tiles)
                .map(|i| cx.multi.channel_key(i).map(str::to_owned))
                .collect();
            let cat = self.catalog.value().cloned();
            let generation = self.catalog.generation();
            let data = match (&cat, &all) {
                (Some(catalog), Some(all)) => Some(picker::Channels {
                    catalog,
                    generation,
                    all,
                }),
                _ => None,
            };
            match p.show(cx, &session, data, &keys) {
                Some((channel, zap)) => {
                    let slot = p.slot();
                    self.place(cx, &session, slot, &channel, zap);
                }
                None => self.picker = Some(p),
            }
        }
    }
}

/// Paints one slot: an empty "add a channel" slot, or a tile (video, status, banner,
/// sound icon), with the focus treatment.
#[allow(clippy::too_many_arguments)]
fn paint_tile(
    cx: &mut AppCtx<'_>,
    ui: &Ui,
    session: &Arc<Session>,
    slot: usize,
    rect: Rect,
    f: &Focus,
    banner: f32,
    selected: bool,
) {
    let painter = ui.painter().clone();
    if f.anim > 0.0 {
        paint::focus_glow(&painter, rect, radius::MD, f.anim);
    }
    let c = rect.center();
    if slot >= cx.multi.len() {
        painter.rect_filled(
            rect,
            CornerRadius::same(radius::MD),
            if f.focused {
                color::SURFACE_2
            } else {
                color::SURFACE_1
            },
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius::MD),
            Stroke::new(1.5, color::STROKE),
            StrokeKind::Inside,
        );
        let fg = if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        painter.circle_filled(c - vec2(0.0, 28.0), 28.0, color::SURFACE_3);
        icons::paint(
            &painter,
            c - vec2(0.0, 28.0),
            icons::PLUS,
            24.0,
            Style::Regular,
            fg,
        );
        painter.text(
            c + vec2(0.0, 22.0),
            Align2::CENTER_CENTER,
            "Add a channel",
            font::title_sm(),
            fg,
        );
        text::line(
            &painter,
            c + vec2(0.0, 48.0),
            Align2::CENTER_CENTER,
            "Press Enter to choose one, or type its number",
            font::caption(),
            color::TEXT_3,
            rect.width() - 32.0,
        );
        if f.focused {
            paint::focus_ring(&painter, rect, radius::MD, 1.0);
        }
        return;
    }

    painter.rect_filled(rect, CornerRadius::same(radius::MD), color::VIDEO);
    let status = cx.multi.status(slot);
    let stopped = cx.multi.is_stopped(slot);
    let show_video = !stopped
        && !matches!(
            status,
            Some(
                PlaybackStatus::Finished
                    | PlaybackStatus::Failed(_)
                    | PlaybackStatus::Unavailable(_)
            )
        );
    if show_video {
        cx.multi.paint_video(slot, ui, rect);
    }
    let time = ui.input(|i| i.time);
    let message = |title: &str, body: &str, action: Option<&str>| {
        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::BG_DEEP);
        let w = rect.width() - 48.0;
        let mut y = c.y - 34.0;
        let r = text::line(
            &painter,
            pos2(c.x, y),
            Align2::CENTER_CENTER,
            title,
            font::title_sm(),
            color::TEXT,
            w,
        );
        y = r.bottom() + 8.0;
        if !body.is_empty() {
            let g = text::galley(&painter, body, font::caption(), color::TEXT_2, w, 3);
            let r = Rect::from_min_size(pos2(c.x - g.size().x / 2.0, y), g.size());
            painter.galley(r.min, g, color::TEXT_2);
            y = r.bottom() + 10.0;
        }
        if let Some(a) = action {
            text::line(
                &painter,
                pos2(c.x, y + 8.0),
                Align2::CENTER_CENTER,
                a,
                font::caption_strong(),
                color::ACCENT,
                w,
            );
        }
    };
    match &status {
        _ if stopped => message("Stopped", "", Some("Press Enter to play it again")),
        Some(PlaybackStatus::Failed(msg)) => {
            message("Not playing", msg, Some("Press Enter to try again"));
        }
        Some(PlaybackStatus::Unavailable(msg)) => message("Video unavailable", msg, None),
        Some(PlaybackStatus::Opening | PlaybackStatus::Buffering) => {
            paint::spinner(&painter, c, 20.0, time, 3.0);
            ui.ctx().request_repaint();
        }
        Some(PlaybackStatus::Reconnecting { attempt }) => {
            let plate = Rect::from_center_size(c + vec2(0.0, 8.0), vec2(220.0, 96.0));
            painter.rect_filled(
                plate,
                CornerRadius::same(radius::MD),
                Color32::from_black_alpha(170),
            );
            paint::spinner(&painter, c - vec2(0.0, 12.0), 18.0, time, 3.0);
            painter.text(
                c + vec2(0.0, 26.0),
                Align2::CENTER_CENTER,
                format!("Reconnecting\u{2026} ({attempt})"),
                font::caption(),
                color::TEXT_2,
            );
            ui.ctx().request_repaint();
        }
        _ => {}
    }
    if let Some(note) = cx
        .multi
        .notice(slot)
        .filter(|_| matches!(status, Some(PlaybackStatus::Reconnecting { .. })))
    {
        let g = text::galley(
            &painter,
            &note,
            font::caption(),
            color::WARNING,
            rect.width() - 48.0,
            2,
        );
        let r = Rect::from_min_size(pos2(c.x - g.size().x / 2.0, c.y + 64.0), g.size());
        painter.rect_filled(
            r.expand(6.0),
            CornerRadius::same(radius::SM),
            Color32::from_black_alpha(170),
        );
        painter.galley(r.min, g, color::WARNING);
    }

    // Banner: number + name, the programme on now and its progress.
    if banner > 0.01 {
        let mut p = painter.clone();
        p.set_opacity(banner);
        // Text never sits on raw video: a scrim that is already dark where the name is.
        let h = (rect.height() * 0.5).clamp(80.0, 136.0);
        let band = Rect::from_min_max(pos2(rect.left(), rect.bottom() - h), rect.max);
        paint::v_gradient(
            &p,
            band,
            &[
                (0.0, Color32::from_black_alpha(0)),
                (0.35, Color32::from_black_alpha(170)),
                (1.0, Color32::from_black_alpha(225)),
            ],
        );
        let x = rect.left() + 16.0;
        let w = rect.width() - 32.0;
        let req = cx.multi.request(slot);
        let number = match req.map(|r| &r.item) {
            Some(crate::playback::PlaybackItem::Live {
                number: Some(n), ..
            }) => format!("{n:03}  "),
            _ => String::new(),
        };
        let title = req.map(|r| r.title.clone()).unwrap_or_default();
        let now_p = cx
            .multi
            .channel(slot)
            .and_then(|ch| session.now_next(ch, cx.now).1);
        let name_y = if now_p.is_some() {
            rect.bottom() - 46.0
        } else {
            rect.bottom() - 22.0
        };
        text::line(
            &p,
            pos2(x, name_y),
            Align2::LEFT_CENTER,
            &format!("{number}{title}"),
            font::body_strong(),
            color::TEXT,
            w,
        );
        if let Some(n) = &now_p {
            let line = format!(
                "{} \u{b7} until {}",
                n.title,
                cx.prefs.format_time(&n.stop.with_timezone(&Local))
            );
            text::line(
                &p,
                pos2(x, rect.bottom() - 24.0),
                Align2::LEFT_CENTER,
                &line,
                font::caption(),
                color::TEXT_2,
                w,
            );
            paint::progress_bar(
                &p,
                Rect::from_min_size(pos2(x, rect.bottom() - 10.0), vec2(w, 3.0)),
                n.progress(cx.now),
                color::ACCENT,
                color::PROGRESS_TRACK,
            );
        }
    }

    // Sound: a speaker on the tile you hear (a crossed one when everything is muted).
    let audible = cx.multi.audible() == Some(slot);
    let muted_here = cx.multi.is_muted() && f.focused;
    if audible || muted_here {
        let center = pos2(rect.right() - 26.0, rect.top() + 26.0);
        painter.circle_filled(center, 17.0, Color32::from_black_alpha(170));
        icons::paint(
            &painter,
            center,
            if audible {
                icons::SPEAKER_HIGH
            } else {
                icons::SPEAKER_SLASH
            },
            18.0,
            Style::Regular,
            color::TEXT,
        );
    }

    paint::round_corners_mask(&painter, rect, f32::from(radius::MD), color::BG);
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::MD),
        Stroke::new(1.0, color::STROKE_SUBTLE),
        StrokeKind::Inside,
    );
    if selected {
        // The tile the header buttons act on (focus is on a button).
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius::MD),
            Stroke::new(2.0, color::ACCENT),
            StrokeKind::Inside,
        );
    }
    if f.focused {
        paint::focus_ring(&painter, rect, radius::MD, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_the_picker_then_the_enlarged_view_then_leaves() {
        assert_eq!(back_step(true, true, true), Back::ClosePicker);
        assert_eq!(back_step(false, true, true), Back::CancelNumber);
        assert_eq!(back_step(false, false, true), Back::LeaveEnlarged);
        assert_eq!(back_step(false, false, false), Back::Leave);
    }

    #[test]
    fn enter_depends_on_the_slot() {
        assert_eq!(activate_action(true, false, None), Activate::Pick);
        assert_eq!(
            activate_action(false, true, Some(&PlaybackStatus::Idle)),
            Activate::Restart
        );
        assert_eq!(
            activate_action(false, false, Some(&PlaybackStatus::Failed("x".into()))),
            Activate::Restart
        );
        for s in [
            PlaybackStatus::Playing,
            PlaybackStatus::Opening,
            PlaybackStatus::Buffering,
            PlaybackStatus::Reconnecting { attempt: 1 },
        ] {
            assert_eq!(
                activate_action(false, false, Some(&s)),
                Activate::ToggleEnlarged
            );
        }
    }
}

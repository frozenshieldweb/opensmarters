// SPDX-License-Identifier: GPL-3.0-or-later
//! Guide: the EPG grid (§6.9). Channels down the left (virtualised: only the rows on
//! screen are built, so 50 000 channels scroll like 50), time across at 6 pt per minute
//! with a half-hour ruler on the viewer's clock, the now-line, and an info panel for the
//! focused programme.
//!
//! Keys: ←/→ previous/next programme (the view follows), ↑/↓ channel above/below keeping
//! the time position, PageUp/PageDown a page of channels, Home back to now. Enter on a
//! programme on air plays the channel; on a past programme with an archive it opens it in
//! Catch-up; on anything else it shows the details. The toolbar (↑ from the first row)
//! has the category filter, Earlier/Later (2 h), ±1 day and Now.
//!
//! Data comes from [`Session::guide_row`], which fetches only the rows on screen (Xtream)
//! or reads the session's bounded XMLTV guide (M3U / EPG URL override).

mod layout;

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Key, Modifiers, Rect, Sense, Stroke, Ui, pos2, vec2,
};
use opensmarters_core::catalog::{CategorySelection, SortOrder};
use opensmarters_core::chrono::{DateTime, Datelike, Duration, Local, Utc};
use opensmarters_store::ContentKind;

use super::common::channel_list;
use super::{AppCtx, Screen, shell};
use crate::focus::{self, Axis, Dir, FocusOpts};
use crate::nav::Section;
use crate::playback::ZapContext;
use crate::services::catalog::{LiveCatalog, LiveChannel, Programme, split_tag};
use crate::services::session::guide::{RowState, XmltvStatus};
use crate::services::{ImageSize, Session};
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::callout::Badge;
use crate::ui::widgets::chip::ChipRow;
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{
    Button, Callout, CalloutKind, ChannelLogo, Chip, Dialog, EmptyState, IconButton, PinPrompt,
    PinPromptResult,
};
use crate::ui::{motion, paint, text};
use layout::{
    CHANNEL_W, GAP, MIN_TEXT_W, ROW_H, ROW_STRIDE, RULER_H, TimeAxis, cursor_for, floor_slot,
    index_of_start, programme_for_time, scroll_row_into_view, scroll_time_into_view, slot_starts,
    visible_rows,
};

/// How far back and ahead the grid scrolls.
const PAST_DAYS: i64 = 7;
const AHEAD_DAYS: i64 = 7;

fn grid_scope() -> Id {
    Id::unique("guide.grid")
}

fn cell_id(key: &str, start: Option<i64>) -> Id {
    Id::unique(("guide.cell", key, start))
}

fn chip_id(sel: &CategorySelection) -> Id {
    match sel {
        CategorySelection::All => Id::unique("guide.chip.all"),
        CategorySelection::Favourites => Id::unique("guide.chip.favs"),
        CategorySelection::Category(id) => Id::unique(("guide.chip", id.as_str())),
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Focused {
    row: usize,
    key: String,
    /// `None` = the row's placeholder cell (loading / no guide).
    start: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
struct ListKey {
    selection: CategorySelection,
    generation: u64,
    favourites: usize,
    parental: bool,
}

/// The PIN asked for before a locked category opens.
#[derive(Debug)]
struct PinGate {
    prompt: PinPrompt,
    selection: CategorySelection,
    name: String,
}

/// The focused cell, painted after every other cell (§6.2 / §9.4): the next row would
/// otherwise cover the bottom of its ring and glow.
#[derive(Debug, Clone)]
enum FocusedPaint {
    Cell {
        rect: Rect,
        programme: Programme,
        anim: f32,
    },
    Placeholder {
        rect: Rect,
    },
}

#[derive(Debug, Clone)]
struct Details {
    channel: LiveChannel,
    index: usize,
    programme: Programme,
}

/// The Guide screen.
#[derive(Debug)]
pub struct GuideScreen {
    catalog: Loader<Arc<LiveCatalog>>,
    selection: CategorySelection,
    list_key: Option<ListKey>,
    channels: Arc<Vec<LiveChannel>>,
    /// Target view start (epoch secs) and what is drawn (animated towards it).
    view_start: f64,
    shown_start: f64,
    /// Target row offset and what is drawn.
    row_offset: f32,
    shown_offset: f32,
    initialized: bool,
    focus: Option<Focused>,
    cursor: Option<DateTime<Utc>>,
    body_h: f32,
    view_secs: f64,
    pin: Option<PinGate>,
    details: Option<Details>,
}

impl Default for GuideScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl GuideScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {
            catalog: Loader::new(),
            selection: CategorySelection::All,
            list_key: None,
            channels: Arc::new(Vec::new()),
            view_start: 0.0,
            shown_start: 0.0,
            row_offset: 0.0,
            shown_offset: 0.0,
            initialized: false,
            focus: None,
            cursor: None,
            body_h: 400.0,
            view_secs: 3.0 * 3600.0,
            pin: None,
            details: None,
        }
    }

    fn load(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        session.start_guide();
        self.catalog.start(cx.rt(), &cx.egui, move |_| async move {
            session.live_catalog().await
        });
    }

    fn bounds(now: DateTime<Utc>) -> (f64, f64) {
        (
            (now - Duration::days(PAST_DAYS)).timestamp() as f64,
            (now + Duration::days(AHEAD_DAYS)).timestamp() as f64,
        )
    }

    fn clamp_view(&self, v: f64, now: DateTime<Utc>) -> f64 {
        let (lo, hi) = Self::bounds(now);
        v.clamp(lo, (hi - self.view_secs).max(lo))
    }

    /// The view start that shows "now" with half an hour of context on the left.
    fn now_view(now: DateTime<Utc>) -> f64 {
        (floor_slot(now, &Local) - Duration::minutes(30)).timestamp() as f64
    }

    fn refresh_list(&mut self, session: &Session, cat: &LiveCatalog) {
        let favs = session.favourite_ids(ContentKind::Live);
        let key = ListKey {
            selection: self.selection.clone(),
            generation: self.catalog.generation(),
            favourites: favs.len(),
            parental: session.parental_enabled(),
        };
        if self.list_key.as_ref() == Some(&key) {
            return;
        }
        let picked = cat
            .select(&self.selection, &favs)
            .into_iter()
            .filter_map(|i| cat.channels.get(i));
        // Locked categories stay out of All/Favourites; a category opened with the PIN
        // shows all of its channels.
        let list = if self.selection.server_category().is_some() {
            channel_list::unlocked(picked, None)
        } else {
            channel_list::unlocked_in_session(session, cat, picked)
        };
        let sorted = channel_list::sorted(list, SortOrder::NumberAsc);
        let changed_selection = self
            .list_key
            .as_ref()
            .is_none_or(|k| k.selection != key.selection);
        self.channels = Arc::new(sorted);
        self.list_key = Some(key);
        if changed_selection {
            self.row_offset = 0.0;
            self.shown_offset = 0.0;
            self.focus = None;
        }
    }

    fn select(&mut self, cx: &mut AppCtx<'_>, sel: CategorySelection, name: &str) {
        let Some(session) = cx.session() else { return };
        if session.is_selection_locked(ContentKind::Live, &sel, name) {
            self.pin = Some(PinGate {
                prompt: PinPrompt::new(Id::unique("guide.pin")),
                selection: sel,
                name: name.to_owned(),
            });
            return;
        }
        self.selection = sel;
    }

    /// Puts the focus back on the grid cell it was on (after a dialog closes).
    fn restore_grid_focus(&self, cx: &AppCtx<'_>) {
        if let Some(f) = &self.focus {
            focus::request(&cx.egui, cell_id(&f.key, f.start));
        }
    }

    fn focus_cell(&mut self, cx: &AppCtx<'_>, row: usize, key: &str, start: Option<i64>) {
        focus::request(&cx.egui, cell_id(key, start));
        self.focus = Some(Focused {
            row,
            key: key.to_owned(),
            start,
        });
    }

    /// ←/→ inside the grid.
    fn nav_horizontal(&mut self, cx: &AppCtx<'_>, session: &Arc<Session>, dir: Dir) {
        let Some(f) = self.focus.clone() else {
            focus::give_back_nav(&cx.egui, dir);
            return;
        };
        let Some(ch) = self.channels.get(f.row).cloned() else {
            focus::give_back_nav(&cx.egui, dir);
            return;
        };
        let row = match session.guide_row(&ch) {
            RowState::Ready(r) => Some(r),
            _ => None,
        };
        let current = row.as_ref().and_then(|r| {
            f.start
                .and_then(|s| index_of_start(r, s))
                .or_else(|| programme_for_time(r, self.cursor.unwrap_or(cx.now)))
        });
        match (row, current) {
            (Some(row), Some(k)) => {
                let target = match dir {
                    Dir::Left => k.checked_sub(1),
                    _ => (k + 1 < row.programmes.len()).then_some(k + 1),
                };
                let Some(t) = target else {
                    if dir == Dir::Left {
                        focus::give_back_nav(&cx.egui, dir);
                    }
                    return;
                };
                let Some(p) = row.programmes.get(t) else {
                    return;
                };
                let bounds = Self::bounds(cx.now);
                if (p.stop.timestamp() as f64) <= bounds.0 {
                    focus::give_back_nav(&cx.egui, dir);
                    return;
                }
                self.view_start = scroll_time_into_view(
                    self.view_start,
                    self.view_secs,
                    p.start.timestamp(),
                    p.stop.timestamp(),
                    bounds,
                );
                self.cursor = Some(cursor_for(p, self.view_start));
                self.focus_cell(cx, f.row, &ch.key, Some(p.start.timestamp()));
            }
            _ => {
                // No schedule: scroll the time instead.
                let step = if dir == Dir::Left { -3600.0 } else { 3600.0 };
                let v = self.clamp_view(self.view_start + step, cx.now);
                if (v - self.view_start).abs() < 1.0 && dir == Dir::Left {
                    focus::give_back_nav(&cx.egui, dir);
                }
                self.view_start = v;
                self.cursor = DateTime::from_timestamp(v as i64, 0);
            }
        }
    }

    /// ↑/↓ (and PageUp/PageDown via `delta`) inside the grid.
    fn nav_vertical(&mut self, cx: &AppCtx<'_>, session: &Arc<Session>, delta: isize) -> bool {
        let Some(f) = self.focus.clone() else {
            return false;
        };
        let n = self.channels.len();
        let target = f.row as isize + delta;
        if n == 0 || (target < 0 && f.row == 0) || (target >= n as isize && f.row + 1 == n) {
            return false;
        }
        let tr = target.clamp(0, n as isize - 1) as usize;
        let Some(ch) = self.channels.get(tr).cloned() else {
            return false;
        };
        let cursor = self.cursor.unwrap_or(cx.now);
        let start = match session.guide_row(&ch) {
            RowState::Ready(row) => programme_for_time(&row, cursor).and_then(|i| {
                let p = row.programmes.get(i)?;
                self.view_start = scroll_time_into_view(
                    self.view_start,
                    self.view_secs,
                    p.start.timestamp(),
                    p.stop.timestamp(),
                    Self::bounds(cx.now),
                );
                Some(p.start.timestamp())
            }),
            _ => None,
        };
        self.row_offset = scroll_row_into_view(self.row_offset, self.body_h, tr);
        self.focus_cell(cx, tr, &ch.key, start);
        true
    }

    fn activate(
        &mut self,
        cx: &mut AppCtx<'_>,
        session: &Arc<Session>,
        row: usize,
        p: Option<Programme>,
    ) {
        let Some(ch) = self.channels.get(row).cloned() else {
            return;
        };
        let now = cx.now;
        match p {
            Some(p) if p.stop <= now && p.has_archive && ch.stream_id().is_some() => {
                if let Some(sid) = ch.stream_id() {
                    super::catchup::open(cx, sid, Some(p.start.timestamp()), true);
                }
            }
            Some(p) if p.start > now || p.stop <= now => {
                self.details = Some(Details {
                    channel: ch,
                    index: row,
                    programme: p,
                });
            }
            _ => self.play_live(cx, session, row),
        }
    }

    fn play_live(&mut self, cx: &mut AppCtx<'_>, session: &Arc<Session>, index: usize) {
        let Some(ch) = self.channels.get(index) else {
            return;
        };
        let zap = ZapContext {
            channels: Arc::clone(&self.channels),
            index,
        };
        match session.live_request(ch, Some(zap)) {
            Ok(req) => cx.play(req, true),
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    /// Moves the drawn values towards their targets (exponential ease; instant with
    /// reduced motion).
    fn animate(&mut self, ctx: &egui::Context) {
        let reduced = motion::reduced(ctx);
        let dt = f64::from(ctx.input(|i| i.stable_dt).min(0.1));
        let k = if reduced { 1.0 } else { (dt * 16.0).min(1.0) };
        let ds = self.view_start - self.shown_start;
        if ds.abs() < 1.0 || ds.abs() > 3.0 * 86_400.0 {
            self.shown_start = self.view_start;
        } else {
            self.shown_start += ds * k;
            ctx.request_repaint();
        }
        let dr = self.row_offset - self.shown_offset;
        if dr.abs() < 0.5 || dr.abs() > 40.0 * ROW_STRIDE {
            self.shown_offset = self.row_offset;
        } else {
            self.shown_offset += dr * k as f32;
            ctx.request_repaint();
        }
    }
}

impl Screen for GuideScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if self.catalog.is_idle() || self.catalog.error().is_some() {
            self.load(cx);
        }
        self.list_key = None;
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if self.pin.is_some() || self.details.is_some() || focus::text_input_focused(ctx) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Home)) {
            self.view_start = Self::now_view(cx.now);
            self.cursor = Some(cx.now);
        }
        if focus::is_in_scope(ctx, grid_scope()) {
            let (up, down) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::PageUp),
                    i.consume_key(Modifiers::NONE, Key::PageDown),
                )
            });
            if (up || down)
                && let Some(session) = cx.session()
            {
                let page = ((self.body_h / ROW_STRIDE).floor() as isize - 1).max(1);
                self.nav_vertical(cx, &session, if up { -page } else { page });
            }
        }
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if let Some(g) = self.pin.take() {
            // Back to the chip that asked for the PIN.
            focus::request(&cx.egui, chip_id(&g.selection));
            return true;
        }
        let closed = self.details.take().is_some();
        if closed {
            self.restore_grid_focus(cx);
        }
        closed
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Guide)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.catalog.poll();
        let Some(session) = cx.session() else { return };
        if !self.initialized {
            self.view_start = Self::now_view(cx.now);
            self.shown_start = self.view_start;
            self.cursor = Some(cx.now);
            self.initialized = true;
        }
        self.animate(ui.ctx());
        shell::status_cluster(cx, ui);
        let content = cx.layout.content;
        let m = cx.layout.margin;
        let compact = content.height() < 760.0;
        shell::page_title(cx, ui, "Guide");

        let catalog = self.catalog.value().cloned();
        if let Some(cat) = &catalog {
            self.refresh_list(&session, cat);
        }

        // Toolbar: categories + time controls.
        let tool_top = content.top() + if compact { 88.0 } else { 96.0 };
        let tool = Rect::from_min_max(
            pos2(content.left() + m - 8.0, tool_top),
            pos2(content.right() - m + 8.0, tool_top + 52.0),
        );
        self.toolbar(cx, ui, &session, catalog.as_deref(), tool);

        // Info panel.
        let info_h = if compact { 96.0 } else { 120.0 };
        let info = Rect::from_min_size(
            pos2(content.left() + m, tool.bottom() + 12.0),
            vec2(content.width() - 2.0 * m, info_h),
        );
        let grid = Rect::from_min_max(
            pos2(content.left() + m, info.bottom() + 12.0),
            pos2(content.right() - m, content.bottom() - 16.0),
        );

        match (&catalog, self.catalog.error()) {
            (None, Some(err)) => {
                let err = err.clone();
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(grid.union(info)));
                let f = EmptyState::new(
                    Id::unique("guide.error"),
                    Illustration::ErrorOffline,
                    err.title(),
                    &err.user_message(),
                )
                .action("Retry", true)
                .show(&mut child);
                if f.is_some_and(|f| f.activated) {
                    self.load(cx);
                }
            }
            (None, None) => {
                if self.catalog.show_skeleton() {
                    let painter = ui.painter().clone();
                    let mut y = grid.top() + RULER_H;
                    while y + ROW_H < grid.bottom() {
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(pos2(grid.left(), y), vec2(CHANNEL_W - GAP, ROW_H)),
                            radius::SM,
                        );
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_max(
                                pos2(grid.left() + CHANNEL_W, y),
                                pos2(grid.right(), y + ROW_H),
                            ),
                            radius::SM,
                        );
                        y += ROW_STRIDE;
                    }
                }
            }
            (Some(_), _) if self.channels.is_empty() => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(grid.union(info)));
                let (ill, head, body) = if self.selection == CategorySelection::Favourites {
                    (
                        Illustration::EmptyFavourites,
                        "No favourite channels yet",
                        "Press \u{2665} next to a channel in Live TV to see its guide here.",
                    )
                } else {
                    (
                        Illustration::EmptyGuide,
                        "No channels here",
                        "Your provider lists no channels in this category.",
                    )
                };
                let _ = EmptyState::new(Id::unique("guide.empty"), ill, head, body)
                    .illustration_size(160.0)
                    .show(&mut child);
            }
            (Some(_), _) => {
                self.grid(cx, ui, &session, grid);
                self.info_panel(cx, ui, &session, info);
            }
        }
        self.overlays(cx, ui, &session);
    }
}

impl GuideScreen {
    fn toolbar(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        catalog: Option<&LiveCatalog>,
        rect: Rect,
    ) {
        // Time controls on the right, measured so the chips always end before them.
        let now_button = Button::secondary("Now")
            .icon(icons::CLOCK)
            .small()
            .id(Id::unique("guide.now"));
        let gap = 8.0;
        let ctl_w = 44.0 * 4.0 + gap * 4.0 + now_button.size(ui).x + 8.0;
        let ctl = Rect::from_min_max(pos2(rect.right() - ctl_w, rect.top()), rect.max);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(ctl)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let mut shift: Option<f64> = None;
        let mut now = false;
        focus::scope(&mut child, Id::unique("guide.time-ctl"), |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(8.0);
            if IconButton::new(icons::CARET_DOUBLE_LEFT)
                .id(Id::unique("guide.day-back"))
                .show(ui)
                .activated
            {
                shift = Some(-86_400.0);
            }
            if IconButton::new(icons::CARET_LEFT)
                .id(Id::unique("guide.earlier"))
                .show(ui)
                .activated
            {
                shift = Some(-7200.0);
            }
            if now_button.show(ui).activated {
                now = true;
            }
            if IconButton::new(icons::CARET_RIGHT)
                .id(Id::unique("guide.later"))
                .show(ui)
                .activated
            {
                shift = Some(7200.0);
            }
            if IconButton::new(icons::CARET_DOUBLE_RIGHT)
                .id(Id::unique("guide.day-next"))
                .show(ui)
                .activated
            {
                shift = Some(86_400.0);
            }
        });
        if let Some(s) = shift {
            self.view_start = self.clamp_view(self.view_start + s, cx.now);
        }
        if now {
            self.view_start = Self::now_view(cx.now);
            self.cursor = Some(cx.now);
        }

        // Category chips (they fade out before the time controls when there are more).
        let Some(cat) = catalog else { return };
        let chips = Rect::from_min_max(rect.min, pos2(ctl.left(), rect.bottom()));
        let mut picked: Option<(CategorySelection, String)> = None;
        let selection = self.selection.clone();
        focus::scope(ui, Id::unique("guide.chips"), |ui| {
            ChipRow::new(Id::unique("guide.chips.row")).show(ui, chips, |ui| {
                for (sel, name) in [
                    (CategorySelection::All, "All"),
                    (CategorySelection::Favourites, "Favourites"),
                ] {
                    if Chip::new(name)
                        .selected(selection == sel)
                        .id(chip_id(&sel))
                        .show(ui)
                        .activated
                    {
                        picked = Some((sel, name.into()));
                    }
                }
                for c in &cat.categories {
                    if cat.count(&c.id) == 0 {
                        continue;
                    }
                    let sel = CategorySelection::Category(c.id.clone());
                    let label = if session.is_category_locked(ContentKind::Live, &c.id, &c.name) {
                        format!("{} {}", icons::LOCK_SIMPLE, c.name)
                    } else {
                        c.name.clone()
                    };
                    if Chip::new(label)
                        .selected(selection == sel)
                        .id(chip_id(&sel))
                        .show(ui)
                        .activated
                    {
                        picked = Some((sel, c.name.clone()));
                    }
                }
            });
        });
        if let Some((sel, name)) = picked {
            self.select(cx, sel, &name);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn grid(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>, rect: Rect) {
        let ctx = ui.ctx().clone();
        let body = Rect::from_min_max(
            pos2(rect.left() + CHANNEL_W, rect.top() + RULER_H),
            rect.max,
        );
        self.body_h = body.height();
        self.view_secs = TimeAxis::secs_for(body.width());
        self.view_start = self.clamp_view(self.view_start, cx.now);
        let max_offset = (self.channels.len() as f32 * ROW_STRIDE - body.height()).max(0.0);
        self.row_offset = self.row_offset.clamp(0.0, max_offset);

        // Keyboard (handled by index: rows/cells off screen don't exist yet).
        if let Some(dir) = focus::take_nav(&ctx, grid_scope(), Axis::Horizontal) {
            self.nav_horizontal(cx, session, dir);
        }
        if let Some(dir) = focus::take_nav(&ctx, grid_scope(), Axis::Vertical) {
            let delta = if dir == Dir::Up { -1 } else { 1 };
            if !self.nav_vertical(cx, session, delta) {
                focus::give_back_nav(&ctx, dir);
            }
        }
        // Mouse wheel / touchpad.
        if ui.rect_contains_pointer(rect) {
            let d = ui.input(|i| i.smooth_scroll_delta);
            if d != egui::Vec2::ZERO {
                self.row_offset = (self.row_offset - d.y).clamp(0.0, max_offset);
                self.view_start = self.clamp_view(
                    self.view_start - f64::from(d.x) / TimeAxis::PT_PER_SEC,
                    cx.now,
                );
                self.shown_offset = self.row_offset;
                self.shown_start = self.view_start;
            }
        }
        self.shown_offset = self.shown_offset.clamp(0.0, max_offset);

        let axis = TimeAxis {
            origin: self.shown_start,
            x0: body.left(),
        };
        let t_left = axis.time_at(body.left());
        let t_right = axis.time_at(body.right());
        let now = cx.now;
        let painter = ui.painter().clone();
        painter.rect_filled(rect, CornerRadius::same(radius::SM), color::BG_DEEP);

        // Ruler.
        let ruler = Rect::from_min_max(
            pos2(body.left(), rect.top()),
            pos2(body.right(), body.top()),
        );
        let rp = painter.with_clip_rect(ruler);
        let grid_lines = painter.with_clip_rect(body);
        for slot in slot_starts(t_left - Duration::minutes(30), t_right, &Local) {
            let x = axis.x(slot);
            let local = slot.with_timezone(&Local);
            let midnight = local.format("%H:%M").to_string() == "00:00";
            let label = if midnight {
                format!("{} {}", weekday(local.weekday()), local.day())
            } else {
                cx.prefs.format_time(&local)
            };
            rp.text(
                pos2(x + 8.0, ruler.center().y),
                Align2::LEFT_CENTER,
                label,
                if midnight {
                    font::caption_strong()
                } else {
                    font::caption()
                },
                if midnight {
                    color::TEXT_2
                } else {
                    color::TEXT_3
                },
            );
            rp.vline(
                x,
                (ruler.bottom() - 10.0)..=ruler.bottom(),
                Stroke::new(1.0, color::STROKE_SUBTLE),
            );
            grid_lines.vline(
                x,
                body.y_range(),
                Stroke::new(1.0, Color32::from_white_alpha(8)),
            );
        }
        painter.hline(
            rect.x_range(),
            ruler.bottom(),
            Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        // Corner: the day of the view.
        let corner = Rect::from_min_max(rect.min, pos2(body.left(), body.top()));
        let mid = axis.time_at(body.center().x).with_timezone(&Local);
        let today = now.with_timezone(&Local).date_naive();
        let day = match (mid.date_naive() - today).num_days() {
            0 => "Today".to_owned(),
            1 => "Tomorrow".to_owned(),
            -1 => "Yesterday".to_owned(),
            _ => format!(
                "{} {} {}",
                weekday(mid.weekday()),
                mid.day(),
                mid.format("%b")
            ),
        };
        text::line(
            &painter,
            pos2(corner.left() + 16.0, corner.center().y),
            Align2::LEFT_CENTER,
            &day,
            font::body_strong(),
            color::TEXT,
            CHANNEL_W - 32.0,
        );

        // Rows.
        let range = visible_rows(self.shown_offset, body.height(), self.channels.len(), 1);
        let mut rows: Vec<usize> = range.collect();
        let in_grid = focus::is_in_scope(&ctx, grid_scope());
        if let Some(f) = &self.focus
            && in_grid
            && !rows.contains(&f.row)
            && f.row < self.channels.len()
        {
            rows.push(f.row);
        }
        let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body));
        body_ui.set_clip_rect(body);
        let cell_painter = painter.with_clip_rect(body);
        let mut focused_paint: Option<FocusedPaint> = None;
        let chan_clip = Rect::from_min_max(
            pos2(rect.left(), body.top()),
            pos2(body.left(), body.bottom()),
        );
        let chan_painter = painter.with_clip_rect(chan_clip);
        let images = cx.services.images.clone();
        let channels = Arc::clone(&self.channels);
        let first_visible = rows.first().copied();
        let mut activated: Option<(usize, Option<Programme>)> = None;
        let mut newly_focused: Option<(Focused, Option<Programme>)> = None;
        let focus_now = self.focus.clone();
        let cursor = self.cursor.unwrap_or(now);
        let mut retarget: Option<(usize, String, Option<i64>)> = None;

        focus::scope(&mut body_ui, grid_scope(), |ui| {
            for &ri in &rows {
                let Some(ch) = channels.get(ri) else { continue };
                let y = body.top() + ri as f32 * ROW_STRIDE - self.shown_offset;
                let focused_row = focus_now.as_ref().is_some_and(|f| f.row == ri);

                // Channel cell (sticky left).
                let cr = Rect::from_min_size(pos2(rect.left(), y), vec2(CHANNEL_W - GAP, ROW_H));
                let hot = focused_row && in_grid;
                chan_painter.rect_filled(
                    cr,
                    CornerRadius::same(radius::SM),
                    if hot {
                        color::SURFACE_3
                    } else {
                        color::SURFACE_1
                    },
                );
                if hot {
                    chan_painter.rect_filled(
                        Rect::from_min_size(
                            pos2(cr.left(), cr.top() + 14.0),
                            vec2(3.0, ROW_H - 28.0),
                        ),
                        CornerRadius::same(2),
                        color::ACCENT,
                    );
                }
                let num = ch.number.map_or_else(String::new, |n| format!("{n}"));
                chan_painter.text(
                    pos2(cr.left() + 40.0, cr.center().y),
                    Align2::RIGHT_CENTER,
                    num,
                    font::caption_strong(),
                    color::TEXT_3,
                );
                let logo = Rect::from_min_size(
                    pos2(cr.left() + 52.0, cr.center().y - 15.0),
                    vec2(48.0, 30.0),
                );
                if chan_clip.intersects(logo) {
                    let slot = images.get(ch.logo.as_deref(), ImageSize::Logo);
                    ChannelLogo::paint(ui, &chan_painter, logo, &ch.name, &slot);
                }
                text::line(
                    &chan_painter,
                    pos2(logo.right() + 12.0, cr.center().y),
                    Align2::LEFT_CENTER,
                    split_tag(&ch.name).1,
                    font::body_strong(),
                    if focused_row && in_grid {
                        color::TEXT
                    } else {
                        color::TEXT_2
                    },
                    cr.right() - logo.right() - 24.0,
                );

                // Programme cells.
                let state = session.guide_row(ch);
                match state {
                    RowState::Ready(row) => {
                        let mut idxs: Vec<usize> = Vec::new();
                        let mut i = row.first_ending_after(t_left);
                        while i < row.programmes.len() && row.programmes[i].start < t_right {
                            idxs.push(i);
                            i += 1;
                        }
                        // The focused placeholder became a schedule (or the schedule
                        // changed): move the focus onto a real programme so it can't vanish.
                        if let Some(f) = focus_now.as_ref().filter(|f| f.row == ri && in_grid) {
                            let lost = f.start.is_some_and(|s| index_of_start(&row, s).is_none());
                            if (f.start.is_none() || lost) && !idxs.is_empty() {
                                let k = programme_for_time(&row, cursor)
                                    .filter(|k| idxs.contains(k))
                                    .unwrap_or(idxs[0]);
                                retarget = Some((
                                    ri,
                                    ch.key.clone(),
                                    Some(row.programmes[k].start.timestamp()),
                                ));
                            } else if lost {
                                retarget = Some((ri, ch.key.clone(), None));
                            }
                        }
                        // Keep the focused cell registered while the view scrolls to it.
                        if let Some(f) = focus_now.as_ref().filter(|f| f.row == ri && in_grid)
                            && let Some(k) = f.start.and_then(|s| index_of_start(&row, s))
                            && !idxs.contains(&k)
                        {
                            idxs.push(k);
                        }
                        if idxs.is_empty() {
                            let r = Rect::from_min_max(
                                pos2(body.left() + GAP, y),
                                pos2(body.right() - GAP, y + ROW_H),
                            );
                            if let Some((f, act)) = placeholder_cell(
                                ui,
                                &cell_painter,
                                ch,
                                r,
                                "No information for this time",
                                false,
                                false,
                            ) {
                                if act {
                                    activated = Some((ri, None));
                                }
                                if f {
                                    focused_paint = Some(FocusedPaint::Placeholder { rect: r });
                                    newly_focused = Some((
                                        Focused {
                                            row: ri,
                                            key: ch.key.clone(),
                                            start: None,
                                        },
                                        None,
                                    ));
                                }
                            }
                        }
                        for k in idxs {
                            let p = &row.programmes[k];
                            let x0 = axis.x(p.start).max(body.left() - 400.0);
                            let x1 = axis.x(p.stop).min(body.right() + 400.0);
                            let r = Rect::from_min_max(
                                pos2(x0 + GAP / 2.0, y),
                                pos2((x1 - GAP / 2.0).max(x0 + GAP), y + ROW_H),
                            );
                            let is_live = p.start <= now && now < p.stop;
                            let autofocus =
                                is_live && Some(ri) == first_visible && focus_now.is_none();
                            let id = cell_id(&ch.key, Some(p.start.timestamp()));
                            let (_, f) = focus::focusable(
                                ui,
                                id,
                                r,
                                FocusOpts::default().autofocus_if(autofocus),
                            );
                            if f.focused {
                                // Painted last, over its neighbours.
                                focused_paint = Some(FocusedPaint::Cell {
                                    rect: r,
                                    programme: p.clone(),
                                    anim: f.anim,
                                });
                            } else {
                                paint_cell(&cell_painter, r, body, p, now, false, &axis, cx.prefs);
                            }
                            if f.focused {
                                newly_focused = Some((
                                    Focused {
                                        row: ri,
                                        key: ch.key.clone(),
                                        start: Some(p.start.timestamp()),
                                    },
                                    Some(p.clone()),
                                ));
                            }
                            if f.activated {
                                activated = Some((ri, Some(p.clone())));
                            }
                        }
                    }
                    RowState::Loading | RowState::Missing => {
                        let loading = matches!(state, RowState::Loading);
                        if let Some(f) = focus_now.as_ref().filter(|f| f.row == ri && in_grid)
                            && f.start.is_some()
                        {
                            retarget = Some((ri, ch.key.clone(), None));
                        }
                        let r = Rect::from_min_max(
                            pos2(body.left() + GAP, y),
                            pos2(body.right() - GAP, y + ROW_H),
                        );
                        if loading {
                            skeleton(&ctx, &cell_painter, r, radius::SM);
                        }
                        if let Some((f, act)) = placeholder_cell(
                            ui,
                            &cell_painter,
                            ch,
                            r,
                            if loading {
                                "Loading the guide…"
                            } else {
                                "No programme information"
                            },
                            loading,
                            Some(ri) == first_visible && focus_now.is_none(),
                        ) {
                            if act {
                                activated = Some((ri, None));
                            }
                            if f {
                                focused_paint = Some(FocusedPaint::Placeholder { rect: r });
                                newly_focused = Some((
                                    Focused {
                                        row: ri,
                                        key: ch.key.clone(),
                                        start: None,
                                    },
                                    None,
                                ));
                            }
                        }
                    }
                }
            }
        });
        if let Some((row, key, start)) = retarget {
            self.focus_cell(cx, row, &key, start);
        }
        // The focused cell over its neighbours: glow → cell → now-line → ring (the ring
        // is the brightest thing on screen and must not be cut by the next row).
        let ring_rect = match &focused_paint {
            Some(FocusedPaint::Cell {
                rect: r,
                programme,
                anim,
            }) => {
                let visible = r.intersect(body);
                if *anim > 0.0 && visible.is_positive() {
                    paint::focus_glow(&cell_painter, visible, radius::SM, *anim);
                }
                paint_cell(
                    &cell_painter,
                    *r,
                    body,
                    programme,
                    now,
                    true,
                    &axis,
                    cx.prefs,
                );
                Some(visible)
            }
            Some(FocusedPaint::Placeholder { rect: r }) => Some(r.intersect(body)),
            None => None,
        };
        // Now-line.
        let nx = axis.x(now);
        if body.x_range().contains(nx) {
            painter.vline(nx, body.y_range(), Stroke::new(2.0, color::LIVE));
            painter.circle_filled(pos2(nx, ruler.bottom() - 3.0), 3.0, color::LIVE);
        }
        if let Some(r) = ring_rect.filter(|r| r.is_positive()) {
            // 6 pt of ring outside the cell may cross the ruler or the channel column.
            let ring_painter = painter.with_clip_rect(body.expand(6.0));
            paint::focus_ring(&ring_painter, r, radius::SM, 1.0);
        }
        if let Some((f, p)) = newly_focused {
            if self.focus.as_ref() != Some(&f) {
                // Focus arrived by pointer or geometric navigation: follow it.
                if let Some(p) = &p {
                    self.cursor = Some(cursor_for(p, self.view_start));
                }
            }
            self.focus = Some(f);
        }
        if let Some((row, p)) = activated {
            self.activate(cx, session, row, p);
        }
    }

    fn info_panel(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>, rect: Rect) {
        let painter = ui.painter().clone();
        // The guide itself is loading/failed (XMLTV).
        if session.guide_uses_xmltv() {
            match session.xmltv_status() {
                XmltvStatus::Loading | XmltvStatus::Idle => {
                    paint::spinner(
                        &painter,
                        pos2(rect.left() + 16.0, rect.top() + 28.0),
                        10.0,
                        ui.input(|i| i.time),
                        2.0,
                    );
                    painter.text(
                        pos2(rect.left() + 36.0, rect.top() + 28.0),
                        Align2::LEFT_CENTER,
                        "Downloading your programme guide…",
                        font::body_strong(),
                        color::TEXT_2,
                    );
                    painter.text(
                        pos2(rect.left() + 36.0, rect.top() + 52.0),
                        Align2::LEFT_CENTER,
                        "Large guides can take a minute. Channels stay available meanwhile.",
                        font::caption(),
                        color::TEXT_3,
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                XmltvStatus::Failed(msg) => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    Callout::new(
                        CalloutKind::Warning,
                        "The programme guide couldn't be loaded.",
                        msg,
                    )
                    .width(rect.width().min(720.0))
                    .show(&mut child);
                    return;
                }
                XmltvStatus::Ready { .. } => {}
            }
        }
        let Some(f) = self.focus.clone() else { return };
        let Some(ch) = self.channels.get(f.row).cloned() else {
            return;
        };
        let now = cx.now;
        let p = match (session.guide_row(&ch), f.start) {
            (RowState::Ready(row), Some(s)) => {
                index_of_start(&row, s).and_then(|i| row.programmes.get(i).cloned())
            }
            _ => None,
        };
        let x = rect.left();
        let w = rect.width();
        let chan = match ch.number {
            Some(n) => format!("{n} · {}", split_tag(&ch.name).1),
            None => split_tag(&ch.name).1.to_owned(),
        };
        let Some(p) = p else {
            text::overline(
                &painter,
                pos2(x, rect.top() + 10.0),
                Align2::LEFT_CENTER,
                &chan,
                color::TEXT_3,
            );
            text::line(
                &painter,
                pos2(x, rect.top() + 40.0),
                Align2::LEFT_CENTER,
                "No programme information",
                font::title(),
                color::TEXT_2,
                w,
            );
            painter.text(
                pos2(x, rect.top() + 72.0),
                Align2::LEFT_CENTER,
                "Press Enter to watch the channel live.",
                font::body(),
                color::TEXT_3,
            );
            return;
        };
        let live = p.start <= now && now < p.stop;
        let archive = p.has_archive && p.stop <= now;
        let mut ox = x;
        if live {
            let r = Badge::live(&painter, pos2(ox, rect.top() + 10.0), Align2::LEFT_CENTER);
            ox = r.right() + 10.0;
        } else if archive {
            icons::paint(
                &painter,
                pos2(ox + 8.0, rect.top() + 10.0),
                icons::CLOCK_COUNTER_CLOCKWISE,
                16.0,
                Style::Regular,
                color::SAGE,
            );
            let r = text::overline(
                &painter,
                pos2(ox + 22.0, rect.top() + 10.0),
                Align2::LEFT_CENTER,
                "Catch-up",
                color::SAGE,
            );
            ox = r.right() + 12.0;
        }
        text::overline(
            &painter,
            pos2(ox, rect.top() + 10.0),
            Align2::LEFT_CENTER,
            &chan,
            color::TEXT_3,
        );
        let tr = text::line(
            &painter,
            pos2(x, rect.top() + 40.0),
            Align2::LEFT_CENTER,
            &p.title,
            font::title(),
            color::TEXT,
            w * 0.7,
        );
        let mins = (p.stop - p.start).num_minutes().max(0);
        let mut meta = format!(
            "{}–{} · {}",
            cx.prefs.format_time(&p.start.with_timezone(&Local)),
            cx.prefs.format_time(&p.stop.with_timezone(&Local)),
            text::duration_short((mins * 60) as f64)
        );
        if live {
            meta.push_str(&format!(
                " · {} min left",
                (p.stop - now).num_minutes().max(0)
            ));
        } else if p.start > now {
            let until = (p.start - now).num_minutes();
            if until < 120 {
                meta.push_str(&format!(" · starts in {until} min"));
            }
        }
        painter.text(
            pos2(tr.right() + 16.0, rect.top() + 42.0),
            Align2::LEFT_CENTER,
            meta,
            font::caption(),
            color::TEXT_3,
        );
        if live {
            paint::progress_bar(
                &painter,
                Rect::from_min_size(pos2(x, rect.top() + 60.0), vec2(160.0, 4.0)),
                p.progress(now),
                color::ACCENT,
                color::PROGRESS_TRACK,
            );
        }
        let rows = if rect.height() > 110.0 { 2 } else { 1 };
        if !p.description.trim().is_empty() {
            text::block(
                &painter,
                pos2(x, rect.top() + 70.0),
                &p.description,
                font::body(),
                color::TEXT_2,
                w * 0.75,
                rows,
            );
        }
        // Hint.
        let hint = if live {
            "Enter  Watch"
        } else if archive {
            "Enter  Replay"
        } else {
            "Enter  Details"
        };
        painter.text(
            pos2(rect.right(), rect.top() + 10.0),
            Align2::RIGHT_CENTER,
            hint,
            font::caption(),
            color::TEXT_3,
        );
    }

    fn overlays(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>) {
        if let Some(d) = self.details.clone() {
            let (close, watch) = details_dialog(
                cx,
                ui.ctx(),
                Id::unique("guide.details"),
                &d.channel,
                &d.programme,
            );
            if close {
                self.details = None;
                self.restore_grid_focus(cx);
            }
            if watch {
                self.details = None;
                self.restore_grid_focus(cx);
                self.play_live(cx, session, d.index);
            }
        }
        if let Some(g) = &mut self.pin {
            let title = format!("\u{201c}{}\u{201d} is locked", text::cap(&g.name, 40));
            match g.prompt.show_dialog(
                ui.ctx(),
                cx.services,
                &title,
                Some("Its channels stay unlocked until you close opensmarters."),
            ) {
                PinPromptResult::Verified(_) => {
                    if let CategorySelection::Category(id) = &g.selection {
                        session.unlock_category(ContentKind::Live, id);
                    }
                    let sel = g.selection.clone();
                    focus::request(&cx.egui, chip_id(&sel));
                    self.selection = sel;
                    self.pin = None;
                    self.list_key = None;
                }
                PinPromptResult::Cancelled | PinPromptResult::Forgot => {
                    focus::request(&cx.egui, chip_id(&g.selection));
                    self.pin = None;
                }
                PinPromptResult::Pending => {}
            }
        }
    }
}

/// "Today", "Tomorrow", "Yesterday" or "Thu 22 Oct" for an instant, on the viewer's clock.
pub(crate) fn relative_day(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    match (local.date_naive() - now.with_timezone(&Local).date_naive()).num_days() {
        0 => "Today".to_owned(),
        1 => "Tomorrow".to_owned(),
        -1 => "Yesterday".to_owned(),
        _ => format!(
            "{} {} {}",
            weekday(local.weekday()),
            local.day(),
            local.format("%b")
        ),
    }
}

/// The details dialog of a programme that can't be played as such (upcoming, or ended
/// without an archive). Returns `(close, watch the channel)`. Also used by Search.
pub(crate) fn details_dialog(
    cx: &AppCtx<'_>,
    ctx: &egui::Context,
    id: Id,
    channel: &LiveChannel,
    p: &Programme,
) -> (bool, bool) {
    let when = format!(
        "{} · {}–{} · {}",
        relative_day(p.start, cx.now),
        cx.prefs.format_time(&p.start.with_timezone(&Local)),
        cx.prefs.format_time(&p.stop.with_timezone(&Local)),
        split_tag(&channel.name).1,
    );
    let ended = p.stop <= cx.now;
    let mut close = false;
    let mut watch = false;
    Dialog::new(id, "").show_custom(ctx, |ui| {
        let w = ui.available_width();
        text::overline(
            ui.painter(),
            ui.cursor().min + vec2(0.0, 8.0),
            Align2::LEFT_CENTER,
            if ended {
                "Ended · not in the archive"
            } else {
                "Coming up"
            },
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
            ui.add_space(16.0);
            let g = text::galley(
                ui.painter(),
                &p.description,
                font::body(),
                color::TEXT_2,
                w,
                8,
            );
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, color::TEXT_2);
        }
        ui.add_space(24.0);
        ui.allocate_ui_with_layout(
            vec2(w, 44.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                watch = Button::primary("Watch the channel now")
                    .icon(icons::PLAY)
                    .id(id.with("watch"))
                    .show(ui)
                    .activated;
                close = Button::secondary("Close")
                    .autofocus()
                    .id(id.with("close"))
                    .show(ui)
                    .activated;
            },
        );
    });
    (close, watch)
}

fn weekday(w: opensmarters_core::chrono::Weekday) -> &'static str {
    use opensmarters_core::chrono::Weekday as W;
    match w {
        W::Mon => "Mon",
        W::Tue => "Tue",
        W::Wed => "Wed",
        W::Thu => "Thu",
        W::Fri => "Fri",
        W::Sat => "Sat",
        W::Sun => "Sun",
    }
}

/// A row-wide placeholder cell (loading / no guide). Returns `(focused, activated)`; the
/// caller paints the ring last.
fn placeholder_cell(
    ui: &mut Ui,
    painter: &egui::Painter,
    ch: &LiveChannel,
    r: Rect,
    label: &str,
    loading: bool,
    autofocus: bool,
) -> Option<(bool, bool)> {
    let (_, f) = focus::focusable(
        ui,
        cell_id(&ch.key, None),
        r,
        FocusOpts::default().autofocus_if(autofocus),
    );
    if !loading {
        painter.rect_filled(
            r,
            CornerRadius::same(radius::SM),
            if f.focused {
                color::SURFACE_3
            } else {
                color::SURFACE_1
            },
        );
    }
    let left = painter.clip_rect().left().max(r.left());
    painter.text(
        pos2(left + 16.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        font::caption(),
        color::TEXT_3,
    );
    Some((f.focused, f.activated))
}

/// Paints a programme cell (the caller paints the focus glow before and the ring after).
#[allow(clippy::too_many_arguments)]
fn paint_cell(
    painter: &egui::Painter,
    r: Rect,
    body: Rect,
    p: &Programme,
    now: DateTime<Utc>,
    focused: bool,
    axis: &TimeAxis,
    prefs: &crate::services::Prefs,
) {
    let live = p.start <= now && now < p.stop;
    let past = p.stop <= now;
    let fill = if focused || live {
        color::SURFACE_3
    } else if past {
        color::SURFACE_1
    } else {
        color::SURFACE_2
    };
    painter.rect_filled(r, CornerRadius::same(radius::SM), fill);
    if live {
        // Elapsed part tinted, 3 pt accent bar on the left edge.
        let nx = axis.x(now).clamp(r.left(), r.right());
        painter.rect_filled(
            Rect::from_min_max(r.min, pos2(nx, r.bottom())),
            CornerRadius {
                nw: radius::SM,
                sw: radius::SM,
                ne: 0,
                se: 0,
            },
            color::ACCENT_SOFT,
        );
        painter.rect_filled(
            Rect::from_min_size(r.min, vec2(3.0, r.height())),
            CornerRadius {
                nw: radius::SM,
                sw: radius::SM,
                ne: 0,
                se: 0,
            },
            color::ACCENT,
        );
    }
    // Text sticks to the visible left edge.
    let visible = r.intersect(body);
    if visible.width() >= MIN_TEXT_W {
        let tx = visible.left() + 12.0;
        let tw = visible.right() - tx - 10.0;
        let catchup = p.has_archive && p.start <= now;
        let title_w = if catchup { tw - 18.0 } else { tw };
        text::line(
            painter,
            pos2(tx, r.top() + 22.0),
            Align2::LEFT_CENTER,
            &p.title,
            font::body_strong(),
            if past && !focused {
                color::TEXT_3
            } else {
                color::TEXT
            },
            title_w.max(8.0),
        );
        let time = format!(
            "{}–{}",
            prefs.format_time(&p.start.with_timezone(&Local)),
            prefs.format_time(&p.stop.with_timezone(&Local))
        );
        text::line(
            painter,
            pos2(tx, r.top() + 44.0),
            Align2::LEFT_CENTER,
            &time,
            font::caption(),
            color::TEXT_3,
            tw.max(8.0),
        );
        if catchup {
            icons::paint(
                painter,
                pos2(visible.right() - 16.0, r.top() + 16.0),
                icons::CLOCK_COUNTER_CLOCKWISE,
                14.0,
                Style::Regular,
                color::SAGE,
            );
        }
    }
}

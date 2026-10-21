// SPDX-License-Identifier: GPL-3.0-or-later
//! The multiscreen channel picker: a modal panel with the categories (All, Favourites,
//! provider categories), a virtualised channel list and channel-number entry. Keyboard
//! first: arrows move, Enter picks, digits jump to a number, Esc closes.
//!
//! Parental control: every list leaves out channels of locked categories (also inside an
//! unlocked category, when a channel is in a locked one too), so a tile's zap list never
//! holds one; a locked category itself is opened in Live TV with the PIN (the picker says
//! so), and a locked channel's number is refused.
//!
//! Category management (Settings › Categories): the categories follow the viewer's order
//! without the hidden ones; "All" (and its count) leaves out channels whose every category
//! is hidden; Favourites are unchanged.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, CornerRadius, Id, Key, Modifiers, Rect, Sense, Stroke, Ui, pos2, vec2,
};
use opensmarters_core::catalog::{CategorySelection, SortOrder, sorted_indices};
use opensmarters_store::ContentKind;

use crate::focus::{self, FocusOpts};
use crate::playback::ZapContext;
use crate::playback::multi::LockedCategories;
use crate::screens::AppCtx;
use crate::services::catalog::{LiveCatalog, LiveChannel};
use crate::services::categories::{self, HiddenCategories};
use crate::services::{ImageSize, Session};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{ChannelLogo, Dialog, MenuRow, VirtualList};
use crate::ui::{paint, text};

/// Channel-number entry picks after this long without another digit.
pub(super) const NUMBER_ENTRY_TIMEOUT: Duration = Duration::from_millis(1500);
const ROW_H: f32 = 56.0;
const ROW_GAP: f32 = 4.0;

fn dialog_id() -> Id {
    Id::unique("multi.pick")
}

fn list_id() -> Id {
    Id::unique("multi.pick.list")
}

fn row_id(key: &str) -> Id {
    Id::unique(("multi.pick.row", key))
}

/// The loaded channel data the picker shows.
#[derive(Debug, Clone, Copy)]
pub(super) struct Channels<'a> {
    /// The live catalogue.
    pub catalog: &'a Arc<LiveCatalog>,
    /// Its loader generation (the list is recomputed when it changes).
    pub generation: u64,
    /// All playable channels (number entry outside the category on screen), built with
    /// `multi::playable`.
    pub all: &'a Arc<Vec<LiveChannel>>,
    /// The parental locks.
    pub locks: &'a LockedCategories,
    /// The hidden categories (Settings › Categories).
    pub hidden: &'a HiddenCategories,
}

/// The open picker.
#[derive(Debug)]
pub(super) struct Picker {
    slot: usize,
    selection: CategorySelection,
    list_key: Option<(CategorySelection, u64, usize, u64)>,
    visible: Arc<Vec<LiveChannel>>,
    /// The tile's channel (focused first).
    current: Option<String>,
    scroll_to: Option<usize>,
    /// Focus this row once the list is ready.
    focus_row: Option<usize>,
    placed_focus: bool,
    number: Option<(String, Instant)>,
    commit: Option<u64>,
}

impl Picker {
    /// A picker for `slot`, starting on its current channel if it has one.
    pub(super) fn new(slot: usize, current: Option<&str>) -> Self {
        Self {
            slot,
            selection: CategorySelection::All,
            list_key: None,
            visible: Arc::new(Vec::new()),
            current: current.map(str::to_owned),
            scroll_to: None,
            focus_row: None,
            placed_focus: false,
            number: None,
            commit: None,
        }
    }

    /// The slot it picks for.
    pub(super) fn slot(&self) -> usize {
        self.slot
    }

    /// Esc: cancels a number being typed first. Returns true if it did.
    pub(super) fn on_back(&mut self) -> bool {
        self.number.take().is_some()
    }

    /// Digits (number entry) and Enter to confirm them, before the focus engine.
    pub(super) fn on_input(&mut self, ctx: &egui::Context) {
        push_digits(&mut self.number, &take_digits(ctx));
        if self.number.is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            self.commit = self.number.take().and_then(|(b, _)| b.parse().ok());
        }
    }

    /// Recomputes the channel list when the category, the catalogue or the favourites
    /// change.
    fn refresh(
        &mut self,
        session: &Session,
        catalog: &LiveCatalog,
        generation: u64,
        locks: &LockedCategories,
        hidden: &HiddenCategories,
    ) {
        // A hidden category isn't browsed: back to All.
        if let CategorySelection::Category(id) = &self.selection
            && hidden.contains(id)
        {
            self.selection = CategorySelection::All;
        }
        let favs = session.favourite_ids(ContentKind::Live);
        let key = (
            self.selection.clone(),
            generation,
            favs.len(),
            session.category_prefs_generation(),
        );
        if self.list_key.as_ref() == Some(&key) {
            return;
        }
        let favourites = self.selection == CategorySelection::Favourites;
        let picked: Vec<LiveChannel> = catalog
            .select(&self.selection, &favs)
            .into_iter()
            .filter_map(|i| catalog.channels.get(i))
            // Never a channel of a locked category: this list becomes a tile's zap list.
            .filter(|c| !locks.hides(c))
            // Hidden categories leave All (Favourites stay as they are).
            .filter(|c| favourites || !hidden.hides(&c.categories))
            .cloned()
            .collect();
        let order = sorted_indices(&picked, SortOrder::NumberAsc);
        let mut slots: Vec<Option<LiveChannel>> = picked.into_iter().map(Some).collect();
        let sorted: Vec<LiveChannel> = order
            .into_iter()
            .filter_map(|i| slots.get_mut(i).and_then(Option::take))
            .collect();
        self.visible = Arc::new(sorted);
        self.list_key = Some(key);
    }

    /// The channel with `number`: in the list on screen first, else among all playable
    /// channels.
    fn by_number(
        &self,
        number: u64,
        catalog: &LiveCatalog,
        all: &Arc<Vec<LiveChannel>>,
        locks: &LockedCategories,
        hidden: &HiddenCategories,
    ) -> Result<(LiveChannel, ZapContext), String> {
        if let Some(i) = self.visible.iter().position(|c| c.number == Some(number)) {
            return Ok((
                self.visible[i].clone(),
                ZapContext {
                    channels: Arc::clone(&self.visible),
                    index: i,
                },
            ));
        }
        let Some(i) = all.iter().position(|c| c.number == Some(number)) else {
            return Err(number_missing(number, catalog, locks, hidden));
        };
        Ok((
            all[i].clone(),
            ZapContext {
                channels: Arc::clone(all),
                index: i,
            },
        ))
    }

    /// Draws the picker. Returns the chosen channel and its zap list (the category on
    /// screen, or all channels for a number).
    pub(super) fn show(
        &mut self,
        cx: &mut AppCtx<'_>,
        session: &Arc<Session>,
        catalog: Option<Channels<'_>>,
        tile_keys: &[Option<String>],
    ) -> Option<(LiveChannel, ZapContext)> {
        let mut picked: Option<(LiveChannel, ZapContext)> = None;
        if let Some(Channels {
            catalog: cat,
            generation,
            all,
            locks,
            hidden,
        }) = catalog
        {
            self.refresh(session, cat, generation, locks, hidden);
            if !self.placed_focus {
                // First frame with data: the tile's channel, else the first one.
                self.placed_focus = true;
                let i = self
                    .current
                    .as_deref()
                    .and_then(|k| self.visible.iter().position(|c| c.key == k))
                    .unwrap_or(0);
                self.focus_row = Some(i);
            }
            let timed_out = self
                .number
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() >= NUMBER_ENTRY_TIMEOUT);
            if timed_out {
                self.commit = self.number.take().and_then(|(b, _)| b.parse().ok());
            }
            if let Some(n) = self.commit.take() {
                match self.by_number(n, cat, all, locks, hidden) {
                    Ok(p) => picked = Some(p),
                    Err(msg) => cx.toasts.info(msg),
                }
            }
        }
        if let Some(i) = self.focus_row.take() {
            match self.visible.get(i) {
                Some(ch) => {
                    focus::request(&cx.egui, row_id(&ch.key));
                    self.scroll_to = Some(i);
                }
                None if self.list_key.is_none() => self.focus_row = Some(i),
                None => {}
            }
        }

        let screen = cx.layout.screen;
        let safe = cx.layout.safe_area();
        let width = (safe.width() - 96.0).clamp(320.0, 1080.0);
        let height = (safe.height() - 120.0).clamp(220.0, 820.0);
        let cats_w = if cx.layout.is_wide() { 272.0 } else { 232.0 };
        let ctx = cx.egui.clone();
        let tile_no = self.slot + 1;
        Dialog::new(dialog_id(), "")
            .width(width.min(screen.width() - 48.0))
            .show_custom(&ctx, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                let painter = ui.painter().clone();
                // Header: title, then the number being typed or the tile.
                let hy = rect.top() + 16.0;
                text::line(
                    &painter,
                    pos2(rect.left(), hy),
                    Align2::LEFT_CENTER,
                    "Choose a channel",
                    font::title(),
                    color::TEXT,
                    rect.width() * 0.6,
                );
                match &self.number {
                    Some((buf, _)) => {
                        let g = painter.layout_no_wrap(
                            format!("Channel {buf}"),
                            font::title_sm(),
                            color::TEXT,
                        );
                        let r = Rect::from_min_size(
                            pos2(rect.right() - g.size().x - 32.0, hy - 20.0),
                            g.size() + vec2(32.0, 40.0 - g.size().y),
                        );
                        painter.rect_filled(r, CornerRadius::same(radius::SM), color::SURFACE_3);
                        painter.galley(r.center() - g.size() / 2.0, g, color::TEXT);
                        ctx.request_repaint_after(Duration::from_millis(200));
                    }
                    None => {
                        text::overline(
                            &painter,
                            pos2(rect.right(), hy),
                            Align2::RIGHT_CENTER,
                            &format!("Tile {tile_no}"),
                            color::TEXT_3,
                        );
                    }
                }
                let top = rect.top() + 52.0;
                let bottom = rect.bottom() - 40.0;
                let cats =
                    Rect::from_min_max(pos2(rect.left(), top), pos2(rect.left() + cats_w, bottom));
                let list =
                    Rect::from_min_max(pos2(cats.right() + 16.0, top), pos2(rect.right(), bottom));
                painter.vline(
                    cats.right() + 4.0,
                    cats.y_range(),
                    Stroke::new(1.0, color::STROKE_SUBTLE),
                );
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cats));
                focus::scope(&mut child, Id::unique("multi.pick.cats"), |ui| {
                    self.categories(cx, ui, session, catalog.map(|c| (c.catalog, c.all.len())));
                });
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
                if let Some(p) =
                    self.channels(cx, &mut child, session, catalog.is_some(), tile_keys)
                {
                    picked = Some(p);
                }
                // Hint.
                let fy = rect.bottom() - 12.0;
                icons::paint(
                    &painter,
                    pos2(rect.left() + 8.0, fy),
                    icons::INFO,
                    16.0,
                    Style::Regular,
                    color::TEXT_3,
                );
                text::line(
                    &painter,
                    pos2(rect.left() + 26.0, fy),
                    Align2::LEFT_CENTER,
                    &format!(
                        "Enter: watch in tile {tile_no} \u{b7} Type a channel number to jump \
                         \u{b7} Esc: close"
                    ),
                    font::caption(),
                    color::TEXT_3,
                    rect.width() - 30.0,
                );
            });
        picked
    }

    fn categories(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        catalog: Option<(&Arc<LiveCatalog>, usize)>,
    ) {
        let Some((cat, all_count)) = catalog else {
            for i in 0..6 {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(
                        r.min + vec2(14.0, 17.0),
                        vec2(90.0 + (i % 3) as f32 * 30.0, 14.0),
                    ),
                    radius::XS,
                );
            }
            return;
        };
        let favs = session.favourite_ids(ContentKind::Live).len();
        let prefs = session.category_prefs(ContentKind::Live);
        let shown = categories::arrange(&cat.categories, &prefs);
        let mut chosen: Option<(CategorySelection, String)> = None;
        egui::ScrollArea::vertical()
            .id_salt("multi.pick.cats.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let row =
                    |ui: &mut Ui, id: Id, label: &str, count: usize, sel: bool, locked: bool| {
                        MenuRow::new(id, label)
                            .count(Some(count))
                            .selected(sel)
                            .locked(locked)
                            .show(ui)
                            .activated
                    };
                if row(
                    ui,
                    Id::unique("multi.pick.cat.all"),
                    "All",
                    all_count,
                    self.selection == CategorySelection::All,
                    false,
                ) {
                    chosen = Some((CategorySelection::All, "All".into()));
                }
                if row(
                    ui,
                    Id::unique("multi.pick.cat.favs"),
                    "Favourites",
                    favs,
                    self.selection == CategorySelection::Favourites,
                    false,
                ) {
                    chosen = Some((CategorySelection::Favourites, "Favourites".into()));
                }
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 17.0), Sense::hover());
                ui.painter().hline(
                    r.x_range(),
                    r.center().y,
                    Stroke::new(1.0, color::STROKE_SUBTLE),
                );
                for c in &shown {
                    let sel = CategorySelection::Category(c.id.clone());
                    let locked = session.is_category_locked(ContentKind::Live, &c.id, &c.name);
                    if row(
                        ui,
                        Id::unique(("multi.pick.cat", c.id.as_str())),
                        &c.name,
                        cat.count(&c.id),
                        self.selection == sel,
                        locked,
                    ) {
                        chosen = Some((sel, c.name.clone()));
                    }
                }
            });
        focus::forward_scroll(ui);
        if let Some((sel, name)) = chosen {
            if session.is_selection_locked(ContentKind::Live, &sel, &name) {
                cx.toasts.info(format!(
                    "\u{201c}{}\u{201d} is locked. Open it in Live TV with your PIN first.",
                    text::cap(&name, 40)
                ));
            } else if sel != self.selection {
                self.selection = sel;
                self.list_key = None;
                self.focus_row = Some(0);
            } else {
                self.focus_row = Some(0);
            }
        }
    }

    fn channels(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        loaded: bool,
        tile_keys: &[Option<String>],
    ) -> Option<(LiveChannel, ZapContext)> {
        let rect = ui.max_rect();
        if !loaded {
            let mut y = rect.top() + 8.0;
            while y + ROW_H < rect.bottom() {
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(pos2(rect.left() + 64.0, y + 11.0), vec2(56.0, 34.0)),
                    radius::SM,
                );
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(pos2(rect.left() + 132.0, y + 14.0), vec2(180.0, 14.0)),
                    radius::XS,
                );
                y += ROW_H + ROW_GAP;
            }
            return None;
        }
        if self.visible.is_empty() {
            let msg = if self.selection == CategorySelection::Favourites {
                "No favourite channels yet. Press \u{2665} next to the Live TV preview to add one."
            } else {
                "No channels here."
            };
            text::block(
                ui.painter(),
                pos2(rect.left() + 16.0, rect.top() + 16.0),
                msg,
                font::body(),
                color::TEXT_2,
                rect.width() - 32.0,
                3,
            );
            return None;
        }
        let visible = Arc::clone(&self.visible);
        let images = cx.services.images.clone();
        let now = cx.now;
        let current = self.current.clone();
        let mut activated: Option<usize> = None;
        VirtualList::new(list_id(), ROW_H, visible.len())
            .gap(ROW_GAP)
            .scroll_to(self.scroll_to.take())
            .show(
                ui,
                |i| visible.get(i).map_or(Id::NULL, |c| row_id(&c.key)),
                |ui, i, rect| {
                    let Some(ch) = visible.get(i) else { return };
                    let (_, f) = focus::focusable(ui, row_id(&ch.key), rect, FocusOpts::default());
                    let painter = ui.painter().clone();
                    let is_current = current.as_deref() == Some(ch.key.as_str());
                    if f.focused {
                        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_3);
                    } else if is_current {
                        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_2);
                    }
                    if is_current {
                        painter.rect_filled(
                            Rect::from_min_size(
                                pos2(rect.left(), rect.top() + 12.0),
                                vec2(3.0, rect.height() - 24.0),
                            ),
                            CornerRadius::same(2),
                            color::ACCENT,
                        );
                    }
                    let num = ch
                        .number
                        .map_or_else(|| "\u{2014}".to_owned(), |n| format!("{n:03}"));
                    painter.text(
                        pos2(rect.left() + 48.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        num,
                        font::caption_strong(),
                        color::TEXT_3,
                    );
                    let logo = Rect::from_min_size(
                        pos2(rect.left() + 60.0, rect.center().y - 17.0),
                        vec2(56.0, 34.0),
                    );
                    let slot = images.get(ch.logo.as_deref(), ImageSize::Logo);
                    ChannelLogo::paint(ui, &painter, logo, &ch.name, &slot);
                    // Right: which tile already shows it.
                    let mut right = rect.right() - 16.0;
                    if let Some(t) = tile_keys
                        .iter()
                        .position(|k| k.as_deref() == Some(ch.key.as_str()))
                    {
                        let r = painter.text(
                            pos2(right, rect.center().y),
                            Align2::RIGHT_CENTER,
                            format!("In tile {}", t + 1),
                            font::caption(),
                            color::TEXT_3,
                        );
                        right = r.left() - 16.0;
                    }
                    let tx = logo.right() + 12.0;
                    let tw = (right - tx).max(40.0);
                    let (_, now_p, _) = session.now_next(ch, now);
                    let name_y = if now_p.is_some() {
                        rect.top() + 18.0
                    } else {
                        rect.center().y
                    };
                    text::line(
                        &painter,
                        pos2(tx, name_y),
                        Align2::LEFT_CENTER,
                        &ch.name,
                        font::body_strong(),
                        color::TEXT,
                        tw,
                    );
                    if let Some(p) = &now_p {
                        text::line(
                            &painter,
                            pos2(tx, rect.top() + 39.0),
                            Align2::LEFT_CENTER,
                            &p.title,
                            font::caption(),
                            color::TEXT_2,
                            tw,
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
        let i = activated?;
        let ch = self.visible.get(i)?.clone();
        Some((
            ch,
            ZapContext {
                channels: Arc::clone(&self.visible),
                index: i,
            },
        ))
    }
}

/// Why number entry found nothing: no such channel, it is in a locked category, or in
/// hidden ones only.
pub(super) fn number_missing(
    number: u64,
    catalog: &LiveCatalog,
    locks: &LockedCategories,
    hidden: &HiddenCategories,
) -> String {
    let with_number = || catalog.channels.iter().filter(|c| c.number == Some(number));
    if with_number().any(|c| locks.hides(c)) {
        format!("Channel {number} is in a locked category. Open it in Live TV with your PIN first.")
    } else if with_number().any(|c| hidden.hides(&c.categories)) {
        format!(
            "Channel {number} is in a hidden category. Show it again in Settings \u{203a} Categories."
        )
    } else {
        format!("There's no channel {number}.")
    }
}

const DIGITS: [(Key, char); 10] = [
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
];

/// Digits pressed this frame (consumed), for number entry.
pub(super) fn take_digits(ctx: &egui::Context) -> Vec<char> {
    ctx.input_mut(|i| {
        DIGITS
            .iter()
            .filter(|(k, _)| i.consume_key(Modifiers::NONE, *k))
            .map(|(_, c)| *c)
            .collect()
    })
}

/// Adds typed digits to a number entry (at most 5 digits; the timeout restarts).
pub(super) fn push_digits(entry: &mut Option<(String, Instant)>, digits: &[char]) {
    if digits.is_empty() {
        return;
    }
    let e = entry.get_or_insert_with(|| (String::new(), Instant::now()));
    for d in digits {
        if e.0.len() < 5 {
            e.0.push(*d);
        }
    }
    e.1 = Instant::now();
}

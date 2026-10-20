// SPDX-License-Identifier: GPL-3.0-or-later
//! Favourites: tabs Live TV / Films / Series, rendered from the store's snapshots (no
//! provider request needed to show them), in the user's order.
//!
//! - **Live TV** is a list: Enter plays the channel (zapping through the favourite
//!   channels). The focused row shows its ▲ ▼ ✕ buttons (remote: → from the row);
//!   Ctrl/Shift+↑/↓ reorder, Delete removes.
//! - **Films / Series** are a poster grid: Enter opens the detail page. The focused
//!   poster shows ◀ ▶ ✕ buttons under its title (remote: ↓ from the poster);
//!   Ctrl/Shift+←/→ move it, Delete removes.
//! - Removing offers Undo for a few seconds; the order is saved in the background
//!   (`Favourites::reorder`).
//! - **Parental control:** a favourite in a locked category shows as "Locked" (no
//!   artwork, no title) and asks for the PIN before it plays or opens; the PIN unlocks
//!   its categories for the session. Until the category names are known, items with a
//!   category count as locked (fail closed).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, Id, Key, Modifiers, Rect, Ui, pos2, vec2};
use opensmarters_core::CategoryId;
use opensmarters_store::{ContentKind, Favourite};

use super::common::locks::{self, CategoryNames};
use super::{AppCtx, Screen, shell};
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{Route, Section};
use crate::playback::ZapContext;
use crate::services::catalog::{LiveCatalog, LiveChannel, split_tag};
use crate::services::{ImageSize, ImageSlot, Session, vod};
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons;
use crate::ui::layout::Layout;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{
    Button, ChannelLogo, EmptyState, IconButton, PinPrompt, PinPromptResult, PosterCard, Tabs,
    VirtualList,
};
use crate::ui::{paint, text};

const ROW_H: f32 = 88.0;
const UNDO_FOR: Duration = Duration::from_secs(8);
const KINDS: [ContentKind; 3] = [ContentKind::Live, ContentKind::Movie, ContentKind::Series];
/// Room under a poster's title for its action buttons.
const STRIP_H: f32 = 60.0;
/// Action buttons of the focused item.
const ACTION_D: f32 = 40.0;

/// Moves the item at `from` by `delta` places (clamped). Returns its new index, or `None`
/// when it can't move.
pub fn move_item<T>(list: &mut Vec<T>, from: usize, delta: isize) -> Option<usize> {
    if from >= list.len() || delta == 0 {
        return None;
    }
    let to = (from as isize + delta).clamp(0, list.len() as isize - 1) as usize;
    if to == from {
        return None;
    }
    let item = list.remove(from);
    list.insert(to, item);
    Some(to)
}

fn row_id(kind: ContentKind, item: &str) -> Id {
    Id::unique(("fav.row", kind, item))
}

fn action_id(kind: ContentKind, item: &str, action: &str) -> Id {
    Id::unique(("fav.action", kind, item, action))
}

const ACTIONS: [&str; 3] = ["up", "down", "remove"];

fn kind_label(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Live => "Live TV",
        ContentKind::Movie => "Films",
        ContentKind::Series => "Series",
    }
}

#[derive(Debug, Clone)]
struct Removed {
    kind: ContentKind,
    favourite: Favourite,
    order: Vec<String>,
    at: Instant,
    locked: bool,
}

/// Film and series category names (keyword locks match names). `None` = unknown.
#[derive(Debug, Clone, Default)]
struct VodNames {
    movies: Option<CategoryNames>,
    series: Option<CategoryNames>,
}

/// The PIN asked before a locked favourite opens.
#[derive(Debug)]
struct Gate {
    prompt: PinPrompt,
    kind: ContentKind,
    item: String,
    unlock: Vec<CategoryId>,
}

/// What a row/card asked for this frame.
#[derive(Debug, Default)]
struct Asked {
    open: Option<String>,
    moved: Option<(String, isize)>,
    removed: Option<String>,
}

/// The Favourites screen.
#[derive(Debug)]
pub struct FavouritesScreen {
    tab: usize,
    lists: Loader<[Vec<Favourite>; 3]>,
    catalog: Loader<Arc<LiveCatalog>>,
    names: Loader<VodNames>,
    removed: Option<Removed>,
    focus_after: Option<Id>,
    loaded_for: Option<[usize; 3]>,
    gate: Option<Gate>,
}

impl Default for FavouritesScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl FavouritesScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {
            tab: 0,
            lists: Loader::new(),
            catalog: Loader::new(),
            names: Loader::new(),
            removed: None,
            focus_after: None,
            loaded_for: None,
            gate: None,
        }
    }

    fn kind(&self) -> ContentKind {
        KINDS.get(self.tab).copied().unwrap_or(ContentKind::Live)
    }

    fn load(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let store = cx.services.store.clone();
        let profile = session.profile_id();
        self.loaded_for = Some(KINDS.map(|k| session.favourite_ids(k).len()));
        self.lists.start_blocking(cx.rt(), &cx.egui, move || {
            let f = store.favourites();
            Ok([
                f.list(profile, ContentKind::Live).map_err(AppError::from)?,
                f.list(profile, ContentKind::Movie)
                    .map_err(AppError::from)?,
                f.list(profile, ContentKind::Series)
                    .map_err(AppError::from)?,
            ])
        });
        if self.catalog.is_idle() || self.catalog.error().is_some() {
            let session = Arc::clone(&session);
            self.catalog.start(cx.rt(), &cx.egui, move |_| async move {
                session.live_catalog().await
            });
        }
        // Category names for keyword locks (only needed while parental control is on).
        if session.is_xtream()
            && session.parental_enabled()
            && (self.names.is_idle() || self.names.error().is_some())
        {
            self.names.start(cx.rt(), &cx.egui, move |_| async move {
                let movies = session.vod_categories().await.ok();
                let series = session.series_categories().await.ok();
                Ok(VodNames {
                    movies: movies.map(|c| CategoryNames::new(c.iter())),
                    series: series.map(|c| CategoryNames::new(c.iter())),
                })
            });
        }
    }

    fn list(&self, kind: ContentKind) -> &[Favourite] {
        let i = KINDS.iter().position(|k| *k == kind).unwrap_or(0);
        self.lists.value().map_or(&[], |l| l[i].as_slice())
    }

    fn list_mut(&mut self, kind: ContentKind) -> Option<&mut Vec<Favourite>> {
        let i = KINDS.iter().position(|k| *k == kind).unwrap_or(0);
        self.lists.value_mut().map(|l| &mut l[i])
    }

    /// The categories that lock `fav` for this session (empty = not locked). Unknown
    /// names count as locked (fail closed) for items that have a category.
    fn lock_of(&self, session: &Session, kind: ContentKind, fav: &Favourite) -> Vec<CategoryId> {
        if !session.parental_enabled() {
            return Vec::new();
        }
        let primary = fav
            .snapshot
            .category_id
            .as_deref()
            .map(|s| CategoryId::from(s.to_owned()));
        match kind {
            ContentKind::Live => {
                let Some(cat) = self.catalog.value() else {
                    return primary.into_iter().collect();
                };
                let names = CategoryNames::new(&cat.categories);
                let ids: Vec<&CategoryId> = match cat.get(&fav.item_id) {
                    Some(ch) => ch.categories.iter().collect(),
                    None => primary.iter().collect(),
                };
                locks::locked_among(session, kind, &ids, &names)
            }
            ContentKind::Movie | ContentKind::Series => {
                let Some(id) = primary else {
                    return Vec::new();
                };
                let names = self.names.value().and_then(|n| {
                    if kind == ContentKind::Movie {
                        n.movies.as_ref()
                    } else {
                        n.series.as_ref()
                    }
                });
                match names {
                    Some(names) => locks::locked_among(session, kind, &[&id], names),
                    None => vec![id],
                }
            }
        }
    }

    fn save_order(cx: &AppCtx<'_>, session: &Session, kind: ContentKind, ids: Vec<String>) {
        let profile = session.profile_id();
        cx.services.blocking("reorder favourites", move |s| {
            s.favourites().reorder(profile, kind, &ids)
        });
    }

    fn reorder(&mut self, cx: &mut AppCtx<'_>, kind: ContentKind, item: &str, delta: isize) {
        let Some(session) = cx.session() else { return };
        let Some(list) = self.list_mut(kind) else {
            return;
        };
        let Some(from) = list.iter().position(|f| f.item_id == item) else {
            return;
        };
        if move_item(list, from, delta).is_some() {
            let ids: Vec<String> = list.iter().map(|f| f.item_id.clone()).collect();
            Self::save_order(cx, &session, kind, ids);
        }
    }

    fn remove(&mut self, cx: &mut AppCtx<'_>, kind: ContentKind, item: &str) {
        let Some(session) = cx.session() else { return };
        let locked = self
            .list(kind)
            .iter()
            .find(|f| f.item_id == item)
            .is_some_and(|f| !self.lock_of(&session, kind, f).is_empty());
        let Some(list) = self.list_mut(kind) else {
            return;
        };
        let Some(pos) = list.iter().position(|f| f.item_id == item) else {
            return;
        };
        let order: Vec<String> = list.iter().map(|f| f.item_id.clone()).collect();
        let fav = list.remove(pos);
        // The next item (or the previous one) keeps the focus.
        self.focus_after = list
            .get(pos)
            .or_else(|| pos.checked_sub(1).and_then(|p| list.get(p)))
            .map(|f| row_id(kind, &f.item_id));
        if session.is_favourite(kind, &fav.item_id) {
            session.toggle_favourite(kind, &fav.item_id, fav.snapshot.clone());
        }
        self.loaded_for = Some(KINDS.map(|k| session.favourite_ids(k).len()));
        self.removed = Some(Removed {
            kind,
            favourite: fav,
            order,
            at: Instant::now(),
            locked,
        });
    }

    fn undo(&mut self, cx: &mut AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        let Some(r) = self.removed.take() else { return };
        if !session.is_favourite(r.kind, &r.favourite.item_id) {
            session.toggle_favourite(r.kind, &r.favourite.item_id, r.favourite.snapshot.clone());
        }
        // Put it back where it was (the store appends; reorder after the add lands).
        let profile = session.profile_id();
        let kind = r.kind;
        let order = r.order.clone();
        cx.services.blocking("restore favourite order", move |s| {
            s.favourites().reorder(profile, kind, &order)
        });
        let id = row_id(r.kind, &r.favourite.item_id);
        if let Some(list) = self.list_mut(r.kind) {
            let pos = r
                .order
                .iter()
                .position(|i| *i == r.favourite.item_id)
                .unwrap_or(list.len())
                .min(list.len());
            list.insert(pos, r.favourite);
        }
        self.loaded_for = Some(KINDS.map(|k| session.favourite_ids(k).len()));
        self.focus_after = Some(id);
    }

    /// Opens (or asks the PIN for) the favourite `item`.
    fn request_open(&mut self, cx: &mut AppCtx<'_>, kind: ContentKind, item: &str) {
        let Some(session) = cx.session() else { return };
        let unlock = self
            .list(kind)
            .iter()
            .find(|f| f.item_id == item)
            .map(|f| self.lock_of(&session, kind, f))
            .unwrap_or_default();
        if unlock.is_empty() {
            self.open(cx, kind, item);
        } else {
            self.gate = Some(Gate {
                prompt: PinPrompt::new(Id::unique("fav.pin")),
                kind,
                item: item.to_owned(),
                unlock,
            });
        }
    }

    fn open(&mut self, cx: &mut AppCtx<'_>, kind: ContentKind, item: &str) {
        let Some(session) = cx.session() else { return };
        match kind {
            ContentKind::Live => {
                let Some(cat) = self.catalog.value().cloned() else {
                    cx.toasts.info("Channels are still loading…");
                    return;
                };
                // Zap through the favourite channels that aren't locked.
                let channels: Vec<LiveChannel> = self
                    .list(kind)
                    .iter()
                    .filter(|f| f.item_id == item || self.lock_of(&session, kind, f).is_empty())
                    .filter_map(|f| cat.get(&f.item_id).cloned())
                    .collect();
                let Some(index) = channels.iter().position(|c| c.key == item) else {
                    cx.toasts
                        .warning("This channel is no longer offered by your provider.");
                    return;
                };
                let channels = Arc::new(channels);
                if let Some(ch) = channels.get(index) {
                    match session.live_request(
                        ch,
                        Some(ZapContext {
                            channels: Arc::clone(&channels),
                            index,
                        }),
                    ) {
                        Ok(req) => cx.play(req, true),
                        Err(e) => cx.toasts.error(e.user_message()),
                    }
                }
            }
            ContentKind::Movie => match item.parse::<u64>() {
                Ok(stream_id) => cx.nav.push(Route::MovieDetail { stream_id }),
                Err(_) => cx.toasts.warning("This film can't be opened."),
            },
            ContentKind::Series => match item.parse::<u64>() {
                Ok(series_id) => cx.nav.push(Route::SeriesDetail { series_id }),
                Err(_) => cx.toasts.warning("This series can't be opened."),
            },
        }
    }

    /// The favourite whose row/card (or one of its buttons) has the focus.
    fn focused_item(&self, ctx: &egui::Context, kind: ContentKind) -> Option<String> {
        let cur = focus::current(ctx)?;
        self.list(kind)
            .iter()
            .find(|f| {
                cur == row_id(kind, &f.item_id)
                    || ACTIONS
                        .iter()
                        .any(|a| cur == action_id(kind, &f.item_id, a))
            })
            .map(|f| f.item_id.clone())
    }

    fn apply(&mut self, cx: &mut AppCtx<'_>, kind: ContentKind, asked: Asked) {
        if let Some(id) = asked.open {
            self.request_open(cx, kind, &id);
        }
        if let Some((id, delta)) = asked.moved {
            self.reorder(cx, kind, &id, delta);
            // Keep the focus on the same button of the moved item (if it still exists).
            let list = self.list(kind);
            let pos = list.iter().position(|f| f.item_id == id).unwrap_or(0);
            let still = if delta < 0 {
                pos > 0
            } else {
                pos + 1 < list.len()
            };
            let action = if delta < 0 { "up" } else { "down" };
            self.focus_after = Some(if still {
                action_id(kind, &id, action)
            } else {
                row_id(kind, &id)
            });
        }
        if let Some(id) = asked.removed {
            self.remove(cx, kind, &id);
        }
    }
}

impl Screen for FavouritesScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.load(cx);
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if let Some(g) = self.gate.take() {
            focus::request(&cx.egui, row_id(g.kind, &g.item));
            return true;
        }
        false
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if self.gate.is_some() || focus::text_input_focused(ctx) {
            return;
        }
        let kind = self.kind();
        let Some(item) = self.focused_item(ctx, kind) else {
            return;
        };
        // Lists move with ↑/↓, poster grids with ←/→.
        let (back_key, fwd_key) = if kind == ContentKind::Live {
            (Key::ArrowUp, Key::ArrowDown)
        } else {
            (Key::ArrowLeft, Key::ArrowRight)
        };
        let (back, fwd, del) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::CTRL, back_key)
                    || i.consume_key(Modifiers::SHIFT, back_key),
                i.consume_key(Modifiers::CTRL, fwd_key) || i.consume_key(Modifiers::SHIFT, fwd_key),
                i.consume_key(Modifiers::NONE, Key::Delete),
            )
        });
        if back || fwd {
            self.reorder(cx, kind, &item, if back { -1 } else { 1 });
            self.focus_after = Some(row_id(kind, &item));
        }
        if del {
            self.remove(cx, kind, &item);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Favourites)
    }

    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.lists.poll();
        self.catalog.poll();
        self.names.poll();
        let Some(session) = cx.session() else { return };
        // Favourites changed elsewhere (e.g. ♥ in Live TV while this screen was covered).
        let counts = KINDS.map(|k| session.favourite_ids(k).len());
        if self.loaded_for.is_some_and(|c| c != counts) && !self.lists.is_loading() {
            self.load(cx);
        }
        if self
            .removed
            .as_ref()
            .is_some_and(|r| r.at.elapsed() > UNDO_FOR)
        {
            self.removed = None;
        }
        shell::status_cluster(cx, ui);
        shell::page_title(cx, ui, "Favourites");
        let content = cx.layout.content;
        let m = cx.layout.margin;
        let kind = self.kind();
        // Lists read best at a comfortable width; poster grids use the page.
        let width = if kind == ContentKind::Live {
            (content.width() - 2.0 * m).min(1100.0)
        } else {
            content.width() - 2.0 * m
        };

        // Tabs.
        let labels: Vec<String> = KINDS
            .iter()
            .map(|k| {
                let n = self.list(*k).len();
                if self.lists.value().is_some() {
                    format!("{} {n}", kind_label(*k))
                } else {
                    kind_label(*k).to_owned()
                }
            })
            .collect();
        let tabs = Rect::from_min_size(
            pos2(content.left() + m - 4.0, content.top() + 92.0),
            vec2(width, 48.0),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(tabs));
        let mut tab = self.tab;
        focus::scope(&mut child, Id::unique("fav.tabs"), |ui| {
            Tabs::new(Id::unique("fav.tab"), &labels).show(ui, &mut tab)
        });
        if tab != self.tab {
            self.tab = tab;
            self.removed = None;
        }
        let kind = self.kind();
        let painter = ui.painter().clone();
        let mut top = tabs.bottom() + 16.0;

        // Undo bar.
        if let Some(r) = self.removed.clone().filter(|r| r.kind == kind) {
            let bar =
                Rect::from_min_size(pos2(content.left() + m, top), vec2(width.min(1100.0), 52.0));
            painter.rect_filled(bar, CornerRadius::same(radius::MD), color::SURFACE_2);
            let what = if r.locked {
                "Removed a locked item from Favourites.".to_owned()
            } else {
                format!(
                    "Removed \u{201c}{}\u{201d} from Favourites.",
                    text::cap(&r.favourite.snapshot.name, 60)
                )
            };
            text::line(
                &painter,
                pos2(bar.left() + 20.0, bar.center().y),
                Align2::LEFT_CENTER,
                &what,
                font::body(),
                color::TEXT,
                bar.width() - 200.0,
            );
            let b = Button::ghost("Undo")
                .icon(icons::ARROW_COUNTER_CLOCKWISE)
                .small()
                .id(Id::unique("fav.undo"));
            let sz = b.size(ui);
            let br = Rect::from_min_size(
                pos2(bar.right() - sz.x - 12.0, bar.center().y - sz.y / 2.0),
                sz,
            );
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(br));
            if focus::scope(&mut c, Id::unique("fav.undo-scope"), |ui| b.show(ui)).activated {
                self.undo(cx);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(500));
            top = bar.bottom() + 12.0;
        }

        let rows = Rect::from_min_max(
            pos2(content.left() + m - 8.0, top),
            pos2(content.left() + m + width + 8.0, content.bottom() - 16.0),
        );
        let Some(_) = self.lists.value() else {
            if let Some(err) = self.lists.error().cloned() {
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rows));
                let f = EmptyState::new(
                    Id::unique("fav.error"),
                    Illustration::ErrorOffline,
                    err.title(),
                    &err.user_message(),
                )
                .action("Retry", true)
                .show(&mut c);
                if f.is_some_and(|f| f.activated) {
                    self.load(cx);
                }
            } else if self.lists.show_skeleton() {
                let mut y = rows.top() + 8.0;
                while y + ROW_H < rows.bottom() {
                    skeleton(
                        &cx.egui,
                        &painter,
                        Rect::from_min_size(pos2(rows.left() + 56.0, y + 12.0), vec2(64.0, 40.0)),
                        radius::SM,
                    );
                    skeleton(
                        &cx.egui,
                        &painter,
                        Rect::from_min_size(pos2(rows.left() + 136.0, y + 24.0), vec2(220.0, 14.0)),
                        radius::XS,
                    );
                    y += ROW_H + 8.0;
                }
            }
            return;
        };
        let list: Vec<Favourite> = self.list(kind).to_vec();
        if list.is_empty() {
            // Centred on the page (a little above the middle), not below the tabs.
            let centre = content.top() + content.height() * 0.45;
            let half = (centre - top).min(content.bottom() - centre).max(120.0);
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
                pos2(content.left(), centre - half),
                pos2(content.right(), centre + half),
            )));
            let (body, action, route) = match kind {
                ContentKind::Live => (
                    "Press \u{2665} next to a playing channel in Live TV to keep it here.",
                    "Browse Live TV",
                    Route::live(),
                ),
                ContentKind::Movie => (
                    "Press \u{2665} on any film to keep it here.",
                    "Browse films",
                    Route::Movies { category: None },
                ),
                ContentKind::Series => (
                    "Press \u{2665} on any series to keep it here.",
                    "Browse series",
                    Route::Series { category: None },
                ),
            };
            let f = EmptyState::new(
                Id::unique(("fav.empty", kind)),
                Illustration::EmptyFavourites,
                "Nothing saved yet",
                body,
            )
            .action(action, true)
            .illustration_size(if cx.layout.tv { 150.0 } else { 200.0 })
            .show(&mut c);
            if f.is_some_and(|f| f.activated) {
                cx.nav.section(route);
            }
            return;
        }

        if let Some(id) = self.focus_after.take() {
            focus::request(&cx.egui, id);
        }
        let locked: Vec<bool> = list
            .iter()
            .map(|f| !self.lock_of(&session, kind, f).is_empty())
            .collect();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows));
        let asked = if kind == ContentKind::Live {
            self.live_list(cx, &mut child, &session, &list, &locked)
        } else {
            self.poster_grid(cx, &mut child, kind, &list, &locked)
        };
        self.apply(cx, kind, asked);

        // Hint (top right, quiet).
        let hint = match (kind == ContentKind::Live, cx.layout.tv) {
            (true, false) => "Enter to play · Ctrl+\u{2191}/\u{2193} to reorder · Delete to remove",
            (true, true) => "Enter to play · \u{2192} for move and remove",
            (false, false) => "Enter to open · Ctrl+\u{2190}/\u{2192} to move · Delete to remove",
            (false, true) => "Enter to open · \u{2193} for move and remove",
        };
        painter.text(
            pos2(content.left() + m + width, content.top() + 116.0),
            Align2::RIGHT_CENTER,
            hint,
            font::caption(),
            color::TEXT_3,
        );

        self.overlays(cx, ui);
    }
}

impl FavouritesScreen {
    /// Live TV: a list; the focused row shows ▲ ▼ ✕.
    #[allow(clippy::too_many_lines)]
    fn live_list(
        &self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        list: &[Favourite],
        locked: &[bool],
    ) -> Asked {
        let kind = ContentKind::Live;
        let catalog = self.catalog.value().cloned();
        let images = cx.services.images.clone();
        let n = list.len();
        let mut asked = Asked::default();
        let ctx = ui.ctx().clone();
        VirtualList::new(Id::unique(("fav.list", kind)), ROW_H, n)
            .gap(8.0)
            .show(
                ui,
                |i| list.get(i).map_or(Id::NULL, |f| row_id(kind, &f.item_id)),
                |ui, i, rect| {
                    let Some(fav) = list.get(i) else { return };
                    let id = &fav.item_id;
                    let is_locked = locked.get(i).copied().unwrap_or(false);
                    let active = focus::is_focused(&ctx, row_id(kind, id))
                        || ACTIONS
                            .iter()
                            .any(|a| focus::is_focused(&ctx, action_id(kind, id, a)));
                    // Buttons on the right, only for the focused row.
                    let bw = 44.0;
                    let gap = 8.0;
                    let actions_w = if active {
                        bw * 3.0 + gap * 2.0 + 16.0
                    } else {
                        0.0
                    };
                    let row_rect =
                        Rect::from_min_max(rect.min, pos2(rect.right() - actions_w, rect.bottom()));
                    let (_, f) = focus::focusable(
                        ui,
                        row_id(kind, id),
                        row_rect,
                        FocusOpts::default().autofocus_if(i == 0),
                    );
                    let painter = ui.painter().clone();
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(radius::MD),
                        if active {
                            color::SURFACE_3
                        } else {
                            color::SURFACE_1
                        },
                    );
                    painter.text(
                        pos2(rect.left() + 36.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{}", i + 1),
                        font::caption_strong(),
                        color::TEXT_3,
                    );
                    let logo = Rect::from_min_size(
                        pos2(rect.left() + 52.0, rect.center().y - 20.0),
                        vec2(64.0, 40.0),
                    );
                    let tx = logo.right() + 16.0;
                    let tw = (row_rect.right() - tx - 16.0).max(80.0);
                    let (title, sub): (String, String) = if is_locked {
                        paint_lock_plate(&painter, logo, active);
                        (
                            "Locked channel".to_owned(),
                            "In a locked category · Enter your PIN to watch".to_owned(),
                        )
                    } else {
                        let slot = images.get(fav.snapshot.icon_url.as_deref(), ImageSize::Logo);
                        ChannelLogo::paint(ui, &painter, logo, &fav.snapshot.name, &slot);
                        let sub = match catalog.as_deref().map(|c| c.get(id)) {
                            Some(Some(ch)) => {
                                let (_, now_p, _) = session.now_next(ch, cx.now);
                                now_p.map_or_else(
                                    || {
                                        ch.number
                                            .map_or_else(String::new, |n| format!("Channel {n}"))
                                    },
                                    |p| format!("Now: {}", p.title),
                                )
                            }
                            Some(None) => "No longer offered by your provider".to_owned(),
                            None => String::new(),
                        };
                        (split_tag(&fav.snapshot.name).1.to_owned(), sub)
                    };
                    let mut nx = tx;
                    if !is_locked && let Some(tag) = split_tag(&fav.snapshot.name).0 {
                        let r = crate::ui::widgets::chip::tag(
                            &painter,
                            pos2(nx, rect.center().y - 22.0),
                            tag,
                        );
                        nx = r.right() + 8.0;
                    }
                    text::line(
                        &painter,
                        pos2(nx, rect.center().y - 12.0),
                        Align2::LEFT_CENTER,
                        &title,
                        font::body_strong(),
                        if active { color::TEXT } else { color::TEXT_2 },
                        (tw - (nx - tx)).max(40.0),
                    );
                    text::line(
                        &painter,
                        pos2(tx, rect.center().y + 14.0),
                        Align2::LEFT_CENTER,
                        &sub,
                        font::caption(),
                        color::TEXT_3,
                        tw,
                    );
                    if f.focused {
                        paint::focus_ring(&painter, rect, radius::MD, 1.0);
                    }
                    if f.activated {
                        asked.open = Some(id.clone());
                    }
                    if !active {
                        return;
                    }
                    let mut x = rect.right() - 16.0 - bw;
                    let cy = rect.center().y;
                    for (action, glyph, enabled) in [
                        ("remove", icons::X, true),
                        ("down", icons::ARROW_DOWN, i + 1 < n),
                        ("up", icons::ARROW_UP, i > 0),
                    ] {
                        let br = Rect::from_min_size(pos2(x, cy - bw / 2.0), vec2(bw, bw));
                        x -= bw + gap;
                        if action_button(ui, kind, id, action, glyph, enabled, br) {
                            match action {
                                "up" => asked.moved = Some((id.clone(), -1)),
                                "down" => asked.moved = Some((id.clone(), 1)),
                                _ => asked.removed = Some(id.clone()),
                            }
                        }
                    }
                },
            );
        asked
    }

    /// Films / series: a poster grid; the focused poster shows ◀ ▶ ✕ under its title.
    fn poster_grid(
        &self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        kind: ContentKind,
        list: &[Favourite],
        locked: &[bool],
    ) -> Asked {
        let pw = cx.layout.poster_width;
        let pad = 16.0;
        let gap = vec2(16.0, 24.0);
        let cell = vec2(pw, PosterCard::height(pw) + STRIP_H);
        let cols = Layout::grid_columns(ui.available_width() - 2.0 * pad, pw);
        let n = list.len();
        let images = cx.services.images.clone();
        let ctx = ui.ctx().clone();
        let tv = cx.layout.tv;
        let mut asked = Asked::default();
        egui::ScrollArea::vertical()
            .id_salt(("fav.grid.scroll", kind))
            .auto_shrink([false, false])
            .content_margin(egui::Margin::same(pad as i8))
            .show(ui, |ui| {
                focus::scope(ui, Id::unique(("fav.grid", kind)), |ui| {
                    let rows = n.div_ceil(cols);
                    let total = vec2(
                        cols as f32 * (cell.x + gap.x) - gap.x,
                        rows as f32 * (cell.y + gap.y) - gap.y,
                    );
                    let (area, _) = ui.allocate_exact_size(total, egui::Sense::hover());
                    for (i, fav) in list.iter().enumerate() {
                        let (r, c) = (i / cols, i % cols);
                        let min = area.min
                            + vec2(c as f32 * (cell.x + gap.x), r as f32 * (cell.y + gap.y));
                        let cell_rect = Rect::from_min_size(min, cell);
                        let id = &fav.item_id;
                        let is_locked = locked.get(i).copied().unwrap_or(false);
                        let active = focus::is_focused(&ctx, row_id(kind, id))
                            || ACTIONS
                                .iter()
                                .any(|a| focus::is_focused(&ctx, action_id(kind, id, a)));
                        let card_rect = Rect::from_min_size(min, vec2(pw, PosterCard::height(pw)));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card_rect));
                        let t = vod::display_title(&fav.snapshot.name);
                        let meta = vod::meta_line([
                            t.year.map(|y| y.to_string()).unwrap_or_default(),
                            format!("Saved {}", fav.added_at_label()),
                        ]);
                        let f = if is_locked {
                            let f =
                                PosterCard::new(row_id(kind, id), &ImageSlot::None, "Locked", pw)
                                    .meta(Some("PIN needed"))
                                    .autofocus(i == 0)
                                    .show(&mut child);
                            // A lock over the placeholder (the title stays hidden).
                            icons::paint(
                                ui.painter(),
                                card_rect.center_top() + vec2(0.0, pw * 0.75 - 40.0),
                                icons::LOCK_SIMPLE,
                                32.0,
                                icons::Style::Regular,
                                color::TEXT_2,
                            );
                            f
                        } else {
                            let img =
                                images.get(fav.snapshot.icon_url.as_deref(), ImageSize::Poster);
                            PosterCard::new(row_id(kind, id), &img, t.title, pw)
                                .meta(Some(meta.as_str()).filter(|m| !m.is_empty()))
                                .meta_on_focus_only(tv)
                                .autofocus(i == 0)
                                .show(&mut child)
                        };
                        if f.activated {
                            asked.open = Some(id.clone());
                        }
                        if !active {
                            continue;
                        }
                        // ◀ ▶ ✕ under the title.
                        let y = cell_rect.bottom() - ACTION_D / 2.0 - 2.0;
                        let spacing = 8.0;
                        let row_w = ACTION_D * 3.0 + spacing * 2.0;
                        let mut x = cell_rect.center().x - row_w / 2.0;
                        for (action, glyph, enabled) in [
                            ("up", icons::ARROW_LEFT, i > 0),
                            ("down", icons::ARROW_RIGHT, i + 1 < n),
                            ("remove", icons::X, true),
                        ] {
                            let br = Rect::from_min_size(
                                pos2(x, y - ACTION_D / 2.0),
                                vec2(ACTION_D, ACTION_D),
                            );
                            x += ACTION_D + spacing;
                            if action_button(ui, kind, id, action, glyph, enabled, br) {
                                match action {
                                    "up" => asked.moved = Some((id.clone(), -1)),
                                    "down" => asked.moved = Some((id.clone(), 1)),
                                    _ => asked.removed = Some(id.clone()),
                                }
                            }
                        }
                    }
                });
            });
        asked
    }

    fn overlays(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(g) = &mut self.gate else { return };
        let title = match g.kind {
            ContentKind::Live => "Enter your PIN to watch this channel",
            ContentKind::Movie => "Enter your PIN to open this film",
            ContentKind::Series => "Enter your PIN to open this series",
        };
        match g.prompt.show_dialog(
            ui.ctx(),
            cx.services,
            title,
            Some(
                "It's in a locked category. The category locks again after 30 minutes without use.",
            ),
        ) {
            PinPromptResult::Verified(_) => {
                let (kind, item, unlock) = (g.kind, g.item.clone(), g.unlock.clone());
                self.gate = None;
                if let Some(session) = cx.session() {
                    for id in &unlock {
                        session.unlock_category(kind, id);
                    }
                }
                focus::request(&cx.egui, row_id(kind, &item));
                self.open(cx, kind, &item);
            }
            PinPromptResult::Cancelled | PinPromptResult::Forgot => {
                focus::request(&cx.egui, row_id(g.kind, &g.item));
                self.gate = None;
            }
            PinPromptResult::Pending => {}
        }
    }
}

/// One of the move/remove buttons of the focused item (a disabled one is drawn dimmed and
/// isn't focusable). Returns true when activated.
fn action_button(
    ui: &mut Ui,
    kind: ContentKind,
    item: &str,
    action: &str,
    glyph: &'static str,
    enabled: bool,
    rect: Rect,
) -> bool {
    if !enabled {
        icons::paint(
            ui.painter(),
            rect.center(),
            glyph,
            18.0,
            icons::Style::Regular,
            color::TEXT_DISABLED,
        );
        return false;
    }
    IconButton::new(glyph)
        .size(rect.width(), 18.0)
        .surface()
        .id(action_id(kind, item, action))
        .show_at(ui, rect)
        .activated
}

/// A logo plate with a lock instead of the channel's logo (one step lighter than the
/// row behind it).
fn paint_lock_plate(painter: &egui::Painter, rect: Rect, row_active: bool) {
    let fill = if row_active {
        color::SURFACE_4
    } else {
        color::SURFACE_3
    };
    painter.rect_filled(rect, CornerRadius::same(radius::SM), fill);
    icons::paint(
        painter,
        rect.center(),
        icons::LOCK_SIMPLE,
        20.0,
        icons::Style::Regular,
        color::TEXT_2,
    );
}

trait AddedLabel {
    fn added_at_label(&self) -> String;
}

impl AddedLabel for Favourite {
    fn added_at_label(&self) -> String {
        use opensmarters_core::chrono::{DateTime, Local};
        DateTime::from_timestamp(self.added_at.as_unix_secs(), 0)
            .map(|t| t.with_timezone(&Local).format("%-d %b %Y").to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_items() {
        let mut v = vec!["a", "b", "c", "d"];
        assert_eq!(move_item(&mut v, 1, -1), Some(0));
        assert_eq!(v, ["b", "a", "c", "d"]);
        assert_eq!(move_item(&mut v, 0, -1), None, "already first");
        assert_eq!(move_item(&mut v, 3, 1), None, "already last");
        assert_eq!(move_item(&mut v, 0, 10), Some(3), "clamped");
        assert_eq!(v, ["a", "c", "d", "b"]);
        assert_eq!(move_item(&mut v, 9, 1), None, "out of range");
        assert_eq!(move_item(&mut v, 1, 0), None);
        let mut empty: Vec<u8> = Vec::new();
        assert_eq!(move_item(&mut empty, 0, 1), None);
    }
}

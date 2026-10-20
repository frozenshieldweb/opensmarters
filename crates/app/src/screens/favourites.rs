// SPDX-License-Identifier: GPL-3.0-or-later
//! Favourites: tabs Live TV / Films / Series, rendered from the store's snapshots (no
//! provider request needed to show the list), in the user's order.
//!
//! Each row: Enter plays the channel (zapping through the favourite channels) or opens
//! the film/series. Reorder with the ▲/▼ buttons on the row (remote) or Ctrl+↑/↓
//! (keyboard); remove with ✕ or Delete, with Undo for a few seconds. The order is saved
//! in the background (`Favourites::reorder`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, Id, Key, Modifiers, Rect, Ui, pos2, vec2};
use opensmarters_store::{ContentKind, Favourite};

use super::{AppCtx, Screen, shell};
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{Route, Section};
use crate::playback::ZapContext;
use crate::services::catalog::{LiveCatalog, LiveChannel, split_tag};
use crate::services::{ImageSize, Session, vod};
use crate::task::Loader;
use crate::ui::assets::Illustration;
use crate::ui::icons;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{Button, ChannelLogo, EmptyState, IconButton, Tabs, VirtualList};
use crate::ui::{paint, text};

const ROW_H: f32 = 96.0;
const UNDO_FOR: Duration = Duration::from_secs(8);
const KINDS: [ContentKind; 3] = [ContentKind::Live, ContentKind::Movie, ContentKind::Series];

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
}

/// The Favourites screen.
#[derive(Debug)]
pub struct FavouritesScreen {
    tab: usize,
    lists: Loader<[Vec<Favourite>; 3]>,
    catalog: Loader<Arc<LiveCatalog>>,
    removed: Option<Removed>,
    focus_after: Option<Id>,
    loaded_for: Option<[usize; 3]>,
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
            removed: None,
            focus_after: None,
            loaded_for: None,
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
            self.catalog.start(cx.rt(), &cx.egui, move |_| async move {
                session.live_catalog().await
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
        let Some(list) = self.list_mut(kind) else {
            return;
        };
        let Some(pos) = list.iter().position(|f| f.item_id == item) else {
            return;
        };
        let order: Vec<String> = list.iter().map(|f| f.item_id.clone()).collect();
        let fav = list.remove(pos);
        // The next row (or the previous one) keeps the focus.
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

    fn open(&mut self, cx: &mut AppCtx<'_>, kind: ContentKind, item: &str) {
        let Some(session) = cx.session() else { return };
        match kind {
            ContentKind::Live => {
                let Some(cat) = self.catalog.value().cloned() else {
                    cx.toasts.info("Channels are still loading…");
                    return;
                };
                let channels: Vec<LiveChannel> = self
                    .list(kind)
                    .iter()
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
}

impl Screen for FavouritesScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.load(cx);
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if focus::text_input_focused(ctx) {
            return;
        }
        let kind = self.kind();
        // Which favourite row (or its buttons) has the focus?
        let Some(cur) = focus::current(ctx) else {
            return;
        };
        let item = self
            .list(kind)
            .iter()
            .find(|f| {
                let id = &f.item_id;
                cur == row_id(kind, id)
                    || cur == action_id(kind, id, "up")
                    || cur == action_id(kind, id, "down")
                    || cur == action_id(kind, id, "remove")
            })
            .map(|f| f.item_id.clone());
        let Some(item) = item else { return };
        let (up, down, del) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::CTRL, Key::ArrowUp)
                    || i.consume_key(Modifiers::SHIFT, Key::ArrowUp),
                i.consume_key(Modifiers::CTRL, Key::ArrowDown)
                    || i.consume_key(Modifiers::SHIFT, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Delete),
            )
        });
        if up || down {
            self.reorder(cx, kind, &item, if up { -1 } else { 1 });
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
        let width = (content.width() - 2.0 * m).min(1100.0);

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
            let bar = Rect::from_min_size(pos2(content.left() + m, top), vec2(width, 52.0));
            painter.rect_filled(bar, CornerRadius::same(radius::MD), color::SURFACE_2);
            text::line(
                &painter,
                pos2(bar.left() + 20.0, bar.center().y),
                Align2::LEFT_CENTER,
                &format!(
                    "Removed \u{201c}{}\u{201d} from Favourites.",
                    text::cap(&r.favourite.snapshot.name, 60)
                ),
                font::body(),
                color::TEXT,
                width - 200.0,
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
                        Rect::from_min_size(pos2(rows.left() + 56.0, y + 12.0), vec2(56.0, 72.0)),
                        radius::SM,
                    );
                    skeleton(
                        &cx.egui,
                        &painter,
                        Rect::from_min_size(pos2(rows.left() + 132.0, y + 30.0), vec2(220.0, 14.0)),
                        radius::XS,
                    );
                    y += ROW_H + 8.0;
                }
            }
            return;
        };
        let list: Vec<Favourite> = self.list(kind).to_vec();
        if list.is_empty() {
            let mut c = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(content.left(), top), content.max)),
            );
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
            .show(&mut c);
            if f.is_some_and(|f| f.activated) {
                cx.nav.section(route);
            }
            return;
        }

        if let Some(id) = self.focus_after.take() {
            focus::request(&cx.egui, id);
        }
        let catalog = self.catalog.value().cloned();
        let images = cx.services.images.clone();
        let n = list.len();
        let mut open: Option<String> = None;
        let mut moved: Option<(String, isize)> = None;
        let mut removed: Option<String> = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows));
        VirtualList::new(Id::unique(("fav.list", kind)), ROW_H, n)
            .gap(8.0)
            .show(
                &mut child,
                |i| list.get(i).map_or(Id::NULL, |f| row_id(kind, &f.item_id)),
                |ui, i, rect| {
                    let Some(fav) = list.get(i) else { return };
                    let id = &fav.item_id;
                    // Buttons on the right (their own focus targets for remotes).
                    let bw = 44.0;
                    let gap = 8.0;
                    let actions_w = bw * 3.0 + gap * 2.0 + 16.0;
                    let row_rect =
                        Rect::from_min_max(rect.min, pos2(rect.right() - actions_w, rect.bottom()));
                    let (_, f) = focus::focusable(
                        ui,
                        row_id(kind, id),
                        row_rect,
                        FocusOpts::default().autofocus_if(i == 0),
                    );
                    let painter = ui.painter().clone();
                    let any_focus = f.focused
                        || ["up", "down", "remove"]
                            .iter()
                            .any(|a| focus::is_focused(ui.ctx(), action_id(kind, id, a)));
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(radius::MD),
                        if any_focus {
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
                    // Artwork.
                    let (art, tx) = if kind == ContentKind::Live {
                        let r = Rect::from_min_size(
                            pos2(rect.left() + 52.0, rect.center().y - 20.0),
                            vec2(64.0, 40.0),
                        );
                        let slot = images.get(fav.snapshot.icon_url.as_deref(), ImageSize::Logo);
                        ChannelLogo::paint(ui, &painter, r, &fav.snapshot.name, &slot);
                        (r, r.right() + 16.0)
                    } else {
                        let r = Rect::from_min_size(
                            pos2(rect.left() + 52.0, rect.top() + 8.0),
                            vec2(53.0, 80.0),
                        );
                        let slot = images.get(fav.snapshot.icon_url.as_deref(), ImageSize::Poster);
                        match slot.texture() {
                            Some(t) => paint::image_rounded(
                                &painter,
                                t.id(),
                                r,
                                paint::cover_uv(t.size_vec2(), r.size(), vec2(0.5, 0.5)),
                                radius::SM,
                                egui::Color32::WHITE,
                            ),
                            None => {
                                painter.rect_filled(
                                    r,
                                    CornerRadius::same(radius::SM),
                                    color::SURFACE_2,
                                );
                                icons::paint(
                                    &painter,
                                    r.center(),
                                    if kind == ContentKind::Movie {
                                        icons::FILM_STRIP
                                    } else {
                                        icons::MONITOR_PLAY
                                    },
                                    20.0,
                                    icons::Style::Regular,
                                    color::TEXT_3,
                                );
                            }
                        }
                        (r, r.right() + 16.0)
                    };
                    let _ = art;
                    let tw = (row_rect.right() - tx - 16.0).max(80.0);
                    // Films/series: tag chip + title, the year goes to the second line.
                    let (tag, name, year) = if kind == ContentKind::Live {
                        let (tag, name) = split_tag(&fav.snapshot.name);
                        (tag, name, None)
                    } else {
                        let t = vod::display_title(&fav.snapshot.name);
                        (t.tag, t.title, t.year)
                    };
                    let mut nx = tx;
                    if let Some(tag) = tag {
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
                        name,
                        font::body_strong(),
                        color::TEXT,
                        (tw - (nx - tx)).max(40.0),
                    );
                    // Second line.
                    let sub = if kind == ContentKind::Live {
                        match catalog.as_deref().map(|c| c.get(id)) {
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
                        }
                    } else {
                        vod::meta_line([
                            year.map(|y| y.to_string()).unwrap_or_default(),
                            format!("Saved {}", fav.added_at_label()),
                        ])
                    };
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
                        open = Some(id.clone());
                    }
                    // Actions.
                    let mut x = rect.right() - 16.0 - bw;
                    let cy = rect.center().y;
                    for (action, glyph, enabled) in [
                        ("remove", icons::X, true),
                        ("down", icons::ARROW_DOWN, i + 1 < n),
                        ("up", icons::ARROW_UP, i > 0),
                    ] {
                        let br = Rect::from_min_size(pos2(x, cy - bw / 2.0), vec2(bw, bw));
                        x -= bw + gap;
                        if !enabled {
                            icons::paint(
                                &painter,
                                br.center(),
                                glyph,
                                20.0,
                                icons::Style::Regular,
                                color::TEXT_DISABLED,
                            );
                            continue;
                        }
                        let fb = IconButton::new(glyph)
                            .id(action_id(kind, id, action))
                            .show_at(ui, br);
                        if fb.activated {
                            match action {
                                "up" => moved = Some((id.clone(), -1)),
                                "down" => moved = Some((id.clone(), 1)),
                                _ => removed = Some(id.clone()),
                            }
                        }
                    }
                },
            );
        if let Some(id) = open {
            self.open(cx, kind, &id);
        }
        if let Some((id, delta)) = moved {
            self.reorder(cx, kind, &id, delta);
            // Keep the focus on the same button of the moved row (if it still exists).
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
        if let Some(id) = removed {
            self.remove(cx, kind, &id);
        }
        // Footer hint.
        let hint = if cx.layout.tv {
            "Enter to play · the arrow buttons reorder, the cross removes"
        } else {
            "Enter to play · Ctrl+\u{2191}/\u{2193} to reorder · Delete to remove"
        };
        painter.text(
            pos2(content.left() + m + width, content.top() + 116.0),
            Align2::RIGHT_CENTER,
            hint,
            font::caption(),
            color::TEXT_3,
        );
    }
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

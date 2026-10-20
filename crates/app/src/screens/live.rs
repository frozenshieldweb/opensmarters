// SPDX-License-Identifier: GPL-3.0-or-later
//! Live TV (§8.3): categories (All, Favourites, provider categories / M3U groups) with a
//! filter, a virtualised channel list with now/next and progress, the inline preview
//! player with the Now/Next card, sort, favourites, parental locks (PIN), channel-number
//! entry and PageUp/PageDown zapping.
//!
//! Behaviour: moving the focus never switches the stream. Enter on a row plays it in the
//! preview; Enter on the playing row (or the Fullscreen button) opens the player.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui,
    pos2, vec2,
};
use opensmarters_core::catalog::{
    CatalogItem, CategorySelection, SearchQuery, SortOrder, sorted_indices,
};
use opensmarters_core::chrono::Local;
use opensmarters_core::{CategoryId, chrono};
use opensmarters_store::{ContentKind, ItemSnapshot, PinOutcome};

use super::{AppCtx, Screen, shell};
use crate::error::AppError;
use crate::focus::{self, FocusOpts};
use crate::nav::{PlayerRoute, Route, Section};
use crate::playback::{PlaybackItem, PlaybackStatus, ZapContext};
use crate::services::catalog::{LiveCatalog, LiveChannel, Programme, split_tag};
use crate::services::epg::EpgState;
use crate::services::{ImageSize, Session};
use crate::task::Loader;
use crate::ui::assets::{self, Asset, Illustration};
use crate::ui::icons::{self, Style};
use crate::ui::layout::Breakpoint;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::feedback::skeleton;
use crate::ui::widgets::{
    Button, ChannelLogo, Chip, Dialog, EmptyState, IconButton, MenuRow, PinPad, PinPadEvent,
    SearchField, VirtualList,
};
use crate::ui::{paint, text};

const ROW_H: f32 = 64.0;
const ROW_GAP: f32 = 8.0;
/// Channel-number entry zaps after this long without another digit.
const NUMBER_ENTRY_TIMEOUT: Duration = Duration::from_millis(1500);

fn list_id() -> Id {
    Id::unique("live.channels")
}

fn row_id(key: &str) -> Id {
    Id::unique(("live.row", key))
}

fn sort_label(s: SortOrder) -> &'static str {
    match s {
        SortOrder::NumberAsc => "Channel number",
        SortOrder::NameAsc => "Name",
        SortOrder::RecentlyAdded => "Recently added",
        _ => "Provider order",
    }
}

fn next_sort(s: SortOrder) -> SortOrder {
    match s {
        SortOrder::NumberAsc => SortOrder::NameAsc,
        SortOrder::NameAsc => SortOrder::RecentlyAdded,
        SortOrder::RecentlyAdded => SortOrder::Server,
        _ => SortOrder::NumberAsc,
    }
}

#[derive(Debug)]
struct PinPrompt {
    selection: CategorySelection,
    name: String,
    pad: PinPad,
    verify: Loader<PinOutcome>,
}

#[derive(Debug, Clone, PartialEq)]
struct ListKey {
    selection: CategorySelection,
    sort: SortOrder,
    generation: u64,
    favourites: usize,
}

/// The Live TV screen.
#[derive(Debug)]
pub struct LiveScreen {
    initial_category: Option<String>,
    catalog: Loader<Arc<LiveCatalog>>,
    selection: CategorySelection,
    category_filter: String,
    sort: SortOrder,
    list_key: Option<ListKey>,
    visible: Arc<Vec<LiveChannel>>,
    preview: Option<String>,
    scroll_to: Option<usize>,
    number_entry: Option<(String, Instant)>,
    pin: Option<PinPrompt>,
    focus_playing_on_load: bool,
}

impl LiveScreen {
    /// Creates the screen, optionally on a category (`"all"`, `"favourites"`, or an id).
    pub fn new(category: Option<String>) -> Self {
        let selection = match category.as_deref() {
            None | Some("all") => CategorySelection::All,
            Some("favourites" | "favorites") => CategorySelection::Favourites,
            Some(id) => CategorySelection::Category(CategoryId::from(id)),
        };
        Self {
            initial_category: category,
            catalog: Loader::new(),
            selection,
            category_filter: String::new(),
            sort: SortOrder::NumberAsc,
            list_key: None,
            visible: Arc::new(Vec::new()),
            preview: None,
            scroll_to: None,
            number_entry: None,
            pin: None,
            focus_playing_on_load: true,
        }
    }

    fn load(&mut self, cx: &AppCtx<'_>) {
        let Some(session) = cx.session() else { return };
        self.catalog.start(cx.rt(), &cx.egui, move |_| async move {
            session.live_catalog().await
        });
    }

    fn category_name(&self, cat: &LiveCatalog) -> String {
        match &self.selection {
            CategorySelection::All => "All channels".into(),
            CategorySelection::Favourites => "Favourites".into(),
            CategorySelection::Category(id) => {
                cat.category_name(id).unwrap_or("Category").to_owned()
            }
        }
    }

    /// Recomputes the visible list when the selection, sort, catalogue or favourites change.
    fn refresh_list(&mut self, session: &Session, cat: &LiveCatalog) {
        let favs = session.favourite_ids(ContentKind::Live);
        let key = ListKey {
            selection: self.selection.clone(),
            sort: self.sort,
            generation: self.catalog.generation(),
            favourites: favs.len(),
        };
        if self.list_key.as_ref() == Some(&key) {
            return;
        }
        let parental = session.parental_enabled();
        let mut picked: Vec<LiveChannel> = cat
            .select(&self.selection, &favs)
            .into_iter()
            .filter_map(|i| cat.channels.get(i))
            .filter(|c| {
                // Channels of locked categories stay out of All/Favourites.
                !parental
                    || self.selection.server_category().is_some()
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
        let order = sorted_indices(&picked, self.sort);
        let mut slots: Vec<Option<LiveChannel>> = picked.drain(..).map(Some).collect();
        let sorted: Vec<LiveChannel> = order
            .into_iter()
            .filter_map(|i| slots.get_mut(i).and_then(Option::take))
            .collect();
        self.visible = Arc::new(sorted);
        self.list_key = Some(key);
    }

    fn select(&mut self, cx: &mut AppCtx<'_>, sel: CategorySelection, name: &str) {
        let Some(session) = cx.session() else { return };
        if session.is_selection_locked(ContentKind::Live, &sel, name) {
            self.pin = Some(PinPrompt {
                selection: sel,
                name: name.to_owned(),
                pad: PinPad::new(Id::unique("live.pin"), session.pin_length()),
                verify: Loader::new(),
            });
            return;
        }
        if self.selection != sel {
            self.selection = sel;
            self.scroll_to = Some(0);
        }
    }

    fn play_preview(&mut self, cx: &mut AppCtx<'_>, index: usize) {
        let Some(session) = cx.session() else { return };
        let Some(ch) = self.visible.get(index).cloned() else {
            return;
        };
        let zap = ZapContext {
            channels: Arc::clone(&self.visible),
            index,
        };
        match session.live_request(&ch, Some(zap)) {
            Ok(req) => {
                self.preview = Some(ch.key.clone());
                cx.playback.play(session, req);
            }
            Err(e) => cx.toasts.error(e.user_message()),
        }
    }

    fn open_fullscreen(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(req) = cx.playback.request().cloned() {
            cx.nav
                .push(Route::Player(PlayerRoute::Request(Arc::new(req))));
        }
    }

    fn zap_number(&mut self, cx: &mut AppCtx<'_>, number: u64) {
        let Some(cat) = self.catalog.value().cloned() else {
            return;
        };
        // Prefer the current list; fall back to All.
        let idx = self.visible.iter().position(|c| c.number == Some(number));
        let idx = match idx {
            Some(i) => Some(i),
            None if cat.by_number(number).is_some() => {
                self.selection = CategorySelection::All;
                self.list_key = None;
                if let Some(s) = cx.session() {
                    self.refresh_list(&s, &cat);
                }
                self.visible.iter().position(|c| c.number == Some(number))
            }
            None => None,
        };
        match idx {
            Some(i) => {
                self.play_preview(cx, i);
                if let Some(ch) = self.visible.get(i) {
                    focus::request(&cx.egui, row_id(&ch.key));
                }
                self.scroll_to = Some(i);
            }
            None => cx.toasts.info(format!("There's no channel {number}.")),
        }
    }

    fn zap(&mut self, cx: &mut AppCtx<'_>, delta: isize) {
        if cx.playback.zap(delta).is_some()
            && let Some(key) = cx.playback.live_key().map(str::to_owned)
        {
            self.preview = Some(key.clone());
            if let Some(i) = self.visible.iter().position(|c| c.key == key) {
                self.scroll_to = Some(i);
                focus::request(&cx.egui, row_id(&key));
            }
        }
    }

    fn poll_pin(&mut self, cx: &mut AppCtx<'_>) {
        let Some(p) = &mut self.pin else { return };
        if !p.verify.poll() {
            if let Some(e) = p.verify.error().cloned() {
                p.pad.set_wrong(e.user_message());
                p.verify = Loader::new();
            }
            return;
        }
        match p.verify.take() {
            Some(PinOutcome::Ok) => {
                if let (Some(session), CategorySelection::Category(id)) =
                    (cx.session(), &p.selection)
                {
                    session.unlock_category(ContentKind::Live, id);
                }
                let sel = p.selection.clone();
                self.pin = None;
                self.selection = sel;
                self.list_key = None;
                self.scroll_to = Some(0);
            }
            Some(PinOutcome::Wrong {
                remaining_before_lockout,
            }) => {
                let msg = if remaining_before_lockout == 1 {
                    "Wrong PIN. 1 attempt left".to_owned()
                } else {
                    format!("Wrong PIN. {remaining_before_lockout} attempts left")
                };
                p.pad.set_wrong(msg);
            }
            Some(PinOutcome::LockedOut { until }) => {
                let now = opensmarters_store::Timestamp::now();
                p.pad.set_locked_until(until.duration_since(now));
            }
            None => {}
        }
    }
}

impl Screen for LiveScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if self.catalog.is_idle() || self.catalog.error().is_some() {
            self.load(cx);
        }
        // Follow what the shared player is playing (e.g. after zapping in fullscreen).
        if let Some(key) = cx.playback.live_key() {
            self.preview = Some(key.to_owned());
            self.focus_playing_on_load = true;
        }
        self.list_key = None;
    }

    fn on_leave(&mut self, cx: &mut AppCtx<'_>) {
        // Keep the stream for the fullscreen player; stop it otherwise.
        if !matches!(cx.next_route, Some(Route::Player(_))) {
            cx.playback.stop();
            self.preview = None;
        }
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if self.pin.is_some() || focus::text_input_focused(ctx) {
            return;
        }
        // Digits: channel-number entry.
        let digits: Vec<char> = ctx.input_mut(|i| {
            let mut v = Vec::new();
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
                if i.consume_key(Modifiers::NONE, k) {
                    v.push(c);
                }
            }
            v
        });
        if !digits.is_empty() {
            let entry = self
                .number_entry
                .get_or_insert_with(|| (String::new(), Instant::now()));
            for d in digits {
                if entry.0.len() < 5 {
                    entry.0.push(d);
                }
            }
            entry.1 = Instant::now();
        }
        if let Some((buf, _)) = &self.number_entry
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
        {
            let n = buf.parse::<u64>().ok();
            self.number_entry = None;
            if let Some(n) = n {
                self.zap_number(cx, n);
            }
        }
        // PageUp/PageDown zap the preview.
        if cx.playback.live_key().is_some() {
            let (up, down) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::PageUp),
                    i.consume_key(Modifiers::NONE, Key::PageDown),
                )
            });
            if up {
                self.zap(cx, -1);
            } else if down {
                self.zap(cx, 1);
            }
        }
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        if self.pin.take().is_some() {
            return true;
        }
        if self.number_entry.take().is_some() {
            return true;
        }
        false
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Live)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.catalog.poll();
        self.poll_pin(cx);
        if let Some((buf, at)) = &self.number_entry
            && at.elapsed() >= NUMBER_ENTRY_TIMEOUT
        {
            let n = buf.parse::<u64>().ok();
            self.number_entry = None;
            if let Some(n) = n {
                self.zap_number(cx, n);
            }
        }
        let Some(session) = cx.session() else { return };
        let content = cx.layout.content;
        let m = cx.layout.margin;
        let wide = cx.layout.breakpoint == Breakpoint::Wide;
        shell::status_cluster(cx, ui);

        // Columns.
        let cats_w = if wide { 248.0 } else { 0.0 };
        let cats_rect = Rect::from_min_size(content.min, vec2(cats_w, content.height()));
        let right_area =
            Rect::from_min_max(pos2(content.left() + cats_w, content.top()), content.max);
        let preview_frac = if wide { 0.42 } else { 0.40 };
        let preview_w = ((right_area.width() - m * 2.0) * preview_frac).round();
        let list_rect = Rect::from_min_max(
            pos2(right_area.left() + m, content.top() + 104.0),
            pos2(
                right_area.right() - m - preview_w - 32.0,
                content.bottom() - 24.0,
            ),
        );
        let preview_rect = Rect::from_min_max(
            pos2(list_rect.right() + 32.0, content.top() + 104.0),
            pos2(right_area.right() - m, content.bottom() - 24.0),
        );

        let catalog = self.catalog.value().cloned();
        if let Some(cat) = &catalog {
            if let Some(initial) = self.initial_category.take()
                && let CategorySelection::Category(id) = &self.selection
                && session.is_category_locked(
                    ContentKind::Live,
                    id,
                    cat.category_name(id).unwrap_or(""),
                )
            {
                // An initially requested locked category needs the PIN too.
                let name = cat.category_name(id).unwrap_or(&initial).to_owned();
                let sel = std::mem::take(&mut self.selection);
                self.select(cx, sel, &name);
            }
            self.refresh_list(&session, cat);
            if self.focus_playing_on_load {
                self.focus_playing_on_load = false;
                if let Some(i) = self
                    .preview
                    .as_ref()
                    .and_then(|k| self.visible.iter().position(|c| &c.key == k))
                {
                    self.scroll_to = Some(i);
                    focus::request(&cx.egui, row_id(&self.visible[i].key));
                }
            }
        }

        // Categories column (wide) or chips (medium/TV).
        if wide {
            let painter = ui.painter().clone();
            painter.rect_filled(cats_rect, 0.0, color::SURFACE_1);
            painter.vline(
                cats_rect.right() - 0.5,
                cats_rect.y_range(),
                Stroke::new(1.0, color::STROKE_SUBTLE),
            );
            painter.text(
                pos2(cats_rect.left() + 24.0, content.top() + 64.0),
                Align2::LEFT_CENTER,
                "Live TV",
                font::heading(),
                color::TEXT,
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
                pos2(cats_rect.left() + 24.0, content.top() + 106.0),
                pos2(cats_rect.right() - 24.0, cats_rect.bottom() - 96.0),
            )));
            focus::scope(&mut child, Id::unique("live.cats"), |ui| {
                self.categories_column(cx, ui, catalog.as_deref(), &session)
            });
            // Footer: profile + guide freshness.
            let fy = cats_rect.bottom() - 64.0;
            text::line(
                &painter,
                pos2(cats_rect.left() + 24.0, fy),
                Align2::LEFT_CENTER,
                &session.profile().name,
                font::caption(),
                color::TEXT_2,
                cats_w - 48.0,
            );
            let note = if session.is_offline() {
                "Offline · showing saved channels".to_owned()
            } else if let Some(age) = self.catalog.age() {
                let t = Local::now() - chrono::Duration::from_std(age).unwrap_or_default();
                format!("Channels updated at {}", cx.prefs.format_time(&t))
            } else {
                String::new()
            };
            text::line(
                &painter,
                pos2(cats_rect.left() + 24.0, fy + 18.0),
                Align2::LEFT_CENTER,
                &note,
                font::caption(),
                color::TEXT_3,
                cats_w - 48.0,
            );
        }

        // List header.
        let painter = ui.painter().clone();
        let head_y = content.top() + 64.0;
        let title = catalog
            .as_deref()
            .map_or_else(|| "Live TV".to_owned(), |c| self.category_name(c));
        let tr = text::line(
            &painter,
            pos2(list_rect.left(), head_y),
            Align2::LEFT_CENTER,
            &title,
            font::heading(),
            color::TEXT,
            list_rect.width() - 160.0,
        );
        if catalog.is_some() {
            let n = self.visible.len();
            painter.text(
                pos2(tr.right() + 12.0, head_y + 4.0),
                Align2::LEFT_CENTER,
                format!("{n} channel{}", if n == 1 { "" } else { "s" }),
                font::caption(),
                color::TEXT_3,
            );
        }

        let mut list_area = list_rect;
        if !wide {
            // Chip row above the list.
            let chips = Rect::from_min_size(list_rect.min, vec2(list_rect.width(), 52.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(chips));
            focus::scope(&mut child, Id::unique("live.chips"), |ui| {
                self.category_chips(cx, ui, catalog.as_deref(), &session)
            });
            list_area.min.y += 60.0;
        }
        text::overline(
            &painter,
            pos2(list_area.left(), list_area.top() + 16.0),
            Align2::LEFT_CENTER,
            "On now",
            color::TEXT_3,
        );
        // Sort button (right of the header).
        let sort_b = Button::ghost(sort_label(self.sort))
            .icon(icons::SORT_ASCENDING)
            .small()
            .id(Id::unique("live.sort"));
        let sz = sort_b.size(ui);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            pos2(list_area.right() - sz.x + 8.0, list_area.top() - 2.0),
            sz,
        )));
        if focus::scope(&mut child, Id::unique("live.list-head"), |ui| {
            sort_b.show(ui)
        })
        .activated
        {
            self.sort = next_sort(self.sort);
            self.list_key = None;
        }
        let rows_rect = Rect::from_min_max(
            pos2(list_area.left() - 8.0, list_area.top() + 36.0),
            pos2(list_area.right() + 8.0, list_area.bottom() - 52.0),
        );

        match (&catalog, self.catalog.error()) {
            (None, Some(err)) => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                let retry = EmptyState::new(
                    Id::unique("live.error"),
                    Illustration::ErrorOffline,
                    err.title(),
                    &err.user_message(),
                )
                .action("Retry", true)
                .show(&mut child);
                if retry.is_some_and(|f| f.activated) {
                    self.load(cx);
                }
            }
            (None, None) => {
                if self.catalog.show_skeleton() {
                    let mut y = rows_rect.top() + 8.0;
                    while y + ROW_H < rows_rect.bottom() {
                        let r = Rect::from_min_size(
                            pos2(rows_rect.left() + 8.0, y),
                            vec2(rows_rect.width() - 16.0, ROW_H),
                        );
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(r.min + vec2(56.0, 12.0), vec2(64.0, 40.0)),
                            radius::SM,
                        );
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(r.min + vec2(132.0, 14.0), vec2(180.0, 14.0)),
                            radius::XS,
                        );
                        skeleton(
                            &cx.egui,
                            &painter,
                            Rect::from_min_size(r.min + vec2(132.0, 36.0), vec2(120.0, 12.0)),
                            radius::XS,
                        );
                        y += ROW_H + ROW_GAP;
                    }
                }
            }
            (Some(_), _) if self.visible.is_empty() => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                let (ill, head, body) = if self.selection == CategorySelection::Favourites {
                    (
                        Illustration::EmptyFavourites,
                        "Nothing saved yet",
                        "Play a channel and press \u{2665} next to the preview to keep it here.",
                    )
                } else {
                    (
                        Illustration::EmptySearch,
                        "No channels here",
                        "Your provider lists no channels in this category.",
                    )
                };
                let _ = EmptyState::new(Id::unique("live.empty"), ill, head, body)
                    .illustration_size(160.0)
                    .show(&mut child);
            }
            (Some(_), _) => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
                self.channel_list(cx, &mut child, &session);
            }
        }
        // Footer hint.
        let fy = list_area.bottom() - 24.0;
        painter.hline(
            list_area.x_range(),
            fy - 22.0,
            Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        icons::paint(
            &painter,
            pos2(list_area.left() + 9.0, fy),
            icons::INFO,
            16.0,
            Style::Regular,
            color::TEXT_3,
        );
        painter.text(
            pos2(list_area.left() + 28.0, fy),
            Align2::LEFT_CENTER,
            "Enter to watch · Type a channel number to jump",
            font::caption(),
            color::TEXT_3,
        );

        // Preview column.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(preview_rect));
        focus::scope(&mut child, Id::unique("live.preview"), |ui| {
            self.preview_column(cx, ui, &session, preview_rect)
        });

        self.overlays(cx, ui);
    }
}

impl LiveScreen {
    fn categories_column(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        catalog: Option<&LiveCatalog>,
        session: &Arc<Session>,
    ) {
        ui.spacing_mut().item_spacing.y = 0.0;
        SearchField::new(Id::unique("live.cat-search"), "Find a category")
            .size(ui.available_width(), 52.0)
            .show(ui, &mut self.category_filter);
        ui.add_space(24.0);
        let Some(cat) = catalog else {
            for i in 0..8 {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
                skeleton(
                    &cx.egui,
                    ui.painter(),
                    Rect::from_min_size(
                        r.min + vec2(12.0, 17.0),
                        vec2(90.0 + (i % 3) as f32 * 30.0, 14.0),
                    ),
                    radius::XS,
                );
            }
            return;
        };
        let q = SearchQuery::new(&self.category_filter);
        let favs = session.favourite_ids(ContentKind::Live).len();
        let mut picked: Option<(CategorySelection, String)> = None;
        egui::ScrollArea::vertical()
            .id_salt("live.cats.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if q.is_empty() {
                    let f = MenuRow::new(Id::unique("live.cat.all"), "All")
                        .count(Some(cat.channels.len()))
                        .selected(self.selection == CategorySelection::All)
                        .show(ui);
                    if f.activated {
                        picked = Some((CategorySelection::All, "All".into()));
                    }
                    let f = MenuRow::new(Id::unique("live.cat.favs"), "Favourites")
                        .count(Some(favs))
                        .selected(self.selection == CategorySelection::Favourites)
                        .show(ui);
                    if f.activated {
                        picked = Some((CategorySelection::Favourites, "Favourites".into()));
                    }
                    let (r, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                    ui.painter().hline(
                        r.x_range(),
                        r.center().y,
                        Stroke::new(1.0, color::STROKE_SUBTLE),
                    );
                }
                for c in &cat.categories {
                    if !q.is_empty() && !q.matches(&c.name) {
                        continue;
                    }
                    let count = cat.count(&c.id);
                    let sel = CategorySelection::Category(c.id.clone());
                    let locked = session.is_category_locked(ContentKind::Live, &c.id, &c.name);
                    let f = MenuRow::new(Id::unique(("live.cat", c.id.as_str())), &c.name)
                        .count(Some(count))
                        .locked(locked)
                        .selected(self.selection == sel)
                        .show(ui);
                    if f.activated {
                        picked = Some((sel, c.name.clone()));
                    }
                }
            });
        if let Some((sel, name)) = picked {
            self.select(cx, sel, &name);
        }
    }

    fn category_chips(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        catalog: Option<&LiveCatalog>,
        session: &Arc<Session>,
    ) {
        let Some(cat) = catalog else { return };
        let mut picked: Option<(CategorySelection, String)> = None;
        egui::ScrollArea::horizontal()
            .id_salt("live.chips.scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(8, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if Chip::new("All")
                        .selected(self.selection == CategorySelection::All)
                        .id(Id::unique("live.chip.all"))
                        .show(ui)
                        .activated
                    {
                        picked = Some((CategorySelection::All, "All".into()));
                    }
                    if Chip::new("Favourites")
                        .selected(self.selection == CategorySelection::Favourites)
                        .id(Id::unique("live.chip.favs"))
                        .show(ui)
                        .activated
                    {
                        picked = Some((CategorySelection::Favourites, "Favourites".into()));
                    }
                    for c in &cat.categories {
                        let sel = CategorySelection::Category(c.id.clone());
                        let label = if session.is_category_locked(ContentKind::Live, &c.id, &c.name)
                        {
                            format!("{} {}", icons::LOCK_SIMPLE, c.name)
                        } else {
                            c.name.clone()
                        };
                        if Chip::new(label)
                            .selected(self.selection == sel)
                            .id(Id::unique(("live.chip", c.id.as_str())))
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

    fn channel_list(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, session: &Arc<Session>) {
        let visible = Arc::clone(&self.visible);
        let playing = cx.playback.live_key().map(str::to_owned);
        let mut activated: Option<usize> = None;
        let mut fav_clicked: Option<usize> = None;
        let now = cx.now;
        let images = cx.services.images.clone();
        VirtualList::new(list_id(), ROW_H, visible.len())
            .gap(ROW_GAP)
            .scroll_to(self.scroll_to.take())
            .show(
                ui,
                |i| visible.get(i).map_or(Id::NULL, |c| row_id(&c.key)),
                |ui, i, rect| {
                    let Some(ch) = visible.get(i) else { return };
                    let is_playing = playing.as_deref() == Some(ch.key.as_str());
                    let (resp, f) = focus::focusable(
                        ui,
                        row_id(&ch.key),
                        rect,
                        FocusOpts::default().autofocus_if(i == 0),
                    );
                    let painter = ui.painter().clone();
                    if f.focused {
                        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_3);
                    } else if is_playing {
                        painter.rect_filled(rect, CornerRadius::same(radius::MD), color::SURFACE_2);
                    }
                    if is_playing {
                        let bar = Rect::from_min_size(
                            pos2(rect.left(), rect.top() + 12.0),
                            vec2(3.0, rect.height() - 24.0),
                        );
                        painter.rect_filled(bar, CornerRadius::same(2), color::ACCENT);
                    }
                    // Number.
                    let num = ch
                        .number
                        .map_or_else(|| "—".to_owned(), |n| format!("{n:03}"));
                    painter.text(
                        pos2(rect.left() + 56.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        num,
                        font::caption_strong(),
                        color::TEXT_3,
                    );
                    // Logo plate.
                    let logo = Rect::from_min_size(
                        pos2(rect.left() + 68.0, rect.center().y - 20.0),
                        vec2(64.0, 40.0),
                    );
                    let slot = images.get(ch.logo.as_deref(), ImageSize::Logo);
                    ChannelLogo::paint(ui, &painter, logo, &ch.name, &slot);
                    // Right side: time range, heart, catch-up.
                    let mut rx = rect.right() - 12.0;
                    let heart =
                        Rect::from_center_size(pos2(rx - 10.0, rect.center().y), vec2(24.0, 24.0));
                    if ch.archive {
                        icons::paint(
                            &painter,
                            pos2(rx - 10.0, rect.center().y),
                            icons::CLOCK_COUNTER_CLOCKWISE,
                            20.0,
                            Style::Regular,
                            color::SAGE,
                        );
                    }
                    rx -= 30.0;
                    let heart = if ch.archive {
                        heart.translate(vec2(-30.0, 0.0))
                    } else {
                        heart
                    };
                    if session.is_favourite(ContentKind::Live, &ch.key) {
                        icons::paint(
                            &painter,
                            heart.center(),
                            icons::HEART,
                            20.0,
                            Style::Fill,
                            color::ACCENT,
                        );
                    }
                    rx -= 32.0;
                    // Name, now, progress.
                    let (epg, now_p, _next) = session.now_next(ch, now);
                    let tx = logo.right() + 12.0;
                    let time_w = 96.0;
                    let text_w = (rx - time_w - tx).max(60.0);
                    let name_c = color::TEXT;
                    let (tag, name) = split_tag(&ch.name);
                    let mut nx = tx;
                    if let Some(tag) = tag {
                        let r = crate::ui::widgets::chip::tag(
                            &painter,
                            pos2(nx, rect.top() + 11.0),
                            tag,
                        );
                        nx = r.right() + 8.0;
                    }
                    text::line(
                        &painter,
                        pos2(nx, rect.top() + 21.0),
                        Align2::LEFT_CENTER,
                        name,
                        font::body_strong(),
                        name_c,
                        (text_w - (nx - tx)).max(40.0),
                    );
                    match (&epg, &now_p) {
                        (_, Some(p)) => {
                            text::line(
                                &painter,
                                pos2(tx, rect.top() + 41.0),
                                Align2::LEFT_CENTER,
                                &p.title,
                                font::caption(),
                                color::TEXT_2,
                                text_w,
                            );
                            let bar =
                                Rect::from_min_size(pos2(tx, rect.top() + 52.0), vec2(96.0, 3.0));
                            paint::progress_bar(
                                &painter,
                                bar,
                                p.progress(now),
                                color::ACCENT,
                                color::PROGRESS_TRACK,
                            );
                            painter.text(
                                pos2(rx, rect.center().y),
                                Align2::RIGHT_CENTER,
                                format!(
                                    "{}–{}",
                                    cx.prefs.format_time(&p.start.with_timezone(&Local)),
                                    cx.prefs.format_time(&p.stop.with_timezone(&Local))
                                ),
                                font::caption(),
                                color::TEXT_3,
                            );
                        }
                        (EpgState::Loading, None) => {
                            skeleton(
                                &cx.egui,
                                &painter,
                                Rect::from_min_size(pos2(tx, rect.top() + 36.0), vec2(120.0, 10.0)),
                                radius::XS,
                            );
                        }
                        _ => {
                            painter.text(
                                pos2(tx, rect.top() + 41.0),
                                Align2::LEFT_CENTER,
                                "No guide",
                                font::caption(),
                                color::TEXT_3,
                            );
                        }
                    }
                    if f.focused {
                        paint::focus_ring(&painter, rect, radius::MD, 1.0);
                    }
                    if f.activated {
                        let on_heart = resp.clicked()
                            && resp
                                .interact_pointer_pos()
                                .is_some_and(|p| heart.expand(6.0).contains(p));
                        if on_heart {
                            fav_clicked = Some(i);
                        } else {
                            activated = Some(i);
                        }
                    }
                },
            );
        if let Some(i) = fav_clicked
            && let Some(ch) = self.visible.get(i).cloned()
        {
            toggle_fav(cx, session, &ch);
            self.list_key = None;
        }
        if let Some(i) = activated {
            let key = self.visible.get(i).map(|c| c.key.clone());
            if key.is_some()
                && key.as_deref() == cx.playback.live_key()
                && !matches!(cx.playback.status(), PlaybackStatus::Failed(_))
            {
                self.open_fullscreen(cx);
            } else {
                self.play_preview(cx, i);
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
        let playing_key = cx.playback.live_key().map(str::to_owned);
        let channel = playing_key
            .as_deref()
            .and_then(|k| self.catalog.value().and_then(|c| c.get(k)).cloned());
        // Header.
        let hy = rect.top() + 16.0;
        if channel.is_some() {
            painter.circle_filled(pos2(rect.left() + 3.0, hy), 3.0, color::LIVE);
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
        if let Some(ch) = &channel {
            let label = match ch.number {
                Some(n) => format!("{n:03} · {}", ch.name),
                None => ch.name.clone(),
            };
            text::line(
                &painter,
                pos2(rect.right(), hy),
                Align2::RIGHT_CENTER,
                &label,
                font::caption(),
                color::TEXT_3,
                rect.width() * 0.5,
            );
        }
        // Video (16:9).
        let video = Rect::from_min_size(
            pos2(rect.left(), rect.top() + 36.0),
            vec2(rect.width(), (rect.width() * 9.0 / 16.0).round()),
        );
        let (vresp, vf) =
            focus::focusable(ui, Id::unique("live.video"), video, FocusOpts::default());
        if vf.anim > 0.0 {
            paint::focus_glow(&painter, video, radius::MD, vf.anim);
        }
        painter.rect_filled(video, CornerRadius::same(radius::MD), color::VIDEO);
        let status = cx.playback.status();
        if channel.is_some() {
            // Round the video corners by painting it inset and covering with the frame.
            cx.playback.paint_video(ui, video);
            // Phase 7 (stream A): the last picture is held, dimmed, while a zap or a
            // reconnect opens the next stream; controller messages become toasts.
            if cx.playback.holding_frame() {
                painter.rect_filled(video, 0.0, Color32::from_black_alpha(110));
            }
            for msg in cx.playback.take_messages() {
                cx.toasts.info(msg);
            }
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
                    if let Some(note) = cx.playback.notice() {
                        text::block(
                            &painter,
                            pos2(video.left() + 24.0, video.center().y + 48.0),
                            &note,
                            font::caption(),
                            color::WARNING,
                            video.width() - 48.0,
                            2,
                        );
                    }
                    ui.ctx().request_repaint();
                }
                PlaybackStatus::Failed(msg) => {
                    painter.rect_filled(video, CornerRadius::same(radius::MD), color::BG_DEEP);
                    text::block(
                        &painter,
                        video.left_top() + vec2(24.0, video.height() / 2.0 - 20.0),
                        msg,
                        font::body(),
                        color::TEXT_2,
                        video.width() - 48.0,
                        3,
                    );
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
                _ => {}
            }
            // Bottom strip: LIVE + name + volume state.
            let strip = Rect::from_min_max(pos2(video.left(), video.bottom() - 56.0), video.max);
            paint::v_gradient(
                &painter,
                strip,
                &[
                    (0.0, Color32::from_black_alpha(0)),
                    (1.0, Color32::from_black_alpha(170)),
                ],
            );
            // "LIVE", or "−02:13" when the preview plays behind live (paused or rewound
            // in the player).
            let br = super::player::timeshift::live_badge(
                &painter,
                pos2(video.left() + 16.0, video.bottom() - 26.0),
                Align2::LEFT_CENTER,
                cx.playback.live_view().map(|v| v.behind),
            );
            if let Some(ch) = &channel {
                text::line(
                    &painter,
                    pos2(br.right() + 12.0, video.bottom() - 26.0),
                    Align2::LEFT_CENTER,
                    &ch.name,
                    font::body_strong(),
                    color::TEXT,
                    video.width() - 160.0,
                );
            }
            let st = cx.playback.state();
            let vol_glyph = if st.muted {
                icons::SPEAKER_SLASH
            } else {
                icons::SPEAKER_HIGH
            };
            icons::paint(
                &painter,
                pos2(video.right() - 30.0, video.bottom() - 26.0),
                vol_glyph,
                22.0,
                Style::Regular,
                color::TEXT,
            );
        } else {
            if !assets::paint_cover(
                ui,
                &painter,
                Asset::PlayerIdle,
                video,
                vec2(0.5, 0.5),
                radius::MD,
                Color32::from_gray(200),
            ) {
                painter.rect_filled(video, CornerRadius::same(radius::MD), color::BG_DEEP);
            }
            let sym = Rect::from_center_size(video.center() - vec2(0.0, 18.0), vec2(48.0, 48.0));
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
                "Choose a channel and press Enter",
                font::body(),
                color::TEXT_2,
            );
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
        if vf.activated || vresp.double_clicked() {
            self.open_fullscreen(cx);
        }

        // Now / Next card.
        let card_top = video.bottom() + 24.0;
        let (now_p, next_p) = match &channel {
            Some(ch) => {
                let (_, n, x) = session.now_next(ch, cx.now);
                (n, x)
            }
            None => (None, None),
        };
        let card_h = 292.0_f32.min(rect.bottom() - card_top - 72.0).max(120.0);
        let card = Rect::from_min_size(pos2(rect.left(), card_top), vec2(rect.width(), card_h));
        if channel.is_some() {
            now_card(cx, &painter, card, now_p.as_ref(), next_p.as_ref());
        }

        // Buttons.
        let by = card.bottom() + 24.0;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            pos2(rect.left(), by),
            vec2(rect.width(), 52.0),
        )));
        child.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let enabled = channel.is_some();
            if Button::primary("Fullscreen")
                .icon(icons::CORNERS_OUT)
                .enabled(enabled)
                .id(Id::unique("live.fullscreen"))
                .show(ui)
                .activated
            {
                self.open_fullscreen(cx);
            }
            if Button::secondary("Guide")
                .icon(icons::ROWS)
                .id(Id::unique("live.guide"))
                .show(ui)
                .activated
            {
                cx.nav.push(Route::Guide);
            }
            super::multiscreen::live_button(cx, ui);
            if let Some(ch) = &channel {
                let fav = session.is_favourite(ContentKind::Live, &ch.key);
                let f = IconButton::new(icons::HEART)
                    .fill(fav)
                    .selected(fav)
                    .surface()
                    .id(Id::unique("live.fav"))
                    .show(ui);
                if f.activated {
                    toggle_fav(cx, session, ch);
                    self.list_key = None;
                }
            }
        });
    }

    fn overlays(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        // Channel-number entry box (top-right).
        if let Some((buf, _)) = &self.number_entry {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                Id::unique("live.number"),
            ));
            let content = cx.layout.content;
            let g = painter.layout_no_wrap(buf.clone(), font::numeric_xl(), color::TEXT);
            let w = (g.size().x + 48.0).max(140.0);
            let r = Rect::from_min_size(
                pos2(content.right() - cx.layout.margin - w, content.top() + 96.0),
                vec2(w, 100.0),
            );
            paint::shadow(&painter, r, radius::LG, crate::ui::theme::elevation::E2);
            painter.rect_filled(r, CornerRadius::same(radius::LG), color::SURFACE_1);
            painter.galley(r.center() - g.size() / 2.0, g, color::TEXT);
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        // PIN dialog.
        if let Some(p) = &mut self.pin {
            let title = format!("\u{201c}{}\u{201d} is locked", text::cap(&p.name, 40));
            let mut submitted = None;
            let mut cancel = false;
            Dialog::new(Id::unique("live.pin.dialog"), "")
                .width(440.0)
                .show_custom(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        let painter = ui.painter().clone();
                        let (ir, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
                        assets::paint_contain(
                            ui,
                            &painter,
                            Asset::Illustration(Illustration::ParentalLock),
                            ir,
                            Color32::WHITE,
                        );
                        ui.add_space(8.0);
                        if let PinPadEvent::Submitted(pin) = p.pad.show(ui, &title) {
                            submitted = Some(pin);
                        }
                        ui.add_space(12.0);
                        if Button::ghost("Cancel")
                            .id(Id::unique("live.pin.cancel"))
                            .show(ui)
                            .activated
                        {
                            cancel = true;
                        }
                    });
                });
            if let Some(pin) = submitted {
                let store = cx.services.store.clone();
                p.verify.start_blocking(cx.rt(), &cx.egui, move || {
                    store.parental().verify_pin(&pin).map_err(AppError::from)
                });
            }
            if cancel {
                self.pin = None;
            }
        }
    }
}

fn toggle_fav(cx: &mut AppCtx<'_>, session: &Arc<Session>, ch: &LiveChannel) {
    let snapshot = ItemSnapshot {
        name: ch.name.clone(),
        icon_url: ch.logo.clone(),
        category_id: ch.categories.first().map(|c| c.as_str().to_owned()),
    };
    let now_fav = session.toggle_favourite(ContentKind::Live, &ch.key, snapshot);
    if now_fav {
        cx.toasts
            .success(format!("{} added to Favourites.", text::cap(&ch.name, 40)));
    } else {
        cx.toasts.info(format!(
            "{} removed from Favourites.",
            text::cap(&ch.name, 40)
        ));
    }
}

fn now_card(
    cx: &AppCtx<'_>,
    painter: &egui::Painter,
    card: Rect,
    now_p: Option<&Programme>,
    next_p: Option<&Programme>,
) {
    painter.rect(
        card,
        CornerRadius::same(radius::LG),
        color::SURFACE_1,
        Stroke::new(1.0, color::STROKE_SUBTLE),
        StrokeKind::Inside,
    );
    let x = card.left() + 24.0;
    let w = card.width() - 48.0;
    let mut y = card.top() + 28.0;
    text::overline(
        painter,
        pos2(x, y),
        Align2::LEFT_CENTER,
        "Now",
        color::TEXT_3,
    );
    y += 26.0;
    match now_p {
        Some(p) => {
            text::line(
                painter,
                pos2(x, y),
                Align2::LEFT_CENTER,
                &p.title,
                font::title(),
                color::TEXT,
                w,
            );
            y += 32.0;
            let range = format!(
                "{}–{}",
                cx.prefs.format_time(&p.start.with_timezone(&Local)),
                cx.prefs.format_time(&p.stop.with_timezone(&Local))
            );
            painter.text(
                pos2(x, y),
                Align2::LEFT_CENTER,
                range,
                font::caption(),
                color::TEXT_3,
            );
            let left = (p.stop - cx.now).num_minutes().max(0);
            painter.text(
                pos2(x + w, y),
                Align2::RIGHT_CENTER,
                format!("{left} min remaining"),
                font::caption(),
                color::TEXT_3,
            );
            y += 16.0;
            paint::progress_bar(
                painter,
                Rect::from_min_size(pos2(x, y), vec2(w, 4.0)),
                p.progress(cx.now),
                color::ACCENT,
                color::PROGRESS_TRACK,
            );
            y += 20.0;
            // Synopsis: as many lines (max 3) as fit above the Next line.
            let bottom = if next_p.is_some() {
                card.bottom() - 56.0
            } else {
                card.bottom() - 16.0
            };
            let rows = ((bottom - y) / 24.0).floor().clamp(0.0, 3.0) as usize;
            if rows > 0 && !p.description.trim().is_empty() {
                text::block(
                    painter,
                    pos2(x, y),
                    &p.description,
                    font::body(),
                    color::TEXT_2,
                    w,
                    rows,
                );
            }
        }
        None => {
            text::line(
                painter,
                pos2(x, y),
                Align2::LEFT_CENTER,
                "No programme information",
                font::title_sm(),
                color::TEXT_2,
                w,
            );
            y += 28.0;
            text::block(
                painter,
                pos2(x, y),
                "Your provider doesn't publish a guide for this channel.",
                font::body(),
                color::TEXT_3,
                w,
                2,
            );
        }
    }
    if let Some(n) = next_p {
        let ny = card.bottom() - 28.0;
        painter.hline(
            x..=(x + w),
            ny - 22.0,
            Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        let r = text::overline(
            painter,
            pos2(x, ny),
            Align2::LEFT_CENTER,
            "Next",
            color::TEXT_3,
        );
        let line = format!(
            "{} · {}",
            cx.prefs.format_time(&n.start.with_timezone(&Local)),
            n.title
        );
        text::line(
            painter,
            pos2(r.right() + 12.0, ny),
            Align2::LEFT_CENTER,
            &line,
            font::caption(),
            color::TEXT_2,
            w - r.width() - 12.0,
        );
    }
}

// Silence an unused-import warning for the PlaybackItem re-export used in docs.
#[allow(dead_code)]
fn _unused(_: PlaybackItem) {}

#[allow(dead_code)]
fn _catalog_item_bound<T: CatalogItem>() {}

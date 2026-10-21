// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Categories: for this profile, per section (Live TV; Movies and Series on
//! Xtream profiles), which categories appear when browsing, in what order, and which
//! regions. The rules live in [`crate::services::categories`].
//!
//! - **Regions:** toggle chips ("FR 23", "No tag 5") from the region tags of the category
//!   names. None selected = every region; selecting chips shows only those regions.
//! - **Show all / Hide all / Reset** (all shown, provider order, no region filter; Reset
//!   asks first).
//! - **The list:** every category in the viewer's order, hidden ones dimmed at their
//!   place, with the channel count when known and a padlock on locked categories (hiding
//!   is harmless, so no PIN). Enter (or a click) shows or hides a row; a row the region
//!   filter hides says "Hidden by region" instead of its switch.
//! - **Order**, like Favourites: Ctrl/Shift+↑/↓ move the focused row (among the rows on
//!   screen while filtering), Ctrl+Home moves it to the top; → reaches the ⤒ ▲ ▼ buttons
//!   of the focused row.
//! - **Every change applies at once** (in the session at once, written in the background,
//!   read back) and the last one can be undone: Undo next to the actions, or Ctrl+Z.
//!
//! Live TV, Movies and Series open this page on their section ("Edit categories", the
//! region note).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, CornerRadius, Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2};
use opensmarters_core::catalog::SearchQuery;
use opensmarters_core::{Category, CategoryId};
use opensmarters_store::categories::is_valid_id;
use opensmarters_store::{CategoryPrefs, ContentKind, ProfileId};

use super::{SettingsPageUi, page_heading, section};
use crate::focus::{self, FocusOpts};
use crate::screens::AppCtx;
use crate::screens::common::categories::take_requested_kind;
use crate::services::Session;
use crate::services::catalog::split_tag;
use crate::services::categories::{
    Visibility, editor_order, minimal_order, move_in_view, move_to_top, region_label, regions,
    regions_summary, visibility,
};
use crate::task::Loader;
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::{
    Button, Callout, CalloutKind, Chip, Dialog, IconButton, SearchField, Segmented, feedback,
};
use crate::ui::{motion, paint, text};

const KINDS: [ContentKind; 3] = [ContentKind::Live, ContentKind::Movie, ContentKind::Series];
const KIND_LABELS: [&str; 3] = ["Live TV", "Movies", "Series"];
const ROW_H: f32 = 56.0;
const ROW_GAP: f32 = 4.0;
const ACTION_D: f32 = 40.0;
const ACTION_GAP: f32 = 8.0;
/// The row's buttons, left to right.
const ACTIONS: [&str; 3] = ["top", "up", "down"];
/// Undo stays offered this long after a change.
const UNDO_FOR: Duration = Duration::from_secs(30);

fn row_id(kind: ContentKind, id: &str) -> Id {
    Id::unique(("settings.categories.row", kind, id))
}

fn action_id(kind: ContentKind, id: &str, action: &str) -> Id {
    Id::unique(("settings.categories.action", kind, id, action))
}

fn kind_noun(kind: ContentKind) -> (&'static str, &'static str) {
    match kind {
        ContentKind::Live => ("channel", "channels"),
        ContentKind::Movie => ("movie", "movies"),
        ContentKind::Series => ("series", "series"),
    }
}

/// One section's categories, with item counts when known (Live TV).
#[derive(Debug)]
struct Section {
    categories: Vec<Category>,
    counts: HashMap<CategoryId, usize>,
}

/// The last change, for Undo.
#[derive(Debug, Clone)]
struct Undo {
    kind: ContentKind,
    before: CategoryPrefs,
    what: String,
    at: Instant,
    /// A run of moves of one category counts as one change.
    moving: Option<String>,
}

/// What the list asked for this frame.
#[derive(Debug, Default)]
struct Asked {
    toggle: Option<String>,
    moved: Option<(String, Move)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Move {
    Up,
    Down,
    Top,
}

/// The categories page.
#[derive(Debug, Default)]
pub struct CategoriesPage {
    kind: usize,
    sections: [Loader<Arc<Section>>; 3],
    filter: String,
    undo: Option<Undo>,
    confirm_reset: bool,
    focus_after: Option<Id>,
    /// The ids of the rows on screen (last frame), for the keys.
    rows: Vec<String>,
    /// Give the first row the focus once the list is there (opened from a shortcut).
    focus_first: bool,
    loaded_for: Option<ProfileId>,
}

impl CategoriesPage {
    fn current_kind(&self) -> ContentKind {
        KINDS.get(self.kind).copied().unwrap_or(ContentKind::Live)
    }

    fn load(&mut self, cx: &AppCtx<'_>, session: &Arc<Session>, k: usize) {
        let s = Arc::clone(session);
        let Some(loader) = self.sections.get_mut(k) else {
            return;
        };
        loader.start(cx.rt(), &cx.egui, move |_| async move {
            match KINDS.get(k).copied().unwrap_or(ContentKind::Live) {
                ContentKind::Live => {
                    let c = s.live_catalog().await?;
                    let counts = c
                        .categories
                        .iter()
                        .map(|x| (x.id.clone(), c.count(&x.id)))
                        .collect();
                    Ok(Arc::new(Section {
                        categories: c.categories.clone(),
                        counts,
                    }))
                }
                ContentKind::Movie => Ok(Arc::new(Section {
                    categories: s.vod_categories().await?.as_ref().clone(),
                    counts: HashMap::new(),
                })),
                ContentKind::Series => Ok(Arc::new(Section {
                    categories: s.series_categories().await?.as_ref().clone(),
                    counts: HashMap::new(),
                })),
            }
        });
    }

    /// The focused row's category (its row or one of its buttons).
    fn focused_row(&self, ctx: &egui::Context) -> Option<String> {
        let cur = focus::current(ctx)?;
        let kind = self.current_kind();
        self.rows
            .iter()
            .find(|id| {
                cur == row_id(kind, id) || ACTIONS.iter().any(|a| cur == action_id(kind, id, a))
            })
            .cloned()
    }

    /// Applies a change (in the session at once, in the store in the background) and
    /// remembers the state before it for Undo.
    fn change<C, W>(
        &mut self,
        session: &Session,
        kind: ContentKind,
        what: String,
        moving: Option<&str>,
        change: C,
        write: W,
    ) where
        C: FnOnce(&mut CategoryPrefs),
        W: FnOnce(
                &opensmarters_store::Store,
                ProfileId,
            ) -> Result<(), opensmarters_store::StoreError>
            + Send
            + 'static,
    {
        let before = session.category_prefs(kind).as_ref().clone();
        let same_run = moving.is_some()
            && self.undo.as_ref().is_some_and(|u| {
                u.kind == kind && u.moving.as_deref() == moving && u.at.elapsed() < UNDO_FOR
            });
        if same_run {
            if let Some(u) = &mut self.undo {
                u.at = Instant::now();
                u.what = what;
            }
        } else {
            self.undo = Some(Undo {
                kind,
                before,
                what,
                at: Instant::now(),
                moving: moving.map(str::to_owned),
            });
        }
        session.change_category_prefs(kind, "save the category settings", change, write);
    }

    fn undo_last(&mut self, session: &Session) {
        let Some(u) = self.undo.take() else { return };
        let kind = u.kind;
        let restore = u.before.clone();
        session.change_category_prefs(
            kind,
            "undo the category change",
            move |p| *p = u.before,
            move |s, profile| s.categories().replace(profile, kind, &restore),
        );
    }

    fn toggle(&mut self, cx: &mut AppCtx<'_>, session: &Session, section: &Section, id: &str) {
        let kind = self.current_kind();
        let Some(c) = section.categories.iter().find(|c| c.id.as_str() == id) else {
            return;
        };
        let prefs = session.category_prefs(kind);
        match visibility(c, &prefs) {
            Visibility::HiddenByRegion => {
                cx.toasts.info(format!(
                    "\u{201c}{}\u{201d} is hidden by the region filter. Choose its region above to show it.",
                    text::cap(&c.name, 40)
                ));
                return;
            }
            _ if !is_valid_id(id) => {
                cx.toasts.info("This category can't be changed.");
                return;
            }
            _ => {}
        }
        let hide = !prefs.hidden.contains(id);
        let name = text::cap(&c.name, 60);
        let what = if hide {
            format!("Hidden \u{201c}{name}\u{201d}")
        } else {
            format!("Showing \u{201c}{name}\u{201d} again")
        };
        let (a, b) = (id.to_owned(), id.to_owned());
        self.change(
            session,
            kind,
            what,
            None,
            move |p| {
                if hide {
                    p.hidden.insert(a);
                } else {
                    p.hidden.remove(&a);
                }
            },
            move |s, profile| s.categories().set_hidden(profile, kind, &b, hide),
        );
    }

    fn set_all(&mut self, session: &Session, section: &Section, hide: bool) {
        let kind = self.current_kind();
        let ids: Vec<String> = section
            .categories
            .iter()
            .map(|c| c.id.as_str().to_owned())
            .filter(|id| is_valid_id(id))
            .collect();
        let what = if hide {
            "Every category hidden"
        } else {
            "Every category shown"
        };
        let mem = ids.clone();
        self.change(
            session,
            kind,
            what.to_owned(),
            None,
            move |p| {
                for id in mem {
                    if hide {
                        p.hidden.insert(id);
                    } else {
                        p.hidden.remove(&id);
                    }
                }
            },
            move |s, profile| s.categories().set_hidden_many(profile, kind, &ids, hide),
        );
    }

    fn reset(&mut self, session: &Session) {
        let kind = self.current_kind();
        self.change(
            session,
            kind,
            "Back to all categories in your provider's order".to_owned(),
            None,
            |p| *p = CategoryPrefs::default(),
            move |s, profile| s.categories().reset(profile, kind),
        );
    }

    fn toggle_region(&mut self, session: &Session, tag: &str) {
        let kind = self.current_kind();
        let mut regions = session.category_prefs(kind).regions.clone();
        if !regions.remove(tag) {
            regions.insert(tag.to_owned());
        }
        let what = if regions.is_empty() {
            "Every region shown".to_owned()
        } else {
            let p = CategoryPrefs {
                regions: regions.clone(),
                ..CategoryPrefs::default()
            };
            format!("Showing {}", regions_summary(&p))
        };
        let mut tags: Vec<String> = regions.iter().cloned().collect();
        tags.sort();
        self.change(
            session,
            kind,
            what,
            None,
            move |p| p.regions = regions,
            move |s, profile| s.categories().set_regions(profile, kind, &tags),
        );
    }

    fn move_row(&mut self, session: &Session, section: &Section, id: &str, how: Move) {
        let kind = self.current_kind();
        let prefs = session.category_prefs(kind);
        let mut order: Vec<String> = editor_order(&section.categories, &prefs)
            .into_iter()
            .map(|c| c.id.as_str().to_owned())
            .collect();
        let moved = match how {
            Move::Top => move_to_top(&mut order, id),
            Move::Up => move_in_view(&mut order, &self.rows, id, -1),
            Move::Down => move_in_view(&mut order, &self.rows, id, 1),
        };
        if !moved {
            return;
        }
        // Ids the store can't keep stay in provider order instead of failing the change.
        let keep: Vec<String> = minimal_order(&order, &section.categories)
            .iter()
            .filter(|i| is_valid_id(i))
            .cloned()
            .collect();
        let name = section
            .categories
            .iter()
            .find(|c| c.id.as_str() == id)
            .map_or_else(String::new, |c| text::cap(&c.name, 60).into_owned());
        let mem = keep.clone();
        self.change(
            session,
            kind,
            format!("Moved \u{201c}{name}\u{201d}"),
            Some(id),
            move |p| p.order = mem,
            move |s, profile| s.categories().set_order(profile, kind, &keep),
        );
    }
}

impl SettingsPageUi for CategoriesPage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(kind) = take_requested_kind(&cx.egui)
            && let Some(i) = KINDS.iter().position(|k| *k == kind)
        {
            // Opened from a browse screen: start on that section's first category.
            self.kind = i;
            self.filter.clear();
            self.focus_first = true;
        }
        // A section that failed to load gets another try.
        for l in &mut self.sections {
            if l.error().is_some() {
                *l = Loader::new();
            }
        }
    }

    fn on_leave(&mut self, _cx: &mut AppCtx<'_>) {
        self.confirm_reset = false;
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if self.confirm_reset {
            self.confirm_reset = false;
            focus::request(&cx.egui, Id::unique("settings.categories.reset"));
            return true;
        }
        false
    }

    fn on_input(&mut self, cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        if self.confirm_reset || focus::text_input_focused(ctx) {
            return;
        }
        let Some(session) = cx.session() else { return };
        let kind = self.current_kind();
        if self.undo.as_ref().is_some_and(|u| u.kind == kind)
            && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z))
        {
            self.undo_last(&session);
            return;
        }
        // Only look for the focused row when a move key is down.
        let moving = ctx.input(|i| {
            let m = i.modifiers;
            ((m.command || m.shift)
                && (i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowDown)))
                || (m.command && i.key_pressed(Key::Home))
        });
        if !moving {
            return;
        }
        let Some(id) = self.focused_row(ctx) else {
            return;
        };
        let how = ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::ArrowUp)
                || i.consume_key(Modifiers::SHIFT, Key::ArrowUp)
            {
                Some(Move::Up)
            } else if i.consume_key(Modifiers::COMMAND, Key::ArrowDown)
                || i.consume_key(Modifiers::SHIFT, Key::ArrowDown)
            {
                Some(Move::Down)
            } else if i.consume_key(Modifiers::COMMAND, Key::Home) {
                Some(Move::Top)
            } else {
                None
            }
        });
        let Some(how) = how else { return };
        let k = self.kind;
        let Some(section) = self.sections.get(k).and_then(|l| l.value()).cloned() else {
            return;
        };
        self.move_row(&session, &section, &id, how);
        self.focus_after = Some(row_id(self.current_kind(), &id));
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        page_heading(
            ui,
            "Categories",
            Some(
                "Choose which categories appear and in what order. Hidden categories and their \
                 channels disappear from browsing, the guide and search. Your favourites always \
                 stay.",
            ),
        );
        let Some(session) = cx.session() else {
            ui.label(
                egui::RichText::new("Open a profile first.")
                    .font(font::body())
                    .color(color::TEXT_3),
            );
            return;
        };
        if self.loaded_for != Some(session.profile_id()) {
            self.loaded_for = Some(session.profile_id());
            self.sections = Default::default();
            self.undo = None;
            self.filter.clear();
        }
        if self
            .undo
            .as_ref()
            .is_some_and(|u| u.at.elapsed() >= UNDO_FOR)
        {
            self.undo = None;
        }
        let labels: &[&str] = if session.is_xtream() {
            &KIND_LABELS
        } else {
            &KIND_LABELS[..1]
        };
        let mut kind = self.kind.min(labels.len() - 1);
        let filter = &mut self.filter;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            Segmented::new(
                Id::unique("settings.categories.kind"),
                labels,
                128.0 * labels.len() as f32,
            )
            .show(ui, &mut kind);
            let w = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(w, 44.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    SearchField::new(
                        Id::unique("settings.categories.filter"),
                        "Filter categories",
                    )
                    .size(ui.available_width().clamp(160.0, 260.0), 44.0)
                    .show(ui, filter);
                },
            );
        });
        if kind != self.kind {
            self.kind = kind;
            self.filter.clear();
        }
        ui.add_space(20.0);

        // Load the section.
        let idle = {
            let loader = &mut self.sections[kind];
            loader.poll();
            loader.is_idle()
        };
        if idle {
            self.load(cx, &session, kind);
        }
        let loader = &self.sections[kind];
        let Some(sec) = loader.value().cloned() else {
            if let Some(e) = loader.error() {
                Callout::new(
                    CalloutKind::Danger,
                    "Couldn't load the categories:",
                    e.user_message(),
                )
                .width(ui.available_width())
                .show(ui);
            } else if loader.show_skeleton() {
                for _ in 0..5 {
                    let (r, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), ROW_H + ROW_GAP),
                        Sense::hover(),
                    );
                    feedback::skeleton(ui.ctx(), ui.painter(), r.shrink2(vec2(0.0, 10.0)), 8);
                }
            }
            return;
        };
        let ck = self.current_kind();
        let prefs = session.category_prefs(ck);

        self.regions_section(ui, &session, &sec, &prefs);
        self.actions_row(cx, ui, &session, &sec);
        ui.add_space(12.0);

        let asked = self.list(cx, ui, &session, &sec, &prefs);
        if let Some(id) = asked.toggle {
            self.toggle(cx, &session, &sec, &id);
        }
        if let Some((id, how)) = asked.moved {
            self.move_row(&session, &sec, &id, how);
            // Keep the focus on the same button while it can still move that way.
            let still = match how {
                Move::Top => false,
                Move::Up => self.rows.first().is_some_and(|f| *f != id),
                Move::Down => self.rows.last().is_some_and(|l| *l != id),
            };
            let action = match how {
                Move::Top => "top",
                Move::Up => "up",
                Move::Down => "down",
            };
            self.focus_after = Some(if still {
                action_id(ck, &id, action)
            } else {
                row_id(ck, &id)
            });
        }
        self.reset_dialog(cx, ui, &session);
    }
}

impl CategoriesPage {
    fn regions_section(
        &mut self,
        ui: &mut Ui,
        session: &Arc<Session>,
        sec: &Section,
        prefs: &CategoryPrefs,
    ) {
        let found = regions(&sec.categories, &prefs.regions);
        if found.is_empty() {
            return;
        }
        section(
            ui,
            "Regions",
            Some("Show only the categories of some regions. None selected: every region."),
        );
        let mut picked = None;
        let kind = self.current_kind();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for r in &found {
                if Chip::new(region_label(&r.tag))
                    .count(r.count)
                    .selected(prefs.regions.contains(&r.tag))
                    .id(Id::unique((
                        "settings.categories.region",
                        kind,
                        r.tag.as_str(),
                    )))
                    .show(ui)
                    .activated
                {
                    picked = Some(r.tag.clone());
                }
            }
        });
        if let Some(tag) = picked {
            self.toggle_region(session, &tag);
        }
        ui.add_space(20.0);
    }

    fn actions_row(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        sec: &Section,
    ) {
        let mut show_all = false;
        let mut hide_all = false;
        let mut undo = false;
        let undo_text = self
            .undo
            .as_ref()
            .filter(|u| u.kind == self.current_kind())
            .map(|u| u.what.clone());
        let mut ask_reset = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            show_all = Button::secondary("Show all")
                .icon(icons::EYE)
                .small()
                .id(Id::unique("settings.categories.show-all"))
                .show(ui)
                .activated;
            hide_all = Button::secondary("Hide all")
                .icon(icons::EYE_SLASH)
                .small()
                .id(Id::unique("settings.categories.hide-all"))
                .show(ui)
                .activated;
            ask_reset = Button::ghost("Reset")
                .icon(icons::ARROW_COUNTER_CLOCKWISE)
                .small()
                .id(Id::unique("settings.categories.reset"))
                .show(ui)
                .activated;
            let w = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(w, 44.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if let Some(t) = &undo_text {
                        undo = Button::ghost("Undo")
                            .icon(icons::ARROW_COUNTER_CLOCKWISE)
                            .small()
                            .id(Id::unique("settings.categories.undo"))
                            .show(ui)
                            .activated;
                        let room = (ui.available_width() - 8.0).max(0.0);
                        if room > 60.0 {
                            let g = text::galley(
                                ui.painter(),
                                t,
                                font::caption(),
                                color::TEXT_2,
                                room,
                                1,
                            );
                            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                            ui.painter().galley(r.min, g, color::TEXT_2);
                        }
                    }
                },
            );
        });
        if show_all {
            self.set_all(session, sec, false);
        }
        if hide_all {
            self.set_all(session, sec, true);
        }
        if ask_reset {
            self.confirm_reset = true;
        }
        if undo {
            self.undo_last(session);
            focus::request(&cx.egui, Id::unique("settings.categories.show-all"));
        }
    }

    #[allow(clippy::too_many_lines)]
    fn list(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        session: &Arc<Session>,
        sec: &Section,
        prefs: &CategoryPrefs,
    ) -> Asked {
        let kind = self.current_kind();
        let q = SearchQuery::new(&self.filter);
        let view: Vec<&Category> = editor_order(&sec.categories, prefs)
            .into_iter()
            .filter(|c| q.is_empty() || q.matches(&c.name))
            .collect();
        self.rows = view.iter().map(|c| c.id.as_str().to_owned()).collect();
        let mut asked = Asked::default();
        if view.is_empty() {
            ui.label(
                egui::RichText::new(if sec.categories.is_empty() {
                    "This profile has no categories here."
                } else {
                    "No category matches the filter."
                })
                .font(font::body())
                .color(color::TEXT_3),
            );
            return asked;
        }
        if self.focus_first {
            self.focus_first = false;
            self.focus_after = view.first().map(|c| row_id(kind, c.id.as_str()));
        }
        if let Some(id) = self.focus_after.take() {
            focus::request(&cx.egui, id);
        }
        let ctx = ui.ctx().clone();
        let (one, many) = kind_noun(kind);
        let w = ui.available_width();
        let actions_w = ACTION_D * 3.0 + ACTION_GAP * 2.0;
        // Rows far outside the visible part are only allocated (1 000 categories stay
        // cheap); the focused row always registers so the focus never gets lost.
        let visible = ui.clip_rect().expand2(vec2(0.0, (ROW_H + ROW_GAP) * 3.0));
        let n = view.len();
        ui.spacing_mut().item_spacing.y = ROW_GAP;
        for (i, c) in view.iter().enumerate() {
            let id = c.id.as_str();
            let (rect, _) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::hover());
            let active = focus::is_focused(&ctx, row_id(kind, id))
                || ACTIONS
                    .iter()
                    .any(|a| focus::is_focused(&ctx, action_id(kind, id, a)));
            if !active && !visible.intersects(rect) {
                continue;
            }
            let vis = visibility(c, prefs);
            let row_rect = Rect::from_min_max(
                rect.min,
                pos2(rect.right() - actions_w - 12.0, rect.bottom()),
            );
            let (_, f) = focus::focusable(ui, row_id(kind, id), row_rect, FocusOpts::default());
            let painter = ui.painter().clone();
            if active {
                painter.rect_filled(row_rect, CornerRadius::same(radius::SM), color::SURFACE_3);
            }
            painter.hline(
                rect.x_range(),
                rect.bottom() + ROW_GAP / 2.0,
                egui::Stroke::new(1.0, color::STROKE_SUBTLE),
            );
            let shown = vis == Visibility::Shown;
            let fg = if !shown {
                color::TEXT_3
            } else if active {
                color::TEXT
            } else {
                color::TEXT_2
            };
            // Right side: switch (or "Hidden by region"), padlock, count.
            let cy = rect.center().y;
            let mut rx = row_rect.right() - 12.0;
            if vis == Visibility::HiddenByRegion {
                let r = painter.text(
                    pos2(rx, cy),
                    Align2::RIGHT_CENTER,
                    "Hidden by region",
                    font::caption(),
                    color::TEXT_3,
                );
                rx = r.left() - 16.0;
            } else {
                let sw = Rect::from_min_size(pos2(rx - 52.0, cy - 15.0), vec2(52.0, 30.0));
                paint_switch(ui, sw, kind, id, shown);
                rx = sw.left() - 16.0;
            }
            if session.is_category_locked(kind, &c.id, &c.name) {
                icons::paint(
                    &painter,
                    pos2(rx - 8.0, cy),
                    icons::LOCK_SIMPLE,
                    16.0,
                    Style::Regular,
                    color::TEXT_3,
                );
                rx -= 28.0;
            }
            if let Some(count) = sec.counts.get(&c.id) {
                let r = painter.text(
                    pos2(rx, cy),
                    Align2::RIGHT_CENTER,
                    format!("{count} {}", if *count == 1 { one } else { many }),
                    font::caption(),
                    color::TEXT_3,
                );
                rx = r.left() - 16.0;
            }
            // Left: tag chip + name.
            let mut x = row_rect.left() + 12.0;
            let (tag, name) = split_tag(&c.name);
            if let Some(tag) = tag {
                let r = crate::ui::widgets::chip::tag(&painter, pos2(x, cy - 10.0), tag);
                x = r.right() + 8.0;
            }
            if !shown {
                icons::paint(
                    &painter,
                    pos2(x + 8.0, cy),
                    icons::EYE_SLASH,
                    16.0,
                    Style::Regular,
                    color::TEXT_3,
                );
                x += 24.0;
            }
            text::line(
                &painter,
                pos2(x, cy),
                Align2::LEFT_CENTER,
                &text::cap(name, 120),
                font::body_strong(),
                fg,
                (rx - x).max(40.0),
            );
            if f.focused {
                paint::focus_ring(&painter, row_rect, radius::SM, 1.0);
            }
            if f.activated {
                asked.toggle = Some(id.to_owned());
            }
            if !active {
                continue;
            }
            // ⤒ ▲ ▼ for the focused row.
            let mut bx = rect.right() - actions_w;
            for (action, glyph, enabled) in [
                ("top", icons::ARROW_LINE_UP, i > 0 || !q.is_empty()),
                ("up", icons::ARROW_UP, i > 0),
                ("down", icons::ARROW_DOWN, i + 1 < n),
            ] {
                let br =
                    Rect::from_min_size(pos2(bx, cy - ACTION_D / 2.0), vec2(ACTION_D, ACTION_D));
                bx += ACTION_D + ACTION_GAP;
                if !enabled {
                    icons::paint(
                        &painter,
                        br.center(),
                        glyph,
                        18.0,
                        Style::Regular,
                        color::TEXT_DISABLED,
                    );
                    continue;
                }
                if IconButton::new(glyph)
                    .size(ACTION_D, 18.0)
                    .surface()
                    .id(action_id(kind, id, action))
                    .show_at(ui, br)
                    .activated
                {
                    asked.moved = Some((
                        id.to_owned(),
                        match action {
                            "top" => Move::Top,
                            "up" => Move::Up,
                            _ => Move::Down,
                        },
                    ));
                }
            }
        }
        // Hint.
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(if cx.layout.tv {
                "Enter shows or hides \u{b7} \u{2192} for move buttons"
            } else {
                "Enter shows or hides \u{b7} Ctrl+\u{2191}/\u{2193} to move \u{b7} Ctrl+Home to move to the top"
            })
            .font(font::caption())
            .color(color::TEXT_3),
        );
        asked
    }

    fn reset_dialog(&mut self, cx: &mut AppCtx<'_>, ui: &Ui, session: &Arc<Session>) {
        if !self.confirm_reset {
            return;
        }
        let label = KIND_LABELS.get(self.kind).copied().unwrap_or("Live TV");
        let body = format!(
            "Every {label} category will show again, in your provider's order, with no region \
             filter."
        );
        let mut close = false;
        let mut reset = false;
        Dialog::new(
            Id::unique("settings.categories.reset.dialog"),
            "Reset the categories?",
        )
        .body(&body)
        .show(ui.ctx(), |ui| {
            if Button::danger("Reset")
                .icon(icons::ARROW_COUNTER_CLOCKWISE)
                .id(Id::unique("settings.categories.reset.yes"))
                .show(ui)
                .activated
            {
                reset = true;
                close = true;
            }
            if Button::secondary("Cancel")
                .autofocus()
                .id(Id::unique("settings.categories.reset.no"))
                .show(ui)
                .activated
            {
                close = true;
            }
        });
        if reset {
            self.reset(session);
        }
        if close {
            self.confirm_reset = false;
            focus::request(&cx.egui, Id::unique("settings.categories.reset"));
        }
    }
}

/// The shown/hidden switch of a row (the row itself takes the focus and the activation;
/// this is only its picture, like [`crate::ui::widgets::Toggle`]).
fn paint_switch(ui: &Ui, rect: Rect, kind: ContentKind, id: &str, on: bool) {
    let painter = ui.painter();
    let t = motion::animate_bool(
        ui.ctx(),
        Id::unique(("settings.categories.switch", kind, id)),
        on,
        crate::ui::theme::motion::BASE,
    );
    let track = paint::lerp_color(color::SURFACE_4, color::ACCENT, t);
    painter.rect_filled(rect, CornerRadius::same(radius::FULL), track);
    let knob_r = 11.0;
    let x = egui::lerp(rect.left() + 4.0 + knob_r..=rect.right() - 4.0 - knob_r, t);
    let knob = paint::lerp_color(color::TEXT_2, color::TEXT_ON_ACCENT, t);
    painter.circle_filled(pos2(x, rect.center().y), knob_r, knob);
}

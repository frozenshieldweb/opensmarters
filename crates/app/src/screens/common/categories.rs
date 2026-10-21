// SPDX-License-Identifier: GPL-3.0-or-later
//! Category management in the browse screens (Live TV, Movies, Series): the **Edit
//! categories** shortcut to Settings › Categories, **H** to hide the focused category at
//! once (with an [`UndoToast`] for [`UNDO_FOR`], and Ctrl+Z), and the quiet "Showing FR,
//! CA · Edit" note while a region filter is set. The rules themselves live in
//! [`crate::services::categories`].

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Id, Key, Modifiers, Pos2, Rect, Ui, pos2, vec2};
use opensmarters_core::{Category, CategoryId};
use opensmarters_store::{CategoryPrefs, ContentKind};

use crate::focus::{self, FocusOpts};
use crate::nav::{Route, SettingsPage};
use crate::screens::AppCtx;
use crate::services::Session;
use crate::services::categories::regions_summary;
use crate::ui::icons;
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::UndoToast;
use crate::ui::{paint, text};

/// How long Undo is offered after hiding a category (like removing a favourite).
pub const UNDO_FOR: Duration = Duration::from_secs(8);

fn requested_kind_id() -> Id {
    Id::unique("categories.editor.kind")
}

/// Opens Settings › Categories on `kind`.
pub fn open_editor(cx: &mut AppCtx<'_>, kind: ContentKind) {
    cx.egui
        .data_mut(|d| d.insert_temp(requested_kind_id(), kind));
    cx.nav.push(Route::Settings {
        page: SettingsPage::Categories,
    });
}

/// The section a shortcut asked the editor to open on (once).
pub fn take_requested_kind(ctx: &egui::Context) -> Option<ContentKind> {
    ctx.data_mut(|d| {
        let kind = d.get_temp::<ContentKind>(requested_kind_id());
        d.remove::<ContentKind>(requested_kind_id());
        kind
    })
}

/// A category hidden with H, for Undo.
#[derive(Debug, Clone)]
pub struct Hidden {
    /// Its section.
    pub kind: ContentKind,
    /// The category.
    pub id: CategoryId,
    /// Its name (for the toast).
    pub name: String,
    at: Instant,
}

impl Hidden {
    /// Undo is no longer offered.
    pub fn expired(&self) -> bool {
        self.at.elapsed() >= UNDO_FOR
    }
}

/// H pressed (no modifier, no text field focused): consumed only when `wanted`.
pub fn take_hide_key(ctx: &egui::Context, wanted: bool) -> bool {
    wanted
        && !focus::text_input_focused(ctx)
        && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::H))
}

/// Ctrl+Z pressed (consumed only when `wanted`).
pub fn take_undo_key(ctx: &egui::Context, wanted: bool) -> bool {
    wanted
        && !focus::text_input_focused(ctx)
        && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Z))
}

/// Hides `category` at once: in memory now, in the store in the background. Returns
/// what Undo needs.
pub fn hide(session: &Session, kind: ContentKind, category: &Category) -> Hidden {
    set_hidden(session, kind, &category.id, true);
    Hidden {
        kind,
        id: category.id.clone(),
        name: category.name.clone(),
        at: Instant::now(),
    }
}

/// Shows a category hidden with [`hide`] again.
pub fn unhide(session: &Session, hidden: &Hidden) {
    set_hidden(session, hidden.kind, &hidden.id, false);
}

fn set_hidden(session: &Session, kind: ContentKind, id: &CategoryId, on: bool) {
    let id = id.as_str().to_owned();
    let write_id = id.clone();
    session.change_category_prefs(
        kind,
        if on {
            "hide the category"
        } else {
            "show the category"
        },
        move |p| {
            if on {
                p.hidden.insert(id);
            } else {
                p.hidden.remove(&id);
            }
        },
        move |s, profile| s.categories().set_hidden(profile, kind, &write_id, on),
    );
}

/// The undo toast for `hidden`, centred on the bottom edge of `area` (just above it).
/// Returns true when Undo was chosen.
pub fn undo_toast(ui: &mut Ui, hidden: &Hidden, area: Rect) -> bool {
    ui.ctx().request_repaint_after(Duration::from_millis(500));
    let msg = format!("Hidden \u{201c}{}\u{201d}", text::cap(&hidden.name, 60));
    UndoToast::new(Id::unique(("categories.undo", hidden.kind)), &msg)
        .icon(icons::EYE_SLASH)
        .show(
            ui,
            pos2(area.center().x, area.bottom() - 4.0),
            area.width().min(520.0),
        )
}

/// The id of the undo toast's button (focus it after H when nothing else is left).
pub fn undo_button_id(kind: ContentKind) -> Id {
    UndoToast::button_id(Id::unique(("categories.undo", kind)))
}

/// Whether a region filter is set (the note shows).
pub fn has_region_filter(prefs: &CategoryPrefs) -> bool {
    !prefs.regions.is_empty()
}

/// "Showing FR, CA · Edit": a quiet, focusable line (left-centred on `pos`, at most
/// `max_w` wide) while a region filter is set. Returns true when activated (open the
/// editor).
pub fn region_note(ui: &mut Ui, id: Id, prefs: &CategoryPrefs, pos: Pos2, max_w: f32) -> bool {
    let painter = ui.painter().clone();
    let tail = " \u{b7} Edit";
    let tail_w = text::width(&painter, tail, font::caption());
    let lead = format!("Showing {}", regions_summary(prefs));
    let lead_r = text::line(
        &painter,
        pos,
        Align2::LEFT_CENTER,
        &lead,
        font::caption(),
        color::TEXT_3,
        (max_w - tail_w).max(40.0),
    );
    let rect = Rect::from_min_max(
        pos2(pos.x, pos.y - 10.0),
        pos2(lead_r.right() + tail_w, pos.y + 10.0),
    )
    .expand2(vec2(6.0, 2.0));
    let (_, f) = focus::focusable(ui, id, rect, FocusOpts::default());
    painter.text(
        pos2(lead_r.right(), pos.y),
        Align2::LEFT_CENTER,
        tail,
        font::caption(),
        if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        },
    );
    if f.focused {
        paint::focus_ring(&painter, rect, radius::SM, 1.0);
    }
    f.activated
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! The shell: the side rail (§6.1) and the status cluster (clock, offline and
//! unencrypted-connection indicators) shared by screens.

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};

use super::AppCtx;
use crate::focus::{self, FocusOpts};
use crate::nav::{Route, Section};
use crate::ui::assets::{self, Asset};
use crate::ui::icons::{self, Style};
use crate::ui::layout::{RAIL_COLLAPSED, RAIL_EXPANDED};
use crate::ui::motion;
use crate::ui::theme::{color, elevation, font, radius};
use crate::ui::widgets::clock;
use crate::ui::{paint, text};

struct NavItem {
    section: Section,
    label: &'static str,
    glyph: &'static str,
}

const MAIN_ITEMS: [NavItem; 7] = [
    NavItem {
        section: Section::Home,
        label: "Home",
        glyph: icons::HOUSE,
    },
    NavItem {
        section: Section::Live,
        label: "Live TV",
        glyph: icons::TELEVISION_SIMPLE,
    },
    NavItem {
        section: Section::Guide,
        label: "Guide",
        glyph: icons::ROWS,
    },
    NavItem {
        section: Section::Movies,
        label: "Movies",
        glyph: icons::FILM_STRIP,
    },
    NavItem {
        section: Section::Series,
        label: "Series",
        glyph: icons::MONITOR_PLAY,
    },
    NavItem {
        section: Section::Catchup,
        label: "Catch-up",
        glyph: icons::CLOCK_COUNTER_CLOCKWISE,
    },
    NavItem {
        section: Section::Favourites,
        label: "Favourites",
        glyph: icons::HEART,
    },
];

/// The route a section opens.
pub fn section_route(s: Section) -> Route {
    match s {
        Section::Search => Route::Search {
            query: String::new(),
        },
        Section::Home => Route::Home,
        Section::Live => Route::live(),
        Section::Guide => Route::Guide,
        Section::Movies => Route::Movies { category: None },
        Section::Series => Route::Series { category: None },
        Section::Catchup => Route::Catchup { stream_id: None },
        Section::Favourites => Route::Favourites,
        Section::Settings => Route::settings(),
    }
}

/// The focus id of a rail item (e.g. to focus "Live TV" programmatically).
pub fn rail_item_id(s: Section) -> Id {
    Id::unique(("shell.rail", s))
}

/// Paints the side rail and handles its items.
pub fn rail(cx: &mut AppCtx<'_>, ui: &mut Ui, current: Option<Section>) {
    let ctx = ui.ctx().clone();
    let screen = cx.layout.screen;
    let collapsed = !cx.layout.is_wide();
    let in_rail = focus::is_in_group(&ctx, focus::shell_group());
    let t = if collapsed {
        motion::animate_bool(
            &ctx,
            Id::unique("shell.rail.expand"),
            in_rail,
            crate::ui::theme::motion::SLOW,
        )
    } else {
        1.0
    };
    let w = egui::lerp(RAIL_COLLAPSED..=RAIL_EXPANDED, t);
    let rect = Rect::from_min_size(screen.min, vec2(w, screen.height()));
    // The rail draws on its own layer above the content (it overlays it when expanding).
    let layer = egui::LayerId::new(egui::Order::Middle, Id::unique("shell.rail.layer"));
    let mut rail_ui = ui.new_child(
        egui::UiBuilder::new()
            .layer_id(layer)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    rail_ui.set_clip_rect(rect.expand2(vec2(40.0, 0.0)));
    let painter = rail_ui.painter().clone();
    if collapsed && t > 0.01 {
        paint::shadow(&painter, rect, 0, elevation::E2);
    }
    painter.rect_filled(rect, 0.0, color::SURFACE_1);
    painter.vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(1.0, color::STROKE_SUBTLE),
    );
    // Block clicks to the content under the expanded overlay.
    let _ = rail_ui.interact(rect, Id::unique("shell.rail.bg"), Sense::hover());

    let show_labels = t > 0.55;
    let label_alpha = ((t - 0.55) / 0.45).clamp(0.0, 1.0);

    // Brand: symbol 28 pt + wordmark 20 pt tall.
    let sym = Rect::from_center_size(
        pos2(rect.left() + 36.0, rect.top() + 36.0),
        vec2(28.0, 28.0),
    );
    assets::paint_contain(&rail_ui, &painter, Asset::Symbol, sym, Color32::WHITE);
    if show_labels {
        let wm_h = 20.0;
        let wm = Rect::from_min_size(
            pos2(sym.right() + 10.0, sym.center().y - wm_h / 2.0 - 1.0),
            vec2(wm_h * assets::aspect(Asset::Wordmark), wm_h),
        );
        assets::paint_exact(
            &rail_ui,
            &painter,
            Asset::Wordmark,
            wm,
            Color32::WHITE.gamma_multiply(label_alpha),
        );
    }

    let item_w = w - 24.0;
    // Adaptive vertical rhythm: 56 pt steps when there is room, tighter on short
    // windows (TV mode at 720p is ~620 pt tall).
    let compact = rect.height() < 760.0;
    let (search_y, divider_y, top0) = if compact {
        (68.0, 124.0, 136.0)
    } else {
        (87.0, 156.0, 176.0)
    };
    let bottom_h = if compact { 44.0 } else { 48.0 };
    let settings_y = rect.bottom() - (if compact { 16.0 } else { 24.0 }) - bottom_h;
    let profile_y = settings_y - bottom_h - 8.0;
    let step =
        ((profile_y - 12.0 - (rect.top() + top0)) / MAIN_ITEMS.len() as f32).clamp(36.0, 56.0);
    let item_h = (step - 6.0).clamp(34.0, 48.0);
    // Entering the rail from the content lands on the current section.
    if !in_rail && let Some(s) = current {
        focus::with(&ctx, |e| e.remember(focus::shell_group(), rail_item_id(s)));
    }
    let mut activated: Option<Section> = None;
    let mut profile_activated = false;
    focus::group(&mut rail_ui, focus::shell_group(), |ui| {
        focus::scope(ui, Id::unique("shell.rail"), |ui| {
            let mut item = |ui: &mut Ui, y: f32, it: &NavItem| {
                let h = if it.section == Section::Settings {
                    bottom_h
                } else {
                    item_h
                };
                let r = Rect::from_min_size(pos2(rect.left() + 12.0, y), vec2(item_w, h));
                if nav_item(
                    ui,
                    r,
                    it,
                    current == Some(it.section),
                    show_labels,
                    label_alpha,
                    rect.right(),
                ) {
                    activated = Some(it.section);
                }
            };
            item(
                ui,
                rect.top() + search_y,
                &NavItem {
                    section: Section::Search,
                    label: "Search",
                    glyph: icons::MAGNIFYING_GLASS,
                },
            );
            painter.hline(
                (rect.left() + 24.0)..=(rect.right() - 24.0),
                rect.top() + divider_y,
                Stroke::new(1.0, color::STROKE_SUBTLE),
            );
            for (i, it) in MAIN_ITEMS.iter().enumerate() {
                item(ui, rect.top() + top0 + i as f32 * step, it);
            }
            // Bottom: profile and settings.
            let profile_r =
                Rect::from_min_size(pos2(rect.left() + 12.0, profile_y), vec2(item_w, bottom_h));
            if profile_item(ui, cx, profile_r, show_labels, label_alpha) {
                profile_activated = true;
            }
            item(
                ui,
                settings_y,
                &NavItem {
                    section: Section::Settings,
                    label: "Settings",
                    glyph: icons::GEAR_SIX,
                },
            );
        });
    });
    if let Some(s) = activated {
        cx.nav.section(section_route(s));
    }
    if profile_activated {
        cx.nav.push(Route::Profiles);
    }
}

fn nav_item(
    ui: &mut Ui,
    r: Rect,
    it: &NavItem,
    selected: bool,
    labels: bool,
    alpha: f32,
    rail_right: f32,
) -> bool {
    let (_, f) = focus::focusable(ui, rail_item_id(it.section), r, FocusOpts::default());
    let painter = ui.painter().clone();
    if f.focused {
        painter.rect_filled(r, CornerRadius::same(radius::SM), color::SURFACE_3);
    }
    let fg = if selected {
        color::ACCENT
    } else if f.focused {
        color::TEXT
    } else {
        color::TEXT_2
    };
    let icon_x = if labels {
        r.left() + 22.0
    } else {
        r.center().x
    };
    icons::paint(
        &painter,
        pos2(icon_x, r.center().y),
        it.glyph,
        24.0,
        if selected {
            Style::Fill
        } else {
            Style::Regular
        },
        fg,
    );
    if labels {
        let label_c = if selected || f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        painter.text(
            pos2(r.left() + 46.0, r.center().y),
            Align2::LEFT_CENTER,
            it.label,
            font::body_strong(),
            label_c.gamma_multiply(alpha),
        );
    }
    if selected {
        let bar = Rect::from_min_size(pos2(rail_right - 3.0, r.center().y - 12.0), vec2(3.0, 24.0));
        painter.rect_filled(bar, CornerRadius::same(2), color::ACCENT);
    }
    if f.focused {
        paint::focus_ring(&painter, r, radius::SM, 1.0);
    }
    f.activated
}

fn profile_item(ui: &mut Ui, cx: &AppCtx<'_>, r: Rect, labels: bool, alpha: f32) -> bool {
    let (_, f) = focus::focusable(
        ui,
        Id::unique("shell.rail.profile"),
        r,
        FocusOpts::default(),
    );
    let painter = ui.painter().clone();
    if f.focused {
        painter.rect_filled(r, CornerRadius::same(radius::SM), color::SURFACE_3);
    }
    let session = cx.session();
    let name = session
        .as_ref()
        .map_or_else(|| "Profiles".to_owned(), |s| s.profile().name.clone());
    let seed = session
        .as_ref()
        .map_or_else(String::new, |s| format!("{:?}", s.profile_id()));
    let c = if labels {
        pos2(r.left() + 22.0, r.center().y)
    } else {
        r.center()
    };
    let av = Rect::from_center_size(c, vec2(32.0, 32.0));
    let img = egui::Image::new(assets::avatar_for(&seed).source())
        .corner_radius(CornerRadius::same(radius::FULL));
    img.paint_at(ui, av);
    if labels {
        let fg = if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        };
        text::line(
            &painter,
            pos2(r.left() + 46.0, r.center().y),
            Align2::LEFT_CENTER,
            &name,
            font::body_strong(),
            fg.gamma_multiply(alpha),
            r.width() - 46.0 - 36.0,
        );
        icons::paint(
            &painter,
            pos2(r.right() - 18.0, r.center().y),
            icons::CARET_DOWN,
            16.0,
            Style::Regular,
            color::TEXT_3.gamma_multiply(alpha),
        );
    }
    if f.focused {
        paint::focus_ring(&painter, r, radius::SM, 1.0);
    }
    f.activated
}

/// Paints the status cluster at the top-right of the content area: offline and
/// unencrypted indicators and the clock. Returns the rect used.
pub fn status_cluster(cx: &AppCtx<'_>, ui: &Ui) -> Rect {
    let painter = ui.painter().clone();
    let y = cx.layout.content.top() + 56.0;
    let right = cx.layout.content.right() - cx.layout.margin;
    let clock_rect = clock::clock(
        &cx.egui,
        &painter,
        pos2(right, y),
        Align2::RIGHT_CENTER,
        cx.prefs,
        font::title_sm(),
        color::TEXT,
    );
    let mut x = clock_rect.left() - 20.0;
    let mut rect = clock_rect;
    if let Some(s) = cx.session() {
        if s.is_offline() {
            let r = indicator(&painter, x, y, icons::WIFI_SLASH, "Offline", color::WARNING);
            x = r.left() - 16.0;
            rect = rect.union(r);
        }
        if s.is_insecure() {
            let r = indicator(
                &painter,
                x,
                y,
                icons::LOCK_SIMPLE_OPEN,
                "Unencrypted",
                color::WARNING,
            );
            rect = rect.union(r);
        }
    }
    rect
}

fn indicator(
    painter: &egui::Painter,
    right: f32,
    y: f32,
    glyph: &str,
    label: &str,
    c: Color32,
) -> Rect {
    let tr = painter.text(
        pos2(right, y),
        Align2::RIGHT_CENTER,
        label,
        font::caption(),
        c,
    );
    let ir = icons::paint(
        painter,
        pos2(tr.left() - 12.0, y),
        glyph,
        16.0,
        Style::Regular,
        c,
    );
    tr.union(ir)
}

/// A screen title at the standard position (`heading`, page margin, 56 pt from the top).
pub fn page_title(cx: &AppCtx<'_>, ui: &Ui, title: &str) -> Rect {
    ui.painter().text(
        pos2(
            cx.layout.content.left() + cx.layout.margin,
            cx.layout.content.top() + 56.0,
        ),
        Align2::LEFT_CENTER,
        title,
        font::heading(),
        color::TEXT,
    )
}

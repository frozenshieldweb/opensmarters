// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › About: "Check for updates" (notify only: [`crate::services::updates`]),
//! versions, the GPL notice, where the source code is, and the third-party licences
//! ([`super::licences`]).

use eframe::egui::{self, Color32, CornerRadius, Id, Sense, Stroke, Ui, pos2, vec2};
use opensmarters_core::chrono::Utc;

use super::licences::{BUNDLED, Component, RUST};
use super::{SettingsPageUi, paragraph, section, value_label};
use crate::focus::Focus;
use crate::screens::AppCtx;
use crate::services::updates::{self, CURRENT, Status};
use crate::ui::assets::{self, Asset};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::{Button, Dialog, SettingRow, Toggle};
use crate::ui::{paint, text};

/// Where the source code lives (from the workspace manifest).
pub const SOURCE_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// The short GPL notice.
pub const GPL_NOTICE: &str = "opensmarters is free software: you can redistribute it and/or \
modify it under the terms of the GNU General Public License as published by the Free Software \
Foundation, either version 3 of the License, or (at your option) any later version. It is \
distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the \
implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General \
Public License (the LICENSE file shipped with the app) for more details.";

/// The about page.
#[derive(Debug, Default)]
pub struct AboutPage {
    licences: bool,
    /// This build can check for updates (the page then starts at its button).
    updates_supported: bool,
}

/// The focus id of "Check for updates".
const CHECK_ID: &str = "settings.about.check";

/// What leads a status line: a status icon or the spinner.
#[derive(Debug, Clone, Copy)]
enum Lead {
    Icon(&'static str, Color32),
    Spinner,
}

/// A settings row whose description is a status line (icon or spinner + text, never
/// colour alone), with an optional button on the right.
fn status_row(
    ui: &mut Ui,
    label: &str,
    lead: Lead,
    status: &str,
    button: Option<Button>,
) -> Option<Focus> {
    let w = ui.available_width();
    let button_w = button.as_ref().map_or(0.0, |b| b.size(ui).x + 24.0);
    let text_w = (w - button_w).max(w * 0.4);
    let painter = ui.painter().clone();
    let label_g = text::galley(&painter, label, font::body_strong(), color::TEXT, text_w, 1);
    let lead_w = 22.0;
    let status_g = text::galley(
        &painter,
        status,
        font::caption(),
        color::TEXT_2,
        text_w - lead_w,
        2,
    );
    let label_h = label_g.size().y;
    let text_h = label_h + 4.0 + status_g.size().y;
    let h = (text_h + 24.0).max(68.0).ceil();
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let top = rect.center().y - text_h / 2.0;
    painter.galley(pos2(rect.left(), top), label_g, color::TEXT);
    let status_top = top + label_h + 4.0;
    // The lead sits on the first line of the status (caption line height 18).
    let lead_c = pos2(rect.left() + 8.0, status_top + 9.0);
    match lead {
        Lead::Icon(glyph, c) => {
            icons::paint(&painter, lead_c, glyph, 16.0, Style::Regular, c);
        }
        Lead::Spinner => {
            paint::spinner(&painter, lead_c, 6.0, ui.input(|i| i.time), 2.0);
            ui.ctx().request_repaint();
        }
    }
    painter.galley(
        pos2(rect.left() + lead_w, status_top),
        status_g,
        color::TEXT_2,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, color::STROKE_SUBTLE),
    );
    let button = button?;
    let ctrl = egui::Rect::from_min_max(
        pos2(rect.left() + text_w + 16.0, rect.top()),
        pos2(rect.right() - 6.0, rect.bottom()),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(ctrl)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    Some(button.show(&mut child))
}

/// Opens the locally built release link in the browser (off the UI thread).
fn open_release_page(cx: &mut AppCtx<'_>, url: String) {
    cx.services.rt().spawn_blocking(move || {
        if let Err(e) = crate::platform::open_url(&url) {
            tracing::warn!(error = %e, "couldn't open the browser");
        }
    });
    cx.toasts.info("Opening the download page in your browser…");
}

impl AboutPage {
    /// The "Updates" section: the installed version, Check for updates, the result, the
    /// automatic check and what updating means.
    fn updates_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let view = cx.services.updates.view();
        self.updates_supported = view.supported;
        section(ui, "Updates", None);
        let label = format!("opensmarters {CURRENT}");
        if !view.supported {
            status_row(
                ui,
                &label,
                Lead::Icon(icons::INFO, color::TEXT_3),
                "Checking for updates isn't available in this build.",
                None,
            );
            return;
        }
        let now = Utc::now().timestamp();
        let (lead, status) = if view.checking {
            (Lead::Spinner, "Checking for updates…".to_owned())
        } else {
            match &view.status {
                Status::NeverChecked => (
                    Lead::Icon(icons::INFO, color::TEXT_3),
                    "Not checked on this computer yet.".to_owned(),
                ),
                Status::UpToDate { checked_at } => (
                    Lead::Icon(icons::CHECK_CIRCLE, color::SUCCESS),
                    format!(
                        "You're up to date (checked {}).",
                        updates::checked_ago(*checked_at, now)
                    ),
                ),
                Status::Available { version, .. } => (
                    Lead::Icon(icons::ARROW_CIRCLE_UP, color::ACCENT),
                    format!("opensmarters {version} is available."),
                ),
                Status::Failed(e) => (
                    Lead::Icon(icons::WARNING_CIRCLE, color::DANGER),
                    e.message().to_owned(),
                ),
            }
        };
        let failed = matches!(view.status, Status::Failed(_)) && !view.checking;
        let button = if view.checking {
            Button::secondary("Checking…")
        } else if failed {
            Button::secondary("Try again").icon(icons::ARROWS_CLOCKWISE)
        } else {
            Button::secondary("Check for updates").icon(icons::ARROWS_CLOCKWISE)
        }
        .id(Id::unique(CHECK_ID));
        if status_row(ui, &label, lead, &status, Some(button)).is_some_and(|f| f.activated)
            && !view.checking
        {
            updates::check_now(cx);
        }

        if let Status::Available {
            version,
            url,
            dismissed,
        } = &view.status
            && !view.checking
        {
            ui.add_space(12.0);
            self.available_card(cx, ui, version, url.as_deref(), *dismissed);
            ui.add_space(4.0);
        }

        SettingRow::new("Check automatically")
            .description(
                "Once a week, opensmarters asks GitHub for the latest version number. GitHub \
                 sees your IP address; nothing else is sent.",
            )
            .show(ui, |ui| {
                let mut on = cx.prefs.check_updates_automatically;
                Toggle::new(Id::unique("settings.about.auto-update")).show(ui, &mut on);
                if on != cx.prefs.check_updates_automatically {
                    cx.prefs.check_updates_automatically = on;
                    cx.save_prefs();
                }
            });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Updating means running the new setup: it keeps your accounts, favourites \
                 and settings.",
            )
            .font(font::caption())
            .color(color::TEXT_3),
        );
    }

    /// "opensmarters X is available": Get the update (opens the release page) and Not now
    /// (no more header indicator or toast for this version).
    fn available_card(
        &mut self,
        cx: &mut AppCtx<'_>,
        ui: &mut Ui,
        version: &updates::Version,
        url: Option<&str>,
        dismissed: bool,
    ) {
        let fill = if dismissed {
            color::SURFACE_2
        } else {
            color::ACCENT_SOFT
        };
        let mut get = false;
        let mut not_now = false;
        egui::Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(radius::MD))
            .inner_margin(egui::Margin::symmetric(20, 16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let w = ui.available_width();
                let painter = ui.painter().clone();
                let title = text::galley(
                    &painter,
                    &format!("opensmarters {version} is available"),
                    font::body_strong(),
                    color::TEXT,
                    w - 36.0,
                    1,
                );
                let body = text::galley(
                    &painter,
                    if dismissed {
                        "You chose not to update for now, so the header won't remind you \
                         about this version. Download the new setup and run it whenever \
                         you like."
                    } else {
                        "Download the new setup and run it: it updates opensmarters in place \
                         and keeps your accounts, favourites and settings."
                    },
                    font::body(),
                    color::TEXT_2,
                    w - 36.0,
                    3,
                );
                let text_h = title.size().y + 2.0 + body.size().y;
                let (r, _) = ui.allocate_exact_size(vec2(w, text_h), Sense::hover());
                icons::paint(
                    &painter,
                    pos2(r.left() + 12.0, r.top() + 12.0),
                    icons::ARROW_CIRCLE_UP,
                    24.0,
                    Style::Regular,
                    color::ACCENT,
                );
                let title_h = title.size().y;
                painter.galley(pos2(r.left() + 36.0, r.top()), title, color::TEXT);
                painter.galley(
                    pos2(r.left() + 36.0, r.top() + title_h + 2.0),
                    body,
                    color::TEXT_2,
                );
                if url.is_none() && dismissed {
                    return;
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space(36.0);
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if url.is_some() {
                        let b = if dismissed {
                            Button::secondary("Get the update")
                        } else {
                            Button::primary("Get the update")
                        };
                        get = b
                            .trailing(icons::ARROW_SQUARE_OUT)
                            .id(Id::unique("settings.about.get-update"))
                            .show(ui)
                            .activated;
                    }
                    if !dismissed {
                        not_now = Button::ghost("Not now")
                            .id(Id::unique("settings.about.not-now"))
                            .show(ui)
                            .activated;
                    }
                });
            });
        if get && let Some(u) = url {
            open_release_page(cx, u.to_owned());
        }
        if not_now {
            cx.services.updates.dismiss(cx.services);
            cx.toasts.push(
                crate::ui::widgets::ToastKind::Info,
                format!("You won't be reminded about opensmarters {version}."),
                Some(updates::TOAST_KEY),
            );
            crate::focus::request(&cx.egui, Id::unique("settings.about.get-update"));
        }
    }
}

fn component_rows(ui: &mut Ui, list: &[Component]) {
    for c in list {
        // Focusable so the list scrolls with the keyboard / a remote.
        super::readable(ui, Id::unique(("settings.about.licence", c.name)), |ui| {
            SettingRow::new(c.name)
                .description(c.role)
                .show(ui, |ui| value_label(ui, c.licence, color::TEXT_2));
        });
    }
}

impl SettingsPageUi for AboutPage {
    fn entry_focus(&self) -> Option<Id> {
        Some(Id::unique(if self.updates_supported {
            CHECK_ID
        } else {
            "settings.about.gpl"
        }))
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        let was_open = std::mem::take(&mut self.licences);
        if was_open {
            crate::focus::request(&cx.egui, Id::unique("settings.about.licences"));
        }
        was_open
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let h = 44.0;
        let (r, _) = ui.allocate_exact_size(
            vec2(h * assets::aspect(Asset::Lockup), h),
            egui::Sense::hover(),
        );
        assets::paint_exact(ui, ui.painter(), Asset::Lockup, r, Color32::WHITE);
        ui.add_space(16.0);
        paragraph(
            ui,
            "A free, open-source player for the IPTV subscription you already have. It comes \
             with no channels of its own. It talks to your provider, and to GitHub only when \
             you check for updates.",
        );
        ui.add_space(24.0);
        self.updates_section(cx, ui);

        ui.add_space(28.0);
        section(ui, "This build", None);
        let mpv = cx
            .playback
            .mpv_version()
            .unwrap_or_else(|| "not loaded".into());
        SettingRow::new("Video engine")
            .description("libmpv")
            .show(ui, |ui| value_label(ui, &mpv, color::TEXT_2));
        if let Some(ff) = cx.playback.ffmpeg_version() {
            SettingRow::new("FFmpeg").show(ui, |ui| value_label(ui, &ff, color::TEXT_2));
        }
        SettingRow::new("Privacy")
            .description("Details in Privacy & security")
            .show(ui, |ui| {
                value_label(ui, "No telemetry, ever", color::SUCCESS)
            });

        ui.add_space(28.0);
        section(ui, "Licence", None);
        SettingRow::new("opensmarters")
            .description("You can study, change and share it")
            .show(ui, |ui| value_label(ui, "GPL-3.0-or-later", color::TEXT_2));
        ui.add_space(8.0);
        super::readable(ui, Id::unique("settings.about.gpl"), |ui| {
            ui.label(
                egui::RichText::new(GPL_NOTICE)
                    .font(font::caption())
                    .color(color::TEXT_3),
            );
        });
        ui.add_space(16.0);
        SettingRow::new("Source code")
            .description(SOURCE_URL)
            .show(ui, |ui| {
                if Button::ghost("Copy link")
                    .icon(icons::COPY)
                    .small()
                    .id(Id::unique("settings.about.copy-source"))
                    .show(ui)
                    .activated
                {
                    cx.egui.copy_text(SOURCE_URL.to_owned());
                    cx.toasts.success("Link copied.");
                }
            });
        SettingRow::new("Third-party software")
            .description("Libraries, fonts and icons, and their licences")
            .show(ui, |ui| {
                if Button::secondary("Show licences")
                    .trailing(icons::CARET_RIGHT)
                    .small()
                    .id(Id::unique("settings.about.licences"))
                    .show(ui)
                    .activated
                {
                    self.licences = true;
                }
            });

        if self.licences {
            let mut close = false;
            Dialog::new(Id::unique("settings.about.licences.dialog"), "")
                .width(720.0)
                .show_custom(ui.ctx(), |ui| {
                    ui.label(
                        egui::RichText::new("Third-party software")
                            .font(font::title())
                            .color(color::TEXT),
                    );
                    ui.add_space(4.0);
                    paragraph(
                        ui,
                        "opensmarters is built on these projects. Thank you to their authors.",
                    );
                    ui.add_space(12.0);
                    let max_h = (ui.ctx().content_rect().height() - 320.0).clamp(160.0, 560.0);
                    egui::ScrollArea::vertical()
                        .id_salt("settings.about.licences.scroll")
                        .max_height(max_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            section(ui, "Bundled", None);
                            component_rows(ui, BUNDLED);
                            ui.add_space(16.0);
                            section(ui, "Rust libraries", None);
                            component_rows(ui, RUST);
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new(
                                    "These libraries use others in turn, under the same kinds of \
                                     permissive licences; release packages include every \
                                     licence text.",
                                )
                                .font(font::caption())
                                .color(color::TEXT_3),
                            );
                        });
                    ui.add_space(16.0);
                    let w = ui.available_width();
                    ui.allocate_ui_with_layout(
                        egui::vec2(w, 44.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if Button::primary("Close")
                                .autofocus()
                                .id(Id::unique("settings.about.licences.close"))
                                .show(ui)
                                .activated
                            {
                                close = true;
                            }
                        },
                    );
                });
            if close {
                self.licences = false;
                crate::focus::request(&cx.egui, Id::unique("settings.about.licences"));
            }
        }
    }
}

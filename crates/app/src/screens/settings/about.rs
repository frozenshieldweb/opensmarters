// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › About: versions, the GPL notice, where the source code is, and the
//! third-party licences ([`super::licences`]).

use eframe::egui::{self, Color32, Id, Ui, vec2};

use super::licences::{BUNDLED, Component, RUST};
use super::{SettingsPageUi, paragraph, section, value_label};
use crate::screens::AppCtx;
use crate::ui::assets::{self, Asset};
use crate::ui::icons;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{Button, Dialog, SettingRow};

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
}

fn component_rows(ui: &mut Ui, list: &[Component]) {
    for c in list {
        SettingRow::new(c.name)
            .description(c.role)
            .show(ui, |ui| value_label(ui, c.licence, color::TEXT_2));
    }
}

impl SettingsPageUi for AboutPage {
    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        std::mem::take(&mut self.licences)
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
             with no channels of its own, and it only talks to your provider.",
        );
        ui.add_space(16.0);
        SettingRow::new("Version").show(ui, |ui| {
            value_label(ui, env!("CARGO_PKG_VERSION"), color::TEXT_2)
        });
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
        ui.label(
            egui::RichText::new(GPL_NOTICE)
                .font(font::caption())
                .color(color::TEXT_3),
        );
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
            }
        }
    }
}

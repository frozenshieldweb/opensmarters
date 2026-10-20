// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings → About: versions, licences, privacy statement, data locations.

use eframe::egui::{self, Color32, Ui, vec2};

use super::{SettingsPageUi, value_label};
use crate::screens::AppCtx;
use crate::ui::assets::{self, Asset};
use crate::ui::theme::{color, font};
use crate::ui::widgets::SettingRow;

/// The about page.
#[derive(Debug, Default)]
pub struct AboutPage;

impl SettingsPageUi for AboutPage {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let h = 44.0;
        let (r, _) = ui.allocate_exact_size(
            vec2(h * assets::aspect(Asset::Lockup), h),
            egui::Sense::hover(),
        );
        assets::paint_exact(ui, ui.painter(), Asset::Lockup, r, Color32::WHITE);
        ui.add_space(16.0);
        ui.label(
            egui::RichText::new("A free, open-source player for your own IPTV subscription. No telemetry, no analytics, no phoning home: it only talks to your provider.")
                .font(font::body())
                .color(color::TEXT_2),
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
            .description("libmpv (GPL build)")
            .show(ui, |ui| value_label(ui, &mpv, color::TEXT_2));
        if let Some(ff) = cx.playback.ffmpeg_version() {
            SettingRow::new("FFmpeg").show(ui, |ui| value_label(ui, &ff, color::TEXT_2));
        }
        SettingRow::new("Licence")
            .description("You can study, change and share it")
            .show(ui, |ui| value_label(ui, "GPL-3.0-or-later", color::TEXT_2));
        SettingRow::new("Fonts")
            .description("Source Sans 3 and Fraunces")
            .show(ui, |ui| {
                value_label(ui, "SIL Open Font License 1.1", color::TEXT_2)
            });
        SettingRow::new("Icons")
            .description("Phosphor Icons")
            .show(ui, |ui| value_label(ui, "MIT", color::TEXT_2));
        SettingRow::new("Privacy")
            .description("Passwords stay in the system keychain")
            .show(ui, |ui| {
                value_label(ui, "No telemetry, ever", color::SUCCESS)
            });
        let data = if cx.services.ephemeral && cx.services.paths.data_dir.is_none() {
            "Ephemeral (nothing saved)".to_owned()
        } else {
            cx.services
                .paths
                .data_dir
                .as_ref()
                .map_or_else(|| "Default".to_owned(), |p| p.display().to_string())
        };
        SettingRow::new("Data folder").show(ui, |ui| value_label(ui, &data, color::TEXT_2));
    }
}

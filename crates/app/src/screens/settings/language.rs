// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Language & region: the 12/24-hour clock (guide, player, status bar) and the
//! display language (English only for now; said plainly).

use eframe::egui::{Id, Ui};
use opensmarters_core::chrono::Local;
use opensmarters_store::settings::TimeFormat;

use super::{SettingsPageUi, page_heading, paragraph, value_label};
use crate::screens::AppCtx;
use crate::ui::theme::color;
use crate::ui::widgets::{Segmented, SettingRow};

/// The language & region page.
#[derive(Debug, Default)]
pub struct LanguagePage;

impl SettingsPageUi for LanguagePage {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        page_heading(
            ui,
            "Language & region",
            Some("These apply to every profile on this computer."),
        );
        let now = cx.prefs.format_time(&Local::now());
        let desc = format!("Guide, player and clock. Now: {now}");
        let mut changed = false;
        SettingRow::new("Clock").description(&desc).show(ui, |ui| {
            let mut sel = usize::from(cx.prefs.time_format == TimeFormat::H12);
            Segmented::new(Id::unique("settings.clock"), &["24-hour", "12-hour"], 240.0)
                .show(ui, &mut sel);
            let tf = if sel == 1 {
                TimeFormat::H12
            } else {
                TimeFormat::H24
            };
            if tf != cx.prefs.time_format {
                cx.prefs.time_format = tf;
                changed = true;
            }
        });
        SettingRow::new("Display language")
            .description("More languages are planned")
            .show(ui, |ui| value_label(ui, "English", color::TEXT_2));
        ui.add_space(12.0);
        paragraph(
            ui,
            "Channel, film and programme names are shown exactly as your provider sends them. \
             Preferred audio and subtitle languages are in Playback.",
        );
        if changed {
            cx.save_prefs();
        }
    }
}

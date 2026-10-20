// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings → Appearance: TV mode, reduced motion, 12/24 h clock (app-wide; applied at
//! once and saved).

use eframe::egui::{Id, Ui};
use opensmarters_store::settings::TimeFormat;

use super::{SettingsPageUi, page_heading};
use crate::screens::AppCtx;
use crate::ui::widgets::{Segmented, SettingRow, Toggle};

/// The appearance page.
#[derive(Debug, Default)]
pub struct AppearancePage;

impl SettingsPageUi for AppearancePage {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        page_heading(
            ui,
            "Appearance",
            Some("These apply to every profile on this computer."),
        );
        let mut changed = false;
        SettingRow::new("TV mode")
            .description("Bigger text and controls for watching from the sofa")
            .show(ui, |ui| {
                let mut on = cx.prefs.tv_mode;
                Toggle::new(Id::unique("settings.tv")).show(ui, &mut on);
                if on != cx.prefs.tv_mode {
                    cx.prefs.tv_mode = on;
                    changed = true;
                }
            });
        SettingRow::new("Reduce motion")
            .description("No zoom on focus, no shimmer; shorter fades")
            .show(ui, |ui| {
                let mut on = cx.prefs.reduced_motion;
                Toggle::new(Id::unique("settings.motion")).show(ui, &mut on);
                if on != cx.prefs.reduced_motion {
                    cx.prefs.reduced_motion = on;
                    changed = true;
                }
            });
        SettingRow::new("Clock")
            .description("How times are shown in the guide and player")
            .show(ui, |ui| {
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
        if changed {
            cx.save_prefs();
        }
    }
}

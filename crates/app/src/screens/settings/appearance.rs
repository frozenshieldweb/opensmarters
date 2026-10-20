// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Appearance: TV mode, UI size, motion (app-wide; applied at once and saved).
//!
//! Motion follows the Windows *Animation effects* switch by default
//! ([`crate::platform::animations_enabled`]); "Reduced" and "Full" override it.

use eframe::egui::{Id, Ui};

use super::{SettingsPageUi, page_heading};
use crate::platform::SystemMotion;
use crate::screens::AppCtx;
use crate::services::prefs::UI_SCALES;
use crate::ui::widgets::{Segmented, SettingRow, Toggle};

/// The appearance page.
#[derive(Debug, Default)]
pub struct AppearancePage {
    system: SystemMotion,
}

/// Motion choice ↔ (follow system, reduced).
fn motion_index(follow: bool, reduced: bool) -> usize {
    match (follow, reduced) {
        (true, _) => 0,
        (false, true) => 1,
        (false, false) => 2,
    }
}

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
        let size_desc = if cx.layout.tv {
            "Desktop mode only: TV mode sizes everything to the screen"
        } else {
            "Text and controls when not in TV mode"
        };
        SettingRow::new("Size")
            .description(size_desc)
            .show(ui, |ui| {
                let mut sel = UI_SCALES
                    .iter()
                    .position(|s| (s - cx.prefs.ui_scale).abs() < 0.01)
                    .unwrap_or(1);
                Segmented::new(
                    Id::unique("settings.scale"),
                    &["90%", "100%", "110%", "125%"],
                    320.0,
                )
                .show(ui, &mut sel);
                let scale = UI_SCALES[sel.min(UI_SCALES.len() - 1)];
                if (scale - cx.prefs.ui_scale).abs() > 0.001 {
                    cx.prefs.ui_scale = scale;
                    changed = true;
                }
            });
        let system = self.system.poll();
        let motion_desc = match system {
            Some(true) => "Follow Windows: animations are on in Windows",
            Some(false) => "Follow Windows: animations are off in Windows",
            None => "Windows' setting can't be read here; motion is full",
        };
        SettingRow::new("Motion")
            .description(motion_desc)
            .show(ui, |ui| {
                let mut sel = motion_index(cx.prefs.motion_follow_system, cx.prefs.reduced_motion);
                Segmented::new(
                    Id::unique("settings.motion"),
                    &["Follow Windows", "Reduced", "Full"],
                    420.0,
                )
                .show(ui, &mut sel);
                if sel != motion_index(cx.prefs.motion_follow_system, cx.prefs.reduced_motion) {
                    cx.prefs.motion_follow_system = sel == 0;
                    if sel != 0 {
                        cx.prefs.reduced_motion = sel == 1;
                    }
                    changed = true;
                }
            });
        ui.add_space(8.0);
        super::paragraph(
            ui,
            "Reduced motion: no zoom when something gets the focus, no shimmering placeholders, \
             no shaking PIN pad, and shorter fades.",
        );
        if changed {
            cx.save_prefs();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_choices_map_both_ways() {
        assert_eq!(motion_index(true, true), 0);
        assert_eq!(motion_index(true, false), 0);
        assert_eq!(motion_index(false, true), 1);
        assert_eq!(motion_index(false, false), 2);
    }
}

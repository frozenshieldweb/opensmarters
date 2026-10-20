// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Recordings & downloads: the recordings folder (choose, open, reset), the
//! padding of scheduled recordings, downloads at once, the free space to keep, and the
//! default clip length. App-wide settings (`dvr::{RECORDINGS_DIR, PAD_BEFORE, …}`); the
//! engine re-reads them after each change.

use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui::{self, Id, Ui};

use super::{SettingsPageUi, page_heading, section, value_label};
use crate::dvr::{
    self, AT_ONCE_CHOICES, CLIP_CHOICES, DvrSettings, KEEP_FREE_CHOICES, PAD_AFTER_CHOICES,
    PAD_BEFORE_CHOICES,
};
use crate::screens::AppCtx;
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{Button, Segmented, SettingRow};

/// The recordings & downloads page.
#[derive(Debug, Default)]
pub struct RecordingsPage {
    settings: Loader<DvrSettings>,
    folder_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
}

fn labels<T: Copy>(choices: &[T], f: impl Fn(T) -> String) -> Vec<String> {
    choices.iter().map(|c| f(*c)).collect()
}

impl RecordingsPage {
    fn load(&mut self, cx: &AppCtx<'_>) {
        let store = cx.services.store.clone();
        let paths = cx.services.paths.clone();
        let ephemeral = cx.services.ephemeral;
        self.settings.start_blocking(cx.rt(), &cx.egui, move || {
            Ok(DvrSettings::load(&store, &paths, ephemeral))
        });
    }

    /// Saves one setting in the background, then the engine re-reads them all.
    fn save<T: serde::Serialize + Send + Sync + 'static>(
        cx: &AppCtx<'_>,
        setting: &'static opensmarters_store::settings::Setting<T>,
        value: T,
    ) {
        let reload = cx.dvr.engine().map(dvr::engine::Engine::settings_reloader);
        cx.services.blocking("save a recording setting", move |s| {
            s.settings().set(setting, &value)?;
            if let Some(r) = reload {
                r();
            }
            Ok(())
        });
    }

    fn pick_folder(&mut self, cx: &AppCtx<'_>, start: Option<PathBuf>) {
        let (tx, rx) = mpsc::channel();
        let ctx = cx.egui.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Choose the recordings folder");
            if let Some(dir) = start.filter(|d| d.is_dir()) {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_folder());
            ctx.request_repaint();
        });
        self.folder_rx = Some(rx);
    }

    fn poll_folder(&mut self, cx: &mut AppCtx<'_>) {
        let Some(rx) = &self.folder_rx else { return };
        let picked = match rx.try_recv() {
            Ok(p) => p,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.folder_rx = None;
        let Some(dir) = picked else { return };
        if !dir.is_absolute() || !dir.is_dir() {
            cx.toasts.error("That folder can't be used.");
            return;
        }
        if let Some(s) = self.settings.value_mut() {
            s.dir = Some(dir.clone());
            s.custom_dir = true;
        }
        cx.toasts.success(format!(
            "New recordings and downloads go to {}. Files already saved stay where they are.",
            crate::ui::text::cap(&dir.display().to_string(), 80)
        ));
        Self::save(cx, &dvr::RECORDINGS_DIR, Some(dir));
    }
}

impl SettingsPageUi for RecordingsPage {
    fn entry_focus(&self) -> Option<Id> {
        Some(Id::unique("settings.rec.folder.change"))
    }

    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.load(cx);
    }

    #[allow(clippy::too_many_lines)]
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.settings.is_idle() {
            self.load(cx);
        }
        self.settings.poll();
        self.poll_folder(cx);
        page_heading(
            ui,
            "Recordings & downloads",
            Some("Where recordings and downloads are saved, and how they run."),
        );
        let Some(cur) = self.settings.value().cloned() else {
            return;
        };

        section(ui, "Folder", None);
        let dir_desc = cur.dir.as_ref().map_or_else(
            || "No folder: choose one".to_owned(),
            |d| d.display().to_string(),
        );
        SettingRow::new("Recordings folder")
            .description(&dir_desc)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if cur.custom_dir
                    && Button::ghost("Use default")
                        .icon(icons::ARROW_COUNTER_CLOCKWISE)
                        .small()
                        .id(Id::unique("settings.rec.folder.reset"))
                        .show(ui)
                        .activated
                {
                    Self::save(cx, &dvr::RECORDINGS_DIR, None);
                    self.load(cx);
                    cx.toasts.info("Recordings go to the default folder again.");
                }
                if Button::ghost("Open")
                    .icon(icons::FOLDER_OPEN)
                    .small()
                    .enabled(cur.dir.is_some())
                    .id(Id::unique("settings.rec.folder.open"))
                    .show(ui)
                    .activated
                    && let Some(d) = &cur.dir
                {
                    let _ = std::fs::create_dir_all(d);
                    if crate::platform::open_folder(d).is_err() {
                        cx.toasts.error("The folder couldn't be opened.");
                    }
                }
                if Button::secondary("Change…")
                    .icon(icons::FOLDER)
                    .small()
                    .enabled(self.folder_rx.is_none())
                    .id(Id::unique("settings.rec.folder.change"))
                    .show(ui)
                    .activated
                {
                    self.pick_folder(cx, cur.dir.clone());
                }
            });
        ui.add_space(28.0);

        section(
            ui,
            "Scheduled recordings",
            Some("Guide times are often a little off: record a margin around the programme."),
        );
        let mins = |m: u32| {
            if m == 0 {
                "None".to_owned()
            } else {
                format!("{m} min")
            }
        };
        SettingRow::new("Start early")
            .description("Begins before the programme")
            .show(ui, |ui| {
                let l = labels(&PAD_BEFORE_CHOICES, mins);
                let refs: Vec<&str> = l.iter().map(String::as_str).collect();
                let cur_i = PAD_BEFORE_CHOICES
                    .iter()
                    .position(|m| m * 60 == cur.pad_before)
                    .unwrap_or(1);
                let mut sel = cur_i;
                Segmented::new(Id::unique("settings.rec.pad-before"), &refs, 360.0)
                    .show(ui, &mut sel);
                if sel != cur_i
                    && let Some(m) = PAD_BEFORE_CHOICES.get(sel)
                {
                    if let Some(s) = self.settings.value_mut() {
                        s.pad_before = m * 60;
                    }
                    Self::save(cx, &dvr::PAD_BEFORE, m * 60);
                }
            });
        SettingRow::new("End late")
            .description("Keeps recording after the programme")
            .show(ui, |ui| {
                let l = labels(&PAD_AFTER_CHOICES, mins);
                let refs: Vec<&str> = l.iter().map(String::as_str).collect();
                let cur_i = PAD_AFTER_CHOICES
                    .iter()
                    .position(|m| m * 60 == cur.pad_after)
                    .unwrap_or(1);
                let mut sel = cur_i;
                Segmented::new(Id::unique("settings.rec.pad-after"), &refs, 360.0)
                    .show(ui, &mut sel);
                if sel != cur_i
                    && let Some(m) = PAD_AFTER_CHOICES.get(sel)
                {
                    if let Some(s) = self.settings.value_mut() {
                        s.pad_after = m * 60;
                    }
                    Self::save(cx, &dvr::PAD_AFTER, m * 60);
                }
            });
        ui.add_space(28.0);

        section(ui, "Downloads", None);
        SettingRow::new("Downloads at once")
            .description("Each one uses a connection of your account")
            .show(ui, |ui| {
                let l = labels(&AT_ONCE_CHOICES, |n| n.to_string());
                let refs: Vec<&str> = l.iter().map(String::as_str).collect();
                let cur_i = AT_ONCE_CHOICES
                    .iter()
                    .position(|n| *n == cur.at_once)
                    .unwrap_or(0);
                let mut sel = cur_i;
                Segmented::new(Id::unique("settings.rec.at-once"), &refs, 180.0).show(ui, &mut sel);
                if sel != cur_i
                    && let Some(n) = AT_ONCE_CHOICES.get(sel)
                {
                    if let Some(s) = self.settings.value_mut() {
                        s.at_once = *n;
                    }
                    Self::save(cx, &dvr::DOWNLOADS_AT_ONCE, *n);
                }
            });
        SettingRow::new("Keep free on the disk")
            .description("Recordings and downloads stop before the disk gets this full")
            .show(ui, |ui| {
                let l = labels(&KEEP_FREE_CHOICES, |g| format!("{g} GB"));
                let refs: Vec<&str> = l.iter().map(String::as_str).collect();
                let cur_i = KEEP_FREE_CHOICES
                    .iter()
                    .position(|g| *g == cur.keep_free_gb)
                    .unwrap_or(1);
                let mut sel = cur_i;
                Segmented::new(Id::unique("settings.rec.keep-free"), &refs, 360.0)
                    .show(ui, &mut sel);
                if sel != cur_i
                    && let Some(g) = KEEP_FREE_CHOICES.get(sel)
                {
                    if let Some(s) = self.settings.value_mut() {
                        s.keep_free_gb = *g;
                    }
                    Self::save(cx, &dvr::KEEP_FREE_GB, *g);
                }
            });
        ui.add_space(28.0);

        section(
            ui,
            "Clips",
            Some(
                "\u{201c}Save clip\u{201d} in the player keeps the last minutes of a live channel from its buffer.",
            ),
        );
        SettingRow::new("Clip length offered first")
            .description("You can still pick another length each time")
            .show(ui, |ui| {
                let l = labels(&CLIP_CHOICES, |m| format!("{m} min"));
                let refs: Vec<&str> = l.iter().map(String::as_str).collect();
                let cur_i = CLIP_CHOICES
                    .iter()
                    .position(|m| *m == cur.clip_minutes)
                    .unwrap_or(1);
                let mut sel = cur_i;
                Segmented::new(Id::unique("settings.rec.clip"), &refs, 360.0).show(ui, &mut sel);
                if sel != cur_i
                    && let Some(m) = CLIP_CHOICES.get(sel)
                {
                    if let Some(s) = self.settings.value_mut() {
                        s.clip_minutes = *m;
                    }
                    Self::save(cx, &dvr::CLIP_MINUTES, *m);
                }
            });
        ui.add_space(28.0);

        // p7c: the coordinator adds the reminder settings here:
        // crate::services::alerts::settings_rows(cx, ui);

        if cx.services.ephemeral && cx.services.paths.data_dir.is_none() {
            ui.add_space(8.0);
            value_label(
                ui,
                "Temporary session: recordings go to a temporary folder.",
                color::TEXT_3,
            );
        }
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Recordings run while opensmarters is open. Downloads pause when you start \
                 watching something and no other connection is free, then continue on their own.",
            )
            .font(font::caption())
            .color(color::TEXT_3),
        );
    }
}

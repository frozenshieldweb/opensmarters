// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Storage & data:
//!
//! - **Space used**: the encrypted provider cache (`store.cache()`: size, entries, cap)
//!   and the image disk cache (`<data>/images`), each with a Clear button.
//! - **This profile's history**: watch history & resume points (confirmed), recent
//!   channels, recent searches ([`crate::services::search_history`]).
//! - **Backup**: export / import of the non-secret data through system file dialogs
//!   ([`crate::services::backup`]; import asks for the parental PIN when one is set and
//!   lists the profiles that need their password afterwards).
//! - **Files**: the data folder (open it) and the log file ("Copy log": the last 256 KB,
//!   already redacted when written and redacted again before copying).
//!
//! All store / file work runs off the UI thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui::{self, Id, Ui};
use opensmarters_store::ProfileKind;

use super::{SettingsPageUi, human_bytes, page_heading, paragraph, section, value_label};
use crate::error::AppError;
use crate::logging::LOG_FILE;
use crate::nav::Route;
use crate::screens::AppCtx;
use crate::services::backup::{self, ImportReport};
use crate::services::{Prefs, search_history};
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::text;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{
    Button, Callout, CalloutKind, Dialog, PinPrompt, PinPromptResult, Segmented, SettingRow,
};

/// Cache size choices (MiB).
const CACHE_CAPS_MIB: [u64; 4] = [256, 512, 1024, 2048];
const CACHE_CAP_LABELS: [&str; 4] = ["256 MB", "512 MB", "1 GB", "2 GB"];
/// "Copy log" copies at most this much of the end of the log.
const LOG_COPY_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Default)]
struct Usage {
    cache_bytes: u64,
    cache_entries: usize,
    cap: u64,
    images_bytes: u64,
    db_bytes: u64,
    history: usize,
    recent: usize,
    searches: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOp {
    Export,
    Import,
}

/// The storage page.
#[derive(Debug, Default)]
pub struct StoragePage {
    usage: Loader<Usage>,
    job: Loader<String>,
    confirm_history: bool,
    file_rx: Option<(FileOp, mpsc::Receiver<Option<PathBuf>>)>,
    import: Loader<(ImportReport, Prefs)>,
    report: Option<ImportReport>,
    gate: Option<PinPrompt>,
    log: Loader<String>,
}

/// Total size of the files under `dir` (recursive, bounded).
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            seen += 1;
            if seen > 200_000 {
                return total;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map_or(0, |m| m.len()),
                _ => {}
            }
        }
    }
    total
}

/// The last `max` bytes of a file as text, starting at a line boundary.
fn tail(path: &Path, max: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    f.read_to_end(&mut buf)?;
    let s = String::from_utf8_lossy(&buf).into_owned();
    Ok(if start > 0 {
        s.split_once('\n')
            .map_or(s.clone(), |(_, rest)| rest.to_owned())
    } else {
        s
    })
}

impl StoragePage {
    fn refresh(&mut self, cx: &AppCtx<'_>) {
        let store = cx.services.store.clone();
        let images = cx.services.paths.image_cache_dir();
        let profile = cx.session().map(|s| s.profile_id());
        self.usage.start_blocking(cx.rt(), &cx.egui, move || {
            let c = store.cache();
            let db_bytes = store.path().map_or(0, |p| {
                let wal = PathBuf::from(format!("{}-wal", p.display()));
                std::fs::metadata(p).map_or(0, |m| m.len())
                    + std::fs::metadata(wal).map_or(0, |m| m.len())
            });
            let (history, recent, searches) = match profile {
                Some(p) => (
                    store.playback().history(p, 100_000)?.len(),
                    store.recent_live().list(p, 10_000)?.len(),
                    search_history::load(&store, p)?.len(),
                ),
                None => (0, 0, 0),
            };
            Ok(Usage {
                cache_bytes: c.total_bytes()?,
                cache_entries: c.len()?,
                cap: c.max_bytes(),
                images_bytes: images.as_deref().map_or(0, dir_size),
                db_bytes,
                history,
                recent,
                searches,
            })
        });
    }

    fn run_job<F>(&mut self, cx: &AppCtx<'_>, f: F)
    where
        F: FnOnce(&opensmarters_store::Store) -> Result<String, AppError> + Send + 'static,
    {
        let store = cx.services.store.clone();
        self.job
            .start_blocking(cx.rt(), &cx.egui, move || f(&store));
    }

    fn pick_file(&mut self, cx: &AppCtx<'_>, op: FileOp) {
        let (tx, rx) = mpsc::channel();
        let ctx = cx.egui.clone();
        let name =
            backup::suggested_file_name(opensmarters_core::chrono::Local::now().date_naive());
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().add_filter("opensmarters backup", &["json"]);
            let picked = match op {
                FileOp::Export => dialog
                    .set_title("Save a backup")
                    .set_file_name(name)
                    .save_file(),
                FileOp::Import => dialog.set_title("Choose a backup").pick_file(),
            };
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.file_rx = Some((op, rx));
    }

    fn poll_file(&mut self, cx: &mut AppCtx<'_>) {
        let Some((op, rx)) = &self.file_rx else {
            return;
        };
        let op = *op;
        let picked = match rx.try_recv() {
            Ok(p) => p,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.file_rx = None;
        let Some(path) = picked else { return };
        match op {
            FileOp::Export => self.run_job(cx, move |store| {
                let json = backup::export(store)?;
                std::fs::write(&path, json)
                    .map_err(|_| AppError::msg("The backup couldn't be written there."))?;
                let file = path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(format!(
                    "Backup saved as {}. Passwords aren't included.",
                    text::cap(&file, 60)
                ))
            }),
            FileOp::Import => {
                let store = cx.services.store.clone();
                self.import.start_blocking(cx.rt(), &cx.egui, move || {
                    let json = backup::read_file(&path)?;
                    let report = backup::import(&store, &json)?;
                    let prefs = Prefs::load(&store)?;
                    Ok((report, prefs))
                });
            }
        }
    }

    fn poll_jobs(&mut self, cx: &mut AppCtx<'_>) {
        if self.job.poll() {
            if let Some(msg) = self.job.take() {
                cx.toasts.success(msg);
            }
            self.refresh(cx);
        } else if let Some(e) = self.job.error().cloned() {
            cx.toasts.error(e.user_message());
            self.job = Loader::new();
        }
        if self.import.poll() {
            if let Some((report, prefs)) = self.import.take() {
                *cx.prefs = prefs;
                cx.save_prefs();
                if let Some(s) = cx.session() {
                    let ctx = cx.egui.clone();
                    cx.services.spawn(async move {
                        s.reload_parental().await;
                        s.reload_favourites().await;
                        ctx.request_repaint();
                    });
                }
                self.report = Some(report);
            }
            self.refresh(cx);
        } else if let Some(e) = self.import.error().cloned() {
            cx.toasts.error(e.user_message());
            self.import = Loader::new();
        }
        if self.log.poll() {
            if let Some(text) = self.log.take() {
                let kb = text.len().div_ceil(1024);
                cx.egui.copy_text(text);
                cx.toasts.success(format!(
                    "Copied the last {kb} KB of the log. Passwords and stream addresses are removed."
                ));
            }
        } else if let Some(e) = self.log.error().cloned() {
            cx.toasts.error(e.user_message());
            self.log = Loader::new();
        }
    }

    fn space_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, u: &Usage) {
        section(ui, "Space used", None);
        let cache_desc = "Your provider's lists, encrypted, for quick starts and offline use";
        SettingRow::new("Saved provider data")
            .description(cache_desc)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if Button::ghost("Clear")
                    .icon(icons::BROOM)
                    .small()
                    .id(Id::unique("settings.storage.cache.clear"))
                    .show(ui)
                    .activated
                {
                    if let Some(s) = cx.session() {
                        cx.services.spawn(async move { s.refresh().await });
                    }
                    self.run_job(cx, |store| {
                        let n = store.cache().clear()?;
                        store.compact()?;
                        Ok(format!(
                            "Cleared {n} saved list{}. They download again when needed.",
                            if n == 1 { "" } else { "s" }
                        ))
                    });
                }
                value_label(
                    ui,
                    &format!(
                        "{} · {} list{}",
                        human_bytes(u.cache_bytes),
                        u.cache_entries,
                        if u.cache_entries == 1 { "" } else { "s" }
                    ),
                    color::TEXT_2,
                );
            });
        SettingRow::new("Cache limit")
            .description("The oldest saved lists are removed above this")
            .show(ui, |ui| {
                let cur = CACHE_CAPS_MIB
                    .iter()
                    .position(|m| m * 1024 * 1024 == u.cap)
                    .unwrap_or(1);
                let mut sel = cur;
                Segmented::new(Id::unique("settings.storage.cap"), &CACHE_CAP_LABELS, 360.0)
                    .show(ui, &mut sel);
                if sel != cur {
                    let bytes = CACHE_CAPS_MIB[sel] * 1024 * 1024;
                    if let Some(v) = self.usage.value_mut() {
                        v.cap = bytes;
                    }
                    self.run_job(cx, move |store| {
                        store.cache().set_max_bytes(bytes)?;
                        Ok(format!("Cache limit set to {}.", CACHE_CAP_LABELS[sel]))
                    });
                }
            });
        let images_dir = cx.services.paths.image_cache_dir();
        SettingRow::new("Pictures")
            .description("Posters and logos kept on disk")
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if Button::ghost("Clear")
                    .icon(icons::BROOM)
                    .small()
                    .enabled(images_dir.is_some())
                    .id(Id::unique("settings.storage.images.clear"))
                    .show(ui)
                    .activated
                    && let Some(dir) = images_dir.clone()
                {
                    cx.services.images.clear_memory();
                    self.run_job(cx, move |_| {
                        crate::services::images::clear_disk_cache(&dir)
                            .map_err(|_| AppError::msg("Some pictures couldn't be deleted."))?;
                        Ok("Pictures cleared. They download again when needed.".to_owned())
                    });
                }
                value_label(ui, &human_bytes(u.images_bytes), color::TEXT_2);
            });
        if u.db_bytes > 0 {
            SettingRow::new("Database file")
                .description("Profiles, favourites, history and the saved lists above")
                .show(ui, |ui| {
                    value_label(ui, &human_bytes(u.db_bytes), color::TEXT_2)
                });
        }
    }

    fn history_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui, u: &Usage) {
        let Some(session) = cx.session() else { return };
        let profile = session.profile_id();
        section(
            ui,
            "History on this profile",
            Some("Stays on this computer; never included in backups."),
        );
        let rows: [(&str, String, &str); 3] = [
            (
                "Watch history",
                format!(
                    "{} films, episodes and programmes, with resume points",
                    u.history
                ),
                "settings.storage.history",
            ),
            (
                "Recent channels",
                format!("{} channels", u.recent),
                "settings.storage.recent",
            ),
            (
                "Recent searches",
                format!("{} searches", u.searches),
                "settings.storage.searches",
            ),
        ];
        for (i, (label, desc, id)) in rows.iter().enumerate() {
            let empty = match i {
                0 => u.history == 0,
                1 => u.recent == 0,
                _ => u.searches == 0,
            };
            let mut clicked = false;
            SettingRow::new(label).description(desc).show(ui, |ui| {
                clicked = Button::ghost("Clear")
                    .icon(icons::TRASH)
                    .small()
                    .enabled(!empty)
                    .id(Id::unique(*id))
                    .show(ui)
                    .activated;
            });
            if !clicked {
                continue;
            }
            match i {
                0 => self.confirm_history = true,
                1 => self.run_job(cx, move |store| {
                    store.recent_live().clear(profile)?;
                    Ok("Recent channels cleared.".to_owned())
                }),
                _ => self.run_job(cx, move |store| {
                    search_history::clear(store, profile)?;
                    Ok("Recent searches cleared.".to_owned())
                }),
            }
        }
    }

    fn backup_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        section(
            ui,
            "Backup",
            Some(
                "A file with your profiles (without passwords), favourites, category locks, \
                 words and settings. Passwords, the PIN and history are never included.",
            ),
        );
        let busy = self.file_rx.is_some() || self.job.is_loading() || self.import.is_loading();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if Button::secondary("Export a backup…")
                .icon(icons::DOWNLOAD_SIMPLE)
                .enabled(!busy)
                .id(Id::unique("settings.storage.export"))
                .show(ui)
                .activated
            {
                self.pick_file(cx, FileOp::Export);
            }
            if Button::secondary("Import a backup…")
                .icon(icons::UPLOAD_SIMPLE)
                .enabled(!busy)
                .id(Id::unique("settings.storage.import"))
                .show(ui)
                .activated
            {
                self.gate = Some(PinPrompt::new(Id::unique("settings.storage.gate")));
            }
        });
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Importing adds profiles and favourites and replaces settings. While parental \
                 control is on, it asks for the PIN.",
            )
            .font(font::caption())
            .color(color::TEXT_3),
        );
    }

    fn files_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        section(ui, "Files", None);
        let data = cx.services.paths.data_dir.clone();
        let data_desc = data.as_ref().map_or_else(
            || "Temporary session: nothing is saved".to_owned(),
            |p| p.display().to_string(),
        );
        SettingRow::new("Data folder")
            .description(&data_desc)
            .show(ui, |ui| {
                if Button::ghost("Open")
                    .icon(icons::FOLDER_OPEN)
                    .small()
                    .enabled(data.as_ref().is_some_and(|d| d.is_dir()))
                    .id(Id::unique("settings.storage.open-data"))
                    .show(ui)
                    .activated
                    && let Some(d) = &data
                    && crate::platform::open_folder(d).is_err()
                {
                    cx.toasts.error("The folder couldn't be opened.");
                }
            });
        let log = cx.services.paths.log_dir().map(|d| d.join(LOG_FILE));
        let log_desc = log.as_ref().map_or_else(
            || "Not written in a temporary session".to_owned(),
            |p| p.display().to_string(),
        );
        SettingRow::new("Log file")
            .description(&log_desc)
            .show(ui, |ui| {
                let exists = log.as_ref().is_some_and(|p| p.is_file());
                if Button::ghost(if self.log.is_loading() {
                    "Copying…"
                } else {
                    "Copy log"
                })
                .icon(icons::COPY)
                .small()
                .enabled(exists && !self.log.is_loading())
                .id(Id::unique("settings.storage.copy-log"))
                .show(ui)
                .activated
                    && let Some(path) = log.clone()
                {
                    let redactor = cx.services.redactor.clone();
                    self.log.start_blocking(cx.rt(), &cx.egui, move || {
                        let text = tail(&path, LOG_COPY_BYTES)
                            .map_err(|_| AppError::msg("The log couldn't be read."))?;
                        // Already redacted when written; again for defence in depth.
                        Ok(redactor.redact(&text))
                    });
                }
            });
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Share the log when reporting a problem. Passwords, usernames and stream \
                 addresses are removed before anything is written to it.",
            )
            .font(font::caption())
            .color(color::TEXT_3),
        );
    }

    fn dialogs(&mut self, cx: &mut AppCtx<'_>, ui: &Ui) {
        if self.confirm_history {
            let mut close = false;
            Dialog::new(
                Id::unique("settings.storage.history.dialog"),
                "Clear watch history?",
            )
            .body(
                "Continue watching and resume points for this profile are removed. Favourites \
                 are kept.",
            )
            .show(ui.ctx(), |ui| {
                if Button::danger("Clear history")
                    .icon(icons::TRASH)
                    .id(Id::unique("settings.storage.history.yes"))
                    .show(ui)
                    .activated
                    && let Some(s) = cx.session()
                {
                    let profile = s.profile_id();
                    self.run_job(cx, move |store| {
                        let n = store.playback().clear_history(profile)?;
                        Ok(format!(
                            "Watch history cleared ({n} item{}).",
                            if n == 1 { "" } else { "s" }
                        ))
                    });
                    close = true;
                }
                if Button::secondary("Keep it")
                    .autofocus()
                    .id(Id::unique("settings.storage.history.no"))
                    .show(ui)
                    .activated
                {
                    close = true;
                }
            });
            if close {
                self.confirm_history = false;
            }
        }
        if let Some(gate) = &mut self.gate {
            match gate.show_dialog(
                ui.ctx(),
                cx.services,
                "Enter your PIN to import a backup",
                None,
            ) {
                PinPromptResult::Verified(_) => {
                    self.gate = None;
                    self.pick_file(cx, FileOp::Import);
                }
                PinPromptResult::Cancelled | PinPromptResult::Forgot => self.gate = None,
                PinPromptResult::Pending => {}
            }
        }
        if let Some(r) = self.report.clone() {
            let mut close = false;
            let mut edit = None;
            Dialog::new(Id::unique("settings.storage.report"), "Backup imported")
                .width(560.0)
                .show_custom(ui.ctx(), |ui| {
                    ui.label(
                        egui::RichText::new("Backup imported")
                            .font(font::title())
                            .color(color::TEXT),
                    );
                    ui.add_space(6.0);
                    paragraph(ui, &import_summary(&r));
                    if !r.needs_secrets.is_empty() {
                        ui.add_space(12.0);
                        Callout::new(
                            CalloutKind::Warning,
                            "Passwords needed:",
                            "backups never contain passwords or playlist addresses. Add them \
                             to use these profiles.",
                        )
                        .width(ui.available_width())
                        .show(ui);
                        ui.add_space(8.0);
                        for n in &r.needs_secrets {
                            let what = match n.kind {
                                ProfileKind::M3uUrl => "playlist address",
                                _ => "password",
                            };
                            let label = text::cap(&n.name, 40);
                            let desc = format!("Needs its {what}");
                            SettingRow::new(&label).description(&desc).show(ui, |ui| {
                                if Button::secondary(format!("Add {what}"))
                                    .icon(icons::KEY)
                                    .small()
                                    .id(Id::unique(("settings.storage.report.edit", n.id)))
                                    .show(ui)
                                    .activated
                                {
                                    edit = Some(n.id);
                                }
                            });
                        }
                    }
                    ui.add_space(20.0);
                    let w = ui.available_width();
                    ui.allocate_ui_with_layout(
                        egui::vec2(w, 44.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if Button::primary("Done")
                                .autofocus()
                                .id(Id::unique("settings.storage.report.done"))
                                .show(ui)
                                .activated
                            {
                                close = true;
                            }
                        },
                    );
                });
            if let Some(id) = edit {
                if let Some(rep) = &mut self.report {
                    rep.needs_secrets.retain(|n| n.id != id);
                }
                cx.nav.push(Route::Onboarding { edit: Some(id) });
            }
            if close {
                self.report = None;
            }
        }
    }
}

/// "2 profiles added (1 was already here), 14 favourites, 3 settings and 1 word."
fn import_summary(r: &ImportReport) -> String {
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut s = format!("{} added", plural(r.profiles_added, "profile", "profiles"));
    if r.profiles_skipped > 0 {
        let verb = if r.profiles_skipped == 1 {
            "was"
        } else {
            "were"
        };
        s.push_str(&format!(" ({} {verb} already here)", r.profiles_skipped));
    }
    s.push_str(&format!(
        ", {}, {} and {}.",
        plural(r.favourites_added, "favourite", "favourites"),
        plural(r.settings_applied, "setting", "settings"),
        plural(r.keywords_added, "locked word", "locked words")
    ));
    s
}

impl SettingsPageUi for StoragePage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.refresh(cx);
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        if self.gate.take().is_some() {
            return true;
        }
        if self.confirm_history {
            self.confirm_history = false;
            return true;
        }
        self.report.take().is_some()
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.usage.is_idle() {
            self.refresh(cx);
        }
        self.usage.poll();
        self.poll_file(cx);
        self.poll_jobs(cx);
        page_heading(
            ui,
            "Storage & data",
            Some("What opensmarters keeps on this computer, and how to back it up."),
        );
        let usage = self.usage.value().cloned().unwrap_or_default();
        self.space_section(cx, ui, &usage);
        ui.add_space(28.0);
        self.history_section(cx, ui, &usage);
        ui.add_space(28.0);
        self.backup_section(cx, ui);
        ui.add_space(28.0);
        self.files_section(cx, ui);
        self.dialogs(cx, ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reads_naturally() {
        let r = ImportReport {
            profiles_added: 2,
            profiles_skipped: 0,
            favourites_added: 14,
            settings_applied: 1,
            keywords_added: 1,
            needs_secrets: Vec::new(),
        };
        assert_eq!(
            import_summary(&r),
            "2 profiles added, 14 favourites, 1 setting and 1 locked word."
        );
    }

    #[test]
    fn dir_size_and_tail() {
        let dir = std::env::temp_dir().join(format!("os-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(dir.join("a.png"), vec![0u8; 1000]).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(dir.join("sub/b.png"), vec![0u8; 24]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dir_size(&dir), 1024);
        assert_eq!(dir_size(&dir.join("missing")), 0);
        let log = dir.join("x.log");
        std::fs::write(&log, "line one\nline two\nline three\n").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            tail(&log, 1024).unwrap_or_default(),
            "line one\nline two\nline three\n"
        );
        // Starts at a line boundary when cut.
        assert_eq!(tail(&log, 14).unwrap_or_default(), "line three\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

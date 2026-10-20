// SPDX-License-Identifier: GPL-3.0-or-later
//! First-run welcome (no profiles yet): what opensmarters is, in three lines, then "Add your
//! account" (onboarding) or "Restore a backup…" (import, then the profile picker).
//! Same art direction as onboarding (§8.1): `hero.jpg` anchored right, a left column.

use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui::{self, Align2, Color32, Id, Rect, ScrollArea, Ui, pos2, vec2};

use super::{AppCtx, Chrome, Screen};
use crate::error::AppError;
use crate::focus;
use crate::nav::Route;
use crate::services::Prefs;
use crate::services::backup::{self, ImportReport};
use crate::task::Loader;
use crate::ui::assets::{self, Asset};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font};
use crate::ui::widgets::Button;
use crate::ui::{paint, text};

const COL_W: f32 = 480.0;

/// The three points under the headline.
pub const POINTS: [(&str, &str); 3] = [
    (
        icons::TELEVISION_SIMPLE,
        "Plays the subscription you already have: sign in with an Xtream Codes account or open an M3U playlist. No channels are included.",
    ),
    (
        icons::SHIELD_CHECK,
        "Private by design: passwords stay in Windows Credential Manager, and nothing is tracked, ever.",
    ),
    (
        icons::KEYBOARD,
        "Made for the sofa and the desk: a keyboard, a mouse or a remote all work.",
    ),
];

/// The welcome screen.
#[derive(Debug, Default)]
pub struct WelcomeScreen {
    file_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    import: Loader<(ImportReport, Prefs)>,
}

impl WelcomeScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self::default()
    }

    fn pick_backup(&mut self, cx: &AppCtx<'_>) {
        let (tx, rx) = mpsc::channel();
        let ctx = cx.egui.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Choose a backup")
                .add_filter("opensmarters backup", &["json"])
                .pick_file();
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.file_rx = Some(rx);
    }

    fn poll(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(rx) = &self.file_rx {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.file_rx = None;
                    let store = cx.services.store.clone();
                    self.import.start_blocking(cx.rt(), &cx.egui, move || {
                        let json = backup::read_file(&path)?;
                        let report = backup::import(&store, &json)?;
                        let prefs = Prefs::load(&store).map_err(AppError::from)?;
                        Ok((report, prefs))
                    });
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => self.file_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.import.poll() {
            if let Some((report, prefs)) = self.import.take() {
                *cx.prefs = prefs;
                cx.save_prefs();
                let n = report.profiles_added;
                if n == 0 {
                    cx.toasts
                        .info("That backup has no profiles. Add your account to start.");
                } else {
                    cx.toasts.success(format!(
                        "Backup restored: {n} profile{}. Choose one, then add its password.",
                        if n == 1 { "" } else { "s" }
                    ));
                    cx.nav.reset(Route::Profiles);
                }
            }
        } else if let Some(e) = self.import.error().cloned() {
            cx.toasts.error(e.user_message());
            self.import = Loader::new();
        }
    }
}

impl Screen for WelcomeScreen {
    fn chrome(&self) -> Chrome {
        Chrome::Fullscreen
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.poll(cx);
        let rect = cx.layout.screen;
        let painter = ui.painter().clone();
        if !assets::paint_cover(
            ui,
            &painter,
            Asset::Hero,
            rect,
            vec2(1.0, 0.5),
            0,
            Color32::WHITE,
        ) {
            painter.rect_filled(rect, 0.0, color::BG);
        }
        if rect.width() / rect.height() < 16.0 / 9.0 - 0.05 {
            paint::scrim_left(&painter, rect);
        }
        let left = rect.left() + if rect.width() >= 1200.0 { 96.0 } else { 56.0 };
        let col_w = COL_W.min(rect.width() - (left - rect.left()) - 32.0);
        let col = Rect::from_min_max(
            pos2(left - 12.0, rect.top()),
            pos2(left + col_w + 12.0, rect.bottom()),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(col));
        let h_id = Id::unique("welcome.content-h");
        let last_h: f32 = ui.ctx().data(|d| d.get_temp(h_id)).unwrap_or(640.0);
        let busy = self.file_rx.is_some() || self.import.is_loading();
        ScrollArea::vertical()
            .id_salt("welcome.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(12, 0))
            .show(&mut child, |ui| {
                ui.add_space(((rect.height() - last_h) / 2.0).max(40.0));
                let start = ui.cursor().top();
                ui.spacing_mut().item_spacing.y = 0.0;
                let wm_h = 36.0;
                let (wr, _) = ui.allocate_exact_size(
                    vec2(wm_h * assets::aspect(Asset::Wordmark), wm_h),
                    egui::Sense::hover(),
                );
                assets::paint_exact(ui, ui.painter(), Asset::Wordmark, wr, Color32::WHITE);
                ui.add_space(32.0);
                let g = ui.painter().layout(
                    "A quiet place\nfor your channels.".to_owned(),
                    font::hero(),
                    color::TEXT,
                    col_w + 200.0,
                );
                let (hr, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
                ui.painter().galley(hr.min, g, color::TEXT);
                ui.add_space(28.0);
                for (glyph, line) in POINTS {
                    let job = text::job(line, font::body(), color::TEXT_2, col_w - 40.0, 3);
                    let gal = ui.painter().layout_job(job);
                    let (r, _) = ui.allocate_exact_size(
                        vec2(col_w, gal.size().y.max(24.0)),
                        egui::Sense::hover(),
                    );
                    icons::paint(
                        ui.painter(),
                        pos2(r.left() + 12.0, r.top() + 12.0),
                        glyph,
                        24.0,
                        Style::Regular,
                        color::ACCENT,
                    );
                    ui.painter()
                        .galley(pos2(r.left() + 40.0, r.top()), gal, color::TEXT_2);
                    ui.add_space(16.0);
                }
                ui.add_space(20.0);
                focus::scope(ui, Id::unique("welcome.actions"), |ui| {
                    if Button::primary("Add your account")
                        .icon(icons::USER_PLUS)
                        .large()
                        .width(col_w)
                        .autofocus()
                        .enabled(!busy)
                        .id(Id::unique("welcome.add"))
                        .show(ui)
                        .activated
                    {
                        cx.nav.push(Route::Onboarding { edit: None });
                    }
                    ui.add_space(12.0);
                    if Button::secondary(if busy {
                        "Restoring…"
                    } else {
                        "Restore a backup…"
                    })
                    .icon(icons::UPLOAD_SIMPLE)
                    .large()
                    .width(col_w)
                    .enabled(!busy)
                    .id(Id::unique("welcome.restore"))
                    .show(ui)
                    .activated
                    {
                        self.pick_backup(cx);
                    }
                });
                ui.add_space(20.0);
                let (cr, _) = ui.allocate_exact_size(vec2(col_w, 20.0), egui::Sense::hover());
                ui.painter().text(
                    cr.left_center(),
                    Align2::LEFT_CENTER,
                    "Free and open source (GPL-3.0). You need an IPTV subscription to watch.",
                    font::caption(),
                    color::TEXT_3,
                );
                let h = ui.cursor().top() - start;
                ui.ctx().data_mut(|d| d.insert_temp(h_id, h));
            });
    }
}

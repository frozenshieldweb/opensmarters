// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings (§8.6): a section list on the left and the selected page on the right. Each
//! page lives in its own file and implements [`SettingsPageUi`]:
//!
//! | Page | File | Status |
//! |---|---|---|
//! | Account | `account.rs` | status, expiry, connections, HTTP warning, edit/refresh |
//! | Playback | `playback.rs` | live container (allowed formats only), timeshift format, User-Agent |
//! | Profiles | (link to the profile picker) | |
//! | Parental control | `parental.rs` | **stub** (Phase 4) |
//! | Appearance | `appearance.rs` | TV mode, reduced motion, 12/24 h |
//! | Storage & cache | `storage.rs` | **stub** (Phase 4) |
//! | About | `about.rs` | versions, licences, privacy |

mod about;
mod account;
mod appearance;
mod parental;
mod playback;
mod storage;

use eframe::egui::{self, Align2, Id, Rect, Stroke, Ui, pos2};

use super::{AppCtx, Screen};
use crate::focus;
use crate::nav::{Route, Section, SettingsPage};
use crate::ui::icons;
use crate::ui::theme::{color, font};
use crate::ui::widgets::MenuRow;

/// One settings page.
pub trait SettingsPageUi {
    /// Called when the page is shown (load data here).
    fn on_enter(&mut self, _cx: &mut AppCtx<'_>) {}
    /// Draws the page into the detail pane.
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui);
}

/// The settings screen.
pub struct SettingsScreen {
    page: SettingsPage,
    account: account::AccountPage,
    playback: playback::PlaybackPage,
    parental: parental::ParentalPage,
    appearance: appearance::AppearancePage,
    storage: storage::StoragePage,
    about: about::AboutPage,
    entered: bool,
}

impl std::fmt::Debug for SettingsScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsScreen")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl SettingsScreen {
    /// Opens on `page`.
    pub fn new(page: SettingsPage) -> Self {
        Self {
            page,
            account: account::AccountPage,
            playback: playback::PlaybackPage::default(),
            parental: parental::ParentalPage,
            appearance: appearance::AppearancePage,
            storage: storage::StoragePage,
            about: about::AboutPage,
            entered: false,
        }
    }

    fn page_mut(&mut self, page: SettingsPage) -> Option<&mut dyn SettingsPageUi> {
        Some(match page {
            SettingsPage::Account => &mut self.account,
            SettingsPage::Playback => &mut self.playback,
            SettingsPage::Parental => &mut self.parental,
            SettingsPage::Appearance => &mut self.appearance,
            SettingsPage::Storage => &mut self.storage,
            SettingsPage::About => &mut self.about,
            SettingsPage::Profiles => return None,
        })
    }
}

fn page_icon(p: SettingsPage) -> &'static str {
    match p {
        SettingsPage::Account => icons::USER_CIRCLE,
        SettingsPage::Playback => icons::PLAY_CIRCLE,
        SettingsPage::Profiles => icons::USERS_THREE,
        SettingsPage::Parental => icons::LOCK_SIMPLE,
        SettingsPage::Appearance => icons::PALETTE,
        SettingsPage::Storage => icons::DATABASE,
        SettingsPage::About => icons::INFO,
    }
}

impl Screen for SettingsScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.entered = true;
        let page = self.page;
        if let Some(p) = self.page_mut(page) {
            p.on_enter(cx);
        }
    }

    fn section(&self) -> Option<Section> {
        Some(Section::Settings)
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let content = cx.layout.content;
        let pane = Rect::from_min_size(content.min, egui::vec2(280.0, content.height()));
        let painter = ui.painter().clone();
        painter.rect_filled(pane, 0.0, color::SURFACE_1);
        painter.vline(
            pane.right() - 0.5,
            pane.y_range(),
            Stroke::new(1.0, color::STROKE_SUBTLE),
        );
        painter.text(
            pos2(pane.left() + 24.0, content.top() + 64.0),
            Align2::LEFT_CENTER,
            "Settings",
            font::heading(),
            color::TEXT,
        );
        super::shell::status_cluster(cx, ui);

        // Section list.
        let mut list = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
            pos2(pane.left() + 16.0, content.top() + 112.0),
            pos2(pane.right() - 16.0, pane.bottom() - 24.0),
        )));
        let mut picked = None;
        focus::scope(&mut list, Id::unique("settings.sections"), |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for p in SettingsPage::ALL {
                let f = MenuRow::new(Id::unique(("settings.section", p.slug())), p.label())
                    .icon(page_icon(p))
                    .selected(p == self.page)
                    .autofocus(p == self.page)
                    .show(ui);
                if f.activated {
                    picked = Some(p);
                }
            }
        });
        if let Some(p) = picked {
            if p == SettingsPage::Profiles {
                cx.nav.push(Route::Profiles);
            } else if p != self.page {
                self.page = p;
                if let Some(page) = self.page_mut(p) {
                    page.on_enter(cx);
                }
            }
        }

        // Detail pane.
        let left = pane.right() + 48.0;
        let detail = Rect::from_min_max(
            pos2(left, content.top() + 104.0),
            pos2(
                (left + 760.0).min(content.right() - cx.layout.margin),
                content.bottom() - 24.0,
            ),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(detail)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let page = self.page;
        egui::ScrollArea::vertical()
            .id_salt(("settings.page", page.slug()))
            .auto_shrink([false, false])
            .content_margin(egui::Margin::symmetric(8, 8))
            .show(&mut child, |ui| {
                focus::scope(ui, Id::unique(("settings.page", page.slug())), |ui| {
                    if let Some(p) = self.page_mut(page) {
                        p.ui(cx, ui);
                    }
                });
            });
    }
}

/// A page heading (`title`) with an optional description.
pub(crate) fn page_heading(ui: &mut Ui, title: &str, description: Option<&str>) {
    ui.label(
        egui::RichText::new(title)
            .font(font::title())
            .color(color::TEXT),
    );
    if let Some(d) = description {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(d)
                .font(font::body())
                .color(color::TEXT_2),
        );
    }
    ui.add_space(16.0);
}

/// A right-aligned value label for a [`crate::ui::widgets::SettingRow`].
pub(crate) fn value_label(ui: &mut Ui, s: &str, c: egui::Color32) {
    ui.label(
        egui::RichText::new(crate::ui::text::cap(s, 120))
            .font(font::body())
            .color(c),
    );
}

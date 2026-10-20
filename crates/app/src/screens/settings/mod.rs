// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings (§8.6): a section list on the left and the selected page on the right. Each
//! page lives in its own file and implements [`SettingsPageUi`]:
//!
//! | Page | File | What |
//! |---|---|---|
//! | Account | `account.rs` | status, expiry, connections, HTTP warning, edit (PIN) / refresh |
//! | Profiles | `profiles.rs` | every profile on this computer: switch, add, edit, delete (PIN) |
//! | Playback | `playback.rs` | this profile: stream format, catch-up format, User-Agent; every profile: audio/subtitle languages, hardware decoding, buffer |
//! | Parental control | `parental.rs` (+ `pin_flow.rs`) | PIN set/change/off (page protected by the PIN), words, category locks, forgot PIN |
//! | Appearance | `appearance.rs` | TV mode, UI size, reduced motion (follows Windows by default) |
//! | Language & region | `language.rs` | clock format, display language |
//! | Storage & data | `storage.rs` | cache usage/clear/cap, history, backup export/import, data folder, logs |
//! | Privacy & security | `privacy.rs` | plain-language privacy facts, optional TMDB key |
//! | About | `about.rs` (+ `licences.rs`) | versions, GPL notice, source, third-party licences |
//!
//! Pages get [`SettingsPageUi::on_enter`] when shown and [`SettingsPageUi::on_leave`] when
//! another page is chosen or Settings closes (the parental page locks itself again), and
//! may consume Esc in [`SettingsPageUi::on_back`] (close their dialogs).

mod about;
mod account;
mod appearance;
mod language;
mod licences;
mod parental;
mod pin_flow;
mod playback;
mod privacy;
mod profiles;
mod storage;

use eframe::egui::{self, Align2, Id, Rect, Stroke, Ui, pos2};

use super::{AppCtx, Screen};
use crate::focus;
use crate::nav::{Section, SettingsPage};
use crate::ui::icons;
use crate::ui::theme::{color, font};
use crate::ui::widgets::MenuRow;

/// One settings page.
pub trait SettingsPageUi {
    /// Called when the page is shown (load data here).
    fn on_enter(&mut self, _cx: &mut AppCtx<'_>) {}
    /// Called when another page is chosen or Settings closes.
    fn on_leave(&mut self, _cx: &mut AppCtx<'_>) {}
    /// Esc/Backspace: return true if consumed (e.g. a dialog closed).
    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        false
    }
    /// The widget that gets the focus when moving Right from the section list into this
    /// page for the first time (default: the nearest one geometrically). Text pages return
    /// their first item so reading starts at the top.
    fn entry_focus(&self) -> Option<Id> {
        None
    }
    /// Draws the page into the detail pane.
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui);
}

/// The settings screen.
pub struct SettingsScreen {
    page: SettingsPage,
    account: account::AccountPage,
    profiles: profiles::ProfilesPage,
    playback: playback::PlaybackPage,
    parental: parental::ParentalPage,
    appearance: appearance::AppearancePage,
    language: language::LanguagePage,
    storage: storage::StoragePage,
    privacy: privacy::PrivacyPage,
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
            account: account::AccountPage::default(),
            profiles: profiles::ProfilesPage::default(),
            playback: playback::PlaybackPage::default(),
            parental: parental::ParentalPage::default(),
            appearance: appearance::AppearancePage::default(),
            language: language::LanguagePage,
            storage: storage::StoragePage::default(),
            privacy: privacy::PrivacyPage::default(),
            about: about::AboutPage::default(),
            entered: false,
        }
    }

    fn page_mut(&mut self, page: SettingsPage) -> &mut dyn SettingsPageUi {
        match page {
            SettingsPage::Account => &mut self.account,
            SettingsPage::Profiles => &mut self.profiles,
            SettingsPage::Playback => &mut self.playback,
            SettingsPage::Parental => &mut self.parental,
            SettingsPage::Appearance => &mut self.appearance,
            SettingsPage::Language => &mut self.language,
            SettingsPage::Storage => &mut self.storage,
            SettingsPage::Privacy => &mut self.privacy,
            SettingsPage::About => &mut self.about,
        }
    }
}

fn page_icon(p: SettingsPage) -> &'static str {
    match p {
        SettingsPage::Account => icons::USER_CIRCLE,
        SettingsPage::Profiles => icons::USERS_THREE,
        SettingsPage::Playback => icons::PLAY_CIRCLE,
        SettingsPage::Parental => icons::LOCK_SIMPLE,
        SettingsPage::Appearance => icons::PALETTE,
        SettingsPage::Language => icons::GLOBE,
        SettingsPage::Storage => icons::DATABASE,
        SettingsPage::Privacy => icons::SHIELD_CHECK,
        SettingsPage::About => icons::INFO,
    }
}

impl Screen for SettingsScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.entered = true;
        let page = self.page;
        self.page_mut(page).on_enter(cx);
    }

    fn on_leave(&mut self, cx: &mut AppCtx<'_>) {
        let page = self.page;
        self.page_mut(page).on_leave(cx);
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        let page = self.page;
        self.page_mut(page).on_back(cx)
    }

    fn on_input(&mut self, _cx: &mut AppCtx<'_>, ctx: &egui::Context) {
        // Right from the section list into a page not visited yet: start at its top.
        let page = self.page;
        let page_scope = Id::unique(("settings.page", page.slug()));
        if focus::is_in_scope(ctx, Id::unique("settings.sections"))
            && focus::scope_memory(ctx, page_scope).is_none()
            && let Some(entry) = self.page_mut(page).entry_focus()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight))
        {
            focus::request(ctx, entry);
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

        // Section list (scrolls if the window is very short, e.g. TV mode at 720p).
        let mut list = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
            pos2(pane.left() + 16.0, content.top() + 104.0),
            pos2(pane.right() - 16.0, pane.bottom() - 16.0),
        )));
        let mut picked = None;
        egui::ScrollArea::vertical()
            .id_salt("settings.sections.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(0, 8))
            .show(&mut list, |ui| {
                focus::scope(ui, Id::unique("settings.sections"), |ui| {
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
            });
        if let Some(p) = picked
            && p != self.page
        {
            let old = self.page;
            self.page_mut(old).on_leave(cx);
            self.page = p;
            self.page_mut(p).on_enter(cx);
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
                    self.page_mut(page).ui(cx, ui);
                    ui.add_space(24.0);
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

/// A section heading inside a page (`title-sm`) with an optional description.
pub(crate) fn section(ui: &mut Ui, title: &str, description: Option<&str>) {
    ui.label(
        egui::RichText::new(title)
            .font(font::title_sm())
            .color(color::TEXT),
    );
    if let Some(d) = description {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(d)
                .font(font::caption())
                .color(color::TEXT_3),
        );
    }
    ui.add_space(8.0);
}

/// Adds `add` as a focusable "reading stop": arrow keys can land on it (with the focus
/// ring), so a page of text scrolls with the keyboard or a remote even where it has no
/// controls. Activation does nothing.
pub(crate) fn readable<R>(ui: &mut Ui, id: Id, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui.scope(add);
    let rect = r.response.rect;
    let (_, f) = focus::focusable(ui, id, rect, focus::FocusOpts::default());
    if f.focused {
        crate::ui::paint::focus_ring(ui.painter(), rect, crate::ui::theme::radius::SM, 1.0);
    }
    r.inner
}

/// A paragraph of body text (`text-2`).
pub(crate) fn paragraph(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(font::body())
            .color(color::TEXT_2),
    );
}

/// A right-aligned value label for a [`crate::ui::widgets::SettingRow`].
pub(crate) fn value_label(ui: &mut Ui, s: &str, c: egui::Color32) {
    ui.label(
        egui::RichText::new(crate::ui::text::cap(s, 120))
            .font(font::body())
            .color(c),
    );
}

/// Human-readable byte size ("12.3 MB").
pub(crate) fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if b < 1000 {
        return format!("{b} bytes");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_human() {
        assert_eq!(human_bytes(0), "0 bytes");
        assert_eq!(human_bytes(999), "999 bytes");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(12_345_678), "12.3 MB");
        assert_eq!(human_bytes(512 * 1024 * 1024), "537 MB");
        assert_eq!(human_bytes(3_000_000_000), "3.0 GB");
    }
}

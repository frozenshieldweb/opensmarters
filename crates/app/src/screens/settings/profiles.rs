// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Profiles: every profile on this computer, with switch / add / edit /
//! delete. While a parental PIN is set, adding, editing and deleting ask for it first
//! (otherwise a new profile would dodge this profile's category locks, and editing an
//! account could point its stored password at another server).

use eframe::egui::{Id, Ui};
use opensmarters_store::{Profile, ProfileId, ProfileSource};

use super::{SettingsPageUi, page_heading, paragraph};
use crate::error::AppError;
use crate::nav::Route;
use crate::screens::AppCtx;
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::text;
use crate::ui::widgets::{Button, Dialog, PinPrompt, PinPromptResult, SettingRow};

#[derive(Debug, Clone)]
enum Action {
    Add,
    Edit(ProfileId),
    Delete(ProfileId, String),
}

/// The profiles page.
#[derive(Debug, Default)]
pub struct ProfilesPage {
    profiles: Loader<Vec<Profile>>,
    gate: Option<(PinPrompt, Action)>,
    confirm: Option<(ProfileId, String)>,
    deleting: Loader<ProfileId>,
}

/// "Xtream · panel.example:8080" (never the username or a playlist address).
pub(crate) fn source_line(p: &Profile) -> String {
    match &p.source {
        ProfileSource::Xtream { server_url, .. } => {
            let host = opensmarters_core::url::Url::parse(server_url)
                .ok()
                .and_then(|u| {
                    u.host_str().map(|h| match u.port() {
                        Some(port) => format!("{h}:{port}"),
                        None => h.to_owned(),
                    })
                })
                .unwrap_or_else(|| "server".to_owned());
            format!("Xtream Codes · {}", text::cap(&host, 60))
        }
        ProfileSource::M3uUrl => "M3U playlist (web address)".to_owned(),
        ProfileSource::M3uFile { .. } => "M3U playlist (file)".to_owned(),
    }
}

impl ProfilesPage {
    fn reload(&mut self, cx: &AppCtx<'_>) {
        let store = cx.services.store.clone();
        self.profiles.start_blocking(cx.rt(), &cx.egui, move || {
            store.profiles().list().map_err(AppError::from)
        });
    }

    fn run(&mut self, cx: &mut AppCtx<'_>, action: Action) {
        match action {
            Action::Add => cx.nav.push(Route::Onboarding { edit: None }),
            Action::Edit(id) => cx.nav.push(Route::Onboarding { edit: Some(id) }),
            Action::Delete(id, name) => self.confirm = Some((id, name)),
        }
    }
}

impl SettingsPageUi for ProfilesPage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.gate = None;
        self.confirm = None;
        self.reload(cx);
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        self.gate.take().is_some() || self.confirm.take().is_some()
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.profiles.poll();
        if self.deleting.poll()
            && let Some(id) = self.deleting.take()
        {
            cx.toasts.success("Profile deleted.");
            if cx.session().is_some_and(|s| s.profile_id() == id) {
                cx.playback.stop();
                cx.sessions.close();
                cx.nav.reset(Route::Profiles);
                return;
            }
            self.reload(cx);
        } else if let Some(e) = self.deleting.error().cloned() {
            cx.toasts.error(e.user_message());
            self.deleting = Loader::new();
        }
        page_heading(
            ui,
            "Profiles",
            Some(
                "Each profile is one provider account or playlist, with its own favourites and history.",
            ),
        );
        let current = cx.session().map(|s| s.profile_id());
        let mut next: Option<Action> = None;
        if let Some(list) = self.profiles.value() {
            for p in list {
                let mut desc = source_line(p);
                if Some(p.id) == current {
                    desc.push_str(" · open now");
                }
                let name = text::cap(&p.name, 60);
                SettingRow::new(&name).description(&desc).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if Button::ghost("Delete")
                        .icon(icons::TRASH)
                        .small()
                        .id(Id::unique(("settings.profiles.delete", p.id)))
                        .show(ui)
                        .activated
                    {
                        next = Some(Action::Delete(p.id, p.name.clone()));
                    }
                    if Button::secondary("Edit")
                        .icon(icons::PENCIL_SIMPLE)
                        .small()
                        .id(Id::unique(("settings.profiles.edit", p.id)))
                        .show(ui)
                        .activated
                    {
                        next = Some(Action::Edit(p.id));
                    }
                });
            }
        }
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if Button::primary("Add a profile")
                .icon(icons::USER_PLUS)
                .id(Id::unique("settings.profiles.add"))
                .show(ui)
                .activated
            {
                next = Some(Action::Add);
            }
            if Button::secondary("Switch profile")
                .icon(icons::USER_SWITCH)
                .id(Id::unique("settings.profiles.switch"))
                .show(ui)
                .activated
            {
                cx.nav.push(Route::Profiles);
            }
        });
        ui.add_space(16.0);
        paragraph(
            ui,
            "While parental control is on, adding, editing or deleting a profile asks for the PIN.",
        );
        if let Some(a) = next {
            self.gate = Some((PinPrompt::new(Id::unique("settings.profiles.gate")), a));
        }

        // PIN gate (passes at once when no PIN is set).
        if let Some((gate, action)) = &mut self.gate {
            let title = match action {
                Action::Add => "Enter your PIN to add a profile",
                Action::Edit(_) => "Enter your PIN to edit this profile",
                Action::Delete(..) => "Enter your PIN to delete this profile",
            };
            match gate.show_dialog(ui.ctx(), cx.services, title, None) {
                PinPromptResult::Verified(_) => {
                    let a = action.clone();
                    self.gate = None;
                    self.run(cx, a);
                }
                PinPromptResult::Cancelled | PinPromptResult::Forgot => self.gate = None,
                PinPromptResult::Pending => {}
            }
        }
        if let Some((id, name)) = self.confirm.clone() {
            let title = format!("Delete \u{201c}{}\u{201d}?", text::cap(&name, 40));
            let mut close = false;
            Dialog::new(Id::unique("settings.profiles.confirm"), &title)
                .body("Its password, favourites and watch history are removed from this computer. Your provider account is not affected.")
                .show(ui.ctx(), |ui| {
                    if Button::danger("Delete")
                        .icon(icons::TRASH)
                        .id(Id::unique("settings.profiles.confirm.yes"))
                        .show(ui)
                        .activated
                    {
                        let store = cx.services.store.clone();
                        self.deleting.start_blocking(cx.rt(), &cx.egui, move || {
                            store.profiles().delete(id)?;
                            Ok(id)
                        });
                        close = true;
                    }
                    if Button::secondary("Keep profile")
                        .autofocus()
                        .id(Id::unique("settings.profiles.confirm.no"))
                        .show(ui)
                        .activated
                    {
                        close = true;
                    }
                });
            if close {
                self.confirm = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use opensmarters_store::{ProfileKind, ProfileSettings, Timestamp};

    use super::*;

    fn profile(source: ProfileSource) -> Profile {
        Profile {
            id: ProfileId::new_random(),
            name: "P".into(),
            source,
            settings: ProfileSettings::default(),
            has_epg_url_override: false,
            sort_order: 0,
            created_at: Timestamp::from_unix_millis(0),
            updated_at: Timestamp::from_unix_millis(0),
            last_used_at: None,
        }
    }

    #[test]
    fn source_line_never_shows_credentials() {
        let p = profile(ProfileSource::Xtream {
            server_url: "http://panel.example.invalid:8080".into(),
            username: "secret-user".into(),
        });
        let line = source_line(&p);
        assert_eq!(line, "Xtream Codes · panel.example.invalid:8080");
        assert!(!line.contains("secret-user"));
        assert_eq!(p.kind(), ProfileKind::Xtream);
        assert_eq!(
            source_line(&profile(ProfileSource::M3uUrl)),
            "M3U playlist (web address)"
        );
    }
}

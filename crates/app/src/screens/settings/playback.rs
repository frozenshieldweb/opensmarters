// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings → Playback (per profile): live stream container (only what the account's
//! `allowed_output_formats` permits), catch-up time format, User-Agent preset.

use eframe::egui::{Id, Ui};
use opensmarters_store::{
    LiveContainer, ProfileId, ProfileSettings, ProfileUpdate, TimeshiftFormat, UserAgent,
};

use super::{SettingsPageUi, page_heading};
use crate::error::AppError;
use crate::screens::AppCtx;
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::widgets::{Button, Callout, CalloutKind, Segmented, SettingRow, TextField};

/// The playback page.
#[derive(Debug, Default)]
pub struct PlaybackPage {
    loaded_for: Option<ProfileId>,
    container: usize,
    timeshift: usize,
    ua: usize,
    custom_ua: String,
    error: Option<String>,
    save: Loader<ProfileId>,
}

impl PlaybackPage {
    fn load_from(&mut self, s: &ProfileSettings) {
        self.container = usize::from(s.live_container == LiveContainer::M3u8);
        self.timeshift = usize::from(s.timeshift_format == TimeshiftFormat::SmartersCompat);
        self.ua = match &s.user_agent {
            UserAgent::Default => 0,
            UserAgent::SmartersCompat => 1,
            UserAgent::Custom(c) => {
                self.custom_ua = c.clone();
                2
            }
        };
    }
}

impl SettingsPageUi for PlaybackPage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(s) = cx.session() {
            self.load_from(&s.profile().settings);
            self.loaded_for = Some(s.profile_id());
        }
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(session) = cx.session() else { return };
        if self.loaded_for != Some(session.profile_id()) {
            self.load_from(&session.profile().settings);
            self.loaded_for = Some(session.profile_id());
        }
        if self.save.poll() {
            if self.save.take().is_some() {
                cx.toasts.success("Playback settings saved.");
                let services = cx.services.clone();
                let (id, name) = (session.profile_id(), session.profile().name.clone());
                cx.sessions.open(&services, id, name);
            } else if let Some(e) = self.save.error() {
                self.error = Some(e.user_message());
            }
        }
        page_heading(
            ui,
            "Playback",
            Some("These settings belong to this profile."),
        );
        let allowed = session.allowed_live_containers();
        SettingRow::new("Live TV stream format")
            .description("MPEG-TS works almost everywhere; HLS can be steadier on busy networks")
            .show(ui, |ui| {
                Segmented::new(
                    Id::unique("settings.pb.container"),
                    &["MPEG-TS", "HLS"],
                    260.0,
                )
                .show(ui, &mut self.container);
            });
        let wanted = if self.container == 1 {
            LiveContainer::M3u8
        } else {
            LiveContainer::Ts
        };
        if !allowed.contains(&wanted) {
            Callout::new(
                CalloutKind::Info,
                "Not available:",
                format!(
                    "your provider doesn't allow .{} streams, so .{} is used.",
                    wanted.extension(),
                    allowed.first().map_or("ts", |c| c.extension())
                ),
            )
            .width(ui.available_width())
            .show(ui);
        }
        SettingRow::new("Catch-up time format")
            .description("Try the other format if catch-up programmes don't start")
            .show(ui, |ui| {
                Segmented::new(
                    Id::unique("settings.pb.timeshift"),
                    &["Standard", "Compatible"],
                    260.0,
                )
                .show(ui, &mut self.timeshift);
            });
        SettingRow::new("User-Agent")
            .description("Some providers only accept known players")
            .show(ui, |ui| {
                Segmented::new(
                    Id::unique("settings.pb.ua"),
                    &["opensmarters", "Smarters", "Custom"],
                    330.0,
                )
                .show(ui, &mut self.ua);
            });
        if self.ua == 2 {
            TextField::new(Id::unique("settings.pb.ua-custom"), "Custom User-Agent")
                .placeholder("MyPlayer/1.0")
                .width(ui.available_width().min(440.0))
                .char_limit(512)
                .show(ui, &mut self.custom_ua);
        }
        if let Some(e) = &self.error {
            Callout::new(CalloutKind::Danger, "", e.clone())
                .width(ui.available_width())
                .show(ui);
        }
        ui.add_space(24.0);
        let busy = self.save.is_loading();
        if Button::primary(if busy {
            "Saving…"
        } else {
            "Save and reconnect"
        })
        .icon(icons::CHECK)
        .enabled(!busy)
        .id(Id::unique("settings.pb.save"))
        .show(ui)
        .activated
        {
            self.error = None;
            let ua = match self.ua {
                1 => UserAgent::SmartersCompat,
                2 => {
                    let c = self.custom_ua.trim().to_owned();
                    if c.is_empty() || !c.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                        self.error =
                            Some("Use 1 to 512 plain characters for the User-Agent.".into());
                        return;
                    }
                    UserAgent::Custom(c)
                }
                _ => UserAgent::Default,
            };
            let settings = ProfileSettings {
                live_container: wanted,
                timeshift_format: if self.timeshift == 1 {
                    TimeshiftFormat::SmartersCompat
                } else {
                    TimeshiftFormat::Standard
                },
                user_agent: ua,
            };
            let store = cx.services.store.clone();
            let id = session.profile_id();
            self.save.start_blocking(cx.rt(), &cx.egui, move || {
                store
                    .profiles()
                    .update(
                        id,
                        ProfileUpdate {
                            settings: Some(settings),
                            ..ProfileUpdate::default()
                        },
                    )
                    .map(|p| p.id)
                    .map_err(AppError::from)
            });
        }
    }
}

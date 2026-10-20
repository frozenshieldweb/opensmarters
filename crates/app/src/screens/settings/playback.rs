// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Playback.
//!
//! - **This profile** (saved with the profile, then it reconnects): live stream container
//!   (only what the account's `allowed_output_formats` permits), catch-up time format,
//!   User-Agent preset.
//! - **Every profile** (app preferences, applied at once): preferred audio language,
//!   subtitles (off or a preferred language; live TV always starts with subtitles off),
//!   hardware decoding and the network buffer (both applied when the player starts, i.e.
//!   the next launch). See [`crate::playback::tracks`].

use eframe::egui::{self, Id, Ui};
use opensmarters_store::{
    LiveContainer, ProfileId, ProfileSettings, ProfileUpdate, TimeshiftFormat, UserAgent,
};

use super::{SettingsPageUi, page_heading, section, value_label};
use crate::error::AppError;
use crate::playback::tracks::{LANGUAGES, language_name};
use crate::playback::{BufferPreset, PlayerTuning};
use crate::screens::AppCtx;
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::theme::color;
use crate::ui::widgets::{
    Button, Callout, CalloutKind, Dialog, MenuRow, Segmented, SettingRow, TextField, Toggle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picker {
    Audio,
    Subtitles,
}

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
    picker: Option<Picker>,
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

    fn profile_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(session) = cx.session() else { return };
        section(
            ui,
            "This profile",
            Some("Saved with the profile; it reconnects to your provider when you save."),
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
        ui.add_space(16.0);
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

    fn app_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        section(
            ui,
            "Every profile",
            Some("Preferences for this computer, used by all profiles."),
        );
        let audio = cx
            .prefs
            .audio_language
            .as_deref()
            .and_then(language_name)
            .unwrap_or("Stream default");
        SettingRow::new("Audio language")
            .description("Chosen when a channel or film has it")
            .show(ui, |ui| {
                if Button::secondary(audio)
                    .trailing(icons::CARET_RIGHT)
                    .id(Id::unique("settings.pb.audio"))
                    .show(ui)
                    .activated
                {
                    self.picker = Some(Picker::Audio);
                }
            });
        let subs = cx
            .prefs
            .subtitle_language
            .as_deref()
            .and_then(language_name)
            .unwrap_or("Off");
        SettingRow::new("Subtitles")
            .description("Films, series and catch-up; live TV starts without subtitles")
            .show(ui, |ui| {
                if Button::secondary(subs)
                    .trailing(icons::CARET_RIGHT)
                    .id(Id::unique("settings.pb.subs"))
                    .show(ui)
                    .activated
                {
                    self.picker = Some(Picker::Subtitles);
                }
            });
        let mut changed = false;
        SettingRow::new("Hardware decoding")
            .description("Uses the graphics card: smoother and cooler. Turn off if video glitches")
            .show(ui, |ui| {
                let mut on = cx.prefs.hw_decoding;
                Toggle::new(Id::unique("settings.pb.hwdec")).show(ui, &mut on);
                if on != cx.prefs.hw_decoding {
                    cx.prefs.hw_decoding = on;
                    changed = true;
                }
            });
        SettingRow::new("Network buffer")
            .description("Larger rides out short drops in films and series, using more memory")
            .show(ui, |ui| {
                let mut sel = usize::from(cx.prefs.network_buffer.min(2));
                let labels: Vec<&str> = BufferPreset::ALL.iter().map(|p| p.label).collect();
                Segmented::new(Id::unique("settings.pb.buffer"), &labels, 330.0).show(ui, &mut sel);
                let v = u8::try_from(sel).unwrap_or(1);
                if v != cx.prefs.network_buffer {
                    cx.prefs.network_buffer = v;
                    changed = true;
                }
            });
        let wanted = PlayerTuning {
            hw_decoding: cx.prefs.hw_decoding,
            buffer: BufferPreset::at(cx.prefs.network_buffer),
        };
        if cx.playback.is_available() && wanted != cx.playback.tuning() {
            ui.add_space(8.0);
            Callout::new(
                CalloutKind::Info,
                "Restart to apply:",
                "hardware decoding and the buffer change the next time opensmarters starts.",
            )
            .width(ui.available_width())
            .show(ui);
        }
        if changed {
            cx.save_prefs();
        }
    }

    fn picker_dialog(&mut self, cx: &mut AppCtx<'_>, ui: &Ui) {
        let Some(which) = self.picker else { return };
        let (title, none_label, current) = match which {
            Picker::Audio => (
                "Audio language",
                "Stream default",
                cx.prefs.audio_language.clone(),
            ),
            Picker::Subtitles => ("Subtitles", "Off", cx.prefs.subtitle_language.clone()),
        };
        let mut chosen: Option<Option<String>> = None;
        let mut close = false;
        Dialog::new(Id::unique(("settings.pb.picker", which as u8)), title)
            .width(420.0)
            .show_custom(ui.ctx(), |ui| {
                ui.label(
                    egui::RichText::new(title)
                        .font(crate::ui::theme::font::title())
                        .color(color::TEXT),
                );
                ui.add_space(12.0);
                let max_h = (ui.ctx().content_rect().height() - 300.0).clamp(160.0, 440.0);
                egui::ScrollArea::vertical()
                    .id_salt("settings.pb.picker.scroll")
                    .max_height(max_h)
                    .auto_shrink([false, true])
                    .content_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let none_sel = current.is_none();
                        if MenuRow::new(
                            Id::unique(("settings.pb.picker.none", which as u8)),
                            none_label,
                        )
                        .selected(none_sel)
                        .autofocus(none_sel)
                        .show(ui)
                        .activated
                        {
                            chosen = Some(None);
                        }
                        for l in LANGUAGES {
                            let sel = current.as_deref() == Some(l.code);
                            if MenuRow::new(
                                Id::unique(("settings.pb.picker.lang", which as u8, l.code)),
                                l.name,
                            )
                            .selected(sel)
                            .autofocus(sel)
                            .show(ui)
                            .activated
                            {
                                chosen = Some(Some(l.code.to_owned()));
                            }
                        }
                    });
                ui.add_space(16.0);
                if Button::ghost("Cancel")
                    .id(Id::unique("settings.pb.picker.cancel"))
                    .show(ui)
                    .activated
                {
                    close = true;
                }
            });
        if let Some(c) = chosen {
            match which {
                Picker::Audio => cx.prefs.audio_language = c,
                Picker::Subtitles => cx.prefs.subtitle_language = c,
            }
            cx.save_prefs();
            close = true;
        }
        if close {
            self.picker = None;
        }
    }
}

impl SettingsPageUi for PlaybackPage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(s) = cx.session() {
            self.load_from(&s.profile().settings);
            self.loaded_for = Some(s.profile_id());
        }
    }

    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        self.picker.take().is_some()
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
        page_heading(ui, "Playback", None);
        self.profile_section(cx, ui);
        ui.add_space(32.0);
        self.app_section(cx, ui);
        let _ = value_label;
        self.picker_dialog(cx, ui);
    }
}

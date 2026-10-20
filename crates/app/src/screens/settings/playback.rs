// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Playback.
//!
//! - **This profile** (saved with the profile, then it reconnects): live stream container
//!   (only what the account's `allowed_output_formats` permits), catch-up time format,
//!   User-Agent preset.
//! - **Simultaneous streams** (this profile, applied at once): Auto (the account's
//!   `max_connections`) or 1–4, stored in `MAX_STREAMS_OVERRIDE` and applied with
//!   `session.connections().set_override(..)`.
//! - **Every profile** (app preferences, applied at once): preferred audio language,
//!   subtitles (off or a preferred language; live TV always starts with subtitles off),
//!   hardware decoding, buffering (Auto / Small / Standard / Large), the live rewind
//!   window (Off / 15 / 30 / 60 min) and resetting the extra buffering of channels that
//!   stalled. See [`crate::playback::tracks`] and [`crate::playback::tuning`].

use eframe::egui::{self, Id, Ui};
use opensmarters_store::{
    LiveContainer, ProfileId, ProfileSettings, ProfileUpdate, TimeshiftFormat, UserAgent,
};

use super::{SettingsPageUi, human_bytes, page_heading, section, value_label};
use crate::error::AppError;
use crate::focus;
use crate::playback::tracks::{LANGUAGES, language_name};
use crate::playback::tuning::rewind_bytes;
use crate::playback::{Buffering, PlayerTuning, REWIND_CHOICES, sanitize_rewind};
use crate::screens::AppCtx;
use crate::services::connections::MAX_STREAMS_OVERRIDE;
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
        SettingRow::new("Buffering")
            .description(
                "Auto (recommended) starts channels quickly, buffers more for films and for \
                 channels that stalled",
            )
            .show(ui, |ui| {
                let mut sel = usize::from(Buffering::from_index(cx.prefs.buffering).index());
                let labels: Vec<&str> = Buffering::ALL.iter().map(|b| b.label()).collect();
                Segmented::new(Id::unique("settings.pb.buffering"), &labels, 400.0)
                    .show(ui, &mut sel);
                let v = u8::try_from(sel).unwrap_or(0);
                if v != cx.prefs.buffering {
                    cx.prefs.buffering = v;
                    changed = true;
                }
            });
        let disk = cx.playback.has_disk_cache();
        let rewind_text = rewind_description(cx.prefs.live_rewind, disk);
        SettingRow::new("Rewind live TV")
            .description(&rewind_text)
            .show(ui, |ui| {
                let labels = ["Off", "15 min", "30 min", "60 min"];
                let mut sel = REWIND_CHOICES
                    .iter()
                    .position(|m| *m == sanitize_rewind(cx.prefs.live_rewind))
                    .unwrap_or(2);
                Segmented::new(Id::unique("settings.pb.rewind"), &labels, 400.0).show(ui, &mut sel);
                let v = REWIND_CHOICES.get(sel).copied().unwrap_or(30);
                if v != cx.prefs.live_rewind {
                    cx.prefs.live_rewind = v;
                    changed = true;
                }
            });
        if let Some(session) = cx.session() {
            let profile = session.profile_id();
            let boosted = cx.playback.boosted_channels(profile);
            if boosted > 0 {
                let label = if boosted == 1 {
                    "1 channel starts with extra buffering".to_owned()
                } else {
                    format!("{boosted} channels start with extra buffering")
                };
                SettingRow::new(&label)
                    .description("They stalled before (Buffering: Auto). Reset to start fresh")
                    .show(ui, |ui| {
                        if Button::secondary("Reset")
                            .icon(icons::ARROW_COUNTER_CLOCKWISE)
                            .id(Id::unique("settings.pb.boost-reset"))
                            .show(ui)
                            .activated
                        {
                            cx.playback
                                .reset_channel_buffering(&cx.services.clone(), profile);
                            cx.toasts.success("Extra buffering reset.");
                            focus::request(&cx.egui, Id::unique("settings.pb.rewind").with(1));
                        }
                    });
            }
        }
        if changed {
            cx.save_prefs();
            // Hardware decoding and buffering apply to what is playing right away.
            cx.playback.set_tuning(PlayerTuning::from_prefs(cx.prefs));
        }
    }

    /// "Simultaneous streams" for this profile: stored in the app settings
    /// ([`MAX_STREAMS_OVERRIDE`]) and applied to the connection budget at once.
    fn streams_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(session) = cx.session() else { return };
        let budget = session.connections();
        section(
            ui,
            "Simultaneous streams",
            Some("This profile; applies at once."),
        );
        let account = budget.account_limit();
        let auto = match account {
            Some(n) => format!("Auto ({n})"),
            None => "Auto".to_owned(),
        };
        let labels = [auto.as_str(), "1", "2", "3", "4"];
        let current = budget.override_limit();
        let mut sel = current.map_or(0, |n| usize::try_from(n.min(4)).unwrap_or(4));
        SettingRow::new("Streams at once")
            .description(
                "The most streams opensmarters opens at once on this account, shared by \
                 multiscreen, recordings and downloads",
            )
            .show(ui, |ui| {
                Segmented::new(Id::unique("settings.pb.streams"), &labels, 400.0)
                    .show(ui, &mut sel);
            });
        let wanted = u32::try_from(sel).ok().filter(|n| *n > 0);
        if wanted != current {
            budget.set_override(wanted);
            let key = session.profile_id().to_string();
            cx.services.blocking("save simultaneous streams", move |s| {
                let mut m = s.settings().get(&MAX_STREAMS_OVERRIDE)?;
                match wanted.and_then(|n| u8::try_from(n).ok()) {
                    Some(n) => {
                        m.insert(key, n);
                    }
                    None => {
                        m.remove(&key);
                    }
                }
                s.settings().set(&MAX_STREAMS_OVERRIDE, &m)
            });
        }
        if let (Some(n), Some(acc)) = (wanted, account)
            && n > acc
        {
            ui.add_space(8.0);
            Callout::new(
                CalloutKind::Info,
                "More than your account allows:",
                format!(
                    "your provider allows {acc}; extra streams will be refused (and can lock \
                     the account out for a minute)."
                ),
            )
            .width(ui.available_width())
            .show(ui);
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
            focus::request(&cx.egui, picker_button(which));
        }
    }
}

/// The "Rewind live TV" description: what it does and the disk (or memory) it may use.
fn rewind_description(minutes: u8, disk: bool) -> String {
    let minutes = sanitize_rewind(minutes);
    if minutes == 0 {
        return "Pause and go back on live channels. Off: Space still pauses, briefly".to_owned();
    }
    if disk {
        // Behind and ahead of the playback position (a long pause), at 8 Mbit/s.
        let bytes = 2 * rewind_bytes(minutes, true);
        format!(
            "Pause and go back up to {minutes} min on live channels. Uses up to {} of disk \
             while watching",
            human_bytes(bytes)
        )
    } else {
        format!(
            "Pause and go back on live channels. Without a data folder it stays in memory, \
             so it holds less than {minutes} min"
        )
    }
}

fn picker_button(p: Picker) -> Id {
    match p {
        Picker::Audio => Id::unique("settings.pb.audio"),
        Picker::Subtitles => Id::unique("settings.pb.subs"),
    }
}

impl SettingsPageUi for PlaybackPage {
    fn entry_focus(&self) -> Option<Id> {
        Some(Id::unique("settings.pb.container").with(self.container))
    }

    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(s) = cx.session() {
            self.load_from(&s.profile().settings);
            self.loaded_for = Some(s.profile_id());
        }
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        match self.picker.take() {
            Some(p) => {
                focus::request(&cx.egui, picker_button(p));
                true
            }
            None => false,
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
        page_heading(ui, "Playback", None);
        self.profile_section(cx, ui);
        ui.add_space(32.0);
        self.streams_section(cx, ui);
        ui.add_space(32.0);
        self.app_section(cx, ui);
        let _ = value_label;
        self.picker_dialog(cx, ui);
    }
}

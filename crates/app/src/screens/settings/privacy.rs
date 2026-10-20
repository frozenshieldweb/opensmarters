// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings › Privacy & security: plain-language facts (where data lives, Credential
//! Manager, no telemetry, what the HTTP warning means, how provider data is handled) and
//! the optional TMDB enrichment (off by default; the user's own API key, kept in the
//! keychain; checked against TMDB only when the user asks).

use eframe::egui::{Id, Ui};
use opensmarters_core::secrecy::{ExposeSecret, SecretString};
use opensmarters_core::{HttpConfig, Redactor};
use opensmarters_store::settings::TMDB_ENABLED;

use super::{SettingsPageUi, page_heading, section, value_label};
use crate::error::{AppError, AppResult};
use crate::screens::AppCtx;
use crate::task::Loader;
use crate::ui::icons;
use crate::ui::theme::color;
use crate::ui::widgets::{Button, Callout, CalloutKind, SettingRow, TextField, Toggle};

/// TMDB's API endpoint used to check a key (no personal data is sent).
const TMDB_CHECK_URL: &str = "https://api.themoviedb.org/3/configuration";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TmdbState {
    enabled: bool,
    has_key: bool,
}

/// The privacy & security page.
#[derive(Debug, Default)]
pub struct PrivacyPage {
    tmdb: Loader<TmdbState>,
    key: String,
    key_error: Option<String>,
    check: Loader<TmdbState>,
}

/// Checks the shape of a TMDB v3 API key (32 hexadecimal characters).
pub(crate) fn tmdb_key_problem(key: &str) -> Option<&'static str> {
    let k = key.trim();
    if k.is_empty() {
        return Some("Paste your TMDB API key first.");
    }
    if k.starts_with("eyJ") {
        return Some(
            "That's a Read Access Token. Paste the \u{201c}API Key\u{201d} (32 letters and digits) instead.",
        );
    }
    if k.len() != 32 || !k.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some("A TMDB API key is 32 letters and digits (0–9, a–f).");
    }
    None
}

/// Asks TMDB whether the key works (one request to `api.themoviedb.org`, the key
/// redacted from logs).
async fn check_tmdb_key(key: &SecretString, redactor: Redactor) -> AppResult<()> {
    let k = key.expose_secret().trim().to_owned();
    redactor.add_secret(&k);
    let http = opensmarters_core::net::HttpClient::new(&HttpConfig::default(), redactor)?;
    let mut url = opensmarters_core::url::Url::parse(TMDB_CHECK_URL)
        .map_err(|_| AppError::msg("TMDB's address is invalid."))?;
    url.query_pairs_mut().append_pair("api_key", &k);
    match http.get_bytes(&url).await {
        Ok(_) => Ok(()),
        Err(opensmarters_core::Error::Http { status: 401 }) => Err(AppError::msg(
            "TMDB says this key isn't valid. Check it on themoviedb.org under Settings › API.",
        )),
        Err(e) => Err(AppError::from(e)),
    }
}

impl PrivacyPage {
    fn load(&mut self, cx: &AppCtx<'_>) {
        let store = cx.services.store.clone();
        self.tmdb.start_blocking(cx.rt(), &cx.egui, move || {
            let s = store.settings();
            Ok(TmdbState {
                enabled: s.get(&TMDB_ENABLED)?,
                has_key: s.tmdb_api_key()?.is_some(),
            })
        });
    }

    fn tmdb_section(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        section(
            ui,
            "Optional: film and series details from TMDB",
            Some(
                "Off by default. With your own free API key, opensmarters can ask The Movie \
                 Database for posters and descriptions your provider lacks. TMDB then sees your \
                 IP address and the titles looked up.",
            ),
        );
        if self.check.poll() {
            if let Some(st) = self.check.take() {
                self.tmdb.set(st);
                self.key.clear();
                cx.toasts
                    .success("TMDB key checked and saved in Windows Credential Manager.");
            }
        } else if let Some(e) = self.check.error().cloned() {
            self.key_error = Some(e.user_message());
            self.check = Loader::new();
        }
        self.tmdb.poll();
        let Some(state) = self.tmdb.value().copied() else {
            return;
        };
        SettingRow::new("Use TMDB")
            .description(if state.has_key {
                "Uses the key saved below"
            } else {
                "Add a key below to use it"
            })
            .show(ui, |ui| {
                let mut on = state.enabled;
                Toggle::new(Id::unique("settings.privacy.tmdb")).show(ui, &mut on);
                if on != state.enabled {
                    self.tmdb.set(TmdbState {
                        enabled: on,
                        ..state
                    });
                    cx.services.blocking("save the TMDB setting", move |s| {
                        s.settings().set(&TMDB_ENABLED, &on)
                    });
                }
            });
        SettingRow::new("API key")
            .description("Stored in Windows Credential Manager, never in files")
            .show(ui, |ui| {
                if state.has_key {
                    if Button::ghost("Remove key")
                        .icon(icons::TRASH)
                        .small()
                        .id(Id::unique("settings.privacy.tmdb.remove"))
                        .show(ui)
                        .activated
                    {
                        self.tmdb.set(TmdbState {
                            enabled: false,
                            has_key: false,
                        });
                        cx.services.blocking("remove the TMDB key", |s| {
                            s.settings().set_tmdb_api_key(None)?;
                            s.settings().set(&TMDB_ENABLED, &false)
                        });
                        cx.toasts.info("TMDB key removed.");
                    }
                    value_label(ui, "Saved", color::SUCCESS);
                } else {
                    value_label(ui, "None", color::TEXT_3);
                }
            });
        ui.add_space(8.0);
        let busy = self.check.is_loading();
        let mut submit = false;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.vertical(|ui| {
                let r = TextField::new(
                    Id::unique("settings.privacy.tmdb.key"),
                    if state.has_key {
                        "Replace the key"
                    } else {
                        "TMDB API key (v3)"
                    },
                )
                .password(true)
                .placeholder("32 letters and digits")
                .width(360.0)
                .char_limit(128)
                .error(self.key_error.as_deref())
                .show(ui, &mut self.key);
                if r.changed {
                    self.key_error = None;
                }
                submit |= r.submitted;
            });
            ui.vertical(|ui| {
                ui.add_space(24.0);
                submit |= Button::secondary(if busy {
                    "Checking…"
                } else {
                    "Check and save"
                })
                .icon(icons::CHECK)
                .enabled(!busy && !self.key.trim().is_empty())
                .id(Id::unique("settings.privacy.tmdb.save"))
                .show(ui)
                .activated;
            });
        });
        if submit && !busy {
            if let Some(p) = tmdb_key_problem(&self.key) {
                self.key_error = Some(p.to_owned());
                return;
            }
            let key = SecretString::from(self.key.trim().to_owned());
            let redactor = cx.services.redactor.clone();
            let store = cx.services.store.clone();
            self.check.start(cx.rt(), &cx.egui, move |_| async move {
                check_tmdb_key(&key, redactor).await?;
                tokio::task::spawn_blocking(move || -> AppResult<TmdbState> {
                    let s = store.settings();
                    s.set_tmdb_api_key(Some(&key))?;
                    s.set(&TMDB_ENABLED, &true)?;
                    Ok(TmdbState {
                        enabled: true,
                        has_key: true,
                    })
                })
                .await
                .map_err(|_| AppError::Cancelled)?
            });
        }
        ui.add_space(8.0);
        super::paragraph(
            ui,
            "Checking the key sends one request to api.themoviedb.org. Film and series pages \
             will use TMDB in a later update.",
        );
    }
}

impl SettingsPageUi for PrivacyPage {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.key.clear();
        self.key_error = None;
        self.load(cx);
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.tmdb.is_idle() {
            self.load(cx);
        }
        page_heading(
            ui,
            "Privacy & security",
            Some("What opensmarters keeps, where, and who it talks to."),
        );
        let w = ui.available_width();
        let data = cx.services.paths.data_dir.as_ref().map_or_else(
            || "is kept in memory only: this is a temporary session".to_owned(),
            |p| format!("is in {}", p.display()),
        );
        let logs = cx.services.paths.log_dir().map_or_else(
            || "not written in this temporary session".to_owned(),
            |p| p.display().to_string(),
        );
        for (i, (glyph_kind, lead, body)) in [
            (
                CalloutKind::Success,
                "No tracking, ever.",
                "No telemetry, analytics, ads or crash reports. opensmarters only connects to \
                 your provider's server, the stream and image addresses your provider gives, \
                 and TMDB if you turn it on below."
                    .to_owned(),
            ),
            (
                CalloutKind::Info,
                "Passwords:",
                "account passwords and playlist addresses are stored in Windows Credential \
                 Manager (entries named \u{201c}opensmarters\u{201d}), never in files, logs \
                 or backups."
                    .to_owned(),
            ),
            (
                CalloutKind::Info,
                "Everything else",
                format!(
                    "(profiles without passwords, favourites, watch history, settings, and a \
                     cache of your provider's lists, encrypted) {data}."
                ),
            ),
            (
                CalloutKind::Info,
                "Logs:",
                format!(
                    "{logs}. Passwords, usernames and stream addresses are removed before \
                     anything is written."
                ),
            ),
            (
                CalloutKind::Warning,
                "Unencrypted connections:",
                "many providers use http:// instead of https://. Your username and password \
                 then travel in the clear, and anyone on your network or along the way could \
                 read them. Accounts like this show a warning. Use https:// if your provider \
                 offers it."
                    .to_owned(),
            ),
            (
                CalloutKind::Info,
                "Provider data is untrusted:",
                "names, guides and images are shown as plain text and pictures (no web pages, \
                 no scripts); the player only opens web streams, and certificate checks are \
                 always on."
                    .to_owned(),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            super::readable(ui, Id::unique(("settings.privacy.fact", i)), |ui| {
                Callout::new(glyph_kind, lead, body).width(w).show(ui);
            });
            ui.add_space(8.0);
        }
        ui.add_space(24.0);
        self.tmdb_section(cx, ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmdb_key_shapes() {
        assert!(tmdb_key_problem("0123456789abcdef0123456789ABCDEF").is_none());
        assert!(tmdb_key_problem(" 0123456789abcdef0123456789abcdef ").is_none());
        assert!(tmdb_key_problem("").is_some());
        assert!(tmdb_key_problem("eyJhbGciOiJIUzI1NiJ9.e30.x").is_some());
        assert!(tmdb_key_problem("short").is_some());
        assert!(tmdb_key_problem("0123456789abcdef0123456789abcdeg").is_some());
    }
}

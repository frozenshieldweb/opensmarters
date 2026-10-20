// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings → Account: profile type, server (never credentials), status, expiry,
//! connections, the permanent unencrypted-connection warning, and actions.

use eframe::egui::{self, Id, Ui};
use opensmarters_core::AccountStatus;
use opensmarters_core::chrono::Utc;
use opensmarters_store::ProfileSource;

use super::{SettingsPageUi, page_heading, value_label};
use crate::nav::Route;
use crate::screens::AppCtx;
use crate::ui::icons;
use crate::ui::theme::color;
use crate::ui::widgets::{Button, Callout, CalloutKind, SettingRow};

/// The account page.
#[derive(Debug, Default)]
pub struct AccountPage;

impl SettingsPageUi for AccountPage {
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(session) = cx.session() else {
            page_heading(ui, "Account", Some("No account is open."));
            return;
        };
        let profile = session.profile().clone();
        page_heading(ui, "Account", Some("The account this profile signs in to."));
        if session.is_insecure() {
            Callout::unencrypted().width(ui.available_width()).show(ui);
            ui.add_space(12.0);
        }
        if session.is_offline() {
            Callout::new(CalloutKind::Warning, "Offline:", "your provider couldn't be reached when this profile opened, so saved data is shown.")
                .width(ui.available_width())
                .show(ui);
            ui.add_space(12.0);
        }
        SettingRow::new("Profile name")
            .show(ui, |ui| value_label(ui, &profile.name, color::TEXT_2));
        let (kind, server) = match &profile.source {
            ProfileSource::Xtream {
                server_url,
                username,
            } => ("Xtream Codes", format!("{server_url} · {username}")),
            ProfileSource::M3uUrl => (
                "M3U playlist (web address)",
                "Stored in the system keychain".to_owned(),
            ),
            ProfileSource::M3uFile { path } => ("M3U playlist (file)", path.display().to_string()),
        };
        SettingRow::new("Type").show(ui, |ui| value_label(ui, kind, color::TEXT_2));
        SettingRow::new(if session.is_xtream() {
            "Server · username"
        } else {
            "Source"
        })
        .show(ui, |ui| value_label(ui, &server, color::TEXT_2));
        if let Some(acc) = session.account() {
            let (status, c) = match &acc.status {
                Some(AccountStatus::Active) | None => ("Active".to_owned(), color::SUCCESS),
                Some(other) => (
                    crate::ui::text::cap(other.as_str(), 24).into_owned(),
                    color::DANGER,
                ),
            };
            SettingRow::new("Status").show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} {status}",
                        if c == color::SUCCESS {
                            icons::CHECK_CIRCLE
                        } else {
                            icons::WARNING_CIRCLE
                        }
                    ))
                    .color(c),
                );
            });
            let expiry = match acc.exp_date {
                Some(d) => {
                    let days = acc.days_left(Utc::now()).unwrap_or(0);
                    let when = if days > 1 {
                        format!("in {days} days")
                    } else if days >= 0 {
                        "very soon".to_owned()
                    } else {
                        "expired".to_owned()
                    };
                    format!("{} ({when})", d.format("%-d %B %Y"))
                }
                None => "No expiry date".to_owned(),
            };
            SettingRow::new("Expires").show(ui, |ui| value_label(ui, &expiry, color::TEXT_2));
            let cons = match (acc.active_cons, acc.max_connections) {
                (Some(a), Some(m)) => format!("{a} of {m} in use"),
                (None, Some(m)) => format!("Up to {m}"),
                _ => "Not reported".to_owned(),
            };
            SettingRow::new("Connections")
                .description("Streams that can play at the same time")
                .show(ui, |ui| value_label(ui, &cons, color::TEXT_2));
            if acc.is_trial {
                SettingRow::new("Trial").show(ui, |ui| value_label(ui, "Yes", color::TEXT_2));
            }
            if !acc.allowed_output_formats.is_empty() {
                SettingRow::new("Stream formats").show(ui, |ui| {
                    value_label(ui, &acc.allowed_output_formats.join(", "), color::TEXT_2)
                });
            }
            if let Some(msg) = &acc.message {
                SettingRow::new("Message from your provider")
                    .show(ui, |ui| value_label(ui, msg, color::TEXT_2));
            }
        }
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if Button::primary("Edit account")
                .icon(icons::PENCIL_SIMPLE)
                .id(Id::unique("settings.account.edit"))
                .show(ui)
                .activated
            {
                cx.nav.push(Route::Onboarding {
                    edit: Some(profile.id),
                });
            }
            if Button::secondary("Refresh channels and films")
                .icon(icons::ARROWS_CLOCKWISE)
                .id(Id::unique("settings.account.refresh"))
                .show(ui)
                .activated
            {
                let s = session.clone();
                let services = cx.services.clone();
                let id = profile.id;
                let name = profile.name.clone();
                cx.services.spawn(async move { s.refresh().await });
                cx.sessions.open(&services, id, name);
                cx.toasts.info("Refreshing from your provider…");
            }
            if Button::ghost("Switch profile")
                .icon(icons::USER_SWITCH)
                .id(Id::unique("settings.account.switch"))
                .show(ui)
                .activated
            {
                cx.nav.push(Route::Profiles);
            }
        });
    }
}

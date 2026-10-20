// SPDX-License-Identifier: GPL-3.0-or-later
//! Profile picker ("Who's watching?", §6.15): choose, add, edit or delete a profile, and
//! the sign-in progress / failure state of the profile being opened (re-validation at
//! startup shows expired/banned accounts here with a clear message).
//!
//! While a parental PIN is set, managing profiles (edit/delete), adding one and editing a
//! failed account ask for it first ([`PinPrompt`]); choosing a profile never does.

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use opensmarters_store::{Profile, ProfileId, ProfileKind};

use super::{AppCtx, Chrome, Screen};
use crate::focus::{self, FocusOpts};
use crate::nav::Route;
use crate::task::Loader;
use crate::ui::assets::{self, Asset};
use crate::ui::icons::{self, Style};
use crate::ui::theme::{color, font, radius};
use crate::ui::widgets::{
    Avatar, Button, Callout, CalloutKind, Dialog, PinPrompt, PinPromptResult,
};
use crate::ui::{paint, text};

#[derive(Debug, Clone, PartialEq)]
enum Modal {
    Actions(ProfileId, String),
    ConfirmDelete(ProfileId, String),
}

/// An action waiting for the parental PIN.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gated {
    Manage,
    Add,
    Edit(ProfileId),
}

fn add_request_id() -> Id {
    Id::unique("profiles.request-add")
}

/// Asks the picker to start "Add profile" when it next draws, behind the same PIN gate as
/// its "Add profile" tile (used for `--route onboarding` on a machine with profiles, so
/// the harness can't skip the gate).
pub fn request_add(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(add_request_id(), true));
}

/// The profile picker.
#[derive(Debug)]
pub struct ProfilesScreen {
    profiles: Loader<Vec<Profile>>,
    manage: bool,
    modal: Option<Modal>,
    deleting: Loader<ProfileId>,
    gate: Option<(PinPrompt, Gated)>,
}

impl Default for ProfilesScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfilesScreen {
    /// Creates the screen.
    pub fn new() -> Self {
        Self {
            profiles: Loader::new(),
            manage: false,
            modal: None,
            deleting: Loader::new(),
            gate: None,
        }
    }

    fn ask_pin(&mut self, action: Gated) {
        self.gate = Some((PinPrompt::new(Id::unique("profiles.gate")), action));
    }

    fn reload(&mut self, cx: &AppCtx<'_>) {
        let store = cx.services.store.clone();
        self.profiles
            .start_blocking(cx.rt(), &cx.egui, move || Ok(store.profiles().list()?));
    }

    fn open(cx: &mut AppCtx<'_>, p: &Profile) {
        if cx.session().is_some_and(|s| s.profile_id() == p.id) {
            cx.nav.reset(Route::Home);
            return;
        }
        let services = cx.services.clone();
        cx.sessions.open(&services, p.id, p.name.clone());
    }

    fn status_for(cx: &AppCtx<'_>, p: &Profile) -> Option<(String, Color32)> {
        let (id, failure) = cx.sessions.failure()?;
        if *id != p.id {
            return None;
        }
        let label = if failure.error.is_network() {
            "Can't connect".to_owned()
        } else if let Some(status) = failure.account.as_ref().and_then(|a| a.status.clone()) {
            text::cap(status.as_str(), 24).into_owned()
        } else if failure.error.is_auth() {
            "Sign-in refused".to_owned()
        } else {
            "Problem".to_owned()
        };
        Some((label, color::DANGER))
    }
}

impl Screen for ProfilesScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        self.reload(cx);
    }

    fn chrome(&self) -> Chrome {
        Chrome::Fullscreen
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if self.gate.take().is_some() || self.modal.take().is_some() {
            return true;
        }
        if self.manage {
            self.manage = false;
            return true;
        }
        if cx.sessions.opening().is_some() {
            cx.sessions.cancel();
            return true;
        }
        false
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.profiles.poll();
        if self.gate.is_none()
            && ui
                .ctx()
                .data_mut(|d| d.remove_temp::<bool>(add_request_id()))
                .unwrap_or(false)
        {
            self.ask_pin(Gated::Add);
        }
        if self.deleting.poll() && self.deleting.value().is_some() {
            if let Some(id) = self.deleting.take()
                && cx.session().is_some_and(|s| s.profile_id() == id)
            {
                cx.playback.stop();
                cx.sessions.close();
            }
            cx.toasts.success("Profile deleted.");
            self.reload(cx);
        }
        if let Some(e) = self.deleting.error() {
            cx.toasts.error(e.user_message());
            self.deleting = Loader::new();
        }
        let rect = cx.layout.screen;
        let painter = ui.painter().clone();
        if !assets::paint_cover(
            ui,
            &painter,
            Asset::HomeAmbient,
            rect,
            vec2(1.0, 0.0),
            0,
            Color32::WHITE,
        ) {
            painter.rect_filled(rect, 0.0, color::BG);
        }

        // Signing in.
        if let Some((_, name)) = cx.sessions.opening().cloned() {
            let c = rect.center();
            let sym = Rect::from_center_size(c - vec2(0.0, 60.0), vec2(64.0, 64.0));
            assets::paint_contain(
                ui,
                &painter,
                Asset::Symbol,
                sym,
                Color32::WHITE.gamma_multiply(0.8),
            );
            paint::spinner(
                &painter,
                c - vec2(0.0, 60.0),
                52.0,
                ui.input(|i| i.time),
                3.0,
            );
            ui.ctx().request_repaint();
            text::line(
                &painter,
                c + vec2(0.0, 24.0),
                Align2::CENTER_CENTER,
                &format!("Signing in to {name}…"),
                font::title(),
                color::TEXT,
                800.0,
            );
            painter.text(
                c + vec2(0.0, 56.0),
                Align2::CENTER_CENTER,
                "Checking your account with the provider",
                font::body(),
                color::TEXT_2,
            );
            let b = Button::secondary("Cancel")
                .id(Id::unique("profiles.cancel"))
                .autofocus();
            let size = b.size(ui);
            let mut child = ui.new_child(
                egui::UiBuilder::new().max_rect(Rect::from_center_size(c + vec2(0.0, 116.0), size)),
            );
            if b.show(&mut child).activated {
                cx.sessions.cancel();
            }
            return;
        }

        let top = rect.top() + rect.height() / 3.0 - 64.0;
        painter.text(
            pos2(rect.center().x, top),
            Align2::CENTER_CENTER,
            if self.manage {
                "Manage profiles"
            } else {
                "Who's watching?"
            },
            font::display(),
            color::TEXT,
        );

        let Some(profiles) = self.profiles.value().cloned() else {
            return;
        };
        // Avatar row (wraps).
        let tile_w = 136.0 + 24.0;
        let gap = 40.0;
        let n = profiles.len() + 1;
        let per_row = (((rect.width() - 96.0 + gap) / (tile_w + gap)).floor() as usize).clamp(1, n);
        let rows = n.div_ceil(per_row);
        let row_h = 136.0 + 12.0 + 26.0 + 20.0 + 24.0;
        let mut y = top + 72.0;
        let mut activated: Option<Profile> = None;
        let mut add = false;
        focus::scope(ui, Id::unique("profiles.row"), |ui| {
            for row in 0..rows {
                let start = row * per_row;
                let count = (n - start).min(per_row);
                let row_w = count as f32 * tile_w + (count as f32 - 1.0) * gap;
                let mut x = rect.center().x - row_w / 2.0;
                for i in start..start + count {
                    let tile = Rect::from_min_size(pos2(x, y), vec2(tile_w, row_h));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(tile));
                    if let Some(p) = profiles.get(i) {
                        let status = Self::status_for(cx, p);
                        let is_last = cx.session().is_some_and(|s| s.profile_id() == p.id)
                            || cx.sessions.failure().is_some_and(|(id, _)| *id == p.id);
                        let mut av = Avatar::new(
                            Id::unique(("profiles.avatar", p.id)),
                            assets::avatar_for(&format!("{:?}", p.id)),
                            &p.name,
                        )
                        .autofocus(is_last || i == 0);
                        if let Some((label, c)) = &status {
                            av = av.status(label, *c);
                        } else if let Some(kind) = kind_label(p.kind()) {
                            av = av.status(kind, color::TEXT_3);
                        }
                        let f = av.show(&mut child);
                        if self.manage {
                            let badge = pos2(tile.center().x + 48.0, tile.top() + 112.0);
                            child.painter().circle_filled(badge, 16.0, color::SURFACE_3);
                            icons::paint(
                                child.painter(),
                                badge,
                                icons::PENCIL_SIMPLE,
                                16.0,
                                Style::Regular,
                                color::TEXT,
                            );
                        }
                        if f.activated {
                            activated = Some(p.clone());
                        }
                    } else if add_tile(&mut child, tile) {
                        add = true;
                    }
                    x += tile_w + gap;
                }
                y += row_h;
            }
        });

        // Failure details (startup re-validation).
        if let Some((id, failure)) = cx.sessions.failure().cloned()
            && let Some(p) = profiles.iter().find(|p| p.id == id)
        {
            let w = 560.0;
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                pos2(rect.center().x - w / 2.0, y + 8.0),
                vec2(w, 200.0),
            )));
            let mut msg = failure.error.user_message();
            if let Some(exp) = failure.account.as_ref().and_then(|a| a.exp_date) {
                msg.push_str(&format!(" Expiry date: {}.", exp.format("%-d %B %Y")));
            }
            Callout::new(
                CalloutKind::Danger,
                format!("{}:", text::cap(&p.name, 40)),
                msg,
            )
            .width(w)
            .show(&mut child);
            child.add_space(12.0);
            child.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if Button::secondary("Try again")
                    .icon(icons::ARROWS_CLOCKWISE)
                    .id(Id::unique("profiles.retry"))
                    .show(ui)
                    .activated
                {
                    let services = cx.services.clone();
                    cx.sessions.open(&services, p.id, p.name.clone());
                }
                if Button::ghost("Edit account")
                    .icon(icons::PENCIL_SIMPLE)
                    .id(Id::unique("profiles.edit-failed"))
                    .show(ui)
                    .activated
                {
                    self.ask_pin(Gated::Edit(p.id));
                }
            });
            y += 140.0;
        }

        // Manage toggle.
        let label = if self.manage {
            "Done"
        } else {
            "Manage profiles"
        };
        let b = Button::ghost(label)
            .icon(if self.manage {
                icons::CHECK
            } else {
                icons::PENCIL_SIMPLE
            })
            .id(Id::unique("profiles.manage"));
        let size = b.size(ui);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
            pos2(rect.center().x - size.x / 2.0, y + 16.0),
            size,
        )));
        if b.show(&mut child).activated {
            if self.manage {
                self.manage = false;
            } else {
                self.ask_pin(Gated::Manage);
            }
        }

        if let Some(p) = activated {
            if self.manage {
                self.modal = Some(Modal::Actions(p.id, p.name.clone()));
            } else {
                Self::open(cx, &p);
            }
        }
        if add {
            self.ask_pin(Gated::Add);
        }
        if let Some((gate, action)) = &mut self.gate {
            let title = match action {
                Gated::Manage => "Enter your PIN to manage profiles",
                Gated::Add => "Enter your PIN to add a profile",
                Gated::Edit(_) => "Enter your PIN to edit this account",
            };
            match gate.show_dialog(ui.ctx(), cx.services, title, None) {
                PinPromptResult::Verified(_) => {
                    let action = *action;
                    self.gate = None;
                    match action {
                        Gated::Manage => self.manage = true,
                        Gated::Add => cx.nav.push(Route::Onboarding { edit: None }),
                        Gated::Edit(id) => cx.nav.push(Route::Onboarding { edit: Some(id) }),
                    }
                }
                PinPromptResult::Cancelled | PinPromptResult::Forgot => self.gate = None,
                PinPromptResult::Pending => {}
            }
        }

        // Dialogs.
        match self.modal.clone() {
            Some(Modal::Actions(id, name)) => {
                let title = text::cap(&name, 60).into_owned();
                let mut next = None;
                Dialog::new(Id::unique("profiles.actions"), &title)
                    .body("Edit the account details, or remove this profile from this computer.")
                    .show(ui.ctx(), |ui| {
                        if Button::primary("Edit account")
                            .icon(icons::PENCIL_SIMPLE)
                            .show(ui)
                            .activated
                        {
                            cx.nav.push(Route::Onboarding { edit: Some(id) });
                            next = Some(None);
                        }
                        if Button::danger("Delete")
                            .icon(icons::TRASH)
                            .show(ui)
                            .activated
                        {
                            next = Some(Some(Modal::ConfirmDelete(id, name.clone())));
                        }
                        if Button::secondary("Cancel").autofocus().show(ui).activated {
                            next = Some(None);
                        }
                    });
                if let Some(n) = next {
                    self.modal = n;
                }
            }
            Some(Modal::ConfirmDelete(id, name)) => {
                let title = format!("Delete \u{201c}{}\u{201d}?", text::cap(&name, 40));
                let mut close = false;
                Dialog::new(Id::unique("profiles.confirm-delete"), &title)
                    .body("Its password, favourites and watch history are removed from this computer. Your provider account is not affected.")
                    .show(ui.ctx(), |ui| {
                        if Button::danger("Delete").icon(icons::TRASH).show(ui).activated {
                            let store = cx.services.store.clone();
                            self.deleting.start_blocking(cx.rt(), &cx.egui, move || {
                                store.profiles().delete(id)?;
                                Ok(id)
                            });
                            close = true;
                        }
                        if Button::secondary("Keep profile")
                            .autofocus()
                            .show(ui)
                            .activated
                        {
                            close = true;
                        }
                    });
                if close {
                    self.modal = None;
                }
            }
            None => {}
        }
        let _ = (
            Sense::hover(),
            Stroke::NONE,
            CornerRadius::ZERO,
            radius::FULL,
            FocusOpts::default(),
        );
    }
}

fn kind_label(kind: ProfileKind) -> Option<&'static str> {
    match kind {
        ProfileKind::Xtream => None,
        ProfileKind::M3uUrl => Some("Playlist"),
        ProfileKind::M3uFile => Some("Playlist file"),
    }
}

/// The dashed "Add profile" tile. Returns true when activated.
fn add_tile(ui: &mut Ui, tile: Rect) -> bool {
    let circle =
        Rect::from_center_size(pos2(tile.center().x, tile.top() + 68.0), vec2(136.0, 136.0));
    let (_, f) = focus::focusable(ui, Id::unique("profiles.add"), circle, FocusOpts::default());
    let painter = ui.painter().clone();
    if f.anim > 0.0 {
        paint::focus_glow(&painter, circle, radius::FULL, f.anim);
    }
    let c = circle.center();
    let r = 67.0;
    let segs = 40;
    for i in 0..segs {
        if i % 2 == 0 {
            let a0 = i as f32 / segs as f32 * std::f32::consts::TAU;
            let a1 = (i as f32 + 1.0) / segs as f32 * std::f32::consts::TAU;
            painter.line_segment(
                [
                    c + vec2(a0.cos(), a0.sin()) * r,
                    c + vec2(a1.cos(), a1.sin()) * r,
                ],
                Stroke::new(
                    2.0,
                    if f.focused {
                        color::TEXT_2
                    } else {
                        color::STROKE
                    },
                ),
            );
        }
    }
    icons::paint(
        &painter,
        c,
        icons::PLUS,
        36.0,
        Style::Regular,
        if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        },
    );
    if f.focused {
        paint::focus_ring_circle(&painter, c, 68.0, 1.0);
    }
    painter.text(
        pos2(tile.center().x, circle.bottom() + 25.0),
        Align2::CENTER_CENTER,
        "Add profile",
        font::title_sm(),
        if f.focused {
            color::TEXT
        } else {
            color::TEXT_2
        },
    );
    f.activated
}

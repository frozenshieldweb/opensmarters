// SPDX-License-Identifier: GPL-3.0-or-later
//! Add / edit an account (§8.1): Xtream (server, username, password), M3U URL or M3U file,
//! validation with clear errors, the unencrypted-HTTP warning, `get.php` → Xtream
//! conversion, a login test before saving, secrets into the keychain via the store, and
//! per-profile settings (User-Agent preset, timeshift format, live container).

use std::path::PathBuf;
use std::sync::mpsc;

use eframe::egui::{self, Align2, Color32, Id, Rect, ScrollArea, Ui, pos2, vec2};
use opensmarters_core::ServerUrl;
use opensmarters_core::m3u::detect_xtream;
use opensmarters_core::secrecy::{ExposeSecret, SecretString};
use opensmarters_store::{
    LiveContainer, NewProfile, NewProfileSource, Profile, ProfileId, ProfileSettings,
    ProfileSource, ProfileUpdate, TimeshiftFormat, UserAgent,
};

use super::{AppCtx, Chrome, Screen};
use crate::error::AppError;
use crate::focus;
use crate::services::session::{self, user_agent_string};
use crate::task::Loader;
use crate::ui::assets::{self, Asset};
use crate::ui::icons;
use crate::ui::theme::{color, font};
use crate::ui::widgets::{Button, Callout, CalloutKind, Segmented, TextField};
use crate::ui::{paint, text};

const COL_W: f32 = 440.0;

#[derive(Debug, Default, Clone)]
struct Errors {
    name: Option<String>,
    server: Option<String>,
    username: Option<String>,
    password: Option<String>,
    m3u: Option<String>,
    ua: Option<String>,
}

impl Errors {
    fn any(&self) -> bool {
        self.name.is_some()
            || self.server.is_some()
            || self.username.is_some()
            || self.password.is_some()
            || self.m3u.is_some()
            || self.ua.is_some()
    }
}

#[derive(Debug)]
struct Saved {
    id: ProfileId,
    name: String,
    note: Option<String>,
}

/// The add/edit account screen.
#[derive(Debug)]
pub struct OnboardingScreen {
    edit: Option<ProfileId>,
    existing: Loader<Option<Profile>>,
    prefilled: bool,
    mode: usize,
    m3u_mode: usize,
    name: String,
    server: String,
    username: String,
    password: String,
    m3u_url: String,
    m3u_file: Option<PathBuf>,
    advanced: bool,
    ua: usize,
    custom_ua: String,
    timeshift: usize,
    container: usize,
    errors: Errors,
    form_error: Option<AppError>,
    submit: Loader<Saved>,
    file_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
}

impl OnboardingScreen {
    /// Add (`None`) or edit a profile.
    pub fn new(edit: Option<ProfileId>) -> Self {
        Self {
            edit,
            existing: Loader::new(),
            prefilled: false,
            mode: 0,
            m3u_mode: 0,
            name: String::new(),
            server: String::new(),
            username: String::new(),
            password: String::new(),
            m3u_url: String::new(),
            m3u_file: None,
            advanced: false,
            ua: 0,
            custom_ua: String::new(),
            timeshift: 0,
            container: 0,
            errors: Errors::default(),
            form_error: None,
            submit: Loader::new(),
            file_rx: None,
        }
    }

    fn prefill(&mut self, p: &Profile) {
        self.name = p.name.clone();
        match &p.source {
            ProfileSource::Xtream {
                server_url,
                username,
            } => {
                self.mode = 0;
                self.server = server_url.clone();
                self.username = username.clone();
            }
            ProfileSource::M3uUrl => {
                self.mode = 1;
                self.m3u_mode = 0;
            }
            ProfileSource::M3uFile { path } => {
                self.mode = 1;
                self.m3u_mode = 1;
                self.m3u_file = Some(path.clone());
            }
        }
        self.ua = match &p.settings.user_agent {
            UserAgent::Default => 0,
            UserAgent::SmartersCompat => 1,
            UserAgent::Custom(s) => {
                self.custom_ua = s.clone();
                2
            }
        };
        self.timeshift =
            usize::from(p.settings.timeshift_format == TimeshiftFormat::SmartersCompat);
        self.container = usize::from(p.settings.live_container == LiveContainer::M3u8);
        self.advanced = self.ua != 0 || self.timeshift != 0 || self.container != 0;
    }

    fn settings(&self) -> ProfileSettings {
        ProfileSettings {
            live_container: if self.container == 1 {
                LiveContainer::M3u8
            } else {
                LiveContainer::Ts
            },
            timeshift_format: if self.timeshift == 1 {
                TimeshiftFormat::SmartersCompat
            } else {
                TimeshiftFormat::Standard
            },
            user_agent: match self.ua {
                1 => UserAgent::SmartersCompat,
                2 => UserAgent::Custom(self.custom_ua.trim().to_owned()),
                _ => UserAgent::Default,
            },
        }
    }

    fn is_http(&self) -> bool {
        let s = if self.mode == 0 {
            self.server.trim()
        } else if self.m3u_mode == 0 {
            self.m3u_url.trim()
        } else {
            return false;
        };
        if s.is_empty() {
            return false;
        }
        let lower = s.to_ascii_lowercase();
        lower.starts_with("http://") || (self.mode == 0 && !lower.contains("://"))
    }

    fn validate(&mut self) -> bool {
        let mut e = Errors::default();
        if self.name.trim().is_empty() {
            e.name = Some("Give this account a name, like \u{201c}Living room\u{201d}.".into());
        } else if self.name.trim().chars().count() > 128 {
            e.name = Some("Use 128 characters or fewer.".into());
        }
        let editing = self.edit.is_some();
        if self.mode == 0 {
            if self.server.trim().is_empty() {
                e.server = Some("Enter the server address your provider gave you.".into());
            } else if let Err(err) = ServerUrl::parse(&self.server) {
                e.server = Some(AppError::from(err).user_message());
            }
            if self.username.trim().is_empty() {
                e.username = Some("Enter your username.".into());
            }
            if self.password.is_empty() && !editing {
                e.password = Some("Enter your password.".into());
            }
        } else if self.m3u_mode == 0 {
            let u = self.m3u_url.trim();
            if u.is_empty() && !editing {
                e.m3u = Some("Enter the playlist address.".into());
            } else if !u.is_empty() {
                let lower = u.to_ascii_lowercase();
                if !(lower.starts_with("http://") || lower.starts_with("https://"))
                    || opensmarters_core::url::Url::parse(u).is_err()
                {
                    e.m3u = Some("Use a web address that starts with http:// or https://.".into());
                }
            }
        } else if self.m3u_file.is_none() {
            e.m3u = Some("Choose a playlist file.".into());
        }
        if self.ua == 2 {
            let ua = self.custom_ua.trim();
            if ua.is_empty() || ua.len() > 512 || !ua.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                e.ua = Some("Use 1 to 512 plain characters (letters, digits, punctuation).".into());
            }
        }
        self.errors = e;
        !self.errors.any()
    }

    fn submit(&mut self, cx: &AppCtx<'_>) {
        self.form_error = None;
        if !self.validate() {
            return;
        }
        let store = cx.services.store.clone();
        let redactor = cx.services.redactor.clone();
        let edit = self.edit;
        let name = self.name.trim().to_owned();
        let settings = self.settings();
        let ua = user_agent_string(&settings.user_agent);
        let mode = self.mode;
        let m3u_mode = self.m3u_mode;
        let server = self.server.trim().to_owned();
        let username = self.username.trim().to_owned();
        let password =
            (!self.password.is_empty()).then(|| SecretString::from(self.password.clone()));
        let m3u_url = (!self.m3u_url.trim().is_empty())
            .then(|| SecretString::from(self.m3u_url.trim().to_owned()));
        let file = self.m3u_file.clone();
        self.submit.start(cx.rt(), &cx.egui, move |_| async move {
            let blocking = |f: Box<dyn FnOnce() -> Result<Saved, AppError> + Send>| async move {
                tokio::task::spawn_blocking(f).await.map_err(|_| AppError::Cancelled)?
            };
            match mode {
                0 => {
                    // Use the stored password when editing without typing a new one.
                    let pw = match (&password, edit) {
                        (Some(p), _) => p.clone(),
                        (None, Some(id)) => {
                            let store = store.clone();
                            tokio::task::spawn_blocking(move || store.profiles().password(id))
                                .await
                                .map_err(|_| AppError::Cancelled)??
                                .ok_or_else(|| AppError::msg("Enter the password again."))?
                        }
                        (None, None) => return Err(AppError::msg("Enter your password.")),
                    };
                    let login = session::test_xtream_login(&server, &username, pw.clone(), &ua, redactor)
                        .await
                        .map_err(|f| f.error)?;
                    let mut settings = settings;
                    let allowed: Vec<LiveContainer> = opensmarters_core::urls::LiveContainer::allowed(&login.user_info)
                        .into_iter()
                        .map(session::store_container)
                        .collect();
                    let mut note = None;
                    if !allowed.contains(&settings.live_container)
                        && let Some(first) = allowed.first()
                    {
                        settings.live_container = *first;
                        note = Some(format!(
                            "Your provider only allows .{} for live TV, so that's what will be used.",
                            first.extension()
                        ));
                    }
                    blocking(Box::new(move || {
                        let p = match edit {
                            Some(id) => {
                                let p = store.profiles().update(
                                    id,
                                    ProfileUpdate {
                                        name: Some(name.clone()),
                                        server_url: Some(server.clone()),
                                        username: Some(username.clone()),
                                        settings: Some(settings),
                                        ..ProfileUpdate::default()
                                    },
                                )?;
                                if password.is_some() {
                                    store.profiles().set_password(id, &pw)?;
                                }
                                p
                            }
                            None => store.profiles().create(NewProfile {
                                name: name.clone(),
                                source: NewProfileSource::Xtream {
                                    server_url: server.clone(),
                                    username: username.clone(),
                                    password: pw,
                                },
                                settings,
                            })?,
                        };
                        // Edits invalidate cached catalogue data.
                        let _ = store.cache().invalidate_profile(p.id);
                        Ok(Saved { id: p.id, name, note })
                    }))
                    .await
                }
                _ if m3u_mode == 0 => {
                    let url = match (&m3u_url, edit) {
                        (Some(u), _) => u.clone(),
                        (None, Some(id)) => {
                            let store = store.clone();
                            tokio::task::spawn_blocking(move || store.profiles().m3u_url(id))
                                .await
                                .map_err(|_| AppError::Cancelled)??
                                .ok_or_else(|| AppError::msg("Enter the playlist address again."))?
                        }
                        (None, None) => return Err(AppError::msg("Enter the playlist address.")),
                    };
                    let n = session::test_m3u_url(&url, &ua, redactor).await?;
                    if n == 0 {
                        return Err(AppError::msg("The playlist is empty."));
                    }
                    blocking(Box::new(move || {
                        let p = match edit {
                            Some(id) => {
                                let p = store.profiles().update(
                                    id,
                                    ProfileUpdate {
                                        name: Some(name.clone()),
                                        settings: Some(settings),
                                        ..ProfileUpdate::default()
                                    },
                                )?;
                                if m3u_url.is_some() {
                                    store.profiles().set_m3u_url(id, &url)?;
                                }
                                p
                            }
                            None => store.profiles().create(NewProfile {
                                name: name.clone(),
                                source: NewProfileSource::M3uUrl { url },
                                settings,
                            })?,
                        };
                        let _ = store.cache().invalidate_profile(p.id);
                        Ok(Saved { id: p.id, name, note: Some(format!("{n} entries found.")) })
                    }))
                    .await
                }
                _ => {
                    let path = file.ok_or_else(|| AppError::msg("Choose a playlist file."))?;
                    let n = session::test_m3u_file(path.clone()).await?;
                    blocking(Box::new(move || {
                        let p = match edit {
                            Some(id) => store.profiles().update(
                                id,
                                ProfileUpdate {
                                    name: Some(name.clone()),
                                    file_path: Some(path),
                                    settings: Some(settings),
                                    ..ProfileUpdate::default()
                                },
                            )?,
                            None => store.profiles().create(NewProfile {
                                name: name.clone(),
                                source: NewProfileSource::M3uFile { path },
                                settings,
                            })?,
                        };
                        Ok(Saved { id: p.id, name, note: Some(format!("{n} entries found.")) })
                    }))
                    .await
                }
            }
        });
    }

    fn pick_file(&mut self, cx: &AppCtx<'_>) {
        let (tx, rx) = mpsc::channel();
        let ctx = cx.egui.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Choose a playlist")
                .add_filter("Playlists", &["m3u", "m3u8", "txt"])
                .pick_file();
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.file_rx = Some(rx);
    }

    fn convert_to_xtream(&mut self) {
        if let Some(link) = detect_xtream(&self.m3u_url) {
            self.mode = 0;
            self.server = link.server.to_string();
            self.username = link.username.clone();
            self.password = link.password.expose_secret().to_owned();
            self.m3u_url.clear();
            self.errors = Errors::default();
        }
    }

    fn form(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        let busy = self.submit.is_loading();
        let editing = self.edit.is_some();
        // Kind (fixed when editing).
        if !editing {
            let mut mode = self.mode;
            Segmented::new(Id::unique("onb.mode"), &["Xtream", "M3U playlist"], COL_W)
                .show(ui, &mut mode);
            if mode != self.mode {
                self.mode = mode;
                self.errors = Errors::default();
            }
            ui.add_space(24.0);
        }
        let first_empty = self.name.is_empty();
        let mut next_focus: Option<Id> = None;
        let r = TextField::new(Id::unique("onb.name"), "Name")
            .placeholder("Living room")
            .error(self.errors.name.as_deref())
            .autofocus(first_empty)
            .char_limit(128)
            .show(ui, &mut self.name);
        let name_te = r.focus.id;
        if r.submitted {
            next_focus = Some(
                Id::unique(if self.mode == 0 {
                    "onb.server"
                } else {
                    "onb.m3u"
                })
                .with("te"),
            );
        }
        let _ = name_te;
        ui.add_space(16.0);
        if self.mode == 0 {
            let r = TextField::new(Id::unique("onb.server"), "Server URL")
                .placeholder("http://provider.example:8080")
                .error(self.errors.server.as_deref())
                .show(ui, &mut self.server);
            if r.submitted {
                next_focus = Some(Id::unique("onb.user").with("te"));
            }
            ui.add_space(16.0);
            let r = TextField::new(Id::unique("onb.user"), "Username")
                .error(self.errors.username.as_deref())
                .char_limit(256)
                .show(ui, &mut self.username);
            if r.submitted {
                next_focus = Some(Id::unique("onb.pass").with("te"));
            }
            ui.add_space(16.0);
            let r = TextField::new(Id::unique("onb.pass"), "Password")
                .placeholder(if editing { "Unchanged" } else { "" })
                .password(true)
                .error(self.errors.password.as_deref())
                .char_limit(1024)
                .show(ui, &mut self.password);
            if r.submitted {
                self.submit(cx);
            }
        } else {
            let mut m = self.m3u_mode;
            Segmented::new(Id::unique("onb.m3u-mode"), &["Playlist URL", "File"], COL_W)
                .show(ui, &mut m);
            self.m3u_mode = m;
            ui.add_space(16.0);
            if self.m3u_mode == 0 {
                let r = TextField::new(Id::unique("onb.m3u"), "Playlist URL")
                    .placeholder(if editing {
                        "Unchanged"
                    } else {
                        "https://provider.example/playlist.m3u"
                    })
                    .error(self.errors.m3u.as_deref())
                    .password(editing)
                    .show(ui, &mut self.m3u_url);
                if r.submitted {
                    self.submit(cx);
                }
                if detect_xtream(&self.m3u_url).is_some() {
                    ui.add_space(12.0);
                    Callout::new(
                        CalloutKind::Info,
                        "This is an Xtream Codes link.",
                        "Sign in with Xtream instead to get movies, series, the guide and catch-up.",
                    )
                    .width(COL_W)
                    .show(ui);
                    ui.add_space(8.0);
                    if Button::secondary("Use Xtream login")
                        .icon(icons::ARROWS_CLOCKWISE)
                        .id(Id::unique("onb.convert"))
                        .show(ui)
                        .activated
                    {
                        self.convert_to_xtream();
                    }
                }
            } else {
                let label = self
                    .m3u_file
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or_else(
                        || "Choose file…".to_owned(),
                        |n| text::cap(&n.to_string_lossy(), 60).into_owned(),
                    );
                if Button::secondary(label)
                    .icon(icons::FILE_ARROW_UP)
                    .width(COL_W)
                    .id(Id::unique("onb.file"))
                    .show(ui)
                    .activated
                    && self.file_rx.is_none()
                {
                    self.pick_file(cx);
                }
                if let Some(e) = &self.errors.m3u {
                    ui.colored_label(color::DANGER, egui::RichText::new(e).font(font::caption()));
                }
            }
        }

        if self.is_http() {
            ui.add_space(16.0);
            Callout::unencrypted().width(COL_W).show(ui);
        }

        // Advanced (per-profile settings).
        ui.add_space(16.0);
        let adv_label = if self.advanced {
            "Hide advanced settings"
        } else {
            "Advanced settings"
        };
        if Button::ghost(adv_label)
            .icon(if self.advanced {
                icons::CARET_UP
            } else {
                icons::CARET_DOWN
            })
            .small()
            .id(Id::unique("onb.advanced"))
            .show(ui)
            .activated
        {
            self.advanced = !self.advanced;
        }
        if self.advanced {
            ui.add_space(8.0);
            caption(ui, "User-Agent sent to the provider and streams");
            Segmented::new(
                Id::unique("onb.ua"),
                &["opensmarters", "Smarters-compatible", "Custom"],
                COL_W,
            )
            .show(ui, &mut self.ua);
            if self.ua == 2 {
                ui.add_space(8.0);
                TextField::new(Id::unique("onb.ua-custom"), "")
                    .placeholder("MyPlayer/1.0")
                    .error(self.errors.ua.as_deref())
                    .char_limit(512)
                    .show(ui, &mut self.custom_ua);
            }
            ui.add_space(12.0);
            caption(ui, "Live TV stream format (if your provider allows it)");
            Segmented::new(
                Id::unique("onb.container"),
                &["MPEG-TS (.ts)", "HLS (.m3u8)"],
                COL_W,
            )
            .show(ui, &mut self.container);
            ui.add_space(12.0);
            caption(ui, "Catch-up time format");
            Segmented::new(
                Id::unique("onb.timeshift"),
                &["Standard", "Smarters-compatible"],
                COL_W,
            )
            .show(ui, &mut self.timeshift);
        }

        if let Some(e) = &self.form_error {
            ui.add_space(16.0);
            Callout::new(
                CalloutKind::Danger,
                format!("{}.", e.title()),
                e.user_message(),
            )
            .width(COL_W)
            .show(ui);
        }

        ui.add_space(20.0);
        let label = if busy {
            "Checking…".to_owned()
        } else if editing {
            "Save".to_owned()
        } else {
            "Sign in".to_owned()
        };
        let f = Button::primary(label)
            .trailing(icons::CARET_RIGHT)
            .large()
            .width(COL_W)
            .enabled(!busy)
            .id(Id::unique("onb.submit"))
            .autofocus_if(!first_empty)
            .show(ui);
        if f.activated {
            self.submit(cx);
        }
        if busy {
            let r = ui.min_rect();
            paint::spinner(
                ui.painter(),
                pos2(r.left() + COL_W - 30.0, r.bottom() - 26.0),
                10.0,
                ui.input(|i| i.time),
                2.0,
            );
            ui.ctx().request_repaint();
        }
        ui.add_space(14.0);
        let privacy = if cx.services.ephemeral {
            "Ephemeral session: nothing is saved to disk or to the system keychain."
        } else if cfg!(windows) {
            "Passwords are stored in Windows Credential Manager. No tracking, ever."
        } else {
            "Passwords are stored in the system keychain. No tracking, ever."
        };
        let g = text::galley(
            ui.painter(),
            privacy,
            font::caption(),
            color::TEXT_3,
            COL_W - 60.0,
            3,
        );
        let (r, _) = ui.allocate_exact_size(vec2(COL_W, g.size().y), egui::Sense::hover());
        ui.painter().galley(r.min, g, color::TEXT_3);
        if let Some(id) = next_focus {
            focus::request(ui.ctx(), id);
        }
    }
}

fn caption(ui: &mut Ui, s: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(COL_W, 20.0), egui::Sense::hover());
    ui.painter().text(
        r.left_center(),
        Align2::LEFT_CENTER,
        s,
        font::caption(),
        color::TEXT_2,
    );
}

impl Screen for OnboardingScreen {
    fn on_enter(&mut self, cx: &mut AppCtx<'_>) {
        if let Some(id) = self.edit
            && self.existing.is_idle()
        {
            let store = cx.services.store.clone();
            self.existing
                .start_blocking(cx.rt(), &cx.egui, move || Ok(store.profiles().get(id)?));
        }
    }

    fn chrome(&self) -> Chrome {
        Chrome::Fullscreen
    }

    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.existing.poll();
        if !self.prefilled
            && let Some(Some(p)) = self.existing.value().cloned()
        {
            self.prefill(&p);
            self.prefilled = true;
        }
        if let Some(rx) = &self.file_rx
            && let Ok(picked) = rx.try_recv()
        {
            if let Some(p) = picked {
                self.m3u_file = Some(p);
                self.errors.m3u = None;
            }
            self.file_rx = None;
        }
        if self.submit.poll() {
            if let Some(saved) = self.submit.take() {
                if let Some(note) = &saved.note {
                    cx.toasts.success(note.clone());
                }
                let services = cx.services.clone();
                cx.sessions.open(&services, saved.id, saved.name);
            } else if let Some(e) = self.submit.error() {
                self.form_error = Some(e.clone());
            }
        }

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
        let col = Rect::from_min_max(
            pos2(left - 12.0, rect.top()),
            pos2(left + COL_W + 12.0, rect.bottom()),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(col));
        let content_h_id = Id::unique("onb.content-h");
        let last_h: f32 = ui.ctx().data(|d| d.get_temp(content_h_id)).unwrap_or(900.0);
        ScrollArea::vertical()
            .id_salt("onb.scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .content_margin(egui::Margin::symmetric(12, 0))
            .show(&mut child, |ui| {
                let top_pad = ((rect.height() - last_h) / 2.0).max(40.0);
                ui.add_space(top_pad);
                let start = ui.cursor().top();
                ui.spacing_mut().item_spacing.y = 0.0;
                // Wordmark (36 pt) and headline.
                let wm_h = 36.0;
                let (wr, _) = ui.allocate_exact_size(
                    vec2(wm_h * assets::aspect(Asset::Wordmark), wm_h),
                    egui::Sense::hover(),
                );
                assets::paint_exact(ui, ui.painter(), Asset::Wordmark, wr, Color32::WHITE);
                ui.add_space(32.0);
                let headline = if self.edit.is_some() {
                    "Edit your\naccount."
                } else {
                    "Make room\nfor good stories."
                };
                let g = ui.painter().layout(
                    headline.to_owned(),
                    font::hero(),
                    color::TEXT,
                    COL_W + 200.0,
                );
                let (hr, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
                ui.painter().galley(hr.min, g, color::TEXT);
                ui.add_space(4.0);
                let sub = if self.edit.is_some() {
                    "Change the details and sign in again."
                } else {
                    "Your channels and stories, in one quiet place."
                };
                let (sr, _) = ui.allocate_exact_size(vec2(COL_W, 24.0), egui::Sense::hover());
                ui.painter().text(
                    sr.left_center(),
                    Align2::LEFT_CENTER,
                    sub,
                    font::body(),
                    color::TEXT_2,
                );
                ui.add_space(32.0);
                focus::scope(ui, Id::unique("onb.form"), |ui| self.form(cx, ui));
                ui.add_space(40.0);
                let h = ui.cursor().top() - start;
                ui.ctx().data_mut(|d| d.insert_temp(content_h_id, h));
            });
    }

    fn on_back(&mut self, cx: &mut AppCtx<'_>) -> bool {
        if self.submit.is_loading() {
            self.submit.cancel();
            return true;
        }
        let _ = cx;
        false
    }
}

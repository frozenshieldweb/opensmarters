// SPDX-License-Identifier: GPL-3.0-or-later
//! The installer window (LAUNCH_PROMPT §B3–B4, B9): 720 × 460 pt, not resizable, no OS
//! title bar (a custom header with the symbol, a drag region and a close button), dark.
//!
//! Layout: the content **card** (`surface-1`, `e3`) on the left; the Codex art panel
//! (`assets/launch/installer/panel@{1x,2x}.jpg`) bleeding to the right edge, with the
//! open-corners symbol over it. The symbol is the progress: it fills with champagne light
//! along its diagonal while the installer works ([`symbol`]); with reduced motion it shows
//! a plain bar inside its frame.
//!
//! Screens ([`Screen`]): the start screen (Install, Update, Repair/Uninstall or the
//! downgrade question, from [`crate::run::survey`]), Options, the licence, "opensmarters is
//! open", working (install/update/repair or remove), Done, Remove?, Removed, Error. Work
//! runs on a worker thread; the UI polls it every frame.

#[cfg(feature = "dev")]
mod harness;
pub mod symbol;
mod widgets;

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eframe::egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Id, Key, Modifiers, Pos2, Rect,
    TextureHandle, TextureOptions, Ui, UiBuilder, Vec2, pos2, text::LayoutJob, vec2,
};

use afterglow::icons::{self, Style};
use afterglow::system_motion::SystemMotion;
use afterglow::theme::{self, color, elevation, font, motion, radius};
use afterglow::{motion as anim, paint};

use crate::brand;
use crate::cli::{Cli, EXIT_CANCELLED, EXIT_ERROR, EXIT_OK};
use crate::env::Environment;
use crate::install::{self, Control, InstallError, Phase};
use crate::payload::Payload;
use crate::run::{self, Pending, Situation};
use crate::uninstall::{self, UninstallError};
use crate::version::Plan;
use widgets::Kind;

/// Logical window size (points).
const WIN: Vec2 = vec2(720.0, 460.0);
/// The art panel's width (it bleeds to the right edge).
const PANEL_W: f32 = 300.0;
/// Header (drag region) height.
const HEADER_H: f32 = 48.0;
/// The content card.
const CARD: Rect = Rect {
    min: pos2(20.0, 52.0),
    max: pos2(448.0, 440.0),
};
/// Padding inside the card.
const PAD: f32 = 36.0;
/// The symbol over the art: centre and width.
const SYMBOL_CENTER: Pos2 = pos2(584.0, 238.0);
const SYMBOL_W: f32 = 132.0;
/// The ready illustration's size on Done.
const READY_PT: f32 = 160.0;
/// Family name of the live wordmark face (Fraunces at opsz 144).
const WORDMARK: &str = "wordmark";

const PANEL_1X: &[u8] = include_bytes!("../../../../assets/launch/installer/panel@1x.jpg");
const PANEL_2X: &[u8] = include_bytes!("../../../../assets/launch/installer/panel@2x.jpg");
const READY: &[u8] = include_bytes!("../../../../assets/launch/installer/ready.png");
const ICON: &[u8] = include_bytes!("../../../../assets/icon/icon-256.png");
const LICENCE: &str = include_str!("../../../../LICENSE");

/// Everything the window needs.
#[derive(Debug)]
pub struct Config {
    /// The command line (options preset, dev flags).
    pub cli: Cli,
    /// Where to work.
    pub env: Environment,
    /// What to install.
    pub payload: Payload<'static>,
    /// This program (copied as `uninstall.exe`).
    pub self_exe: PathBuf,
    /// The log file, if logging works.
    pub log: Option<PathBuf>,
}

/// What the user is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Install, update or repair.
    Install,
    /// Uninstall.
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
enum Screen {
    /// Install / Update / Repair-or-Uninstall / downgrade question (from the plan).
    Start,
    /// Install location and shortcuts.
    Options,
    /// The GPL text.
    Licence,
    /// The app is open; waiting for it to close.
    Running(Action),
    /// Work in progress.
    Working(Action),
    /// Installed.
    Done {
        app_exe: PathBuf,
        leftover: Option<PathBuf>,
    },
    /// "Remove opensmarters?"
    Remove,
    /// Removed.
    Removed(uninstall::Outcome),
    /// Something failed.
    Error {
        message: String,
        details: String,
        retry: Action,
    },
}

enum WorkResult {
    Installed(Result<install::Outcome, InstallError>),
    Removed(Result<uninstall::Outcome, UninstallError>),
}

struct Worker {
    ctl: Control,
    rx: mpsc::Receiver<WorkResult>,
    asking_cancel: bool,
}

struct Art {
    panel_1x: TextureHandle,
    panel_2x: TextureHandle,
    ready: TextureHandle,
}

fn texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(
        name,
        color,
        TextureOptions {
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::LINEAR
        },
    ))
}

/// The shared result of the window: exit code and files to delete after the parent left.
type Shared = Arc<Mutex<(i32, Option<Pending>)>>;

/// Opens the window and runs until it closes. Returns the exit code (and, for the
/// uninstaller's temporary copy, what is left to delete).
pub fn run(cfg: Config) -> (i32, Option<Pending>) {
    let shared: Shared = Arc::new(Mutex::new((EXIT_CANCELLED, None)));
    let icon = eframe::icon_data::from_png_bytes(ICON).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("{} setup", brand::PRODUCT))
        .with_app_id("opensmarters-setup")
        .with_inner_size(WIN)
        .with_resizable(false)
        .with_maximize_button(false)
        .with_decorations(false);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport,
        centered: true,
        ..Default::default()
    };
    let quiet_fallback = (
        cfg.cli.clone(),
        cfg.env.clone(),
        cfg.payload.clone(),
        cfg.self_exe.clone(),
    );
    let s2 = Arc::clone(&shared);
    let result = eframe::run_native(
        "opensmarters-setup",
        options,
        Box::new(move |cc| Ok(Box::new(SetupApp::new(cc, cfg, s2)))),
    );
    if let Err(e) = result {
        // No usable OpenGL (a VM without a graphics driver, a remote session…): offer the
        // same install without a window.
        tracing::error!(error = %e, "could not open the setup window");
        let (cli, env, payload, self_exe) = quiet_fallback;
        let question = if cli.uninstall {
            "Setup couldn't open its window. Remove opensmarters now? Your data and saved passwords stay."
        } else {
            "Setup couldn't open its window. Install opensmarters with the default options now?"
        };
        let yes = rfd::MessageDialog::new()
            .set_title(brand::PRODUCT)
            .set_description(question)
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !yes {
            return (EXIT_CANCELLED, None);
        }
        let mut cli = cli;
        cli.purge_data = false;
        cli.purge_passwords = false;
        return run::quiet(&cli, &env, &payload, &self_exe);
    }
    shared
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

struct SetupApp {
    env: Environment,
    payload: Payload<'static>,
    self_exe: PathBuf,
    log: Option<PathBuf>,
    cli: Cli,
    situation: Situation,
    screen: Screen,
    /// Where Back from Options/Licence goes.
    back_to: Screen,
    changed_at: f64,
    forward: bool,
    opts: install::Options,
    open_when_done: bool,
    remove_opts: uninstall::Options,
    options_error: Option<String>,
    note: Option<String>,
    copied: bool,
    worker: Option<Worker>,
    /// The light shown (eased towards the real progress, never backwards).
    light: f32,
    /// Preview mode: a fixed progress and no work.
    preview: Option<f32>,
    preview_phase: Phase,
    system_motion: SystemMotion,
    art: Option<Art>,
    shared: Shared,
    frame: u64,
    focus_default: bool,
    last_poll: f64,
    licence_scroll: f32,
    #[cfg(feature = "dev")]
    harness: Option<harness::Harness>,
}

impl SetupApp {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, shared: Shared) -> Self {
        let ctx = &cc.egui_ctx;
        let mut fonts = theme::font_definitions();
        fonts.font_data.insert(
            WORDMARK.to_owned(),
            theme::variable_face(
                theme::display_font_bytes(),
                &[
                    ("opsz", 144.0),
                    ("wght", 560.0),
                    ("SOFT", 100.0),
                    ("WONK", 0.0),
                ],
            ),
        );
        fonts
            .families
            .insert(FontFamily::Name(WORDMARK.into()), vec![WORDMARK.to_owned()]);
        theme::install(ctx);
        ctx.set_fonts(fonts);
        // egui's own subtle focus outline stays off; ours is the ring.
        ctx.style_mut_of(egui::Theme::Dark, |s| {
            s.visuals.widgets.hovered.expansion = 0.0;
        });
        let art = (|| {
            Some(Art {
                panel_1x: texture(ctx, "panel@1x", PANEL_1X)?,
                panel_2x: texture(ctx, "panel@2x", PANEL_2X)?,
                ready: texture(ctx, "ready", READY)?,
            })
        })();
        let Config {
            cli,
            env,
            payload,
            self_exe,
            log,
        } = cfg;
        let situation = run::survey(&env, &payload);
        let fresh = situation.installed.is_none();
        let opts = install::Options {
            dir: cli.dir.clone().unwrap_or_else(|| situation.dir.clone()),
            start_menu: if fresh {
                cli.start_menu()
            } else {
                !cli.no_shortcuts && (situation.start_menu_exists || !fresh)
            },
            desktop: cli.desktop_shortcut
                || (!fresh && !cli.no_shortcuts && situation.desktop_exists),
        };
        let mut app = Self {
            screen: if cli.uninstall {
                Screen::Remove
            } else {
                Screen::Start
            },
            back_to: Screen::Start,
            changed_at: 0.0,
            forward: true,
            open_when_done: !cli.no_launch,
            remove_opts: uninstall::Options {
                purge_data: cli.purge_data,
                purge_passwords: cli.purge_passwords,
            },
            options_error: None,
            note: None,
            copied: false,
            worker: None,
            light: 0.0,
            preview: None,
            preview_phase: Phase::Unpacking,
            system_motion: SystemMotion::default(),
            art,
            shared,
            frame: 0,
            focus_default: true,
            last_poll: 0.0,
            licence_scroll: 0.0,
            #[cfg(feature = "dev")]
            harness: None,
            env,
            payload,
            self_exe,
            log,
            situation,
            opts,
            cli,
        };
        app.apply_preview();
        #[cfg(feature = "dev")]
        {
            let steps = app
                .cli
                .keys
                .as_deref()
                .map(harness::parse_script)
                .transpose()
                .unwrap_or_else(|e| {
                    tracing::warn!("bad --keys: {e}");
                    None
                })
                .unwrap_or_default();
            let shot = app.cli.screenshot_after.zip(app.cli.screenshot_out.clone());
            if !steps.is_empty() || shot.is_some() || app.cli.exit_after.is_some() {
                app.harness = Some(harness::Harness::new(steps, shot, app.cli.exit_after));
            }
        }
        app
    }

    /// `--preview <screen>[:<progress>]` (dev): show a screen without doing any work.
    fn apply_preview(&mut self) {
        let Some(spec) = self.cli.preview.clone() else {
            return;
        };
        let (name, arg) = spec
            .split_once(':')
            .map_or((spec.as_str(), None), |(n, a)| (n, Some(a)));
        let fraction = arg.and_then(|a| a.parse::<f32>().ok()).unwrap_or(0.6);
        let v = self.payload.manifest.version.clone();
        let dir = self.opts.dir.clone();
        self.preview = Some(fraction);
        self.light = fraction;
        self.screen = match name {
            "options" => {
                self.back_to = Screen::Start;
                Screen::Options
            }
            "licence" => Screen::Licence,
            "update" => {
                self.situation.plan = Plan::Update {
                    from: "0.0.9".into(),
                };
                Screen::Start
            }
            "same" => {
                self.situation.plan = Plan::Same;
                Screen::Start
            }
            "downgrade" => {
                self.situation.plan = Plan::Downgrade {
                    installed: "9.0.0".into(),
                };
                Screen::Start
            }
            "running" => Screen::Running(Action::Install),
            "installing" => {
                self.preview_phase = if fraction < 0.78 {
                    Phase::Unpacking
                } else {
                    Phase::Checking
                };
                Screen::Working(Action::Install)
            }
            "cancel" => {
                self.preview_phase = Phase::Unpacking;
                Screen::Working(Action::Install)
            }
            "removing" => {
                self.preview_phase = Phase::Removing;
                Screen::Working(Action::Remove)
            }
            "done" => Screen::Done {
                app_exe: dir.join(brand::APP_EXE),
                leftover: None,
            },
            "uninstall" => Screen::Remove,
            "removed" => Screen::Removed(uninstall::Outcome {
                dir: Some(dir),
                ..Default::default()
            }),
            "error" => Screen::Error {
                message: InstallError::Io {
                    step: "write",
                    path: dir.clone(),
                    source: std::io::Error::other("preview"),
                }
                .message(),
                details: "preview".into(),
                retry: Action::Install,
            },
            "damaged" => Screen::Error {
                message: InstallError::Damaged("preview".into()).message(),
                details: "preview".into(),
                retry: Action::Install,
            },
            _ => {
                let _ = v;
                Screen::Start
            }
        };
        if name == "cancel" {
            self.worker = Some(Worker {
                ctl: Control::default(),
                rx: mpsc::channel().1,
                asking_cancel: true,
            });
        }
    }

    fn reduced(&self, ctx: &egui::Context) -> bool {
        anim::reduced(ctx)
    }

    fn go(&mut self, ctx: &egui::Context, screen: Screen, forward: bool) {
        self.screen = screen;
        self.changed_at = ctx.input(|i| i.time);
        self.forward = forward;
        self.focus_default = true;
        self.copied = false;
        ctx.memory_mut(|m| {
            if let Some(f) = m.focused() {
                m.surrender_focus(f);
            }
        });
    }

    fn set_exit(&self, code: i32, pending: Option<Pending>) {
        *self.shared.lock().unwrap_or_else(PoisonError::into_inner) = (code, pending);
    }

    fn close(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Starts the work for `action` (after checking that the app is closed).
    fn start(&mut self, ctx: &egui::Context, action: Action) {
        self.note = None;
        if self.preview.is_some() {
            return;
        }
        let installed_dir = self.situation.installed.as_ref().map(|(d, _)| d.clone());
        if let Some(dir) = &installed_dir
            && install::app_running(dir)
        {
            self.go(ctx, Screen::Running(action), true);
            return;
        }
        if action == Action::Install
            && let Err(p) = crate::paths::check_target(&self.opts.dir, &self.env.protected)
        {
            self.options_error = Some(p.to_string());
            self.back_to = Screen::Start;
            self.go(ctx, Screen::Options, true);
            return;
        }
        let ctl = Control {
            slow: self.cli.slow.then(|| Duration::from_millis(12)),
            ..Control::default()
        };
        let (tx, rx) = mpsc::channel();
        let env = self.env.clone();
        let payload = self.payload.clone();
        let self_exe = self.self_exe.clone();
        let opts = self.opts.clone();
        let remove_opts = self.remove_opts;
        let ctl2 = ctl.clone();
        let ctx2 = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("setup-worker".into())
            .spawn(move || {
                let r = match action {
                    Action::Install => WorkResult::Installed(install::install(
                        &env, &payload, &opts, &self_exe, &ctl2,
                    )),
                    Action::Remove => WorkResult::Removed(uninstall::uninstall(
                        &env,
                        remove_opts,
                        &ctl2,
                        &self_exe,
                    )),
                };
                let _ = tx.send(r);
                ctx2.request_repaint();
            });
        if let Err(e) = spawned {
            self.fail(
                ctx,
                "Setup couldn't start working. Try again.".into(),
                e.to_string(),
                action,
            );
            return;
        }
        self.light = 0.0;
        self.worker = Some(Worker {
            ctl,
            rx,
            asking_cancel: false,
        });
        self.go(ctx, Screen::Working(action), true);
    }

    fn fail(&mut self, ctx: &egui::Context, message: String, technical: String, retry: Action) {
        let details = format!(
            "{} setup {}\nWhat happened: {message}\nTechnical details: {technical}\nLog: {}\n",
            brand::PRODUCT,
            brand::VERSION,
            self.log
                .as_ref()
                .map_or_else(|| "(no log)".to_owned(), |p| p.display().to_string()),
        );
        self.set_exit(EXIT_ERROR, None);
        self.go(
            ctx,
            Screen::Error {
                message,
                details,
                retry,
            },
            true,
        );
    }

    /// Polls the worker; moves to the next screen when it is done.
    fn poll_worker(&mut self, ctx: &egui::Context) {
        let Some(worker) = &self.worker else {
            return;
        };
        let Ok(result) = worker.rx.try_recv() else {
            return;
        };
        self.worker = None;
        match result {
            WorkResult::Installed(Ok(outcome)) => {
                self.set_exit(EXIT_OK, None);
                self.situation = run::survey(&self.env, &self.payload);
                self.go(
                    ctx,
                    Screen::Done {
                        app_exe: outcome.app_exe,
                        leftover: outcome.leftovers.into_iter().next(),
                    },
                    true,
                );
            }
            WorkResult::Installed(Err(InstallError::Cancelled)) => {
                self.set_exit(EXIT_CANCELLED, None);
                self.note = Some(InstallError::Cancelled.message());
                self.go(ctx, Screen::Start, false);
            }
            WorkResult::Installed(Err(InstallError::AppRunning)) => {
                self.go(ctx, Screen::Running(Action::Install), true);
            }
            WorkResult::Installed(Err(InstallError::Path(p))) => {
                self.options_error = Some(p.to_string());
                self.back_to = Screen::Start;
                self.go(ctx, Screen::Options, true);
            }
            WorkResult::Installed(Err(e)) => {
                self.fail(ctx, e.message(), e.to_string(), Action::Install);
            }
            WorkResult::Removed(Ok(outcome)) => {
                let pending = outcome.dir.clone().map(|dir| Pending {
                    dir,
                    files: outcome.pending.clone(),
                });
                self.set_exit(EXIT_OK, pending);
                self.go(ctx, Screen::Removed(outcome), true);
            }
            WorkResult::Removed(Err(UninstallError::AppRunning)) => {
                self.go(ctx, Screen::Running(Action::Remove), true);
            }
            WorkResult::Removed(Err(e)) => {
                self.fail(ctx, e.message(), e.to_string(), Action::Remove);
            }
        }
    }

    /// Esc / the close button / Alt+F4, per screen.
    fn back(&mut self, ctx: &egui::Context, closing: bool) {
        match self.screen.clone() {
            Screen::Options | Screen::Licence if !closing => {
                let to = self.back_to.clone();
                self.go(ctx, to, false);
            }
            Screen::Running(_) if !closing => self.go(ctx, Screen::Start, false),
            Screen::Working(Action::Install) => {
                if let Some(w) = &mut self.worker {
                    w.asking_cancel = !w.asking_cancel || closing;
                    self.focus_default = true;
                }
            }
            Screen::Working(Action::Remove) => {}
            _ => self.close(ctx),
        }
    }

    fn working(&self) -> bool {
        matches!(self.screen, Screen::Working(_)) && self.preview.is_none()
    }
}

impl eframe::App for SetupApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw: &mut egui::RawInput) {
        #[cfg(feature = "dev")]
        if let Some(h) = &mut self.harness {
            h.inject(_raw);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = color::BG;
        [
            f32::from(c.r()) / 255.0,
            f32::from(c.g()) / 255.0,
            f32::from(c.b()) / 255.0,
            1.0,
        ]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        if self.frame == 1 {
            self.first_frame(&ctx);
        }
        #[cfg(feature = "dev")]
        if let Some(h) = &mut self.harness {
            h.during_pass(&ctx);
        }
        // Reduced motion: Windows' Animation effects switch (or the dev flag).
        let system = self.system_motion.poll();
        anim::set_reduced(&ctx, self.cli.reduced_motion || system == Some(false));

        self.poll_worker(&ctx);
        let now = ctx.input(|i| i.time);
        if let Screen::Running(action) = self.screen
            && now - self.last_poll > 1.0
            && self.preview.is_none()
        {
            self.last_poll = now;
            let still = self
                .situation
                .installed
                .as_ref()
                .is_some_and(|(d, _)| install::app_running(d));
            if !still {
                self.start(&ctx, action);
            }
        }
        if let Screen::Running(_) = self.screen {
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        // Closing the window (Alt+F4, taskbar): never in the middle of the work.
        if ctx.input(|i| i.viewport().close_requested()) && self.working() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.back(&ctx, true);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.back(&ctx, false);
        }

        // The light follows the real progress, eased, never backwards.
        let (phase, target) = match (&self.worker, self.preview) {
            (_, Some(p)) => (self.preview_phase, p),
            (Some(w), None) => w.ctl.progress.get(),
            (None, None) => (
                Phase::Finishing,
                if matches!(self.screen, Screen::Done { .. }) {
                    1.0
                } else {
                    self.light
                },
            ),
        };
        if phase == Phase::RollingBack {
            self.light = self.light.min(target.max(0.0));
        } else if self.reduced(&ctx) {
            self.light = self.light.max(target);
        } else {
            let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
            let eased = self.light + (target - self.light) * (1.0 - (-dt * 7.0).exp());
            self.light = self.light.max(eased).min(1.0);
            if (target - self.light).abs() > 0.001 {
                ctx.request_repaint();
            }
        }
        if self.working() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }

        self.paint_frame(ui, phase);
    }
}

impl SetupApp {
    fn first_frame(&mut self, ctx: &egui::Context) {
        #[cfg(feature = "dev")]
        if let Some(ppp) = self.cli.ppp {
            let native = ctx
                .input(|i| i.viewport().native_pixels_per_point)
                .unwrap_or(1.0)
                .max(0.25);
            let ppp = ppp.clamp(0.5, 4.0);
            ctx.set_zoom_factor(ppp / native);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(WIN * ppp / native));
        }
        // Without OS decorations Windows draws no shadow; ask for the undecorated one.
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn paint_frame(&mut self, ui: &mut Ui, phase: Phase) {
        let ctx = ui.ctx().clone();
        let full = Rect::from_min_size(Pos2::ZERO, WIN);
        let painter = ui.painter().clone();
        painter.rect_filled(full, CornerRadius::ZERO, color::BG);

        // The art panel, bleeding to the edges; dimmed while working so the symbol is the
        // one light.
        let panel = Rect::from_min_max(pos2(WIN.x - PANEL_W, 0.0), pos2(WIN.x, WIN.y));
        if let Some(art) = &self.art {
            let tex = if ctx.pixels_per_point() >= 1.5 {
                &art.panel_2x
            } else {
                &art.panel_1x
            };
            painter.image(
                tex.id(),
                panel,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let working =
            matches!(self.screen, Screen::Working(_)) || matches!(self.screen, Screen::Running(_));
        let dim = anim::animate_bool(&ctx, Id::unique("panel-dim"), working, motion::SLOW);
        if dim > 0.0 {
            painter.rect_filled(panel, CornerRadius::ZERO, paint::bg_alpha(0.55 * dim));
        }
        // The symbol: at rest, filling with light, or full.
        let (rest, lit) = match &self.screen {
            Screen::Working(Action::Install) => (0.5, self.light),
            Screen::Working(Action::Remove) => (0.5, 1.0 - self.light),
            Screen::Done { .. } => (0.5, 1.0),
            _ => (0.6, 0.0),
        };
        let frame = symbol::frame(SYMBOL_CENTER, SYMBOL_W);
        if anim::reduced(&ctx) && matches!(self.screen, Screen::Working(_)) {
            let f = if matches!(self.screen, Screen::Working(Action::Remove)) {
                1.0 - self.light
            } else {
                self.light
            };
            symbol::paint_with_bar(&painter, frame, rest, f, 1.0);
        } else {
            symbol::paint(
                &painter,
                frame,
                symbol::Look {
                    rest,
                    lit,
                    glow: true,
                    opacity: 1.0,
                },
            );
        }

        // The card.
        paint::shadow(&painter, CARD, radius::LG, elevation::E3);
        painter.rect_filled(CARD, CornerRadius::same(radius::LG), color::SURFACE_1);
        painter.rect_stroke(
            CARD,
            CornerRadius::same(radius::LG),
            egui::Stroke::new(1.0, color::STROKE_SUBTLE),
            egui::StrokeKind::Inside,
        );

        // Header: drag region, symbol mark, close button.
        let header = Rect::from_min_size(Pos2::ZERO, vec2(WIN.x, HEADER_H));
        let drag = ui.interact(header, Id::unique("header-drag"), egui::Sense::DRAG);
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let mark = symbol::frame(pos2(CARD.left() + 15.0, HEADER_H / 2.0 + 1.0), 22.0);
        symbol::paint(
            &painter,
            mark,
            symbol::Look {
                rest: 0.0,
                lit: 1.0,
                glow: false,
                opacity: 1.0,
            },
        );
        widgets::text(
            ui,
            pos2(mark.right() + 10.0, HEADER_H / 2.0 + 1.0),
            Align2::LEFT_CENTER,
            "Setup",
            font::caption(),
            color::TEXT_3,
        );
        let close_enabled = !matches!(self.screen, Screen::Working(Action::Remove));
        if close_enabled
            && widgets::close_button(
                ui,
                Id::unique("close"),
                pos2(WIN.x - 30.0, HEADER_H / 2.0 + 2.0),
            )
            .clicked()
        {
            self.back(&ctx, true);
        }

        // Screen content, sliding/fading in (240 ms; reduced motion: a 100 ms fade).
        let now = ctx.input(|i| i.time);
        let reduced = anim::reduced(&ctx);
        let dur = if reduced {
            0.1
        } else {
            f64::from(motion::SLOW)
        };
        let t = (((now - self.changed_at) / dur).clamp(0.0, 1.0)) as f32;
        let e = egui::emath::easing::cubic_out(t);
        if t < 1.0 {
            ctx.request_repaint();
        }
        let travel = if reduced { 0.0 } else { 24.0 * (1.0 - e) };
        let dx = if self.forward { travel } else { -travel };
        let content = CARD.shrink(PAD).translate(vec2(dx, 0.0));
        let mut child = ui.new_child(UiBuilder::new().max_rect(CARD.expand(8.0)));
        child.set_clip_rect(CARD.expand(8.0));
        child.set_opacity(e.max(0.0));
        self.screen_ui(&mut child, content, phase);
    }

    /// Requests focus for `id` once after a screen change, and whenever nothing has focus.
    fn default_focus(&mut self, ui: &Ui, resp: &egui::Response) {
        let nothing = ui.ctx().memory(|m| m.focused().is_none());
        if self.focus_default || nothing {
            resp.request_focus();
            self.focus_default = false;
        }
    }

    fn screen_ui(&mut self, ui: &mut Ui, r: Rect, phase: Phase) {
        match self.screen.clone() {
            Screen::Start => self.start_ui(ui, r),
            Screen::Options => self.options_ui(ui, r),
            Screen::Licence => self.licence_ui(ui, r),
            Screen::Running(action) => self.running_ui(ui, r, action),
            Screen::Working(action) => self.working_ui(ui, r, action, phase),
            Screen::Done { app_exe, leftover } => {
                self.done_ui(ui, r, &app_exe, leftover.as_deref())
            }
            Screen::Remove => self.remove_ui(ui, r),
            Screen::Removed(outcome) => self.removed_ui(ui, r, &outcome),
            Screen::Error {
                message,
                details,
                retry,
            } => self.error_ui(ui, r, &message, &details, retry),
        }
    }

    // ---- building blocks ------------------------------------------------------------

    /// The live wordmark (Fraunces opsz 144, wght 560, SOFT 100, WONK 0, tracking −1%).
    fn wordmark(ui: &Ui, pos: Pos2, size: f32) -> Rect {
        let mut job = LayoutJob::default();
        job.append(
            brand::PRODUCT,
            0.0,
            egui::TextFormat {
                font_id: FontId::new(size, FontFamily::Name(WORDMARK.into())),
                color: color::CREAM,
                extra_letter_spacing: -0.01 * size,
                ..Default::default()
            },
        );
        let g = ui.painter().layout_job(job);
        let rect = Rect::from_min_size(pos, g.size());
        ui.painter().galley(pos, g, color::CREAM);
        rect
    }

    fn title_height(ui: &Ui, width: f32, text: &str) -> f32 {
        ui.painter()
            .layout(
                text.to_owned(),
                font::display_sized(32.0),
                color::TEXT,
                width,
            )
            .size()
            .y
    }

    fn body_height(ui: &Ui, width: f32, text: &str) -> f32 {
        ui.painter()
            .layout(text.to_owned(), font::body(), color::TEXT_2, width)
            .size()
            .y
    }

    /// Top of a block of `height` placed between `top` and `bottom`, a little above the
    /// middle (where the eye rests).
    fn centred(top: f32, bottom: f32, height: f32) -> f32 {
        (top + (bottom - top - height) * 0.42).max(top).round()
    }

    /// Screen title (Fraunces), wrapped to the content width. Returns its bottom.
    fn title(ui: &Ui, r: Rect, y: f32, text: &str) -> f32 {
        widgets::wrapped(
            ui,
            pos2(r.left(), y),
            r.width(),
            text,
            font::display_sized(32.0),
            color::TEXT,
        )
        .bottom()
    }

    /// Body text in `text-2`, wrapped. Returns its bottom.
    fn body(ui: &Ui, r: Rect, y: f32, text: &str) -> f32 {
        widgets::wrapped(
            ui,
            pos2(r.left(), y),
            r.width(),
            text,
            font::body(),
            color::TEXT_2,
        )
        .bottom()
    }

    /// A row of buttons from the left at `y`; returns which was clicked. The first is
    /// focused by default.
    fn buttons(&mut self, ui: &mut Ui, r: Rect, y: f32, items: &[(&str, Kind)]) -> Option<usize> {
        let mut x = r.left();
        let mut clicked = None;
        for (i, (label, kind)) in items.iter().enumerate() {
            let large = *kind == Kind::Primary;
            let size = widgets::button_size(ui, label, large);
            let top = if large { y } else { y + 4.0 };
            let rect = Rect::from_min_size(pos2(x, top), size);
            let resp = widgets::button(ui, Id::unique(("btn", *label)), rect, label, *kind, true);
            if i == 0 {
                self.default_focus(ui, &resp);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            x = rect.right() + 12.0;
        }
        clicked
    }

    fn version(&self) -> String {
        self.payload.manifest.version.clone()
    }

    fn size_mb(&self) -> u64 {
        (self.payload.manifest.installed_size as f64 / (1024.0 * 1024.0)).round() as u64
    }

    // ---- screens ----------------------------------------------------------------------

    fn start_ui(&mut self, ui: &mut Ui, r: Rect) {
        let ctx = ui.ctx().clone();
        let v = self.version();
        let wm = Self::wordmark(ui, pos2(r.left(), r.top() - 6.0), 24.0);
        let area_top = wm.bottom() + 8.0;
        let area_bottom = r.bottom() - 24.0;
        match self.situation.plan.clone() {
            Plan::Fresh | Plan::Update { .. } => {
                let fresh = self.situation.plan == Plan::Fresh;
                let title = if fresh {
                    "Install opensmarters".to_owned()
                } else {
                    format!("Update opensmarters to {v}")
                };
                let line = if fresh {
                    format!(
                        "Version {v} · about {} MB · no admin rights needed",
                        self.size_mb()
                    )
                } else {
                    "Your accounts, favourites and settings stay as they are.".to_owned()
                };
                let h = Self::title_height(ui, r.width(), &title)
                    + 8.0
                    + Self::body_height(ui, r.width(), &line)
                    + 32.0
                    + 52.0;
                let top = Self::centred(area_top, area_bottom, h);
                let mut y = Self::title(ui, r, top, &title) + 8.0;
                y = Self::body(ui, r, y, &line) + 32.0;
                let primary = if fresh { "Install" } else { "Update" };
                match self.buttons(
                    ui,
                    r,
                    y,
                    &[(primary, Kind::Primary), ("Options", Kind::Ghost)],
                ) {
                    Some(0) => self.start(&ctx, Action::Install),
                    Some(_) => {
                        self.back_to = Screen::Start;
                        self.go(&ctx, Screen::Options, true);
                    }
                    None => {}
                }
                if let Some(note) = &self.note {
                    widgets::text(
                        ui,
                        pos2(r.left(), y + 52.0 + 18.0),
                        Align2::LEFT_TOP,
                        note,
                        font::caption(),
                        color::TEXT_3,
                    );
                }
            }
            Plan::Same => {
                let title = format!("opensmarters {v} is installed");
                let line = "Repair checks every file and puts back any that changed. Your accounts and settings stay as they are.";
                let h = Self::title_height(ui, r.width(), &title)
                    + 8.0
                    + Self::body_height(ui, r.width(), line)
                    + 28.0
                    + 52.0;
                let top = Self::centred(area_top, area_bottom, h);
                let mut y = Self::title(ui, r, top, &title) + 8.0;
                y = Self::body(ui, r, y, line) + 28.0;
                match self.buttons(
                    ui,
                    r,
                    y,
                    &[("Repair", Kind::Primary), ("Uninstall", Kind::Secondary)],
                ) {
                    Some(0) => self.start(&ctx, Action::Install),
                    Some(_) => self.go(&ctx, Screen::Remove, true),
                    None => {}
                }
            }
            Plan::Downgrade { installed } => {
                let title = "A newer version is installed";
                let line = format!(
                    "This PC has opensmarters {installed}. This installer has {v}, which is older. Your data stays as it is either way."
                );
                let h = Self::title_height(ui, r.width(), title)
                    + 8.0
                    + Self::body_height(ui, r.width(), &line)
                    + 28.0
                    + 52.0;
                let top = Self::centred(area_top, area_bottom, h);
                let mut y = Self::title(ui, r, top, title) + 8.0;
                y = Self::body(ui, r, y, &line) + 28.0;
                let keep = format!("Keep {installed}");
                let older = format!("Install {v}");
                match self.buttons(
                    ui,
                    r,
                    y,
                    &[(&keep, Kind::Primary), (&older, Kind::Secondary)],
                ) {
                    Some(0) => self.close(&ctx),
                    Some(_) => self.start(&ctx, Action::Install),
                    None => {}
                }
            }
        }
        // Licence line at the bottom of the card.
        let y = r.bottom() - 8.0;
        let lead = "Free software under GPL-3.0 · ";
        let lr = widgets::text(
            ui,
            pos2(r.left(), y),
            Align2::LEFT_CENTER,
            lead,
            font::caption(),
            color::TEXT_3,
        );
        if widgets::link(
            ui,
            Id::unique("licence-link"),
            pos2(lr.right(), y),
            "View licence",
            font::caption(),
        )
        .clicked()
        {
            self.back_to = Screen::Start;
            self.licence_scroll = 0.0;
            self.go(&ctx, Screen::Licence, true);
        }
    }

    fn options_ui(&mut self, ui: &mut Ui, r: Rect) {
        let ctx = ui.ctx().clone();
        let mut y = Self::title(ui, r, r.top() - 4.0, "Options") + 18.0;
        widgets::text(
            ui,
            pos2(r.left(), y),
            Align2::LEFT_TOP,
            "Install location",
            font::caption(),
            color::TEXT_3,
        );
        y += 22.0;
        let installed = self.situation.installed.is_some();
        let change_w = if installed {
            0.0
        } else {
            widgets::button_size(ui, "Change…", false).x + 12.0
        };
        let shown = widgets::ellipsize_middle(
            ui,
            &self.opts.dir.display().to_string(),
            &font::body(),
            r.width() - change_w,
        );
        widgets::text(
            ui,
            pos2(r.left(), y + 22.0),
            Align2::LEFT_CENTER,
            &shown,
            font::body(),
            color::TEXT,
        );
        if !installed {
            let rect = Rect::from_min_size(
                pos2(r.right() - change_w + 12.0, y),
                widgets::button_size(ui, "Change…", false),
            );
            if widgets::button(
                ui,
                Id::unique("change"),
                rect,
                "Change…",
                Kind::Secondary,
                true,
            )
            .clicked()
            {
                self.pick_folder();
            }
        }
        y += 50.0;
        let hint = match (&self.options_error, installed) {
            (Some(e), _) => Some((e.clone(), color::DANGER)),
            (None, true) => Some((
                "To move opensmarters, uninstall it first.".to_owned(),
                color::TEXT_3,
            )),
            (None, false) => None,
        };
        if let Some((h, c)) = hint {
            if c == color::DANGER {
                icons::paint(
                    ui.painter(),
                    pos2(r.left() + 8.0, y + 9.0),
                    icons::WARNING_CIRCLE,
                    16.0,
                    Style::Regular,
                    c,
                );
                y = widgets::wrapped(
                    ui,
                    pos2(r.left() + 22.0, y),
                    r.width() - 22.0,
                    &h,
                    font::caption(),
                    c,
                )
                .bottom()
                    + 10.0;
            } else {
                y = widgets::wrapped(ui, pos2(r.left(), y), r.width(), &h, font::caption(), c)
                    .bottom()
                    + 10.0;
            }
        }
        y += 4.0;
        let row_w = r.width() + 16.0;
        let mut rows: [(&str, &str, bool); 3] = [
            ("opt-start", "Start menu shortcut", self.opts.start_menu),
            ("opt-desktop", "Desktop shortcut", self.opts.desktop),
            (
                "opt-open",
                "Open opensmarters when done",
                self.open_when_done,
            ),
        ];
        for (i, (id, label, value)) in rows.iter_mut().enumerate() {
            let rect = Rect::from_min_size(pos2(r.left() - 12.0, y), vec2(row_w, 40.0));
            let resp = widgets::checkbox_row(ui, Id::unique(*id), rect, label, value);
            if i == 0 {
                self.default_focus(ui, &resp);
            }
            y += 44.0;
        }
        self.opts.start_menu = rows[0].2;
        self.opts.desktop = rows[1].2;
        self.open_when_done = rows[2].2;
        let back_y = r.bottom() - 44.0;
        let rect = Rect::from_min_size(
            pos2(r.left(), back_y),
            widgets::button_size(ui, "Back", false),
        );
        if widgets::button(ui, Id::unique("back"), rect, "Back", Kind::Secondary, true).clicked() {
            let to = self.back_to.clone();
            self.go(&ctx, to, false);
        }
    }

    fn pick_folder(&mut self) {
        if self.preview.is_some() {
            return;
        }
        let start = self.opts.dir.parent().map_or_else(
            || self.env.default_install_dir.clone(),
            std::path::Path::to_path_buf,
        );
        let Some(chosen) = rfd::FileDialog::new()
            .set_title("Choose where to install opensmarters")
            .set_directory(&start)
            .pick_folder()
        else {
            return;
        };
        match crate::paths::resolve_choice(&chosen, &self.env.protected) {
            Ok(dir) => {
                self.opts.dir = dir;
                self.options_error = None;
            }
            Err(p) => self.options_error = Some(p.to_string()),
        }
    }

    fn licence_ui(&mut self, ui: &mut Ui, r: Rect) {
        let ctx = ui.ctx().clone();
        let mut y = Self::title(ui, r, r.top() - 4.0, "Licence") + 6.0;
        y = widgets::wrapped(
            ui,
            pos2(r.left(), y),
            r.width(),
            "opensmarters is free software under the GNU General Public License, version 3 or later. You can use, study, share and change it.",
            font::caption(),
            color::TEXT_2,
        )
        .bottom()
            + 12.0;
        let back_y = r.bottom() - 44.0;
        let area = Rect::from_min_max(
            pos2(r.left() - 12.0, y),
            pos2(r.right() + 16.0, back_y - 14.0),
        );
        ui.painter()
            .rect_filled(area, CornerRadius::same(radius::SM), color::BG_DEEP);
        // Keyboard scrolling (the Back button keeps the focus).
        let step = ctx.input(|i| {
            let mut d = 0.0;
            if i.key_pressed(Key::ArrowDown) {
                d += 40.0;
            }
            if i.key_pressed(Key::ArrowUp) {
                d -= 40.0;
            }
            if i.key_pressed(Key::PageDown) {
                d += 200.0;
            }
            if i.key_pressed(Key::PageUp) {
                d -= 200.0;
            }
            d
        });
        self.licence_scroll = (self.licence_scroll + step).max(0.0);
        let mut inner = ui.new_child(UiBuilder::new().max_rect(area.shrink2(vec2(12.0, 8.0))));
        let out = egui::ScrollArea::vertical()
            .id_salt("licence")
            .vertical_scroll_offset(self.licence_scroll)
            .auto_shrink([false, false])
            .show(&mut inner, |ui| {
                ui.label(
                    egui::RichText::new(LICENCE)
                        .font(FontId::proportional(13.0))
                        .color(color::TEXT_2),
                );
            });
        self.licence_scroll = out.state.clamped_offset().y;
        let rect = Rect::from_min_size(
            pos2(r.left(), back_y),
            widgets::button_size(ui, "Back", false),
        );
        let resp = widgets::button(ui, Id::unique("back"), rect, "Back", Kind::Secondary, true);
        self.default_focus(ui, &resp);
        if resp.clicked() {
            let to = self.back_to.clone();
            self.go(&ctx, to, false);
        }
    }

    fn running_ui(&mut self, ui: &mut Ui, r: Rect, action: Action) {
        let ctx = ui.ctx().clone();
        let mut y = Self::title(ui, r, r.top() + 24.0, "opensmarters is open") + 8.0;
        y = Self::body(
            ui,
            r,
            y,
            "Close it to continue. Setup carries on by itself once it's closed.",
        ) + 32.0;
        match self.buttons(
            ui,
            r,
            y,
            &[("Try again", Kind::Primary), ("Cancel", Kind::Ghost)],
        ) {
            Some(0) => self.start(&ctx, action),
            Some(_) => {
                let to = if action == Action::Remove {
                    Screen::Remove
                } else {
                    Screen::Start
                };
                self.go(&ctx, to, false);
            }
            None => {}
        }
    }

    fn working_ui(&mut self, ui: &mut Ui, r: Rect, action: Action, phase: Phase) {
        let ctx = ui.ctx().clone();
        let title = match (action, &self.situation.plan) {
            (Action::Remove, _) => "Removing opensmarters".to_owned(),
            (Action::Install, Plan::Update { .. }) => "Updating opensmarters".to_owned(),
            (Action::Install, Plan::Same) => "Repairing opensmarters".to_owned(),
            (Action::Install, _) => "Installing opensmarters".to_owned(),
        };
        let block = Self::title_height(ui, r.width(), &title) + 10.0 + 24.0 + 20.0;
        let mut y = Self::centred(r.top(), r.bottom() - 60.0, block);
        y = Self::title(ui, r, y, &title) + 10.0;
        let status = match phase {
            Phase::Preparing => "Getting ready…",
            Phase::Unpacking => "Unpacking…",
            Phase::Checking => "Checking files…",
            Phase::Shortcuts if action == Action::Remove => "Removing shortcuts…",
            Phase::Shortcuts => "Adding shortcuts…",
            Phase::Finishing => "Finishing…",
            Phase::Removing => "Removing files…",
            Phase::RollingBack => "Putting everything back…",
        };
        // The status line changes calmly: a short crossfade keyed on the text.
        let a = anim::animate_bool(&ctx, Id::unique(("status", status)), true, motion::BASE);
        widgets::text(
            ui,
            pos2(r.left(), y),
            Align2::LEFT_TOP,
            status,
            font::body(),
            color::TEXT_2.gamma_multiply(a.max(0.2)),
        );
        y += 24.0;
        // A thin tabular percentage, quiet (caption, text-3).
        let pct = format!("{:.0}%", (self.light * 100.0).clamp(0.0, 100.0).floor());
        widgets::text(
            ui,
            pos2(r.left(), y + 2.0),
            Align2::LEFT_TOP,
            &pct,
            font::caption(),
            color::TEXT_3,
        );
        if action == Action::Remove {
            return;
        }
        let asking = self.worker.as_ref().is_some_and(|w| w.asking_cancel);
        let by = r.bottom() - 52.0;
        if asking {
            widgets::text(
                ui,
                pos2(r.left(), by - 30.0),
                Align2::LEFT_TOP,
                "Stop installing? Nothing will be changed.",
                font::body(),
                color::TEXT,
            );
            match self.buttons(
                ui,
                r,
                by,
                &[
                    ("Keep installing", Kind::Primary),
                    ("Stop", Kind::Secondary),
                ],
            ) {
                Some(0) => {
                    if let Some(w) = &mut self.worker {
                        w.asking_cancel = false;
                    }
                    self.focus_default = true;
                }
                Some(_) => {
                    if let Some(w) = &mut self.worker {
                        w.ctl
                            .cancel
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        w.asking_cancel = false;
                    }
                }
                None => {}
            }
        } else {
            let cancelling = self
                .worker
                .as_ref()
                .is_some_and(|w| w.ctl.cancel.load(std::sync::atomic::Ordering::Relaxed));
            let rect = Rect::from_min_size(
                pos2(r.left(), by + 4.0),
                widgets::button_size(ui, "Cancel", false),
            );
            // Nothing is focused here by default: the light is the one bright thing on
            // screen. Tab reaches Cancel; Esc asks to cancel too.
            let resp = widgets::button(
                ui,
                Id::unique("cancel"),
                rect,
                "Cancel",
                Kind::Secondary,
                !cancelling && phase < Phase::Shortcuts,
            );
            if resp.clicked()
                && let Some(w) = &mut self.worker
            {
                w.asking_cancel = true;
                self.focus_default = true;
            }
        }
    }

    fn done_ui(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        app_exe: &std::path::Path,
        leftover: Option<&std::path::Path>,
    ) {
        let ctx = ui.ctx().clone();
        let title = "opensmarters is ready.";
        let block = READY_PT - 28.0 + Self::title_height(ui, r.width(), title) + 28.0 + 52.0;
        let top = Self::centred(r.top(), r.bottom(), block);
        if let Some(art) = &self.art {
            // The PNG has a transparent margin; pull it so the drawing lines up with the text.
            let rect =
                Rect::from_min_size(pos2(r.left() - 14.0, top - 16.0), Vec2::splat(READY_PT));
            ui.painter().image(
                art.ready.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let mut y = Self::title(ui, r, top + READY_PT - 28.0, title) + 8.0;
        if let Some(l) = leftover {
            y = widgets::wrapped(
                ui,
                pos2(r.left(), y),
                r.width(),
                &format!(
                    "An old folder was kept because it has files of yours: {}",
                    l.display()
                ),
                font::caption(),
                color::TEXT_3,
            )
            .bottom()
                + 8.0;
        }
        y += 20.0;
        let items: [(&str, Kind); 2] = if self.open_when_done {
            [
                ("Open opensmarters", Kind::Primary),
                ("Close", Kind::Secondary),
            ]
        } else {
            [
                ("Close", Kind::Primary),
                ("Open opensmarters", Kind::Secondary),
            ]
        };
        if let Some(i) = self.buttons(ui, r, y, &items) {
            if items[i].0 == "Open opensmarters" {
                run::launch(&self.env, app_exe);
            }
            self.close(&ctx);
        }
    }

    fn remove_ui(&mut self, ui: &mut Ui, r: Rect) {
        let ctx = ui.ctx().clone();
        let mut y = Self::title(ui, r, r.top() - 4.0, "Remove opensmarters?") + 16.0;
        let row_w = r.width() + 16.0;
        let mut data = self.remove_opts.purge_data;
        let mut pw = self.remove_opts.purge_passwords;
        let rect = Rect::from_min_size(pos2(r.left() - 12.0, y), vec2(row_w, 48.0));
        widgets::checkbox_row(
            ui,
            Id::unique("purge-data"),
            rect,
            "Also delete my data (favourites, history, settings)",
            &mut data,
        );
        y += 52.0;
        let rect = Rect::from_min_size(pos2(r.left() - 12.0, y), vec2(row_w, 48.0));
        widgets::checkbox_row(
            ui,
            Id::unique("purge-passwords"),
            rect,
            "Also remove saved passwords from Windows Credential Manager",
            &mut pw,
        );
        self.remove_opts = uninstall::Options {
            purge_data: data,
            purge_passwords: pw,
        };
        y += 64.0;
        icons::paint(
            ui.painter(),
            pos2(r.left() + 9.0, y + 10.0),
            icons::FILM_STRIP,
            18.0,
            Style::Regular,
            color::SAGE,
        );
        y = widgets::wrapped(
            ui,
            pos2(r.left() + 28.0, y),
            r.width() - 28.0,
            "Recordings and downloads in your Videos folder are never deleted.",
            font::body(),
            color::TEXT_2,
        )
        .bottom();
        let by = (y + 24.0).max(r.bottom() - 52.0);
        match self.buttons(
            ui,
            r,
            by,
            &[("Remove", Kind::Primary), ("Keep it", Kind::Secondary)],
        ) {
            Some(0) => self.start(&ctx, Action::Remove),
            Some(_) => self.close(&ctx),
            None => {}
        }
    }

    fn removed_ui(&mut self, ui: &mut Ui, r: Rect, o: &uninstall::Outcome) {
        let ctx = ui.ctx().clone();
        let (title, mut lines): (&str, Vec<String>) = if o.nothing_installed {
            (
                "opensmarters isn't installed.",
                vec!["There was nothing to remove.".to_owned()],
            )
        } else {
            let kept = match (
                self.remove_opts.purge_data,
                self.remove_opts.purge_passwords,
            ) {
                (false, false) => {
                    "Your data and saved passwords are still on this PC, in case you come back."
                }
                (true, false) => {
                    "Your data was deleted. Saved passwords are still in Windows Credential Manager."
                }
                (false, true) => "Saved passwords were removed. Your data is still on this PC.",
                (true, true) => "Your data and saved passwords were removed too.",
            };
            let l = vec![
                kept.to_owned(),
                "Recordings and downloads in your Videos folder are still there.".to_owned(),
            ];
            ("opensmarters has been removed.", l)
        };
        if o.folder_kept
            && o.pending.is_empty()
            && let Some(d) = &o.dir
        {
            lines.push(format!("Files you added are still in {}.", d.display()));
        }
        if !o.problems.is_empty() {
            lines.push("Some items couldn't be removed; the log has the details.".to_owned());
        }
        let mut y = Self::title(ui, r, r.top() + 24.0, title) + 10.0;
        for l in &lines {
            y = Self::body(ui, r, y, l) + 6.0;
        }
        y += 24.0;
        if self
            .buttons(ui, r, y, &[("Close", Kind::Primary)])
            .is_some()
        {
            self.close(&ctx);
        }
    }

    fn error_ui(&mut self, ui: &mut Ui, r: Rect, message: &str, details: &str, retry: Action) {
        let ctx = ui.ctx().clone();
        let top = r.top() + 24.0;
        icons::paint(
            ui.painter(),
            pos2(r.left() + 12.0, top + 20.0),
            icons::WARNING_CIRCLE,
            26.0,
            Style::Regular,
            color::DANGER,
        );
        let title_r = Rect::from_min_max(pos2(r.left() + 36.0, r.top()), r.max);
        let mut y = Self::title(ui, title_r, top, "Setup didn't finish") + 10.0;
        y = widgets::wrapped(
            ui,
            pos2(r.left(), y),
            r.width(),
            message,
            font::body(),
            color::TEXT,
        )
        .bottom()
            + 28.0;
        match self.buttons(
            ui,
            r,
            y,
            &[
                ("Try again", Kind::Primary),
                ("Copy details", Kind::Secondary),
            ],
        ) {
            Some(0) => {
                if retry == Action::Remove {
                    self.go(&ctx, Screen::Remove, false);
                } else {
                    self.situation = run::survey(&self.env, &self.payload);
                    self.go(&ctx, Screen::Start, false);
                }
            }
            Some(_) => {
                ctx.copy_text(details.to_owned());
                self.copied = true;
            }
            None => {}
        }
        if self.copied {
            widgets::text(
                ui,
                pos2(r.left(), y + 70.0),
                Align2::LEFT_TOP,
                "Details copied: the log's location and what went wrong.",
                font::caption(),
                color::TEXT_3,
            );
        }
    }
}

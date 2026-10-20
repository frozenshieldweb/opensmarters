// SPDX-License-Identifier: GPL-3.0-or-later
//! The eframe application: owns the router, services, player, preferences and harness,
//! and runs the per-frame loop (see `docs/APP_ARCHITECTURE.md` → Frame loop).

use std::sync::Arc;

use eframe::egui::{self, Rect, UiBuilder};
use opensmarters_core::chrono::Utc;
use opensmarters_player::LogLevel;
use opensmarters_store::ProfileId;

use crate::cli::Cli;
use crate::focus;
use crate::harness::{self, Harness};
use crate::nav::{NavCommand, Navigator, Route, Router};
use crate::playback::PlaybackController;
use crate::screens::{self, AppCtx, Chrome, Screen, shell};
use crate::services::{Prefs, Services};
use crate::sessions::{SessionEvent, SessionManager};
use crate::ui::layout::Layout;
use crate::ui::theme::color;
use crate::ui::widgets::{ToastKind, Toasts};
use crate::ui::{motion, theme};

/// What to show first.
#[derive(Debug, Clone)]
pub enum Startup {
    /// Open this profile (last used, or the demo profile).
    Open(ProfileId, String),
    /// No profiles yet.
    Onboarding,
    /// Several profiles and none was used last.
    Profiles,
}

/// Startup configuration (from `main`).
#[derive(Debug)]
pub struct AppConfig {
    /// Parsed command line.
    pub cli: Cli,
    /// First screen / profile to open.
    pub startup: Startup,
    /// Initial preferences.
    pub prefs: Prefs,
    /// The restored window geometry; `Some` enables window-state saving (not for
    /// `--ephemeral` or `--window-size` runs).
    pub window: Option<Option<crate::window_state::WindowState>>,
    /// Keeps the demo panel alive.
    #[cfg(feature = "demo")]
    pub demo: Option<crate::services::demo::Demo>,
}

/// The app.
pub struct OpensmartersApp {
    services: Services,
    router: Router<Box<dyn Screen>>,
    nav: Navigator,
    toasts: Toasts,
    playback: PlaybackController,
    prefs: Prefs,
    prefs_dirty: bool,
    sessions: SessionManager,
    harness: Harness,
    pending_route: Option<Route>,
    tv_override: bool,
    ppp_override: Option<f32>,
    window_size: Option<(u32, u32)>,
    frame_count: u64,
    system_motion: crate::platform::SystemMotion,
    window: Option<crate::window_state::WindowTracker>,
    dvr: crate::dvr::Dvr,
    sleep: crate::playback::sleep::SleepTimer,
    alerts: crate::services::alerts::Alerts,
    multi: crate::playback::multi::MultiView,
    #[cfg(feature = "demo")]
    demo: Option<crate::services::demo::Demo>,
}

impl std::fmt::Debug for OpensmartersApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpensmartersApp")
            .field(
                "routes",
                &self
                    .router
                    .routes()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Builds an [`AppCtx`] from the app's fields (everything but the router).
macro_rules! cx {
    ($s:ident, $layout:expr, $next:expr) => {
        AppCtx {
            egui: $s.services.egui().clone(),
            services: &$s.services,
            nav: &mut $s.nav,
            toasts: &mut $s.toasts,
            playback: &mut $s.playback,
            prefs: &mut $s.prefs,
            sessions: &mut $s.sessions,
            layout: $layout,
            now: Utc::now(),
            next_route: $next,
            dvr: &mut $s.dvr,
            sleep: &mut $s.sleep,
            alerts: &mut $s.alerts,
            multi: &mut $s.multi,
            prefs_dirty: &mut $s.prefs_dirty,
        }
    };
}

impl OpensmartersApp {
    /// Creates the app inside the eframe creation closure.
    pub fn new(cc: &eframe::CreationContext<'_>, services: Services, config: AppConfig) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        egui_extras::install_image_loaders(ctx);
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut system_motion = crate::platform::SystemMotion::default();
        crate::ui::motion::set_reduced(
            ctx,
            config.prefs.effective_reduced_motion(system_motion.poll()),
        );

        let mpv_log = LogLevel::from_mpv_str(&config.cli.mpv_log).unwrap_or(LogLevel::Info);
        let tuning = crate::playback::PlayerTuning::from_prefs(&config.prefs);
        // Empty the player's disk cache folder (left over by a crash) before the player
        // uses it; `--ephemeral` without `--data-dir` caches in memory.
        crate::playback::tuning::init_disk_cache(services.paths.data_dir.as_deref());
        let mut playback = PlaybackController::new(
            cc.get_proc_address.clone(),
            ctx,
            services.redactor.clone(),
            config.prefs.video_fit,
            config.prefs.volume,
            mpv_log,
            tuning,
        );
        playback.set_track_prefs(
            config.prefs.audio_language.clone(),
            config.prefs.subtitle_language.clone(),
        );

        let steps = config
            .cli
            .keys
            .as_deref()
            .map(harness::parse_script)
            .transpose()
            .unwrap_or_else(|e| {
                tracing::error!("--keys: {e}");
                None
            })
            .unwrap_or_default();
        let final_shot = config
            .cli
            .screenshot_after
            .zip(config.cli.screenshot_out.clone());
        let harness = Harness::new(steps, final_shot, config.cli.exit_after);
        let pending_route = config
            .cli
            .route
            .as_deref()
            .and_then(|r| match Route::parse(r) {
                Ok(route) => Some(route),
                Err(e) => {
                    tracing::error!("--route: {e}");
                    None
                }
            });

        let mut app = Self {
            services,
            router: Router::new(),
            nav: Navigator::default(),
            toasts: Toasts::default(),
            playback,
            prefs: config.prefs,
            prefs_dirty: false,
            sessions: SessionManager::new(),
            harness,
            pending_route,
            tv_override: config.cli.tv,
            ppp_override: config.cli.ppp,
            window_size: config.cli.window_size,
            frame_count: 0,
            system_motion,
            window: config.window.map(crate::window_state::WindowTracker::new),
            dvr: crate::dvr::Dvr::default(),
            sleep: crate::playback::sleep::SleepTimer::default(),
            alerts: crate::services::alerts::Alerts::default(),
            multi: crate::playback::multi::MultiView::default(),
            #[cfg(feature = "demo")]
            demo: config.demo,
        };
        // A `--route` that needs no session (profiles, onboarding) opens right away and
        // skips the automatic sign-in.
        let explicit_first = match &app.pending_route {
            Some(r) if !r.needs_session() => app.pending_route.take(),
            _ => None,
        };
        let first = match (&config.startup, explicit_first) {
            (_, Some(route)) => route,
            (Startup::Open(id, name), None) => {
                app.sessions.open(&app.services, *id, name.clone());
                Route::Profiles
            }
            (Startup::Onboarding, None) => Route::Welcome,
            (Startup::Profiles, None) => Route::Profiles,
        };
        app.nav.reset(first);
        app
    }

    fn apply_zoom(&self, ctx: &egui::Context) {
        let native = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0)
            .max(0.25);
        let tv = self.prefs.tv_mode || self.tv_override;
        let width_px = ctx.content_rect().width() * ctx.pixels_per_point();
        let target_ppp = if tv {
            (width_px / 1100.0).max(1.0)
        } else {
            // UI size (Settings → Appearance), never so large that the page drops below the
            // 960 pt "medium" layout.
            let base = self.ppp_override.unwrap_or(native);
            (base * self.prefs.ui_scale).min((width_px / 960.0).max(base))
        };
        let zoom = target_ppp / native;
        if (ctx.zoom_factor() - zoom).abs() > 0.005 {
            ctx.set_zoom_factor(zoom);
        }
    }

    fn first_frame(&self, ctx: &egui::Context) {
        if let Some((w, h)) = self.window_size {
            let native = ctx
                .input(|i| i.viewport().native_pixels_per_point)
                .unwrap_or(1.0)
                .max(0.25);
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                320.0, 240.0,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                w as f32 / native,
                h as f32 / native,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(8.0, 8.0)));
        }
    }

    fn poll_sessions(&mut self, ctx: &egui::Context, layout: Layout) {
        match self.sessions.poll(ctx) {
            Some(SessionEvent::Opened(session)) => {
                {
                    let mut cx = cx!(self, layout, None);
                    crate::dvr::on_session_opened(&mut cx, &session);
                }
                let route = self.pending_route.take().unwrap_or(Route::Home);
                tracing::info!(route = %route, "profile opened");
                if session.is_offline() {
                    self.toasts
                        .warning("Your provider can't be reached. Showing saved data.");
                }
                // Live screen keeps nothing from another profile.
                self.playback.stop();
                if matches!(route, Route::Home) {
                    self.nav.reset(Route::Home);
                } else {
                    self.nav.reset(Route::Home);
                    self.nav.push(route);
                }
            }
            Some(SessionEvent::Failed(_))
                if !matches!(self.router.top().map(|e| &e.route), Some(Route::Profiles)) =>
            {
                self.nav.reset(Route::Profiles);
            }
            Some(SessionEvent::Failed(_)) => {}
            None => {}
        }
    }

    fn apply_nav(&mut self, ctx: &egui::Context, layout: Layout) {
        for cmd in self.nav.drain() {
            // Session-only routes without a session go to the profile picker.
            let cmd = match cmd {
                NavCommand::Push(r)
                | NavCommand::Replace(r)
                | NavCommand::Section(r)
                | NavCommand::Reset(r)
                    if r.needs_session() && self.sessions.current().is_none() =>
                {
                    tracing::debug!(route = %r, "no session: showing profiles");
                    NavCommand::Reset(Route::Profiles)
                }
                other => other,
            };
            let prev_focus = focus::current(ctx);
            let change = self.router.apply(cmd, &mut |r| screens::create(r));
            if !change.top_changed {
                continue;
            }
            let next = self.router.top().map(|e| e.route.clone());
            if change.covered_previous {
                let idx = self.router.len().saturating_sub(2);
                let Self { router, .. } = self;
                if let Some(entry) = router.entry_mut(idx) {
                    entry.saved_focus = prev_focus;
                    let screen = &mut entry.screen;
                    let mut cx = cx!(self, layout, next.clone());
                    screen.on_leave(&mut cx);
                }
            }
            for mut removed in change.removed {
                let mut cx = cx!(self, layout, next.clone());
                removed.screen.on_leave(&mut cx);
            }
            let restore = {
                let Self { router, .. } = self;
                router.top_mut().map(|e| e.saved_focus.take())
            };
            if let Some(entry) = self.router.top_mut() {
                let screen = &mut entry.screen;
                let mut cx = cx!(self, layout, None);
                screen.on_enter(&mut cx);
            }
            match restore.flatten() {
                Some(id) => focus::request(ctx, id),
                None => focus::clear(ctx),
            }
            tracing::debug!(stack = ?self.router.routes().map(ToString::to_string).collect::<Vec<_>>(), "navigated");
        }
    }

    fn save_prefs_if_dirty(&mut self, ctx: &egui::Context) {
        if !self.prefs_dirty {
            return;
        }
        self.prefs_dirty = false;
        motion::set_reduced(
            ctx,
            self.prefs
                .effective_reduced_motion(self.system_motion.last()),
        );
        self.playback.set_track_prefs(
            self.prefs.audio_language.clone(),
            self.prefs.subtitle_language.clone(),
        );
        let prefs = self.prefs.clone();
        self.services
            .blocking("save preferences", move |s| prefs.save(s));
    }
}

impl eframe::App for OpensmartersApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.harness.inject(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.frame_count == 0 {
            self.first_frame(&ctx);
        }
        self.frame_count += 1;
        focus::set_pointer_enabled(&ctx, !self.harness.is_active() && self.frame_count > 20);
        self.apply_zoom(&ctx);
        let system_animations = self.system_motion.poll();
        motion::set_reduced(&ctx, self.prefs.effective_reduced_motion(system_animations));
        let early_layout = Layout::new(ctx.content_rect(), self.prefs.tv_mode || self.tv_override);
        self.poll_sessions(&ctx, early_layout);
        self.playback.update();
        for what in self.services.take_background_errors() {
            self.toasts.push(
                ToastKind::Error,
                crate::services::background_error_message(what),
                Some(what),
            );
        }
        if let Some(tracker) = &mut self.window {
            let ppp = ctx.pixels_per_point();
            ctx.input(|i| tracker.observe(i.viewport(), ppp));
            if let Some(state) = tracker.due() {
                self.services
                    .blocking("remember the window size and position", move |s| {
                        s.settings().set(&crate::window_state::WINDOW, &Some(state))
                    });
            }
        }
        if self.frame_count <= 2 {
            // Apply the startup route before drawing anything.
            let layout = Layout::new(ctx.content_rect(), self.prefs.tv_mode || self.tv_override);
            self.apply_nav(&ctx, layout);
        }

        let screen = ctx.content_rect();
        let tv = self.prefs.tv_mode || self.tv_override;
        let mut layout = Layout::new(screen, tv);
        let chrome = self
            .router
            .top()
            .map_or(Chrome::Fullscreen, |e| e.screen.chrome());
        let rail_w = if chrome == Chrome::Shell {
            layout.rail_width()
        } else {
            0.0
        };
        layout.content =
            Rect::from_min_max(egui::pos2(screen.left() + rail_w, screen.top()), screen.max);

        // Feature hooks (Phase 7): recordings, sleep timer, reminders. They may claim keys
        // and queue navigation like a screen.
        {
            let mut cx = cx!(self, layout, None);
            crate::dvr::on_frame(&mut cx);
            crate::playback::sleep::on_frame(&mut cx);
            crate::services::alerts::on_frame(&mut cx);
        }

        // 1. Screen-owned keys, then the focus engine, then back.
        if let Some(entry) = self.router.top_mut() {
            let screen = &mut entry.screen;
            let mut cx = cx!(self, layout, None);
            screen.on_input(&mut cx, &ctx);
        }
        let keys = focus::begin_frame(&ctx);
        if keys.back {
            let consumed = match self.router.top_mut() {
                Some(entry) => {
                    let screen = &mut entry.screen;
                    let mut cx = cx!(self, layout, None);
                    screen.on_back(&mut cx)
                }
                None => false,
            };
            if !consumed {
                if self.router.len() > 1 {
                    self.nav.back();
                } else if chrome == Chrome::Shell {
                    // Back on the root: move to the side rail (TV convention).
                    if let Some(id) = focus::group_memory(&ctx, focus::shell_group()) {
                        focus::request(&ctx, id);
                    }
                }
            }
        }

        // 2. Paint.
        ui.painter().rect_filled(screen, 0.0, color::BG);
        let section = self
            .router
            .top()
            .and_then(|e| e.screen.section().or_else(|| e.route.section()));
        if let Some(entry) = self.router.top_mut() {
            let screen_obj = &mut entry.screen;
            let mut content_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(layout.content)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            content_ui.set_clip_rect(layout.content);
            let mut cx = cx!(self, layout, None);
            focus::group(&mut content_ui, focus::content_group(), |ui| {
                screen_obj.ui(&mut cx, ui);
            });
        }
        if chrome == Chrome::Shell {
            let mut cx = cx!(self, layout, None);
            shell::rail(&mut cx, ui, section);
        }
        {
            let mut cx = cx!(self, layout, None);
            crate::services::alerts::show(&mut cx, ui);
        }
        let bottom = layout.safe_area().bottom();
        self.toasts.show(&ctx, bottom);
        focus::end_frame(&ctx);

        // 3. Apply navigation and persist preferences.
        self.apply_nav(&ctx, layout);
        self.save_prefs_if_dirty(&ctx);
        if self.harness.is_active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        self.dvr.shutdown();
        if let Some(gl) = gl {
            self.multi.destroy(gl);
            self.playback.destroy(gl);
        }
        // One small settings write at exit (the background pool may already be gone).
        if let Some(state) = self.window.as_mut().and_then(|t| t.take_unsaved())
            && let Err(e) = self
                .services
                .store
                .settings()
                .set(&crate::window_state::WINDOW, &Some(state))
        {
            tracing::warn!(error = %e, "couldn't save the window position");
        }
        #[cfg(feature = "demo")]
        if let Some(demo) = self.demo.take()
            && !self.services.ephemeral
        {
            let _ = self.services.store.profiles().delete(demo.profile);
        }
        let _ = Arc::strong_count(&self.services.store);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        color::BG.to_normalized_gamma_f32()
    }
}

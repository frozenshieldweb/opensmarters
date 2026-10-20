// SPDX-License-Identifier: GPL-3.0-or-later
//! [`VideoView`]: an egui widget that shows a [`Player`]'s video.
//!
//! How it works: inside an `egui_glow` paint callback (where eframe's GL
//! context is guaranteed current) mpv renders into an offscreen FBO sized to
//! the widget in physical pixels. The FBO is then blitted into egui's
//! framebuffer at the widget's rectangle, clipped by the widget's clip rect.
//! Anything egui paints later in the frame (an overlay) lands on top. mpv's
//! update callback calls `request_repaint`, so frames arrive at video rate
//! with no polling. mpv letterboxes inside the FBO, so aspect handling is
//! entirely mpv's (`video-aspect-override`, `panscan`, `video-zoom`,
//! `keepaspect`).

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ThreadId;

use egui_glow::glow::{self, HasContext as _};

use crate::player::{Core, Player};
use crate::render::{GetProcAddress, RenderContext};

/// Shows a [`Player`]'s video inside egui (glow backend only).
///
/// ```no_run
/// # use opensmarters_player::{Player, PlayerConfig, VideoView};
/// struct App { player: Player, video: VideoView }
///
/// impl App {
///     fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, Box<dyn std::error::Error>> {
///         let player = Player::new(PlayerConfig::default())?;
///         let ctx = cc.egui_ctx.clone();
///         player.set_event_notify(move || ctx.request_repaint());
///         let gpa = cc.get_proc_address.clone().ok_or("glow renderer required")?;
///         Ok(Self { player, video: VideoView::new(gpa) })
///     }
/// }
///
/// impl eframe::App for App {
///     fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
///         let response = self.video.show(ui, &self.player);
///         // …draw an overlay on top of `response.rect`…
///     }
///     fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
///         if let Some(gl) = gl { self.video.destroy(gl); }
///     }
/// }
/// ```
///
/// * Create it on the UI thread; it is `!Send`.
/// * One `VideoView` per [`Player`] at a time (libmpv allows one render
///   context per core). Showing a different player rebinds it.
/// * Call [`VideoView::destroy`] from `eframe::App::on_exit` so GL objects
///   and the render context are freed while the GL context still exists. A
///   `VideoView` dropped on the UI thread during the session also cleans up.
pub struct VideoView {
    state: Arc<Mutex<ViewState>>,
    get_proc_address: GetProcAddress,
    bound_player: Option<u64>,
    _not_send: PhantomData<*const ()>,
}

impl std::fmt::Debug for VideoView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoView")
            .field("bound_player", &self.bound_player)
            .finish_non_exhaustive()
    }
}

/// Counters for diagnostics ([`VideoView::stats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoStats {
    /// Frames mpv rendered into the FBO.
    pub frames_rendered: u64,
    /// Paint callbacks that blitted a frame to the screen.
    pub frames_presented: u64,
    /// Current FBO size in pixels.
    pub target_size: (i32, i32),
}

struct GlTarget {
    fbo: glow::Framebuffer,
    texture: glow::Texture,
    width: i32,
    height: i32,
}

struct ViewState {
    /// GL thread, recorded at the first paint.
    owner: Option<ThreadId>,
    /// eframe's GL context, captured at the first paint so cleanup on drop
    /// can delete our GL objects.
    gl: Option<Arc<glow::Context>>,
    render: Option<RenderContext>,
    target: Option<GlTarget>,
    bind_request: Option<Arc<Core>>,
    egui_ctx: Option<egui::Context>,
    has_frame: bool,
    /// Keep showing the last frame while the player has no video
    /// ([`VideoView::set_hold_last_frame`]).
    hold_last_frame: bool,
    destroyed: bool,
    error: Option<String>,
    stats: VideoStats,
}

// SAFETY: `ViewState` holds a `!Send` `RenderContext` and GL object names.
// It is shared with egui paint callbacks, which egui requires to be
// `Send + Sync`, but every code path that touches `render` or `target`
// first checks that it runs on `owner` (the GL thread, recorded at the first
// paint), and `Drop` leaks them instead of freeing them on any other thread.
// The remaining fields are plain `Send` data.
unsafe impl Send for ViewState {}

impl ViewState {
    fn on_owner_thread(&self) -> bool {
        self.owner.is_none_or(|o| o == std::thread::current().id())
    }

    fn release_gl(&mut self, gl: &glow::Context) {
        // Drop the render context first (frees mpv's GL resources), then ours.
        self.render = None;
        if let Some(t) = self.target.take() {
            // SAFETY: names were created on this GL context, which is current
            // (we are on the owner thread inside a paint callback / on_exit).
            unsafe {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.texture);
            }
        }
        self.has_frame = false;
    }

    /// Creates or resizes the offscreen target. Returns true if (re)allocated.
    fn ensure_target(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<bool, String> {
        if let Some(t) = &self.target
            && t.width == w
            && t.height == h
        {
            return Ok(false);
        }
        // SAFETY: GL context current on this thread (paint callback). We
        // create/resize a texture and attach it to an FBO, restoring the
        // texture binding afterwards (egui rebinds its own per mesh).
        unsafe {
            let (fbo, texture) = match &self.target {
                Some(t) => (t.fbo, t.texture),
                None => (gl.create_framebuffer()?, gl.create_texture()?),
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                // Drop the objects so the next paint retries from scratch.
                self.target = None;
                self.has_frame = false;
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
                return Err(format!("offscreen framebuffer incomplete (0x{status:x})"));
            }
            // Start black so a partially-initialised frame never shows garbage.
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            self.target = Some(GlTarget {
                fbo,
                texture,
                width: w,
                height: h,
            });
        }
        self.stats.target_size = (w, h);
        Ok(true)
    }

    fn paint(
        &mut self,
        info: &egui::PaintCallbackInfo,
        painter: &egui_glow::Painter,
        gpa: &GetProcAddress,
        has_video: bool,
    ) {
        if self.destroyed {
            return;
        }
        let me = std::thread::current().id();
        match self.owner {
            None => self.owner = Some(me),
            Some(o) if o != me => {
                self.error = Some("VideoView painted from a second thread".into());
                return;
            }
            Some(_) => {}
        }
        if self.gl.is_none() {
            self.gl = Some(Arc::clone(painter.gl()));
        }
        let gl: &glow::Context = painter.gl();

        if let Some(core) = self.bind_request.take() {
            self.render = None;
            self.has_frame = false;
            // SAFETY: we are inside an egui_glow paint callback, so eframe's
            // GL context is current on this thread, and all later uses (paint
            // callbacks, destroy/drop on the UI thread) happen on this same
            // thread with the same context — enforced by the owner check.
            match unsafe { RenderContext::from_core(core, Arc::clone(gpa)) } {
                Ok(mut rc) => {
                    if let Some(ctx) = self.egui_ctx.clone() {
                        rc.set_update_callback(move || ctx.request_repaint());
                    }
                    self.render = Some(rc);
                    self.error = None;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if self.render.is_none() {
            return;
        }

        let vp = info.viewport_in_pixels();
        let max = i32::try_from(painter.max_texture_side())
            .unwrap_or(8192)
            .max(1);
        let (w, h) = (vp.width_px.clamp(1, max), vp.height_px.clamp(1, max));
        let resized = match self.ensure_target(gl, w, h) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e);
                restore_framebuffer(gl, painter);
                return;
            }
        };
        if resized {
            // A new target starts black: nothing to hold any more.
            self.has_frame = false;
        }
        let (Some(rc), Some(target)) = (&self.render, &self.target) else {
            return;
        };
        // While holding, the FBO keeps the last frame: don't let mpv redraw it (black)
        // until the next file has video again.
        let holding = self.hold_last_frame && self.has_frame && !has_video && !resized;
        if (rc.needs_render() || resized) && !holding {
            match rc.render(target.fbo.0.get(), w, h, true) {
                Ok(()) => {
                    self.has_frame = true;
                    self.stats.frames_rendered += 1;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }

        if self.has_frame && (has_video || self.hold_last_frame) {
            let clip = info.clip_rect_in_pixels();
            // SAFETY: GL context current (paint callback). mpv may have
            // changed scissor/framebuffer state, so set what the blit needs:
            // read from our FBO, draw into egui's target, clipped to the
            // widget's clip rect. Blits are unaffected by blending/shaders.
            unsafe {
                gl.enable(glow::SCISSOR_TEST);
                gl.scissor(
                    clip.left_px,
                    clip.from_bottom_px,
                    clip.width_px,
                    clip.height_px,
                );
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.fbo));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, painter.intermediate_fbo());
                gl.blit_framebuffer(
                    0,
                    0,
                    w,
                    h,
                    vp.left_px,
                    vp.from_bottom_px,
                    vp.left_px + w,
                    vp.from_bottom_px + h,
                    glow::COLOR_BUFFER_BIT,
                    glow::NEAREST,
                );
            }
            self.stats.frames_presented += 1;
        }
        restore_framebuffer(gl, painter);
    }
}

fn restore_framebuffer(gl: &glow::Context, painter: &egui_glow::Painter) {
    // SAFETY: GL context current (paint callback); egui_glow documents that
    // callbacks must leave its intermediate FBO bound. It restores the rest
    // of its state itself after the callback.
    unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, painter.intermediate_fbo()) };
}

impl Drop for ViewState {
    fn drop(&mut self) {
        if !self.on_owner_thread() {
            // Freeing GL objects / the render context needs the GL thread.
            // Leaking is the only safe choice here (never happens with eframe,
            // where paint callbacks and the app share the main thread).
            tracing::warn!("VideoView dropped off its GL thread; leaking GL resources");
            std::mem::forget(self.render.take());
            self.target = None;
            return;
        }
        // On the owner thread (eframe keeps its GL context current on the
        // main thread outside of painting too) free everything now.
        if let Some(gl) = self.gl.take() {
            self.release_gl(&gl);
        }
    }
}

impl VideoView {
    /// Creates the widget. `get_proc_address` is eframe's
    /// `CreationContext::get_proc_address` (available with the glow
    /// renderer). No GL work happens until the first paint.
    pub fn new(get_proc_address: GetProcAddress) -> Self {
        Self {
            state: Arc::new(Mutex::new(ViewState {
                owner: None,
                gl: None,
                render: None,
                target: None,
                bind_request: None,
                egui_ctx: None,
                has_frame: false,
                hold_last_frame: false,
                destroyed: false,
                error: None,
                stats: VideoStats::default(),
            })),
            get_proc_address,
            bound_player: None,
            _not_send: PhantomData,
        }
    }

    /// Fills the available space with the video (black when there is none)
    /// and returns the widget's response (click/drag sensing), whose `rect`
    /// is where an overlay should go.
    pub fn show(&mut self, ui: &mut egui::Ui, player: &Player) -> egui::Response {
        let size = ui.available_size();
        self.show_sized(ui, player, size)
    }

    /// Like [`VideoView::show`] with an explicit size in points.
    pub fn show_sized(
        &mut self,
        ui: &mut egui::Ui,
        player: &Player,
        size: egui::Vec2,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        self.paint_at(ui, rect, player);
        response
    }

    /// Paints the video into `rect` without allocating layout space.
    pub fn paint_at(&mut self, ui: &egui::Ui, rect: egui::Rect, player: &Player) {
        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
        if self.bound_player != Some(player.id()) {
            let mut st = self.lock();
            st.bind_request = Some(Arc::clone(player.core()));
            st.egui_ctx = Some(ui.ctx().clone());
            drop(st);
            self.bound_player = Some(player.id());
        }
        if !rect.is_positive() {
            return;
        }
        let has_video = player.with_state(crate::PlayerState::has_video);
        let state = Arc::clone(&self.state);
        let gpa = Arc::clone(&self.get_proc_address);
        let callback = egui_glow::CallbackFn::new(move |info, painter| {
            state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .paint(&info, painter, &gpa, has_video);
        });
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(callback),
        });
    }

    /// The last rendering error, if any (e.g. render context creation
    /// failed). Cleared when a render context is created successfully.
    pub fn last_error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    /// Diagnostic counters.
    pub fn stats(&self) -> VideoStats {
        self.lock().stats
    }

    /// When on, the view keeps showing the last rendered frame while the
    /// player has no video (after a stop, while the next stream opens),
    /// instead of going black, until the next file renders its first frame.
    /// Off by default. Use [`VideoView::forget_frame`] when the next item is
    /// unrelated (the old picture must not show under it).
    pub fn set_hold_last_frame(&self, hold: bool) {
        self.lock().hold_last_frame = hold;
    }

    /// Drops the held frame: black until the next frame is rendered.
    pub fn forget_frame(&self) {
        self.lock().has_frame = false;
    }

    /// Frees the render context and GL objects. Call from
    /// `eframe::App::on_exit(gl)` (GL context still current). After this
    /// the view shows black.
    pub fn destroy(&mut self, gl: &glow::Context) {
        let mut st = self.lock();
        if !st.on_owner_thread() {
            st.error = Some("VideoView::destroy called off the GL thread".into());
            return;
        }
        st.release_gl(gl);
        st.gl = None;
        st.bind_request = None;
        st.destroyed = true;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ViewState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

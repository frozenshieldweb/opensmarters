// SPDX-License-Identifier: GPL-3.0-or-later
//! [`RenderContext`]: libmpv's OpenGL render API, bound to the GL thread.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::Arc;

use crate::error::Error;
use crate::ffi::{self, mpv_render_context, mpv_render_param};
use crate::player::{Core, Player};

/// Resolves an OpenGL function by name for the current context, e.g.
/// eframe's `CreationContext::get_proc_address`.
pub type GetProcAddress = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

type UpdateFn = Box<dyn Fn() + Send + Sync>;

/// An mpv OpenGL render context.
///
/// It is `!Send` and `!Sync`: it lives and dies on the thread that created
/// it, which must be the thread where its OpenGL context is current (the
/// constructor is `unsafe` for exactly that reason). It also keeps the
/// [`Player`]'s mpv core alive, so the core is never destroyed before the
/// render context is freed, as libmpv requires.
///
/// Most apps use [`crate::VideoView`] instead of this type directly.
pub struct RenderContext {
    core: Arc<Core>,
    ctx: NonNull<mpv_render_context>,
    get_proc_address: *mut GetProcAddress,
    update_callbacks: Vec<*mut UpdateFn>,
    _not_send: PhantomData<*mut ()>,
}

impl std::fmt::Debug for RenderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderContext")
            .field("player_id", &self.core.id)
            .finish_non_exhaustive()
    }
}

impl RenderContext {
    /// Creates the render context for `player`.
    ///
    /// # Safety
    /// * An OpenGL (2.1+/GLES 2+) context must be current on the calling
    ///   thread, and that same context must be current whenever any method
    ///   of the returned value runs **and when it is dropped**.
    /// * `get_proc_address` must resolve functions of that context.
    /// * Do not call blocking [`Player`] APIs from inside the update
    ///   callback. (All `Player` control methods are async and fine to call
    ///   from the GL thread at any other time.)
    ///
    /// # Errors
    /// [`Error::Render`] if libmpv cannot create the renderer (e.g. another
    /// render context already exists for this player).
    pub unsafe fn new(player: &Player, get_proc_address: GetProcAddress) -> Result<Self, Error> {
        // SAFETY: forwarded from the caller.
        unsafe { Self::from_core(Arc::clone(player.core()), get_proc_address) }
    }

    /// # Safety
    /// As for [`RenderContext::new`].
    pub(crate) unsafe fn from_core(
        core: Arc<Core>,
        get_proc_address: GetProcAddress,
    ) -> Result<Self, Error> {
        let gpa = Box::into_raw(Box::new(get_proc_address));
        let mut init = ffi::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address_trampoline),
            get_proc_address_ctx: gpa.cast::<c_void>(),
        };
        let mut params = [
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_API_TYPE,
                data: ffi::MPV_RENDER_API_TYPE_OPENGL
                    .as_ptr()
                    .cast_mut()
                    .cast::<c_void>(),
            },
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&raw mut init).cast::<c_void>(),
            },
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut out: *mut mpv_render_context = std::ptr::null_mut();
        // SAFETY: valid core handle; `params` is a properly terminated array
        // whose pointees live until the call returns; mpv keeps only the
        // get_proc_address fn/ctx, and `gpa` stays alive until Drop. The GL
        // context is current per the caller's contract.
        let r = unsafe {
            (core.api.render_context_create)(&raw mut out, core.raw(), params.as_mut_ptr())
        };
        let Some(ctx) = NonNull::new(out).filter(|_| r >= 0) else {
            // SAFETY: `gpa` came from Box::into_raw above and mpv did not
            // keep it (creation failed).
            drop(unsafe { Box::from_raw(gpa) });
            return Err(Error::Render(format!(
                "mpv_render_context_create failed: {}",
                core.api.error_string(r)
            )));
        };
        core.render_attached();
        Ok(Self {
            core,
            ctx,
            get_proc_address: gpa,
            update_callbacks: Vec::new(),
            _not_send: PhantomData,
        })
    }

    /// Id of the [`Player`] this context renders.
    pub fn player_id(&self) -> u64 {
        self.core.id
    }

    /// Sets the function mpv calls (from its own threads) when a new frame
    /// is ready or a redraw is needed. It must not call into mpv; requesting
    /// a UI repaint is the intended use.
    pub fn set_update_callback(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let boxed: *mut UpdateFn = Box::into_raw(Box::new(Box::new(f)));
        // SAFETY: valid render context. mpv stores (callback, ctx) and may
        // call it from any thread until replaced or freed; `boxed` is kept
        // alive until after mpv_render_context_free in Drop. Previous
        // callbacks are kept alive too, so a call racing with this swap can
        // never see freed memory.
        unsafe {
            (self.core.api.render_context_set_update_callback)(
                self.ctx.as_ptr(),
                Some(update_trampoline),
                boxed.cast::<c_void>(),
            );
        }
        self.update_callbacks.push(boxed);
    }

    /// Asks mpv whether a new frame must be rendered
    /// (`mpv_render_context_update` & `MPV_RENDER_UPDATE_FRAME`).
    pub fn needs_render(&self) -> bool {
        // SAFETY: valid context, called on its GL thread (type is !Send and
        // the constructor's contract keeps the context current).
        let flags = unsafe { (self.core.api.render_context_update)(self.ctx.as_ptr()) };
        flags & ffi::MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Renders the current frame into framebuffer object `fbo` (0 = default
    /// framebuffer) of `width`×`height` pixels. Never blocks waiting for the
    /// frame's display time. Leaves arbitrary GL state behind; callers must
    /// restore what they need.
    ///
    /// # Errors
    /// [`Error::Render`] on invalid size or libmpv failure.
    pub fn render(&self, fbo: u32, width: i32, height: i32, flip_y: bool) -> Result<(), Error> {
        if width <= 0 || height <= 0 {
            return Err(Error::Render("empty render target".into()));
        }
        let mut target = ffi::mpv_opengl_fbo {
            fbo: c_int::try_from(fbo).map_err(|_| Error::Render("fbo id out of range".into()))?,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = c_int::from(flip_y);
        let mut block: c_int = 0;
        let mut params = [
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&raw mut target).cast::<c_void>(),
            },
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_FLIP_Y,
                data: (&raw mut flip).cast::<c_void>(),
            },
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: (&raw mut block).cast::<c_void>(),
            },
            mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        // SAFETY: valid context on its GL thread with the context current
        // (constructor contract); `params` is terminated and its pointees
        // outlive the call.
        let r = unsafe {
            (self.core.api.render_context_render)(self.ctx.as_ptr(), params.as_mut_ptr())
        };
        if r < 0 {
            return Err(Error::Render(self.core.api.error_string(r)));
        }
        Ok(())
    }

    /// Tells mpv a frame was presented (optional timing hint). If you call
    /// it, call it after every presented frame.
    pub fn report_swap(&self) {
        // SAFETY: valid context on its GL thread.
        unsafe { (self.core.api.render_context_report_swap)(self.ctx.as_ptr()) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        // SAFETY: valid context, dropped on its own thread with the GL
        // context current (constructor contract). Clearing the callback
        // first, then freeing, guarantees mpv makes no further callback
        // invocations before we free the boxes below.
        unsafe {
            (self.core.api.render_context_set_update_callback)(
                self.ctx.as_ptr(),
                None,
                std::ptr::null_mut(),
            );
            (self.core.api.render_context_free)(self.ctx.as_ptr());
        }
        for cb in self.update_callbacks.drain(..) {
            // SAFETY: each pointer came from Box::into_raw in
            // set_update_callback and mpv can no longer call it.
            drop(unsafe { Box::from_raw(cb) });
        }
        // SAFETY: came from Box::into_raw in from_core; the render context
        // that used it is gone.
        drop(unsafe { Box::from_raw(self.get_proc_address) });
        self.core.render_detached();
        // `self.core` drops after this; if it was the last reference the mpv
        // core is destroyed now, after the render context as required.
    }
}

unsafe extern "C" fn get_proc_address_trampoline(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ctx` is the `*mut GetProcAddress` we registered; it lives as
    // long as the render context that calls this.
    let f = unsafe { &*ctx.cast::<GetProcAddress>() };
    // SAFETY: mpv passes a NUL-terminated function name.
    let name = unsafe { CStr::from_ptr(name) };
    // Never unwind into C.
    std::panic::catch_unwind(AssertUnwindSafe(|| f(name)))
        .map_or(std::ptr::null_mut(), <*const c_void>::cast_mut)
}

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is a `*mut UpdateFn` we registered; it is only freed
    // after the render context is freed.
    let f = unsafe { &*ctx.cast::<UpdateFn>() };
    let _ = std::panic::catch_unwind(AssertUnwindSafe(f));
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal hand-written FFI for the parts of libmpv's client and render APIs
//! this crate uses, resolved at runtime with `libloading`.
//!
//! Layouts mirror `mpv/client.h`, `mpv/render.h` and `mpv/render_gl.h` from
//! client API 2.x. C enums are `int`-sized on every platform we target, so
//! they are represented as `c_int`.
//!
//! Nothing here is public API; the safe wrappers live in `player.rs` and
//! `render.rs`.

#![allow(non_camel_case_types)]

use std::ffi::{CStr, c_char, c_double, c_int, c_ulong, c_void};
use std::marker::{PhantomData, PhantomPinned};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::LoadError;

/// Opaque `mpv_handle`.
#[repr(C)]
pub(crate) struct mpv_handle {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Opaque `mpv_render_context`.
#[repr(C)]
pub(crate) struct mpv_render_context {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

pub(crate) type mpv_format = c_int;
pub(crate) const MPV_FORMAT_NONE: mpv_format = 0;
pub(crate) const MPV_FORMAT_STRING: mpv_format = 1;
pub(crate) const MPV_FORMAT_FLAG: mpv_format = 3;
pub(crate) const MPV_FORMAT_INT64: mpv_format = 4;
pub(crate) const MPV_FORMAT_DOUBLE: mpv_format = 5;
pub(crate) const MPV_FORMAT_NODE: mpv_format = 6;
pub(crate) const MPV_FORMAT_NODE_ARRAY: mpv_format = 7;
pub(crate) const MPV_FORMAT_NODE_MAP: mpv_format = 8;

pub(crate) type mpv_event_id = c_int;
pub(crate) const MPV_EVENT_NONE: mpv_event_id = 0;
pub(crate) const MPV_EVENT_SHUTDOWN: mpv_event_id = 1;
pub(crate) const MPV_EVENT_LOG_MESSAGE: mpv_event_id = 2;
pub(crate) const MPV_EVENT_SET_PROPERTY_REPLY: mpv_event_id = 4;
pub(crate) const MPV_EVENT_COMMAND_REPLY: mpv_event_id = 5;
pub(crate) const MPV_EVENT_START_FILE: mpv_event_id = 6;
pub(crate) const MPV_EVENT_END_FILE: mpv_event_id = 7;
pub(crate) const MPV_EVENT_FILE_LOADED: mpv_event_id = 8;
pub(crate) const MPV_EVENT_VIDEO_RECONFIG: mpv_event_id = 17;
pub(crate) const MPV_EVENT_AUDIO_RECONFIG: mpv_event_id = 18;
pub(crate) const MPV_EVENT_SEEK: mpv_event_id = 20;
pub(crate) const MPV_EVENT_PLAYBACK_RESTART: mpv_event_id = 21;
pub(crate) const MPV_EVENT_PROPERTY_CHANGE: mpv_event_id = 22;
pub(crate) const MPV_EVENT_QUEUE_OVERFLOW: mpv_event_id = 24;

pub(crate) const MPV_END_FILE_REASON_EOF: c_int = 0;
pub(crate) const MPV_END_FILE_REASON_STOP: c_int = 2;
pub(crate) const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub(crate) const MPV_END_FILE_REASON_ERROR: c_int = 4;
pub(crate) const MPV_END_FILE_REASON_REDIRECT: c_int = 5;

pub(crate) const MPV_ERROR_OPTION_NOT_FOUND: c_int = -5;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union mpv_node_u {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: c_double,
    pub list: *mut mpv_node_list,
    pub ba: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct mpv_node {
    pub u: mpv_node_u,
    pub format: mpv_format,
}

#[repr(C)]
pub(crate) struct mpv_node_list {
    pub num: c_int,
    pub values: *mut mpv_node,
    pub keys: *mut *mut c_char,
}

#[repr(C)]
pub(crate) struct mpv_event {
    pub event_id: mpv_event_id,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub(crate) struct mpv_event_property {
    pub name: *const c_char,
    pub format: mpv_format,
    pub data: *mut c_void,
}

#[repr(C)]
pub(crate) struct mpv_event_log_message {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

#[repr(C)]
pub(crate) struct mpv_event_end_file {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

pub(crate) type mpv_render_param_type = c_int;
pub(crate) const MPV_RENDER_PARAM_INVALID: mpv_render_param_type = 0;
pub(crate) const MPV_RENDER_PARAM_API_TYPE: mpv_render_param_type = 1;
pub(crate) const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: mpv_render_param_type = 2;
pub(crate) const MPV_RENDER_PARAM_OPENGL_FBO: mpv_render_param_type = 3;
pub(crate) const MPV_RENDER_PARAM_FLIP_Y: mpv_render_param_type = 4;

pub(crate) const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: mpv_render_param_type = 12;

pub(crate) const MPV_RENDER_API_TYPE_OPENGL: &CStr = c"opengl";
pub(crate) const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub(crate) struct mpv_render_param {
    pub type_: mpv_render_param_type,
    pub data: *mut c_void,
}

pub(crate) type GetProcAddressFn =
    unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void;

#[repr(C)]
pub(crate) struct mpv_opengl_init_params {
    pub get_proc_address: Option<GetProcAddressFn>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub(crate) struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub(crate) type RenderUpdateFn = unsafe extern "C" fn(cb_ctx: *mut c_void);

/// Resolved libmpv entry points. Lives for the whole process once loaded (the
/// library is intentionally never unloaded: mpv starts threads that would
/// otherwise run unmapped code).
pub(crate) struct MpvApi {
    pub path: PathBuf,
    pub client_api_version: unsafe extern "C" fn() -> c_ulong,
    pub error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    pub free: unsafe extern "C" fn(*mut c_void),
    pub create: unsafe extern "C" fn() -> *mut mpv_handle,
    pub initialize: unsafe extern "C" fn(*mut mpv_handle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(*mut mpv_handle),
    pub set_option_string:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub set_option:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int,
    pub command_async: unsafe extern "C" fn(*mut mpv_handle, u64, *mut *const c_char) -> c_int,
    pub command_node_async: unsafe extern "C" fn(*mut mpv_handle, u64, *mut mpv_node) -> c_int,
    pub set_property_async:
        unsafe extern "C" fn(*mut mpv_handle, u64, *const c_char, mpv_format, *mut c_void) -> c_int,
    pub get_property:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, mpv_format, *mut c_void) -> c_int,
    pub observe_property:
        unsafe extern "C" fn(*mut mpv_handle, u64, *const c_char, mpv_format) -> c_int,
    pub request_log_messages: unsafe extern "C" fn(*mut mpv_handle, *const c_char) -> c_int,
    pub wait_event: unsafe extern "C" fn(*mut mpv_handle, c_double) -> *mut mpv_event,
    pub wakeup: unsafe extern "C" fn(*mut mpv_handle),
    pub render_context_create: unsafe extern "C" fn(
        *mut *mut mpv_render_context,
        *mut mpv_handle,
        *mut mpv_render_param,
    ) -> c_int,
    pub render_context_set_update_callback:
        unsafe extern "C" fn(*mut mpv_render_context, Option<RenderUpdateFn>, *mut c_void),
    pub render_context_update: unsafe extern "C" fn(*mut mpv_render_context) -> u64,
    pub render_context_render:
        unsafe extern "C" fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int,
    pub render_context_report_swap: unsafe extern "C" fn(*mut mpv_render_context),
    pub render_context_free: unsafe extern "C" fn(*mut mpv_render_context),
}

/// The DLL / shared object name we look for.
#[cfg(windows)]
pub const LIBMPV_FILE_NAME: &str = "libmpv-2.dll";
#[cfg(target_os = "macos")]
pub const LIBMPV_FILE_NAME: &str = "libmpv.2.dylib";
#[cfg(all(unix, not(target_os = "macos")))]
pub const LIBMPV_FILE_NAME: &str = "libmpv.so.2";

/// Minimum libmpv client API major version we were written against.
const REQUIRED_API_MAJOR: c_ulong = 2;

static API: Mutex<Option<&'static MpvApi>> = Mutex::new(None);

/// Returns the loaded API, loading libmpv on first use.
///
/// Success is cached for the life of the process; failure is not, so a user
/// who installs the DLL can retry without restarting.
pub(crate) fn api(explicit: Option<&Path>) -> Result<&'static MpvApi, LoadError> {
    let mut guard = API
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(api) = *guard {
        return Ok(api);
    }
    let api: &'static MpvApi = Box::leak(Box::new(load(explicit)?));
    *guard = Some(api);
    Ok(api)
}

/// Candidate absolute paths, in search order. Never relative to the current
/// working directory (classic DLL-planting vector). `PATH` is searched only by
/// debug builds: a release build ships `libmpv-2.dll` next to the executable, and
/// a directory on `PATH` may be writable by other users (installers that create
/// `C:\SomeTool\` inherit an ACL that lets any user add files).
pub(crate) fn candidate_paths(explicit: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = explicit {
        out.push(p.to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join(LIBMPV_FILE_NAME));
    }
    // Development builds also look in <repo>/vendor/libmpv (fetched by
    // tools/fetch-libmpv.ps1). Release builds never embed a build-machine path.
    #[cfg(debug_assertions)]
    {
        let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("vendor")
            .join("libmpv")
            .join(LIBMPV_FILE_NAME);
        out.push(dev);
    }
    #[cfg(all(windows, debug_assertions))]
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.is_absolute() {
                out.push(dir.join(LIBMPV_FILE_NAME));
            }
        }
    }
    out
}

/// Restricts where the process looks for DLLs that are loaded **by name** later
/// (by libmpv, ffmpeg, GPU drivers, …): the application directory, System32 and
/// directories added with `AddDllDirectory`, but never the current working
/// directory or `PATH`. Returns whether the stricter order is in effect.
///
/// Windows only (always `false` elsewhere). Idempotent. [`crate::Player::new`]
/// calls it before loading libmpv; an app should call it first thing in `main`,
/// before creating windows, so its own late-loaded DLLs are covered too.
pub fn harden_dll_search_path() -> bool {
    #[cfg(windows)]
    {
        const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetDefaultDllDirectories(directory_flags: u32) -> i32;
            fn SetDllDirectoryW(path_name: *const u16) -> i32;
        }
        // SAFETY: plain Win32 call with a documented flag value; it only changes
        // the process's default DLL search order.
        let ok = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) } != 0;
        let empty: [u16; 1] = [0];
        // SAFETY: `empty` is a valid NUL-terminated (empty) wide string that
        // outlives the call. An empty string removes the current directory from
        // the legacy search order used by callers that bypass the default dirs.
        let _ = unsafe { SetDllDirectoryW(empty.as_ptr()) };
        ok
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn load(explicit: Option<&Path>) -> Result<MpvApi, LoadError> {
    #[cfg(windows)]
    if !harden_dll_search_path() {
        tracing::warn!("could not restrict the DLL search path");
    }
    let mut tried = Vec::new();
    for path in candidate_paths(explicit) {
        if !path.is_file() {
            tried.push(path);
            continue;
        }
        match open_library(&path) {
            Ok(lib) => return resolve(lib, path),
            Err(e) => {
                return Err(LoadError::Open {
                    path,
                    message: e.to_string(),
                });
            }
        }
    }
    // On Unix, finally defer to the dynamic loader's standard search (which,
    // unlike Windows' LoadLibrary, never consults the working directory).
    #[cfg(unix)]
    {
        // SAFETY: loading libmpv runs its (trusted, pinned) initialisers; we
        // pass a plain file name so the system loader's search path applies.
        if let Ok(lib) = unsafe { libloading::Library::new(LIBMPV_FILE_NAME) } {
            return resolve(lib, PathBuf::from(LIBMPV_FILE_NAME));
        }
    }
    Err(LoadError::NotFound { tried })
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    // SAFETY: `path` is absolute. The flags make Windows resolve libmpv's own
    // dependencies from its directory and System32 only, never from the
    // current directory or PATH. Loading runs libmpv's DllMain/initialisers,
    // which is the point of this call.
    unsafe {
        Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map(Into::into)
}

#[cfg(not(windows))]
fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    // SAFETY: `path` is an absolute path to libmpv; loading runs its
    // initialisers, which is the point of this call.
    unsafe { libloading::Library::new(path) }
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {{
        // SAFETY: the symbol is looked up by its exact C name and cast to the
        // signature declared in mpv's public headers (client API 2.x). The
        // function pointer stays valid because the library is leaked below
        // and never unloaded.
        match unsafe { $lib.get($name) } {
            Ok(s) => *s,
            Err(e) => {
                return Err(LoadError::MissingSymbol {
                    symbol: $name.to_string_lossy().into_owned(),
                    message: e.to_string(),
                });
            }
        }
    }};
}

fn resolve(lib: libloading::Library, path: PathBuf) -> Result<MpvApi, LoadError> {
    let api = MpvApi {
        client_api_version: sym!(lib, c"mpv_client_api_version"),
        error_string: sym!(lib, c"mpv_error_string"),
        free: sym!(lib, c"mpv_free"),
        create: sym!(lib, c"mpv_create"),
        initialize: sym!(lib, c"mpv_initialize"),
        terminate_destroy: sym!(lib, c"mpv_terminate_destroy"),
        set_option_string: sym!(lib, c"mpv_set_option_string"),
        set_option: sym!(lib, c"mpv_set_option"),
        command_async: sym!(lib, c"mpv_command_async"),
        command_node_async: sym!(lib, c"mpv_command_node_async"),
        set_property_async: sym!(lib, c"mpv_set_property_async"),
        get_property: sym!(lib, c"mpv_get_property"),
        observe_property: sym!(lib, c"mpv_observe_property"),
        request_log_messages: sym!(lib, c"mpv_request_log_messages"),
        wait_event: sym!(lib, c"mpv_wait_event"),
        wakeup: sym!(lib, c"mpv_wakeup"),
        render_context_create: sym!(lib, c"mpv_render_context_create"),
        render_context_set_update_callback: sym!(lib, c"mpv_render_context_set_update_callback"),
        render_context_update: sym!(lib, c"mpv_render_context_update"),
        render_context_render: sym!(lib, c"mpv_render_context_render"),
        render_context_report_swap: sym!(lib, c"mpv_render_context_report_swap"),
        render_context_free: sym!(lib, c"mpv_render_context_free"),
        path,
    };
    // SAFETY: plain getter with no arguments.
    let version = unsafe { (api.client_api_version)() };
    if version >> 16 != REQUIRED_API_MAJOR {
        return Err(LoadError::IncompatibleVersion {
            found: format!("{}.{}", version >> 16, version & 0xffff),
            required: format!("{REQUIRED_API_MAJOR}.x"),
        });
    }
    // Keep the library mapped forever: mpv cores may outlive any owner we
    // could tie it to, and their threads run code from it.
    std::mem::forget(lib);
    Ok(api)
}

impl MpvApi {
    /// `mpv_error_string` as an owned Rust string.
    pub(crate) fn error_string(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string accepts any int and returns a pointer to a
        // static NUL-terminated string (never NULL).
        let p = unsafe { (self.error_string)(code) };
        if p.is_null() {
            return format!("mpv error {code}");
        }
        // SAFETY: non-null, static, NUL-terminated (see above).
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    /// Client API version as (major, minor).
    pub(crate) fn version(&self) -> (u32, u32) {
        // SAFETY: plain getter with no arguments.
        let v = unsafe { (self.client_api_version)() };
        // Both halves are 16-bit fields, so the conversions cannot fail.
        // `c_ulong` is u32 on Windows but u64 on Linux, hence the allow.
        #[allow(clippy::useless_conversion)]
        let half = |x: c_ulong| u32::try_from(x & 0xffff).unwrap_or(0);
        (half(v >> 16), half(v))
    }
}

/// Copies a possibly-NULL C string into an owned `String` (lossy UTF-8).
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string valid for reads.
pub(crate) unsafe fn opt_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        // SAFETY: caller guarantees a valid NUL-terminated string.
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

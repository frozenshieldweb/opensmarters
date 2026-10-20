// SPDX-License-Identifier: GPL-3.0-or-later
//! # opensmarters-player
//!
//! A safe, hardened wrapper over **libmpv** plus an **egui video widget**.
//! This is the only crate in the workspace allowed to use `unsafe`; every
//! `unsafe` block carries a `// SAFETY:` comment.
//!
//! ## Loading libmpv
//!
//! libmpv is loaded at runtime (`libloading`), not linked, so the app starts
//! even when it is missing and can show [`LoadError`]'s message ("…not found
//! … See docs/LIBMPV.md"). Search order, absolute paths only:
//! [`PlayerConfig::libmpv_path`] → next to the executable →
//! `<repo>/vendor/libmpv/` and directories on `PATH` (debug builds only).
//! On Windows dependencies are resolved from the DLL's directory and
//! System32 only, never the working directory, and before loading libmpv the
//! process-wide DLL search order is restricted with
//! [`harden_dll_search_path`] (apps should call it at startup too). Run
//! `tools/fetch-libmpv.ps1` to install the pinned build into
//! `vendor/libmpv/`; release builds must ship `libmpv-2.dll` next to the
//! executable.
//!
//! ## The pieces
//!
//! * [`PlayableUrl`]: **the only way to load media.** It accepts
//!   `http`/`https` URLs (plus `rtmp`/`rtsp`/`udp` behind [`UrlPolicy`]) and
//!   rejects everything else (`file:`, `edl://`, `lavfi://`, control
//!   characters, …). [`UserAgent`] and [`Referrer`] validate the header
//!   values. None of them implements `Display`, and their `Debug` output is
//!   redacted.
//! * [`Player`]: an mpv core created with hardened options (`config=no`,
//!   `load-scripts=no`, `ytdl=no`, no input bindings, an ffmpeg
//!   `protocol_whitelist`, `tls-verify=yes`, …; see `docs/LIBMPV.md`). Its
//!   control methods are asynchronous and never block, so they are safe to
//!   call from the UI/GL thread. A background thread keeps a
//!   [`PlayerState`] snapshot up to date and queues [`PlayerEvent`]s.
//! * [`StreamTuning`]: per-stream buffering (initial buffer, cache sizes, a seekable or
//!   disk-backed cache for pausing and rewinding live TV), sent with
//!   [`Player::load_with`] or applied at once with [`Player::set_tuning`].
//!   [`Player::set_hwdec`] switches hardware decoding while playing.
//!   [`PlayerState::cache`] ([`CacheState`]) reports the seekable window.
//! * [`VideoView`] (feature `egui`, on by default): renders the player's
//!   video through libmpv's OpenGL render API into an offscreen FBO and
//!   blits it into egui's framebuffer, with egui overlays drawn on top.
//! * [`RenderContext`]: the low-level render API (`!Send`, unsafe to
//!   create) for integrations other than egui.
//!
//! ## Usage with eframe (glow renderer)
//!
//! ```no_run
//! use opensmarters_player::{LoadOptions, PlayableUrl, Player, PlayerConfig, PlayerEvent,
//!     EndReason, UserAgent, VideoView};
//!
//! struct App { player: Player, video: VideoView }
//!
//! impl eframe::App for App {
//!     fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
//!         for ev in self.player.poll_events() {
//!             match ev {
//!                 PlayerEvent::EndFile(EndReason::Eof) => { /* VOD finished / live dropped */ }
//!                 PlayerEvent::EndFile(EndReason::Error(e)) => { /* show error, maybe retry */ }
//!                 PlayerEvent::Log(line) => { /* REDACT line.text before logging it */ }
//!                 _ => {}
//!             }
//!         }
//!         let r = self.video.show(ui, &self.player);
//!         let st = self.player.state();
//!         // draw controls over r.rect using st.time_pos, st.duration, …
//!         # let _ = (r, st);
//!     }
//!     fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
//!         if let Some(gl) = gl { self.video.destroy(gl); }
//!     }
//! }
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let options = eframe::NativeOptions {
//!         renderer: eframe::Renderer::Glow,
//!         ..Default::default()
//!     };
//!     eframe::run_native("demo", options, Box::new(|cc| {
//!         let player = Player::new(
//!             PlayerConfig::default().user_agent(UserAgent::new("opensmarters/0.1.0")?),
//!         )?;
//!         let ctx = cc.egui_ctx.clone();
//!         player.set_event_notify(move || ctx.request_repaint());
//!         let url = PlayableUrl::parse("http://127.0.0.1:8765/test.m3u8")?;
//!         player.load(&url, &LoadOptions::default())?;
//!         let gpa = cc.get_proc_address.clone().ok_or("glow renderer required")?;
//!         Ok(Box::new(App { player, video: VideoView::new(gpa) }))
//!     }))?;
//!     Ok(())
//! }
//! ```
//!
//! ## Threading and shutdown
//!
//! * `Player` is `Send + Sync`; its methods can be called from any thread.
//!   Prefer [`Player::state`] over querying mpv, and do not block the UI
//!   thread on [`Player::wait_event`].
//! * The render context lives on the GL thread (eframe's main thread). mpv
//!   calls `request_repaint` from its own threads when a frame is ready.
//! * Shutdown order: call [`VideoView::destroy`] in `on_exit` (frees the
//!   render context while GL is current), then drop the `Player`. The mpv
//!   core is destroyed when both are gone; the render context always goes
//!   first because it holds a reference to the core.
//!
//! ## Logging
//!
//! This crate never logs URLs. mpv's log lines arrive **unredacted** as
//! [`PlayerEvent::Log`]; the app must redact them before writing them
//! anywhere.

#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod error;
mod ffi;
mod node;
mod playable;
mod player;
mod render;
#[cfg(feature = "egui")]
mod video;

pub use config::{
    CURL_CONNECT_TIMEOUT_SECS, CURL_MAX_REDIRECTS, Hwdec, LogLevel, MAX_CACHE_BYTES, PlayerConfig,
    StreamTuning,
};
pub use error::{Error, LoadError, MpvError, ValidationError};
pub use ffi::{LIBMPV_FILE_NAME, harden_dll_search_path};
pub use playable::{
    Container, MAX_HEADER_LEN, MAX_URL_LEN, MediaOutput, PlayableUrl, Referrer, SNIFF_BYTES,
    UrlPolicy, UserAgent, sniff_container,
};
pub use player::{
    Aspect, CacheState, EndReason, LoadOptions, LogLine, MAX_SEEK_RANGES, Op, Player, PlayerEvent,
    PlayerState, Property, SeekRange, Track, TrackKind, TrackSelection, VideoFit,
};
pub use render::{GetProcAddress, RenderContext};
#[cfg(feature = "egui")]
pub use video::{VideoStats, VideoView};

/// Re-export of the `egui` version this crate is built against.
#[cfg(feature = "egui")]
pub use egui;
/// Re-export of the `egui_glow` version this crate is built against.
#[cfg(feature = "egui")]
pub use egui_glow;

/// The absolute paths searched for libmpv, in order (for diagnostics/UI).
pub fn libmpv_search_paths(explicit: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    ffi::candidate_paths(explicit)
}

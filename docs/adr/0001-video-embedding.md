# ADR 0001: Embedding libmpv video in the egui UI

- **Status:** accepted (Phase 0 spike verified on Windows 11, NVIDIA RTX 5070, driver 617.42)
- **Date:** 2026-10-20
- **Scope:** `crates/player` (`opensmarters-player`)

## Context

The app UI is eframe/egui (BUILD_PROMPT §3.1). Video must play **inside** the app window with egui controls drawn on top. libmpv offers three ways to show video:

- (a) the **render API**, where we render each frame into our own OpenGL context or texture;
- (b) **`wid` embedding**, where mpv draws into a native child window we give it;
- (c) a **separate window** that mpv owns, controlled over the client API.

Constraints:

- Keyboard, mouse and remote navigation is handled by egui, so mpv must not take focus or input.
- An OSD and controls must overlay the video.
- Aspect and zoom must survive rebuffering.
- No tearing, no leaks, and resizing must work.
- Only `crates/player` may use `unsafe`.

## Options considered

| Option | Pros | Cons |
|---|---|---|
| **(a) Render API (OpenGL) into eframe's glow context** | Real compositing: egui widgets paint over the video. One window, one input path. Works on all desktop OSes eframe supports. libmpv itself recommends it. | Needs the glow backend, not wgpu. Must respect libmpv's render-thread rules. More `unsafe` code (FFI and GL). |
| (b) `wid` child window | Little code. mpv handles vsync and presentation. | egui cannot draw over a native child window (airspace problem), so the overlay would need a second window or have to live outside the video. Focus and keyboard routing on Windows is fragile. Platform-specific `HWND` code. |
| (c) mpv-owned window | Trivial. | Not "inside the app window". Two windows to keep in sync. Poor for keyboard/remote UX. |
| D3D11 render API (`MPV_RENDER_API_TYPE_D3D11`) | Native on Windows. Best hwdec interop. | egui/eframe uses OpenGL or wgpu. Sharing a D3D11 texture with wgpu needs vendor-specific interop; Windows only. |
| SW render API into an egui texture | No GL interop. | CPU colour conversion and scaling; far too slow for 1080p/4K. |

## Decision

Use **(a): the libmpv OpenGL render API inside eframe's glow renderer**. It worked on the first attempt, so (b) and (c) were not needed.

### How it works

`crates/player/src/video.rs` (`VideoView`):

1. `VideoView::show(ui, &player)` allocates the widget rect and fills it black. It then adds an `egui_glow::CallbackFn` paint callback.
2. Inside the callback, eframe's GL context is guaranteed current. On first use the callback:
   - creates the `RenderContext` (`mpv_render_context_create`, with `get_proc_address` taken from `eframe::CreationContext::get_proc_address`);
   - registers an update callback that calls `egui::Context::request_repaint()`.
3. On every paint the callback:
   - (re)allocates an offscreen RGBA8 texture and FBO matching the widget's size in **physical pixels**;
   - calls `mpv_render_context_render(FBO, w, h, FLIP_Y=1, BLOCK_FOR_TARGET_TIME=0)` when mpv reports a new frame (`mpv_render_context_update`) or the size changed;
   - `glBlitFramebuffer`s the FBO into egui's target, clipped by the widget's clip rect (scissor);
   - restores the framebuffer binding (`Painter::intermediate_fbo()`); egui_glow restores the rest of its state itself.
4. Letterboxing, aspect override (`video-aspect-override`), stretch (`keepaspect=no`), crop (`panscan`) and zoom (`video-zoom`) are all done by mpv inside the FBO.
5. Shapes egui paints after the callback land on top, which is how the overlay works.
6. eframe presents with vsync (`GlowConfiguration::vsync` defaults to true), so there is no tearing.
7. mpv drives repaints: frames arrive at video rate, and an idle or paused player costs nothing.

### Threading rules, encoded in types

- **`Player` is `Send + Sync`.**
  - All of its control methods use the **async** libmpv calls (`mpv_command_async`, `mpv_command_node_async`, `mpv_set_property_async`). libmpv documents these as safe on the render thread. That matters because in egui the UI thread *is* the render thread, and synchronous calls there can deadlock against the VO ("…not being called or stuck").
  - Errors come back as `PlayerEvent::CommandFailed`.
  - State comes from a background event thread (`mpv_wait_event`) into a `PlayerState` snapshot, so the UI never queries mpv synchronously.
- **`RenderContext` is `!Send + !Sync`** (it holds `PhantomData<*mut ()>`).
  - Its constructor is `unsafe`. The contract is that the GL context is current, and stays current, on the creating thread for every later call and for the drop.
  - It holds an `Arc` to the mpv core, so the core can never be destroyed before the render context is freed, as libmpv requires.
- **`VideoView` is `!Send`.** egui requires paint callbacks to be `Send + Sync`, so the callback's shared state wraps the render context:
  - The wrapper records the GL thread at the first paint and refuses to touch GL from any other thread.
  - On a foreign-thread drop it leaks rather than free.
- **Update callback lifetime:** the callback box is freed only after `mpv_render_context_free`, which happens after the callback is cleared.

### "Load before the view exists"

libmpv fails `vo=libmpv` when playback starts before a render context exists ("No render context set"). It then disables video for that file. Re-enabling video later needs a seek, which **live streams cannot do**; we observed this with a non-seekable HTTP TS. So:

- **Pending loads.** With video output enabled (the default), `Player::load` holds the load until a `RenderContext` attaches, then sends it immediately (`Player::has_pending_load()`).
- **Re-attach.** When a render context is re-attached while a file is already playing, the video track is reselected (`vid=no` then `vid=auto`). That works for seekable media; for live media the app should reload.
- **Headless and audio-only.** Players for these use `PlayerConfig::video_output(false)` (`vid=no`) and never wait.

## Consequences

- **eframe must run the glow renderer:** `NativeOptions { renderer: eframe::Renderer::Glow, .. }` with eframe feature `glow`.
  - eframe 0.37 defaults to wgpu, so the app crate must enable `glow`.
  - It must pass `cc.get_proc_address` to `VideoView::new`.
- **Versions pinned by this crate:** `egui 0.37`, `egui_glow 0.37`, `glow 0.17` (the version egui_glow 0.37 uses), `eframe 0.37` (dev-dependency, for the spike).
- **One `VideoView` per `Player` at a time.** Multiscreen means one player and one view per tile; one render context per mpv core is a libmpv rule.
- **Shutdown order:**
  1. `VideoView::destroy(gl)` in `App::on_exit` (frees the render context while GL is current);
  2. drop the `Player` (it stops playback and joins its event thread);
  3. the mpv core is destroyed when the last of the two goes.
- **Hardware decoding:** works through the GL render API (NVDEC/CUDA interop was selected on the test machine).
  - d3d11va→GL interop needs `WGL_NV_DX_interop`, which was not available in this context. AMD/Intel machines may therefore fall back to copy-back or software decoding.
  - If that turns out too slow, a future ADR could add the D3D11 render API with a wgpu/D3D11 shared texture.
- **The DLL is loaded at runtime with `libloading`** and a minimal hand-written FFI, not linked through the `libmpv2` crate.
  - There is no MSVC import library to manage. libmpv ships only a MinGW `.dll.a`, so linking would need `lib.exe` steps.
  - A missing DLL is a clean runtime error that names `docs/LIBMPV.md`, not a build failure.
  - The search path is ours, so it never includes the working directory.
  - The FFI is small (about 20 functions) and is exercised by integration tests against the real DLL.
- **Rejected fallbacks:** (b) and (c) are documented here but not implemented.
  - If the render API ever fails on some GPU or driver, (b) would be the next step: a child `HWND` via `wid`, with controls in a separate egui strip below the video, because of the airspace problem.

## Verification (2026-10-20)

- **Spike runs** (`crates/player/examples/spike.rs`):
  - Real video frames appear in the egui window with the overlay on top, for a local TS, a local MKV, HTTP TS and HTTP HLS.
  - In the MKV: audio track switched to 2, subtitles on, aspect overrides 4:3 / Stretch / Crop, absolute and relative seek, and pause all worked.
  - The VOD end produced `EndFile(Eof)`.
  - Resizing the window to portrait and to ultra-wide letterboxed and stretched correctly.
- **Integration tests** (`crates/player/tests/mpv.rs`) run against the real `libmpv-2.dll`, headless. They cover:
  - EOF, the start/resume option, track switching, seek and pause;
  - aspect persisting across loads;
  - User-Agent and Referer on every HLS request;
  - the protocol whitelist blocking a `file:` HLS segment, and a `302 → file://` being refused;
  - deferred loads.

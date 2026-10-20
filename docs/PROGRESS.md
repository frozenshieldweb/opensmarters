# Progress

This tracks the phases in [BUILD_PROMPT.md](BUILD_PROMPT.md#7-how-to-work).

| Phase | Status | Notes |
|---|---|---|
| 0: workspace, libmpv, playback spike | **done** | libmpv render API into egui (ADR-0001); hardened options; `tools/fetch-libmpv.ps1` |
| 1: core (protocol, parsers) | **done** | 148 tests; 3 fuzz targets ran 5 min each with no crashes |
| 2: store, keychain, fake-panel | **done** | Encrypted cache and leak test; fake-panel with quirk modes and generated media |
| Art direction | **done** | "Afterglow": `docs/DESIGN.md`, `assets/` (generated with Codex) |
| 3: UI shell, navigation, profiles, Live TV | **done** | App foundation: architecture (`docs/APP_ARCHITECTURE.md`), Afterglow UI kit, spatial focus engine, router, session/cache/EPG/images, shared player with reconnect and progress, onboarding, profiles, Home, Live TV, fullscreen player, dev harness |
| 4: VOD, series, resume, catch-up, favourites, parental, settings | **done** | 4A: Movies, Series, Detail, resume, next episode. 4B: Catch-up (with timeshift-format fallback), Guide (virtualised EPG grid, bounded XMLTV), Search, Favourites. 4C: parental control, Settings pages, Storage & data, privacy, welcome screen, window state. Workspace: ~350 tests; fmt, clippy and deny all clean |
| 5: reviews, hardening, packaging, SECURITY.md | in progress | Security, code and UX reviews running |
| 6: real-provider test | partial | Core, player and foundation app verified against the user's panel (see below); final pass after fixes |
| 7: recording, multiscreen, sleep timer, reminders, stable streams | **foundation done** | Plan and contracts: [PHASE7.md](PHASE7.md). Foundation: schema v2 (recordings, reminders), connection budget (leases; viewing preempts downloads), owned-media playback and `dump-cache`/`stream-record` into validated outputs, routes/stubs (Recordings, Multiscreen, Settings › Recordings), app-wide hooks. Streams A–D in progress |

## Real-provider test (2026-10-20, the user's Xtream account, via `crates/core/examples/live_probe.rs`)

- **Login:** works. Status, expiry, `max_connections=3` and `allowed_output_formats=[m3u8, ts]` all parse correctly.
- **Catalogue:** 85 live categories with 9,833 channels; 39 VOD categories with 35,482 movies; 37 series categories with 5,871 series. Zero items were skipped. "All movies" takes about 23 s, so the app loads per category and caches.
- **EPG:** the short EPG decodes correctly, accents included. Some titles contain decorative superscript characters.
- **Playback (player crate):**
  - Live 720p H.264 plays with NVDEC.
  - Movie: 1080p HEVC with DTS, two audio tracks; seeking works.
  - Series episode: plays, and seeking works.
- **Catch-up:** the panel returns `503` for every timeshift variant: path with Standard or Smarters format, `.ts` or `.m3u8`, and `timeshift.php`. The URLs were confirmed correct, so this looks like a provider-side problem. The app must degrade gracefully.
- **Connection limit:** quick probing left the panel counting stale connections. Every stream then returns `403` (JSON) until they time out, which takes about 1–3 min each. The app must stop the old stream before zapping, retry with backoff, and explain the connection limit to the user.
- **Live subtitles:** mpv auto-selected embedded closed captions on a live channel, so live subtitles should default to off.

## Polish backlog (from the real-provider app test, 2026-10-20)

- **Live: Enter on an idle preview card does nothing.** Before anything has played, it should play the channel that is highlighted or was last focused.
- **Live: zapping gives no feedback.** After CH+/CH− (PageUp/PageDown) in fullscreen, show a channel banner (number, logo, name, now/next) for about 3 s. Show a buffering indicator over the last frame while the new stream starts.
- **Live: stalls aren't caught.** If `time-pos` stops advancing for more than about 8 s while the player isn't paused, show "Stream stalled — reconnecting…" and reconnect with backoff. We saw a frozen frame for at least 12 s after a zap.
- **Live: CH− wraps around.** From channel 001, CH− (PageDown) jumps to the last of 9,833 channels. Consider stopping at the ends of the list, or making the wrap visible in the banner.
- **Live: channel logos are tiny.** Wide provider logos shrink into the square tile. Contain-fit them larger with less padding, and use a neutral plate behind transparent logos.
- **Player: no OSD on fullscreen entry.** Briefly show the OSD when entering fullscreen.
- **Home/VOD: the year comes from `added`.** Every card shows 2026. This was sent to the Phase 4A agent.

## Known issues / follow-ups

- XMLTV in a non-UTF-8 encoding stops at the first invalid byte. The fix would be quick-xml's `encoding` feature.
- The timeshift format still needs checking against real panels. Both formats are implemented and selectable per profile.
- Phosphor icons: `egui-phosphor` 0.14 targets egui 0.36, so the app loads the official Phosphor TTFs from `assets/fonts/` (MIT) with generated glyph constants (`crates/app/src/ui/icons/glyphs.rs`).
- App (3a) follow-ups: Home backdrop crossfade on focus; M3U EPG uses the first `url-tvg` only; compact breakpoint (< 960 pt) keeps the collapsed rail instead of a top bar.
- 4C follow-ups: Movies/Series PIN prompts must use `session.pin_length()` (or `PinPrompt`); Search should record queries via `services::search_history`; TMDB is only a setting + key so far (no detail-page enrichment); third-party licence *texts* for release packages are still to be generated (Phase 5).
- Phase 7 A (stable streams), resolved: hardware decoding and buffering now apply at once (`Player::set_hwdec`, `Player::set_tuning`, per-load `StreamTuning`). Follow-ups:
  - **libcurl has no read timeout** (it reads `http(s)` and HLS segments in this mpv build; `docs/LIBMPV.md` → Network). The controller's stall detection (being rewritten on `main`) is the only remedy for a silent stream: it should also cover films/episodes (no progress while not paused: reload at the last position) and streams that never open (curl only bounds the TCP connect, 10 s), and must not count a user pause or a seek as a stall (live rewind).
  - Extra buffering per channel is raised after 3 rebuffers or a reconnect while the stream was starving; once the new stall detector lands, its "stalled" signal could feed `stream::before_load`/`boost_current` directly.
  - Leaving the fullscreen player while paused leaves the Live TV preview paused (it now shows "−mm:ss"); consider resuming on return.
  - The demo's live channels are 20 s files served with `Range`, not real-time streams: the demo can't show a long rewind window, and a fake-panel "real-time live" quirk would make rewind testable end to end.
  - "Save the last N minutes" (stream B) reads the same buffer: with Rewind live TV off the back buffer is 32 MiB (about 30 s at 8 Mbit/s).
  - Turning libcurl off for live TS (`curl-enabled=no` per load) would give FFmpeg's transport-level reconnects that keep the rewind buffer; it changes the TLS stack and redirect handling, so it needs a security review first.
  - `docs/PHASE7.md` "Facts learned" says FFmpeg `demuxer-lavf-o` options still apply to HLS segments: in this build segments go through libcurl too (verified, see `docs/LIBMPV.md`).
- 7C (sleep timer, reminders) follow-ups: the multiscreen tiles' sound isn't faded (the
  sleep timer fades the main player only; `cx.multi` has no volume API yet); the Guide's
  info-panel hint still says "Enter  Watch" / "Enter  Details" (now Enter opens the actions
  menu); `alerts::settings_rows` still has to be placed on Settings › Recordings &
  downloads; Recordings › Scheduled doesn't list reminders yet; "Cancel recording" isn't
  offered for a running recording (it opens Recordings); whether eframe keeps running the
  frame loop while the window is **minimised** (needed for reminders to fire then) wasn't
  verified; a prompt that needs an answer can't be reached from a focused text field until
  the field loses the focus (by design: typing is never interrupted).
- Multiscreen (7D) follow-ups:
  - Tile *reconnects* are not staggered. The shared controller schedules its own retries (2, 5, 10, 30, 30 s), so tiles that drop at the same moment reconnect together. Only first opens and zaps go through `multi::next_start`. A start hook or retry jitter in `PlaybackController` would cover it.
  - Opening multiscreen closes the main stream and opens the same channel again in tile 1 (0.8 s later). A real panel may count the closed connection as stale for 1–3 min, so with a limit of 3 the third tile can get a 403 until it expires. The tile then retries and says "Connection limit reached". Handing the main player's open stream to tile 1 would avoid it.
  - Leaving multiscreen leaves Live TV's preview empty; it doesn't resume a tile's channel there.
  - The picker can't unlock a locked category (no PIN prompt in it); it points to Live TV.
  - Up to four idle tile engines (mpv cores and tile-sized offscreen framebuffers) stay alive until exit once used. They are freed in `on_exit` with the GL context current.
  - Not exercised in the app yet: coming back to a covered multiscreen (nothing can cover it until reminders or the sleep timer navigate) and the "Stopped" tile state (`stop_all` from the sleep timer). Both are covered only by reading the code and the empty-view unit test.
  - Demo media: some demo channels (e.g. 001) are short static clips, and every player, tiles included, goes to "Reconnecting" about 20 s after opening them (stall detection).
- Real-provider findings already handled in the app: one stream at a time (stop before load, debounced zapping), live reconnect 2/5/10/30/30 s, connection-limit message from `account_info()` after a stream failure, subtitles off by default, live OSD without scrubber, catalogue titles (never mpv's media title), catch-up failure message, Home never fetches the VOD/series "All" lists, decorative symbols mapped (★ ➤) and CJK/Arabic/superscripts via the OS font fallback.
- Phase 7B (recording & downloads) follow-ups:
  - "Record through the player" (no connection free, the user watches that channel) stops when they change channel and doesn't hand over to a connection that frees later; the rest of the programme isn't recorded.
  - Recording an HLS stream keeps audio only when it is muxed into the video variant; separate audio renditions, byte-range segments and encrypted streams are refused with a message.
  - The clip length shown is an estimate (what played since the stream (re)started); mpv's back buffer may hold less, and `dump-cache` then saves less. The real length shows once the clip is played.
  - Downloads resume from the `.part` length and check `Content-Range`, but not an `ETag`/`Last-Modified` (a provider replacing the file between two runs with one of the same size would mix them).
  - The Scheduled tab lists recordings scheduled from the Guide (stream C adds the Guide's Record menu); in this branch it was checked with tests, not in the app.
  - Free space uses `fs4` (added to the app crate; MIT/Apache-2.0, no new transitive crates).
  - `--ephemeral` runs save into `%TEMP%\opensmarters-ephemeral\recordings`, which nothing cleans up.
  - `cargo +nightly fuzz build` doesn't link on this Windows/MSVC toolchain (`__stop___sancov_pcs`) for any target, so the new `hls` target was type-checked (`cargo +nightly check`) but not run.

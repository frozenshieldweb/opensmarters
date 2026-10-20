# Progress

This tracks the phases in [BUILD_PROMPT.md](BUILD_PROMPT.md#7-how-to-work).

| Phase | Status | Notes |
|---|---|---|
| 0: workspace, libmpv, playback spike | **done** | libmpv render API into egui (ADR-0001); hardened options; `tools/fetch-libmpv.ps1` |
| 1: core (protocol, parsers) | **done** | 148 tests; 3 fuzz targets ran 5 min each with no crashes |
| 2: store, keychain, fake-panel | **done** | Encrypted cache and leak test; fake-panel with quirk modes and generated media |
| Art direction | **done** | "Afterglow": `docs/DESIGN.md`, `assets/` (generated with Codex) |
| 3: UI shell, navigation, profiles, Live TV | **3a done** (app foundation) | `crates/app`: architecture (`docs/APP_ARCHITECTURE.md`), Afterglow UI kit, spatial focus engine, router, session/cache/EPG/images, shared player with reconnect/progress; screens: shell, onboarding, profiles, Home, Live TV (preview, PIN locks, number entry), fullscreen player, Settings (account, playback, appearance, about); stubs for Movies/Series/Detail/Catch-up/Guide/Search/Favourites/Parental/Storage; dev harness (`--demo --ephemeral --route --keys --screenshot-*`). 50 tests. |
| 4: VOD, series, resume, catch-up, favourites, parental, settings | not started | |
| 5: hardening, packaging, SECURITY.md | not started | `cargo deny check` passes (documented exceptions) |
| 6: real-provider test | not started | |

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

## Known issues / follow-ups

- XMLTV in a non-UTF-8 encoding stops at the first invalid byte. The fix would be quick-xml's `encoding` feature.
- The timeshift format still needs checking against real panels. Both formats are implemented and selectable per profile.
- Phosphor icons: `egui-phosphor` 0.14 targets egui 0.36, so the app loads the official Phosphor TTFs from `assets/fonts/` (MIT) with generated glyph constants (`crates/app/src/ui/icons/glyphs.rs`).
- App (3a) follow-ups: Home backdrop crossfade on focus; reduced motion doesn't follow the Windows animation setting yet; M3U EPG uses the first `url-tvg` only; compact breakpoint (< 960 pt) keeps the collapsed rail instead of a top bar.
- Real-provider findings already handled in the app: one stream at a time (stop before load, debounced zapping), live reconnect 2/5/10/30/30 s, connection-limit message from `account_info()` after a stream failure, subtitles off by default, live OSD without scrubber, catalogue titles (never mpv's media title), catch-up failure message, Home never fetches the VOD/series "All" lists, decorative symbols mapped (★ ➤) and CJK/Arabic/superscripts via the OS font fallback.

# Prompt: build opensmarters, a secure open-source IPTV player in Rust

> Paste everything below the line into a new Claude Code session opened in this folder (`opensmarters/`), or say: *"Follow docs/BUILD_PROMPT.md."*

---

You are building **opensmarters**: a free, open-source desktop IPTV player written in Rust. I'll use it to watch my own IPTV subscription in place of IPTV Smarters Pro. It must work with real providers and be secure by design.

The app ships with **no content, no channel lists and no provider endpoints**. It is a player for whatever account the user enters, nothing more.

## 1. Read these first (required)

- `docs/PROTOCOL.md` is the compatibility spec. It covers Xtream Codes API calls and the fields that are actually read, stream URL templates, catch-up/timeshift, EPG (base64 titles), M3U parsing and TMDB. **Treat it as the source of truth for wire behaviour.**
- `docs/REVERSE_ENGINEERING.md` (private notes, not in the public repository) covers how the original works: its features, persisted data and bugs. Don't reproduce any of the problems it lists.
- The original's unpacked source may exist in `_reference/` (gitignored; it can be re-created with `tools/asar-extract.js`). You may *read* it to answer behaviour questions. **Never copy its code, images, logos, branding or text** into this project. Design your own UI and assets.

## 2. Environment

- Windows 11, Rust 1.97 stable (MSVC toolchain), git, ffmpeg 9 on PATH. Node is available but must not be needed by the app.
- **libmpv is not installed.** Set up a Windows libmpv dev build: `libmpv-2.dll` plus headers and an import library for MSVC.
  - Get it from the builds linked at mpv.io and verify the checksum.
  - Document the exact steps in the README.
  - Make the build fail with a clear message if libmpv is missing.
- Cross-platform (Linux/macOS) support is nice to have. Don't block on it, but don't add Windows-only code outside the platform layer.

## 3. Non-negotiable security requirements

1. **No HTML/JS rendering of provider data.** Use a native Rust UI. The recommendation is `eframe`/`egui`, which rules out the XSS→RCE class of bugs entirely. If you choose a webview stack instead (e.g. Tauri 2), justify it in an ADR. It must then have:
   - a strict CSP with no remote scripts
   - no `innerHTML` and no `eval`
   - a minimal Tauri capability allowlist
   - every provider string rendered as text
2. **All provider data is hostile input.** That covers JSON, M3U, EPG, image URLs and strings.
   - Parse defensively. Never panic on bad data; skip invalid entries and keep going.
   - Cap response sizes (configurable, e.g. 256 MB for huge `get_live_streams`) and image sizes (e.g. 10 MB).
   - Set connect and read timeouts on every request.
   - Cap string lengths before display.
3. **Credentials:**
   - Store passwords only in the OS keychain (`keyring` crate → Windows Credential Manager). Never put them in config files, SQLite, logs, crash output, test fixtures or git.
   - Percent-encode every query value and path segment.
   - Stream URLs contain credentials in the path, so **redact them in all logs** with a `tracing` redaction layer. Include mpv's log output, which echoes URLs.
4. **The parental PIN** is hashed with Argon2id plus a salt, compared in constant time, and rate-limited after failed attempts.
5. **TLS:** use `rustls`, with certificate verification always on.
   - Many panels are HTTP-only. Allow `http://`, but show a clear "unencrypted: your password is visible on the network" warning on the account.
   - No "ignore certificate errors" toggle.
6. **Harden mpv.** Untrusted URLs must not reach dangerous mpv/ffmpeg features.
   - Set `config=no`, `load-scripts=no`, `ytdl=no` and `input-default-bindings=no`. Bind your own keys.
   - Keep `load-unsafe-playlists=no`.
   - Before passing a URL to mpv, check it against a scheme allowlist: `http`, `https`, and optionally `rtmp`/`rtsp`/`udp` behind a setting. Reject `file:`, `edl:`, `lavfi:`, `av:`, `memory:`, `fd:` and the like, plus anything with control characters.
   - Pass URLs as a single `loadfile` argument, never as string-built commands.
   - Research and apply mpv/ffmpeg protocol-whitelist options where they exist, and document what you set.
7. **No telemetry, analytics or phoning home.** Network traffic goes only to:
   - the user's panel or playlist host
   - the stream hosts that panel returns
   - image hosts in the provider data
   - optionally TMDB, which is opt-in and uses a user-supplied key. **Never embed an API key.**
8. **Supply chain:**
   - Run `cargo deny` (licenses, advisories, bans) and `cargo audit` in CI. Commit the lockfile.
   - Use `#![forbid(unsafe_code)]` in every crate except the thin libmpv/platform FFI layer.
   - Keep dependencies lean.
9. **Fuzz** the M3U parser, the XMLTV parser and the Xtream JSON models (`cargo-fuzz`, or `proptest` at minimum).

## 4. Features

### MVP (must work end-to-end against a real provider)

- **Accounts**
  - Multiple profiles, of two kinds: Xtream Codes (name, server URL, username, password) and M3U (from a URL or a local file).
  - Add, edit, delete and switch between profiles.
  - Re-validate at startup and when switching.
  - Show status, expiry date (`exp_date` checked against the server time), and active/max connections.
  - If an M3U URL looks like `get.php?username=…&password=…`, offer to convert it to an Xtream login.
- **Live TV**
  - Categories, including the pseudo-categories *All* and *Favourites*.
  - A virtualised channel list with logos, plus search and sort (name, number, recently added).
  - An inline preview player and now/next EPG.
  - Channel up/down zapping and number entry.
  - The `.ts`/`.m3u8` choice, offering only what `allowed_output_formats` permits.
- **Movies and series**
  - A category grid with posters (virtualised, with lazily loaded, disk-cached images).
  - A detail page.
  - Seasons and episodes for series.
  - Play, and **resume from the last position**.
  - A "Continue watching" row.
- **Catch-up**
  - Channels with `tv_archive`, then programmes by day, then timeshift playback.
  - Use the default format plus a per-account fallback format; see the "Catch-up" section of `docs/PROTOCOL.md`.
- **Player (libmpv)**
  - Play/pause, seek (±10 s and ±60 s), volume, mute and fullscreen.
  - Audio and subtitle track selection.
  - Aspect ratio and zoom that **persist through rebuffering**.
  - Live streams reconnect with backoff. **The end of a VOD is "finished", not an error.**
  - An OSD with title and time.
- **Favourites** (live, movies and series), stored per account. Use a stable account id; do not key on the server URL alone.
- **Parental control.** Lock categories whose names match a configurable keyword list. Match **whole words, case-insensitive** ("adult", "xxx", …) so that "Sussex" does not match.
- **Settings:** stream format, time format (12/24h), User-Agent, PIN management, cache size, and the TMDB key (opt-in).
- **Navigation that works with keyboard, mouse and remote.**
  - Spatial focus (arrows / D-pad), Enter to select, and Esc **and** Backspace both go back, on every screen.
  - A visible focus ring.
  - No mouse-only screens.

### v1, after the MVP is solid

- An XMLTV EPG grid. Sources: the panel's `xmltv.php`, the M3U `url-tvg`, or a user-supplied URL. Convert times correctly using timestamps and the server timezone.
- Multiscreen: 2×2, with audio following focus, limited by `max_connections`.
- Opt-in TMDB enrichment.
- Import accounts and favourites from an existing IPTV Smarters Pro install. Read its localStorage (schema in `REVERSE_ENGINEERING.md` §5), move any passwords into the keychain, and get explicit confirmation first.
- External subtitles, and per-item `#EXTVLCOPT` headers (User-Agent and Referer, with Referer limited to the stream's own host).

## 5. Compatibility rules (summary; full detail in PROTOCOL.md)

- Base API URL: `{server}/player_api.php?username=…&password=…&action=…`.
  - Normalise the server URL: trim whitespace, strip trailing `/`, and add `http://` if there is no scheme.
  - For "All", **omit** `category_id`. Don't send `ALL`.
- Stream host:
  - Use `server_info.server_protocol`, `url`, and `port` or `https_port`.
  - Fall back to the login URL's host and port when those fields are empty.
  - The stream host may differ from the API host.
- URL templates:
  - Live: `/live/{u}/{p}/{id}.{ts|m3u8}`
  - Movie: `/movie/{u}/{p}/{id}.{container_extension}`
  - Episode: `/series/{u}/{p}/{episode_id}.{container_extension}`
- Xtream JSON is loosely typed. Numbers may arrive as strings or numbers, ids may be strings, and fields may be `null`. `backdrop_path` may be a string or an array, and `episodes` may be an object keyed by season or an array. Write lenient `serde` deserialisers with tests for each variant.
- EPG `title` and `description` are base64-encoded UTF-8. Decode leniently and fall back to the raw text on error.
- Use the start/stop **timestamps** for display and the server-local `start` string for timeshift.
- User-Agent:
  - Default to `opensmarters/<version>`.
  - Offer a preset, "IPTV Smarters compatible" (`IPTVSmartersPro`), for panels that whitelist player UAs.
  - Apply it to **both** API requests and mpv (`user-agent`).
- The M3U parser must:
  - work line by line and tokenise attributes properly
  - handle BOM, CRLF, and both `#EXTINF:-1` and `#EXTINF:0`
  - read `#EXTGRP`, `tvg-id`, `tvg-name`, `tvg-logo`, `tvg-chno`, `group-title`, `catchup*` and the header's `url-tvg`/`x-tvg-url`
  - classify VOD by `/movie/` and `/series/` paths and by file extensions
  - **never** inherit a URL from a previous entry

## 6. Recommended architecture

Cargo workspace:

```
crates/
  core/      # models, Xtream client, M3U + XMLTV parsers, URL builders, catch-up formats; no UI, no FFI
  store/     # SQLite (rusqlite, bundled): profiles (no secrets), favourites, resume points, caches with TTL; keyring wrapper
  player/    # safe wrapper over libmpv (libmpv2 crate or minimal FFI); the only crate allowed `unsafe`
  app/       # eframe/egui UI, focus/navigation system, image cache, settings
  fake-panel/# dev-only axum server: synthetic Xtream API + M3U + XMLTV + test streams (for tests and demos)
fuzz/
```

- Use `tokio` + `reqwest` (rustls) for async. Never block the UI thread; send results to the UI over channels.
- **Spike playback first (Phase 0).** Get libmpv rendering video inside the chosen UI on Windows. Try these in order and use the first that works reliably:
  - (a) the libmpv render API into the UI's OpenGL context or texture
  - (b) mpv in a native child window (`wid`)
  - (c) a dedicated mpv-owned player window controlled over the client API

  Record the choice in `docs/adr/0001-video-embedding.md`.
- License: **GPL-3.0-or-later** (compatible with GPL builds of libmpv). Add `LICENSE` and SPDX headers.

## 7. How to work

- `git init` at the start, then make small, frequent commits with clear messages. Never commit secrets, `_reference/`, binaries or test media.
- Keep `docs/PROGRESS.md` as a checklist (phase, done, next, known issues) so that a later session can pick up where you left off.
- Work in phases. A phase is done only when `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cargo deny check` all pass.

| Phase | Work | Done when |
|---|---|---|
| 0 | Workspace, CI config, libmpv setup, playback spike, ADR | A local `.ts` and `.m3u8` test stream (generated with ffmpeg by `fake-panel`) plays inside the app window on Windows |
| 1 | `core`: models, lenient deserialisers, Xtream client, URL builders, M3U/XMLTV parsers, catch-up formats, plus fuzz targets | Unit tests cover every quirk listed in PROTOCOL.md and fuzzers run clean for 5 minutes each |
| 2 | `store` and keychain; `fake-panel` with synthetic data (no real names or credentials) | Integration tests run the full login → categories → streams → URL flow against `fake-panel` |
| 3 | UI shell: focus/navigation system, profile management, Live TV with player and EPG | The whole flow works keyboard-only against `fake-panel` |
| 4 | Movies, series, resume / continue watching, catch-up, favourites, search/sort, parental control, settings | MVP checklist complete against `fake-panel` |
| 5 | Hardening and packaging: redaction review, mpv option review, threat model, release build bundling `libmpv-2.dll`, README | `docs/SECURITY.md` (threat model and mitigations) is written, and a release build runs from a clean folder |
| 6 | Real-provider test with me (below), then v1 features | — |

- **Real credentials.**
  - Never ask me to paste credentials into chat.
  - When you need a real-world test, ask me to run the app and log in through its own UI. Have me report what I see, along with logs, which are redacted by then.
  - Never write my server, username or password into any file, test, commit or log.
- When a decision is genuinely mine (UI look, scope trade-offs), ask briefly with a recommendation. Otherwise use your judgement and record notable decisions as ADRs.
- Be honest in reports: say what you verified, what you didn't, and what failed.

## 8. Definition of done (MVP)

- [ ] I can add my Xtream account and see my live categories, channels, movies and series.
- [ ] Live channels play with now/next EPG, and zapping works by keyboard.
- [ ] A movie and a series episode play, and resume from where I stopped after an app restart.
- [ ] Catch-up plays a past programme, or shows a clear message if the panel doesn't support it.
- [ ] An M3U URL and a local `.m3u` file both load and play.
- [ ] Favourites, settings and the PIN survive a restart. The password appears in Windows Credential Manager and nowhere else on disk (verify with a search of the data folder).
- [ ] Every screen is usable with a keyboard alone.
- [ ] Tests, clippy, `cargo deny` and `cargo audit` are clean. Fuzz targets exist and run.
- [ ] `README.md` covers build, run and libmpv setup. `docs/SECURITY.md` covers the threat model.
- [ ] A release build runs on a clean Windows 11 machine from an unzipped folder.

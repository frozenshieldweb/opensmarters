# Progress

This tracks the phases in [BUILD_PROMPT.md](BUILD_PROMPT.md#7-how-to-work).

| Phase | Status | Notes |
|---|---|---|
| 0: workspace, libmpv, playback spike | **done** | libmpv render API into egui (ADR-0001); hardened options; `tools/fetch-libmpv.ps1` |
| 1: core (protocol, parsers) | **done** | Lenient Xtream models, M3U/XMLTV parsers, redaction; 3 fuzz targets ran 5 min each with no crashes |
| 2: store, keychain, fake-panel | **done** | Encrypted cache, leak test, Argon2id PIN; fake-panel with quirk modes and generated media |
| Art direction | **done** | "Afterglow": `docs/DESIGN.md`, `assets/` (generated with Codex) |
| 3: UI shell, navigation, profiles, Live TV | **done** | See `docs/APP_ARCHITECTURE.md` |
| 4: VOD, series, resume, catch-up, favourites, parental, settings | **done** | 4A Movies/Series/Detail; 4B Catch-up/Guide/Search/Favourites; 4C Parental/Settings/Storage |
| 5: reviews, hardening, packaging, SECURITY.md | **done** | Security, code-correctness and UX reviews, then 4 fix batches (A playback/live, B services/security, C1 home/detail/shell, C2 guide/catch-up/search/favourites/settings). `docs/SECURITY.md`; `tools/package.ps1` release zip with licences |
| 6: real-provider test | **done** | The app tested end to end against the user's panel (below) |
| 7: recording, multiscreen, sleep timer, reminders, stable streams | **done** | Plan and contracts: [PHASE7.md](PHASE7.md). Foundation: schema v2 (recordings, reminders), connection budget (leases; viewing preempts downloads, never recordings), owned-media playback, `dump-cache`/`stream-record` into validated outputs. A: per-stream buffering (disk cache, auto-tune per channel), hardware decoding/buffer applied at once, pause and rewind live TV, stream details (D), "Streams at once". B: film/episode downloads (resume, preemption, offline playback), live and scheduled recordings (TS and HLS), clips from the buffer, Recordings screen and settings, parental control for saved items. C: sleep timer with fade and "Still watching?", app-wide prompts, Guide/Search actions (remind, switch, record, stop recording). D: multiscreen 1×2/2×2 with sound following focus and staggered starts. Not yet tested with the real provider |

Workspace gates at the end of Phase 6: `cargo fmt`, clippy (`-D warnings`, with and without `--all-features`), **508 tests**, `cargo deny check` and `cargo audit` all pass.

## Real-provider test (2026-10-20, the user's Xtream account)

**Protocol** (`crates/core/examples/live_probe.rs`):
- Login, status, expiry, `max_connections=3` and `allowed_output_formats` all parse.
- Catalogue: 85 live categories with 9,833 channels; 39 VOD categories with 35,482 movies; 37 series categories with 5,871 series. Zero items were skipped.
- "All movies" takes about 23 s, so the app loads per category.

**App, end to end** (in `--ephemeral` mode; credentials typed into the UI, never stored):

| Area | Result |
|---|---|
| Onboarding | Shows the unencrypted-HTTP warning |
| Home | Real posters, correct years and ratings, cleaned titles; the hero follows focus |
| Live TV | 9,833 channels with now/next. Inline preview, fullscreen, Ctrl+↓ zapping with the banner, "First channel" edge |
| Stall detection | **More than 50 s of live playback with no false stall reconnect** (the pre-fix build reconnected every 20 s) |
| Movie | Played 43 s, then Home showed "Resume 0:43" and the Continue watching rail |
| Catch-up | 27 channels, 14 days on the viewer's clock, newest first. The panel's `503` gives the "isn't available right now / Try another way" card |
| Guide | Real EPG grid, category chips |
| Connections | Every stream logged "ended for good" on exit, and the provider's active count went back to 0 once it caught up (it takes 1–3 min) |

**Codecs:** live 720p H.264 and 1080p HEVC/DTS both play with NVDEC.

## Known issues and follow-ups

- **Catch-up on this provider:** every timeshift variant returns `503`. The URLs are verified correct, so this looks like a provider-side problem. The format fallback exists for panels that need the other format.
- **XMLTV encoding:** XMLTV in a non-UTF-8 encoding stops at the first invalid byte. The fix would be quick-xml's `encoding` feature.
- **M3U:** EPG uses only the first `url-tvg`. There is no catch-up for M3U `catchup-source` templates yet.
- **Live TV:** the 1280×720 zap banner covers a good part of the preview video. Going back from the player to Catch-up stops the stream.
- **Search:** films and series are searched only in loaded categories. "Search all films and series" loads the rest on request.
- **TMDB:** only the setting and key exist; there is no enrichment on the detail page yet.
- **Compact layout:** below 960 pt the collapsed rail stays; there is no top bar.
- **Parental control:** an item whose category can't be resolved counts as locked while a PIN is set (fail closed). Continue watching checks only the primary category returned by `get_vod_info`.
- **Final QA polish left open (S3):**
  - Catch-up: after "Try another way" succeeds, focus lands on the channel list instead of the "Use it from now on" button.
  - Typing a locked channel's number says "Channel not found", while opening it by route says "Channel locked".
- **Phase 7** (details: `docs/PHASE7.md`, `docs/APP_ARCHITECTURE.md` §7, §7.1–7.4, §14):
  - **Not yet tested with the real provider:** recordings/downloads (each uses a connection), multiscreen (several connections at once; the panel counts stale connections after bursts), clips from the rewind buffer, libcurl range retries.
  - **libcurl has no read timeout** (it fetches every `http(s)` stream and HLS segment in this mpv build, `docs/LIBMPV.md` → Network): the progress tracker is the only stall detector. Turning libcurl off for live TS (`curl-enabled=no` per load) would give FFmpeg's transport reconnects that keep the rewind buffer; it changes the TLS stack and redirect handling, so it needs a security review first.
  - **Live rewind:** leaving the player while paused leaves the Live TV preview paused (shows "−mm:ss"); turning rewind on while playing gives the full window from the next channel; the demo's live channels are 20 s files, so a long rewind (and recording) can't be shown with them (fake-panel `live-endless` quirk exists for tests).
  - **Clips:** the length shown is an estimate; with Rewind live TV off the back buffer is 32 MiB (≈ 30 s at 8 Mbit/s).
  - **Recordings:** "record through the player" (no free connection, same channel) stops when the user changes channel; HLS with separate audio renditions, byte ranges or encryption is refused; resumes check `Content-Range` but not `ETag`; `--ephemeral` runs save into `%TEMP%\opensmarters-ephemeral\recordings`, never cleaned; file names in the recordings folder contain titles (also for locked items, SECURITY.md §4.10); the `hls` fuzz target doesn't link on Windows/MSVC (no target does).
  - **Multiscreen:** tile reconnects are not staggered; opening it reopens the main channel in tile 1 0.8 s after closing it (a third tile can then hit the panel's stale count on a 3-stream account); leaving it leaves the Live TV preview empty; the picker has no PIN prompt (unlock in Live TV); a channel in both an open and a locked category is left out of the picker; up to four idle tile engines stay alive until exit.
  - **Sleep timer / reminders:** multiscreen sound isn't faded; whether reminders fire while the window is minimised wasn't verified; a prompt that needs an answer waits until a focused text field loses the focus (by design).
- **Release:**
  - Builds are not Authenticode-signed.
  - CI actions are pinned by tag, not SHA.
  - The libmpv DLL comes from the pinned shinchiro build (see `docs/LIBMPV.md`).
  - The public repository is <https://github.com/frozenshieldweb/opensmarters> (`Cargo.toml` `repository`, shown in Settings → About, and `packaging/THIRD_PARTY.md`). Builds made before this was set, including the 0.1.0 zip in `dist/`, still name the old placeholder `github.com/opensmarters/opensmarters`: rebuild before publishing.

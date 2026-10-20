# libmpv setup

opensmarters plays video with **libmpv**, loaded at runtime from `libmpv-2.dll`. Nothing is linked at build time, so the workspace builds without libmpv. Without the DLL, the player reports:

> libmpv-2.dll not found (looked next to the executable, in vendor/libmpv and on PATH). See docs/LIBMPV.md for how to install it.

## Quick start (Windows x86_64)

```powershell
powershell -ExecutionPolicy Bypass -File tools\fetch-libmpv.ps1
```

The script downloads the pinned build below, checks its SHA-256, and extracts these files into `vendor/libmpv/` (gitignored):

| File | Purpose |
|---|---|
| `libmpv-2.dll` | The library the player loads at runtime |
| `include/mpv/*.h` | C headers (`client.h`, `render.h`, `render_gl.h`, `stream_cb.h`), kept for reference. The Rust FFI is hand-written in `crates/player/src/ffi.rs`. |
| `libmpv.dll.a` | MinGW import library. **Not used**, because we load at runtime. |
| `VERSION.txt` | The archive name, both SHA-256 hashes, the source URL and the fetch time |

Run it again with `-Force` to re-download. It does nothing if the pinned version is already there.

## Pinned build

| | |
|---|---|
| Source | [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake), the Windows builds linked from <https://mpv.io/installation/> |
| Archive | `mpv-dev-x86_64-20261018-git-01e2533f31.7z` (baseline x86-64; the `-v3` variant needs AVX2) |
| Archive SHA-256 | `d7f5621f42b6e161fe032df914a10bdece28a89df3e57c6c3e16a5a250929b5e` |
| `libmpv-2.dll` SHA-256 | `82bae3b6bf7bb737afc9b52d590b477983c715f3c63c1a361f9ee06523dc5dc2` |
| mpv | `v0.41.0-1205-g01e2533f3` (git snapshot after 0.41.0), client API 2.6 |
| FFmpeg | `N-127617-g94cba3150` (statically linked into the DLL) |
| Primary URL | `https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261018/<archive>` |
| Mirror | `https://downloads.sourceforge.net/project/mpv-player-windows/libmpv/<archive>` |

Notes:

- GitHub keeps only about 30 daily releases, so the primary URL will expire in roughly a month.
- The SourceForge mirror keeps the weekly builds. On 2026-10-20 it served a byte-identical file (same SHA-256).
- Both URLs are checked against the same hash, and nothing is extracted on a mismatch.
- No stable-release dev archive exists: the project publishes `mpv-dev-*` builds only as git snapshots.

### Runtime dependencies

The DLL imports only Windows system DLLs, plus **`vulkan-1.dll`**. That file is installed by every current GPU driver (NVIDIA, AMD, Intel). It can be missing on:

- VMs with no GPU driver
- Windows Server

If it is missing, loading fails with `failed to load …libmpv-2.dll`. Install the [Vulkan Runtime](https://vulkan.lunarg.com/sdk/home) or a GPU driver.

## Licence

This build is **GPL**:

- mpv's `gpl` feature is enabled (mpv is GPL-2.0-or-later in that configuration).
- The DLL statically contains GPL components such as x264.
- It also contains LGPL FFmpeg libraries and many permissive libraries (libass, libplacebo, zlib, libcurl, …).

opensmarters is GPL-3.0-or-later, which is compatible. **When distributing a release that bundles `libmpv-2.dll`**, you must:

- ship the GPL text
- point to the corresponding source:
  - mpv at commit `01e2533f31`
  - FFmpeg at `94cba3150`
  - the build recipe at shinchiro/mpv-winbuild-cmake, tag `20261018`

  Our own `LICENSE` covers the first point. Add a `THIRD_PARTY.md` with the source links to the release zip. This is tracked for Phase 5.

## Where the app looks for the DLL

The player searches these locations in order, using absolute paths only (`opensmarters_player::libmpv_search_paths()` returns the list):

1. `PlayerConfig::libmpv_path(..)`, if the app sets one.
2. **The executable's directory.** This is the location for release builds: ship `libmpv-2.dll` next to `opensmarters.exe`.
3. `<repo>/vendor/libmpv/libmpv-2.dll`. Debug builds only; the path is baked in at compile time.
4. Each absolute directory on `PATH`.

The current working directory is **never** searched, which closes off DLL planting. The DLL is loaded with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`, so its own dependencies resolve only from:

- its own directory
- System32

The library is never unloaded.

## Manual install (if the script cannot run)

1. Download `mpv-dev-x86_64-20261018-git-01e2533f31.7z` from one of the URLs above.
2. Check the hash. It must equal the archive SHA-256 in the table above:

   ```powershell
   (Get-FileHash .\mpv-dev-x86_64-20261018-git-01e2533f31.7z -Algorithm SHA256).Hash
   ```

3. Extract it. Use Windows' `C:\Windows\System32\tar.exe -xf <archive>` (bsdtar reads `.7z`) or 7-Zip. Git Bash's `tar` cannot read `.7z`.
4. Copy `libmpv-2.dll` to `vendor/libmpv/` for development, or next to the executable for a release.

## Upgrading the pinned build

1. Pick a newer `mpv-dev-x86_64-*.7z` that exists on **both** GitHub and SourceForge.
2. Update `$ReleaseTag`, `$Version` and `$Sha256` in `tools/fetch-libmpv.ps1`.
3. Run `tools/fetch-libmpv.ps1 -Force`.
4. Run `cargo test -p opensmarters-player`. The integration tests in `crates/player/tests/mpv.rs` exercise the real DLL. Among other things, they fail if any best-effort hardening option is no longer recognised.
5. Run the spike, as described below.
6. Update this file.

## mpv options set by the player (security rule #6)

All options are applied with `mpv_set_option*` **before** `mpv_initialize`; they are defined in `crates/player/src/config.rs`.

- **Mandatory** options make `Player::new` fail if they cannot be set.
- **Best-effort** options are skipped if this mpv build does not know them. `Player::skipped_options()` lists any that were skipped; with the pinned build the list is empty.

### Hardening (mandatory)

| Option | Value | Why |
|---|---|---|
| `config` | `no` | Read no `mpv.conf`, `input.conf`, `script-opts` or watch-later files from any config directory |
| `load-scripts` | `no` | No user Lua/JS scripts |
| `ytdl` | `no` | Never run youtube-dl/yt-dlp on a URL (external process) |
| `osc` | `no` | No built-in on-screen controller script |
| `input-default-bindings`, `input-vo-keyboard`, `input-terminal`, `terminal` | `no` | mpv binds no keys and reads no terminal; the app binds its own keys |
| `input-ipc-server` | *(empty)* | No JSON IPC socket or pipe |
| `load-unsafe-playlists` | `no` | A playlist fetched from the network may only reference network URLs (mpv's stream-origin rules). Verified: an HTTP-served playlist that points at `file:///…` is refused (`No protocol handler found`). |
| `use-filedir-conf`, `resume-playback`, `save-position-on-quit` | `no` | Read or write no per-file config or watch-later state |
| `ordered-chapters` | `no` | Never follow MKV segment links to other local files |
| `demuxer-lavf-o` | `protocol_whitelist=http,https,tls,tcp,crypto,data,httpproxy` | Limits what libavformat may open **itself**: HLS variant playlists, segments and keys. See below. |
| `stream-lavf-o` | the same whitelist | The same limit for top-level URLs opened through FFmpeg's protocols |
| `tls-verify` | `yes` | HTTPS certificate verification. mpv's default is **no**. |

### Embedding and playback (mandatory)

| Option | Value | Why |
|---|---|---|
| `vo` | `libmpv` | Render API only; mpv never opens its own window |
| `idle` | `yes` | The core stays alive between files |
| `force-window` | `no` | — |
| `keep-open` | `no` | When a VOD ends, mpv emits `END_FILE(reason=eof)` and goes idle. The crate surfaces this as `PlayerEvent::EndFile(EndReason::Eof)`, which means *finished*, not an error. |
| `hwdec` | `auto-safe` | Default; `Hwdec::Disabled` sets `no`. NVDEC was selected on the test machine. |
| `network-timeout` | `15` | Seconds; configurable from 1 to 600 |
| `demuxer-max-bytes`, `demuxer-max-back-bytes` | `64MiB`, `16MiB` | Configurable cache caps |
| `volume` | `100` | Initial volume (configurable) |
| `user-agent` | only if the config sets one | Validated `UserAgent`; overridden per load |
| `ao` | `null` | Only with `audio_output(false)` (tests, muted previews) |
| `vid` | `no` | Only with `video_output(false)` (headless and audio-only) |

### Best effort

| Option | Value | Why |
|---|---|---|
| `input-builtin-bindings`, `input-media-keys` | `no` | No implicit key handling, including the Windows media keys |
| `load-stats-overlay`, `load-console`, `load-osd-console`, `load-auto-profiles`, `load-select`, `load-commands`, `load-positioning`, `load-context-menu` | `no` | Disable every built-in script |
| `autoload-files`, `sub-auto`, `audio-file-auto`, `cover-art-auto` | `no` | Never pull sibling files from disk |
| `embeddedfonts` | `no` | Skip fonts embedded in media, which reduces the FreeType attack surface |
| `osd-level`, `osd-on-seek` | `0`, `no` | The app draws its own OSD |
| `video-timing-offset` | `0` | Together with `BLOCK_FOR_TARGET_TIME=0`, rendering never blocks the UI thread |
| `audio-client-name` | `opensmarters` | Name shown in the Windows volume mixer |

### The protocol whitelist

When libavformat opens a URL, mpv passes `protocol_whitelist` through `demuxer-lavf-o`, along with the network options.

- **HLS:** the HLS demuxer opens variant playlists, segments and AES keys through the same context, so the whitelist applies to every nested open. A remote `.m3u8` cannot reach `file:`, `pipe:`, `subfile:`, `concat:` or other protocols.
- **Verified:** a remote `.m3u8` whose segment is `file:///…/seg_000.ts` fails with `Protocol 'file' not on whitelist 'http,https,tls,tcp,crypto,data,httpproxy'!`. This is covered by the integration test `remote_playlist_cannot_reach_local_files`. The spike log shows the same.
- **The list grows with the policy.** If the app enables `UrlPolicy { allow_rtmp | allow_rtsp | allow_udp }`, the matching FFmpeg protocols are added. `file` is added only with `PlayerConfig::allow_local_files(true)`, which is for development and tests.
- **libcurl:** this mpv build has mpv's libcurl stream enabled, and the logs show plain `http(s)://` media fetched by `curl:` rather than by FFmpeg's `http` protocol.
  - The FFmpeg whitelist therefore governs only what libavformat opens itself.
  - The curl path is still constrained, in three ways:
    - Only `http`/`https` URLs reach mpv at all (`PlayableUrl`).
    - An HTTP `302` to `file:///…` is refused. The integration test `http_redirect_to_a_local_file_is_refused` covers this; an ordinary redirect still plays.
    - mpv's origin checks refuse local streams that are referenced from network content.
  - `tls-verify=yes` and the `user-agent`/`referrer` options apply to the curl path. This was verified:
    - `https://expired.badssl.com` and `https://self-signed.badssl.com` are rejected with "TLS certificate verification failed".
    - A valid HTTPS HLS stream plays.
    - The integration test asserts that the playlist and every segment request carry the configured User-Agent and Referer.

### How URLs and headers reach mpv

- **The URL** (`PlayableUrl`, http/https only by default) is passed as the single `url` argument of `loadfile`. The call goes through `mpv_command_node_async` with named arguments: `{name: loadfile, url, flags: replace, options: {start}}`.
  - Nothing is string-built.
  - The only per-file option is the resume position, `start`, a number we format ourselves.
  - Named arguments are used instead of positional `argv` because mpv 0.38 inserted a positional `index` parameter before `options`. Named arguments cannot be misread.
- **User-Agent and Referer** are validated (`UserAgent`, `Referrer`: printable ASCII, no CR/LF/control characters, at most 1 KiB, and the referrer must be an http(s) URL). They are set with `mpv_set_property_async("user-agent" / "referrer")` immediately before `loadfile`. libmpv processes a client's requests in order.
  - We use the async form of `mpv_set_property_string` because `load()` may run on the GL/render thread, where libmpv's synchronous calls can deadlock against the renderer.

### Logging

- The crate never logs URLs.
- mpv log lines (`PlayerEvent::Log`) are forwarded **raw** and contain URLs with credentials, for example `Playing: http://host/live/user/pass/1.ts`. The app must redact them.
- libmpv routes FFmpeg's process-global log callback to only **one** mpv core. With several `Player`s alive (for example multiscreen), FFmpeg-originated lines may arrive on another player's channel.

## Verifying a build by hand

Run these from the repo root:

```powershell
powershell -ExecutionPolicy Bypass -File tools\gen-test-media.ps1      # -> testdata/generated/player/
$env:CARGO_TARGET_DIR = "target/player"
cargo run -p opensmarters-player --example spike -- testdata/generated/player/test-tracks.mkv
python -I -m http.server 8765 --bind 127.0.0.1 --directory testdata/generated/player
cargo run -p opensmarters-player --example spike -- http://127.0.0.1:8765/hls/index.m3u8 `
    --no-audio --screenshot-after 4 --screenshot-out shot.png
```

Note that Python's `http.server` does not support HTTP Range requests, so mpv cannot seek in a plain `.ts` served by it. HLS seeks fine.

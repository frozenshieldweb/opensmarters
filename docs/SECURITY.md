# Security

opensmarters treats everything a provider sends as hostile input: a panel, a playlist or
a network attacker on plain HTTP must never be able to run code on the viewer's PC.
Security is the headline requirement ([BUILD_PROMPT.md §3](BUILD_PROMPT.md#3-non-negotiable-security-requirements)).
This document is the threat model, the list of mitigations with pointers into the code, the
known residual risks, and a checklist to re-verify all of it.

- **Report a vulnerability:** see [Reporting a vulnerability](#9-reporting-a-vulnerability).
- **Last review:** 2026-10-20 (Phase 5 security review). Findings and their status are in
  [§7](#7-security-review-2026-10-20).

---

## 1. What we protect (assets)

| Asset | Where it lives | Why it matters |
|---|---|---|
| Xtream password | OS keychain (`profile/<uuid>/password`) | Pays for the subscription; reused by people elsewhere |
| M3U playlist URL | OS keychain (`profile/<uuid>/m3u_url`) | Usually `get.php?username=…&password=…`: the credentials themselves |
| EPG URL override | OS keychain (`profile/<uuid>/epg_url`) | `xmltv.php?username=…&password=…` |
| TMDB API key (opt-in) | OS keychain (`tmdb_api_key`) | The user's own API key |
| Cache master key | OS keychain (`cache_key`) | Decrypts the response cache (which holds playlists with credentials) |
| Parental PIN | SQLite, Argon2id PHC hash only | Keeps locked categories from a child; people reuse PINs |
| Stream / API URLs | Memory only (`PlayableUrl`, `url::Url`) | Every Xtream stream URL embeds username and password |
| Viewing history, favourites | SQLite (plain) | Private, not secret |
| Recordings, downloads, clips | The recordings folder (default `Videos\opensmarters`), job list in SQLite (ids, titles, file names; never URLs) | Private, not secret; the folder is a place the app writes provider bytes to |
| The PC itself | — | A media player must not become a code-execution vector |

Usernames and server URLs are stored in SQLite (not secret on their own, but never logged
next to a password).

## 2. Who we defend against (actors)

| Actor | Capabilities | In scope? |
|---|---|---|
| **Malicious provider / playlist author** | Controls every byte of Xtream JSON, M3U, XMLTV, EPG text, images, stream content, HTTP status codes, redirects and headers; can point stream/image URLs at any host, including LAN addresses | **Yes, primary** |
| **Network attacker on plain HTTP** | Many panels are `http://` only: reads credentials, rewrites any response (same power as a malicious provider for that session) | Yes: same defences as above; credential exposure is warned about, not prevented |
| **Network attacker against HTTPS** | Can intercept but has no trusted certificate | Yes: TLS verification is always on, no downgrade |
| **Child at the keyboard** | Uses the app (mouse, keyboard, remote), can restart it, can type command-line flags into a shortcut, may know the provider password | Yes, for parental control |
| **Other local user on the same PC** | Can write to shared/badly-ACLed folders (e.g. a `C:\Tool` on `PATH`) | Partly: DLL planting and file permissions |
| **Malware running as the same user** | Can read the keychain, edit the database, inject into the process | **No** (nothing local can stop it) |
| **Supply chain** | Compromised crate, compromised libmpv build, compromised CI action | Yes: pinning, audits, review |

## 3. Trust boundaries

```text
 provider / playlist / EPG host (untrusted)                    TMDB (opt-in, user key)
        │ HTTP(S) JSON, M3U, XMLTV, images                          │
        ▼                                                           ▼
 ┌──────────────── crates/core (forbid unsafe) ───────────────────────────────┐
 │ net::HttpClient: rustls + platform verifier, timeouts, 256 MiB cap after   │
 │ gzip, redirect policy (≤5, no https→http)                                   │
 │ de / model (lenient JSON) · m3u (line parser, caps) · xmltv (streaming,    │
 │ no DTD, caps) · urls (percent-encoded builders) · redact (log scrubbing)   │
 └───────────────┬───────────────────────────────┬────────────────────────────┘
                 │ typed models (text only)      │ URLs (secret)
                 ▼                               ▼
 ┌──── crates/app (forbid unsafe) ────┐   ┌──── crates/player (only unsafe crate) ────┐
 │ egui: every provider string is     │   │ PlayableUrl allowlist (http/https)         │
 │ painted as text, capped, controls/ │──▶│ UserAgent / Referrer validated             │
 │ bidi stripped; images decoded with │   │ hardened mpv options, ffmpeg whitelist     │
 │ limits; parental gates; logging    │   │ ────────── FFI boundary ──────────         │
 │ through RedactingWriter            │   │ libmpv-2.dll (C: mpv, FFmpeg, libcurl,     │
 └──────┬──────────────────┬──────────┘   │ OpenSSL, libass…)  ◀── stream bytes from  │
        │                  │              │                         provider hosts     │
        ▼                  ▼              └────────────────────────────────────────────┘
 OS keychain          SQLite + logs + image cache (%LOCALAPPDATA%\opensmarters\…)
 (secrets only)       (no secrets; cache values encrypted)
```

The most important boundary is the **FFI boundary to libmpv**: FFmpeg's demuxers and
decoders parse provider-controlled bytes in C. Everything before it is memory-safe Rust.

Besides the provider's hosts, the app talks to two others, both only on the user's request:
TMDB (the user's own key, off by default) and **GitHub's API** for "Check for updates"
(Settings › About, or the weekly automatic check, off by default; §4.5). Their answers are
untrusted input too.

## 4. Mitigations

### 4.1 Provider data never becomes code or markup

- **No web engine.** The UI is native egui (`crates/app`); there is no HTML, JavaScript,
  CSS or webview anywhere, so the XSS→RCE class of bugs does not exist. Provider
  strings are painted as text.
- **Display hygiene.** `crates/app/src/ui/text.rs` (`cap`, `cap_paragraph`) caps lengths and
  replaces control and bidi-override characters before display (no RTL spoofing of channel
  names). EPG base64 is decoded leniently (`crates/core/src/epg.rs`).
- **No shell, no string-built commands.** mpv is driven with node-based commands; the URL
  is one argument (`issue_load` in `crates/player/src/player.rs`). The only processes the
  app starts are `explorer` for "open data folder" (`crates/app/src/platform.rs`), the
  system browser for a validated YouTube watch URL after confirmation
  (`crates/app/src/screens/detail/trailer.rs`), and the system browser for the release
  page of a newer version when the user chooses "Get the update" (a URL built locally,
  never taken from GitHub's answer; `crates/app/src/services/updates.rs`); all pass a
  single argument, no shell.
- **Trailers** are never embedded: only `https://www.youtube.com/watch?v=<11 chars>` is
  ever handed to the OS.

### 4.2 Hostile-input parsing (bounded, panic-free)

| Input | Parser | Limits | Code |
|---|---|---|---|
| Any HTTP body | `net::HttpClient::get_bytes` | 256 MiB after decompression (images 10 MiB); connect 10 s, read 30 s, total 300 s; ≤ 5 redirects | `crates/core/src/net.rs` |
| Xtream JSON | serde + lenient helpers | serde_json recursion limit (128); malformed items skipped and counted; error messages capped at 200 bytes, never the body | `crates/core/src/de.rs` |
| M3U | line parser | 256 MiB, 256 KiB/line, 1 M entries, **64 attributes/line, 16 XMLTV URLs, 256 header attributes**; never inherits a URL | `crates/core/src/m3u.rs` |
| XMLTV | quick-xml streaming, gzip | 1 GiB decompressed, 16 MiB per text node, 8 KiB per field, 16 values/field, 100 k channels, 5 M programmes; **DTDs ignored** (no XXE, no entity expansion); no recursion | `crates/core/src/xmltv.rs` |
| Images | `image` crate (png, jpeg; pure Rust) | 10 MiB download, 8192 px per side, 256 MiB decode allocation, 6 concurrent, downscaled to ≤ 1920 px | `crates/app/src/services/images.rs` |
| mpv node trees | `NodeOut::from_raw` | depth 16, 4096 items | `crates/player/src/node.rs` |
| Backup import | serde + store validation | 64 MiB, 1000 profiles, 100 k favourites/profile, all-or-nothing transaction | `crates/store/src/export.rs` |
| Update check answer (GitHub "latest release") | `updates::parse_release` | **256 KiB**; connect 10 s, idle 15 s, total 20 s; ≤ 3 redirects, https only (never → http); only `tag_name`, `draft`, `prerelease` read; tag ≤ 64 bytes, exactly `v?MAJOR.MINOR.PATCH(-PRE)?` with `u32` parts (no build metadata, no leading zeros); drafts/pre-releases skipped; no URL or text from the answer is used or shown | `crates/app/src/services/updates.rs` |
| Media downloads / recordings | `download::Downloader` (streaming) | connect 10 s, read-idle 30 s, no overall deadline (live); ≤ 5 redirects, http(s) only, never https → http; identity encoding; `206` checked against `Content-Range`; no size cap but a free-space guard | `crates/core/src/download.rs` |
| HLS playlists (recording HLS-only channels) | `hls::parse` | 4 MiB, 16 KiB/line, 20 000 segments (newest kept), 256 variants, 8 KiB/URI; web URIs only; encrypted / byte-range streams refused | `crates/core/src/hls.rs` |

Robustness: `proptest` suites run in `cargo test` (`crates/core/tests/robustness.rs`) and
four `cargo-fuzz` targets exist (`fuzz/fuzz_targets/{m3u,xmltv,xtream_json,hls}.rs`).

### 4.3 Playback (libmpv) hardening

- **One entry point.** Media reaches mpv only as a `PlayableUrl`
  (`crates/player/src/playable.rs`): absolute `http`/`https` URL with a host (plus
  `rtmp`/`rtsp`/`udp` only if a `UrlPolicy` enables them; the app does not), checked on the
  raw text *and* after WHATWG normalisation, ≤ 8 KiB, no control/whitespace/zero-width/bidi
  characters, no backslashes. `file:`, `edl://`, `lavfi://`, `av://`, `memory://`, `fd://`,
  `appending://`, `concat:`, `subfile:` … are rejected. The player re-checks its own
  policy at `load` time.
- **Headers.** `UserAgent` and `Referrer` accept printable ASCII only (no CR/LF header
  injection), ≤ 1 KiB, and are set as whole properties, never as option strings. M3U
  `#EXTVLCOPT` values that fail validation are dropped; a referrer is used only for the
  stream's own host (`stream_headers` in `crates/app/src/playback/request.rs`).
- **Options** (all before `mpv_initialize`; mandatory ones abort player creation): `config=no`,
  `load-scripts=no`, `ytdl=no`, no input bindings/terminal/IPC, `load-unsafe-playlists=no`,
  `use-filedir-conf=no`, `ordered-chapters=no`, no resume files, `tls-verify=yes`, and an
  FFmpeg `protocol_whitelist` (`http,https,tls,tcp,crypto,data,httpproxy`) for top-level and
  nested opens; best effort: no auto-loaded subtitles/audio/cover art, no embedded fonts, no
  built-in OSC/console/stats/select scripts. Full table: [LIBMPV.md](LIBMPV.md#mpv-options-set-by-the-player-security-rule-6).
- **Stream origin.** mpv tags everything opened from the network: a remote playlist, EDL,
  HLS manifest or `hex://`/`memory://` blob it contains can only open other network URLs.
  The pinned build's HTTP backend is **libcurl**, which verifies certificates (self-signed
  refused, public CA accepted) and does not follow redirects to `file:`.
- **Verified against the real DLL** (`crates/player/tests/mpv.rs`): HLS segment pointing at
  a local file; 302 → `file://`; remote M3U with `file://`, raw paths, `edl://`, lavfi,
  `archive://`, `slice://`, `mf://`, `dvd/bd/cdda`, `fd://` and EDLs smuggled through
  `hex://`; remote file with an EDL header; redirects to 13 non-HTTP protocols. None plays
  or touches local files.
- **The player crate never logs URLs**; mpv log lines are forwarded raw and redacted by the
  app before logging (`crates/app/src/playback/mod.rs`).

### 4.4 Credentials

- **Keychain only** (`crates/store/src/secrets.rs`): Windows Credential Manager via
  `keyring`, one generic credential per key, all calls serialised. No column for any
  secret exists (`crates/store/src/db.rs`); server URLs with userinfo/query and item ids
  that look like URLs are rejected. `crates/store/tests/leak.rs` plants passwords, M3U/EPG
  URLs, a TMDB key and PINs through every API, then scans the raw `.db`, `-wal` and `-shm`
  bytes (UTF-8 and UTF-16) for them.
- **In memory**: passwords are `secrecy::SecretString` (zeroised on drop); keychain bytes
  and cache keys use `Zeroizing`. Strings that travel through URLs, the redactor and egui
  text fields are not zeroised (out of our control).
- **Never printed**: `Debug` of `Client`, `StreamUrls`, `PlayableUrl`, `Referrer`,
  `XtreamM3uLink`, `M3uEntry`, `Playlist`, `Redactor`, secret stores and `CachedValue` is
  redacted; the login response's echoed `user_info.password` is not even deserialised;
  `Error`/`Display` values never contain URLs (`core::Error::InvalidUrl` carries a reason,
  network messages have the URL stripped and are redacted).
- **Log redaction** (`crates/core/src/redact.rs`), applied to every line by
  `RedactingWriter` (`crates/app/src/logging.rs`) and again to mpv lines:
  1. *structural*: query values of `username`, `password`, `user`, `pass`, `pwd`, `passwd`,
     `token`, `auth`, `api_key`, `apikey`, `access_token`, `secret`; the two segments after
     `/live/`, `/movie/`, `/series/`, `/timeshift/`; the short live form
     `host/{user}/{pass}/{id}`; URL userinfo;
  2. *known secrets*: every registered password, username, playlist/EPG URL **and the
     credentials embedded in those URLs**, in raw, percent-encoded (four encode sets, both
     hex cases), form-encoded, `Debug`-escaped and JSON-escaped spellings.
  Verified end to end: the fake panel's `special` user (password `p@ss/w#rd %&+`) logged
  in through the UI with `OPENSMARTERS_LOG=trace --mpv-log debug`, played a stream; the
  password appears in no encoding in the log file or stderr (also for an M3U profile whose
  streams use the short form, see SR-01).
- **Percent-encoding**: every query value (form encoding) and path segment (RFC 3986
  unreserved kept) is encoded (`crates/core/src/xtream.rs`, `crates/core/src/urls.rs`).
- **Exports** contain no secrets, no PIN hash and no history (`crates/store/src/export.rs`,
  `crates/app/src/services/backup.rs`); they do contain server URLs and usernames.

### 4.5 Network

- **TLS**: reqwest with rustls (aws-lc-rs) and the platform verifier (Windows certificate
  store); no `danger_*` option exists anywhere; OpenSSL/native-tls are banned in
  `deny.toml`. mpv: `tls-verify=yes`.
- **No downgrade**: a redirect from an `https` request to `http` fails with
  `Error::InsecureRedirect` (`redirect_policy` in `crates/core/src/net.rs`).
- **HTTP is allowed but visible**: plain-HTTP accounts show an "Unencrypted" warning
  during onboarding and in the status cluster.
- **No Referer, no cookies** on API/playlist/image requests.
- **No telemetry**: the only hosts contacted are the panel/playlist host, the stream and
  image hosts it returns, an EPG host the user or playlist names, `api.themoviedb.org`
  (only after the user enters their own key) and `api.github.com` (only when the user
  chooses **Check for updates** or turns on the weekly automatic check). The trailer
  button opens youtube.com in the browser after a confirmation. No API key is embedded.
- **Update check (notify only)** (`crates/app/src/services/updates.rs`): one
  `GET https://api.github.com/repos/<owner>/<repo>/releases/latest` (owner/repo from the
  build's `CARGO_PKG_REPOSITORY`; anything that isn't a GitHub repository turns the
  feature off) with `User-Agent: opensmarters/<version>` and
  `Accept: application/vnd.github+json` and nothing else (no cookies, no `Referer`, no
  identifier; an integration test checks the exact header set). GitHub sees the IP
  address. Threat model: the answer is untrusted (a compromised or impersonated GitHub,
  or a bug): it is size-capped and only a strictly validated version number is taken
  from it (§4.2); the "Get the update" link is built locally from `CARGO_PKG_HOMEPAGE`
  (if it is a plain https URL) or `https://github.com/<owner>/<repo>/releases/tag/v<X.Y.Z>`,
  never from `html_url` or asset links; release notes are never rendered; nothing is
  downloaded, installed or run (the user runs the new setup; the installer has no network
  code at all, §4.11). The automatic check is **off by default**, runs at most weekly
  (after a failure, at most daily), and is silent on failure. Its state (last check,
  latest version seen, dismissed version) is machine-local and never exported. The
  dev/test URL override (`OPENSMARTERS_UPDATE_URL`) exists only in builds with the
  `harness` feature (SR-03), and even there accepts only https or http to loopback.
- **SSRF to the LAN is accepted**: a provider can make the app fetch images or streams from
  `127.0.0.1` or LAN addresses (GET only, nothing reflected back). This is inherent to
  IPTV and acceptable for a desktop player.

### 4.6 Local storage

- **Location**: `%LOCALAPPDATA%\opensmarters\opensmarters\data\` (database, `logs\`,
  `images\`), protected by the per-user profile ACL; on Unix the directory is `0700` and
  the database `0600`.
- **SQLite hardening**: `SQLITE_DBCONFIG_DEFENSIVE`, `trusted_schema=OFF`, STRICT tables,
  `secure_delete=FAST`, forward-only migrations; a newer schema is refused.
- **Encrypted cache** (`crates/store/src/cache.rs`): random 256-bit master key in the
  keychain; per-purpose keys derived with keyed BLAKE2b (personalisation strings);
  XChaCha20-Poly1305 with a fresh random 192-bit nonce per write (no nonce-reuse risk);
  the row id (keyed BLAKE2b of scope + key) is the AEAD associated data, so rows cannot be
  swapped; cache keys themselves are never stored. Tampered rows fail authentication and
  are dropped. A local attacker could roll a row back to an older ciphertext of the same
  key (out of scope).
- **PIN** (`crates/store/src/parental.rs`): 4–8 digits; Argon2id m = 64 MiB, t = 3, p = 4
  (RFC 9106 second recommendation), random salt, constant-time verify with parameters
  bounded (a tampered hash cannot ask for > 1 GiB). Persisted rate limit: 4 free tries,
  then 30 s, doubling to 1 h per further failure; never reset except by a correct PIN;
  moving the clock backwards cannot lengthen a lockout. Once the lockout reaches 1 h, a
  4-digit PIN takes about 200 days of attempts at the keyboard on average.
- **Backups**: import is PIN-gated, validated field by field and applied in one
  transaction; machine-local settings (PIN length, window, search history, update-check
  state) are stripped.
- **Logs**: `logs\opensmarters.log`, rotated at 5 MiB with one old file, redacted at write
  time. "Copy log" copies a redacted tail.

### 4.7 Parental control

Categories whose names match whole-word keywords (NFKC, case-insensitive, so "Sussex" does
not match "sex") or that are locked manually need the PIN in Live TV, Movies, Series,
Guide, Catch-up and Search; "All"/"Favourites" lists in Live/Movies/Series hide their
items. The PIN also guards the parental settings page, profile add/edit/delete and backup
import. "Forgot PIN" requires an Xtream password or M3U address stored on the PC.

Locks are also enforced **where content is used**: Session::live_request,
movie_request, episode_request, catchup_request(_with) and 
esume_request refuse
an item when *any* of its categories is locked (AppError::Locked, the UI asks for the
PIN and retries); items whose categories aren't known yet are refused until a cached
get_vod_info/get_series_info look-up settles them (crates/app/src/services/session/locks.rs).
Unlocks live only in memory and expire after 30 minutes without user input, on profile
switch and on restart. Artwork of locked categories is never written to the image disk
cache, and the cache is wiped when a PIN is set. The "Forgot PIN" limiter is persisted.

**Known gaps** at the time of the review are listed in §7 (SR-02's screen part, SR-03).

### 4.8 Supply chain

- `Cargo.lock` committed; `cargo deny check` (advisories, licences, bans, sources:
  crates.io only, no git) passes, with five documented Linux-only advisory exceptions.
- `#![forbid(unsafe_code)]` in `core`, `store`, `app`, `fake-panel`, `web`; `unsafe` exists only in
  `crates/player` (FFI), where clippy's `undocumented_unsafe_blocks` is `deny` and every
  block has a `// SAFETY:` comment.
- Dependency closure on Windows: core ≈ 120 crates, store ≈ 100, player ≈ 90, app ≈ 300
  (normal + build). Native code compiled from source: SQLite (bundled), aws-lc (TLS crypto).
- libmpv (`tools/fetch-libmpv.ps1`, [LIBMPV.md](LIBMPV.md)): a pinned shinchiro build from
  the mpv.io-linked GitHub releases or the SourceForge mirror; the archive's SHA-256 is
  pinned and checked before extraction; nothing is extracted on mismatch.
- The fake panel is dev-only and compiled into the app only with `--features demo`.

### 4.10 Recordings and downloads (writing provider bytes to disk)

Phase 7 lets the app save what a provider sends: films and episodes for offline viewing,
live recordings, and clips from the player's buffer (`crates/app/src/dvr/`). The provider
controls the bytes, the titles that become file names, the HTTP answers and the playlists.

- **Where files go:** only the recordings folder (a user setting, default
  `Videos\opensmarters`; `--ephemeral` uses a temporary folder). Every path is
  `folder/<bare name>` (`dvr::files::path_in`); nothing else is created, renamed or deleted.
- **Names:** built only by `dvr::file_name` / `dvr::files::download_name`: provider text
  reduced to letters, digits, spaces and `- _ ( ) ' , &`, capped at 100 characters,
  reserved device names prefixed, a fixed extension from a short list. Checked again by
  the store (`recordings::validate_file_name`) and, for files mpv writes, by
  `MediaOutput::new` (no separators, `..`, `:`, `%`, control characters; `.ts/.mkv/.mp4`).
- **Never overwrite:** a new `.part` is created with `create_new`; names are made unique
  against the database, final files and `.part` files; mpv only writes a `MediaOutput`
  (a file that doesn't exist yet). Deletion removes only regular files (not folders or
  links) named by a valid name.
- **Content check before keeping anything:** the first 4 KiB must be a known container
  (`opensmarters_player::sniff_container`: TS, MKV/WebM, MP4/MOV, AVI, FLV, MPEG-PS, ASF).
  An HTML page, a playlist or anything else deletes the part file and fails with "The
  provider sent something that isn't a video". HLS segments are checked one by one (TS
  sync bytes / ISO-BMFF boxes); bad segments are skipped. Saved files play only through
  `PlayableUrl::from_owned_media` (inside the folder, sniffed again), so a "film" that is
  really a playlist can't make mpv open other files or URLs.
- **No URLs on disk:** rows hold provider ids; stream URLs (with credentials) are rebuilt
  from the profile when a job starts and never stored or logged unredacted.
- **Network:** connect and read-idle timeouts, redirects only to `http`/`https` (at most
  5, never from `https` down to `http`), `Range` resumes validated against `Content-Range`
  (a mismatch restarts the file; a server that ignores the range sends it again from 0).
  No response size cap (a film is gigabytes), but jobs stop when free space drops below
  the "keep free" setting (2 GB by default).
- **HLS:** playlists are hostile input (capped, fuzzed, web URIs only, resolved against
  the playlist's own address); encrypted streams are refused rather than half-supported.
- **Parental control (SR-02):** the lock gates *starting* a job (Download, Record: the
  viewer must be allowed to use the item now, else the PIN) and *showing or playing* the
  result (the Recordings screen shows a locked item as "Locked", without title or artwork,
  and Play asks for the PIN; recordings and clips are locked by their channel, episodes
  by their series; offline, the PIN is always needed). The transfer itself doesn't ask:
  the engine builds its URLs with unchecked session helpers
  (`services/session/dvr.rs`, used only by `crate::dvr`), so a job started by someone who
  had the PIN still finishes after a restart or the 30-minute re-lock. Residual: the
  recordings folder itself (Explorer, "Open folder") shows file names (titles).
- **Connections:** every job holds a lease of the connection budget, so recording never
  pushes the account over `max_connections`; clips use the player's buffer (no extra
  connection).
- **Residual:** saved files are later parsed by FFmpeg when played (the same risk as
  streaming them); a provider can fill the disk up to the guard; segment URLs may point
  to other hosts or use plain `http` from an `https` playlist (as when mpv plays HLS).

### 4.9 Windows specifics

- **libmpv location**: absolute paths only — `PlayerConfig::libmpv_path`, the executable's
  directory, (debug builds: `vendor/libmpv`, `PATH`); never the working directory. Loaded
  with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`.
- **Process DLL search order**: `opensmarters_player::harden_dll_search_path()` calls
  `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)` and `SetDllDirectoryW(L"")`
  before libmpv is loaded, so DLLs loaded by name later (by mpv, FFmpeg, drivers) come only
  from the app directory and System32.
- The `windows` crate is used only through its safe WinRT projection
  (`UISettings.AnimationsEnabled`). File dialogs are `rfd` (system common dialogs). The exe
  embeds only an icon resource; no elevation is requested.

### 4.11 The installer (`crates/installer`, `opensmarters-setup.exe`)

The setup exe is downloaded from the web and run from the Downloads folder by people who
trust it with their user account. Design: [ADR 0002](adr/0002-installer.md).

- **No elevation, ever.** Per-user install (`%LOCALAPPDATA%\Programs\opensmarters`), an
  `asInvoker` manifest, no services, scheduled tasks, autoruns or browser changes. The app
  is opened after install through `%SystemRoot%\explorer.exe` (absolute path), so it runs
  as the desktop user even if someone started setup elevated.
- **No network.** The installer contains no HTTP client, TLS stack or network runtime
  (`crates/installer/tests/no_network.rs` checks the dependency graph with `cargo tree`).
  Updates stay manual: the app can tell the user a newer version exists (§4.5), and the
  user downloads and runs the new setup, which updates in place.
- **DLL planting** (the classic installer flaw when run from Downloads): the setup binary
  is linked with `/DEPENDENTLOADFLAG:0x800`, so its static imports are resolved from
  System32 only, and `main` first calls `opensmarters_player::harden_dll_search_path()`
  (default DLL directories, current directory removed) before anything loads by name.
- **Integrity first.** The payload is one xz archive with a SHA-256 manifest, embedded in
  the exe (covered by the Authenticode signature once releases are signed). The archive
  hash is checked before anything is extracted; every file is hashed as it is written and
  again by reading it back; any mismatch stops with "The installer is damaged. Download it
  again and check its SHA-256." (`payload.rs`, `tests/rollback.rs`).
- **Atomic and recoverable.** Files go to `<dir>.new`, are verified, then swapped with the
  old folder; shortcuts and the Apps & features entry are written last; any failure or a
  cancel rolls everything back (registry values restored exactly, shortcuts restored byte
  for byte). Tested by injecting failures at each stage (`tests/rollback.rs`).
- **Paths.** The install folder is canonicalised and refused if it is a network path (UNC,
  mapped drive), a drive root, reached through a junction or symbolic link, inside Windows /
  Program Files / ProgramData / the app's data folder / the recordings folder, or one of
  the user's special folders itself. Setup installs only into an empty folder or one
  holding its own `install-manifest.txt` (`paths.rs`).
- **Uninstall deletes only what it installed**: files listed in `install-manifest.txt`
  (relative paths validated: no `..`, drives, device names; never through a junction),
  then folders left empty. Files the user added stay. Shortcuts are removed only if they
  point into the install folder. The app's data folder and saved passwords are deleted
  **only** when the user ticks the unticked boxes (or passes `--purge-data` /
  `--purge-passwords`); passwords are found by enumerating generic credentials whose
  service part is exactly `opensmarters` (`keychain.rs`). Recordings and downloads in the
  Videos folder are never touched.
- **Self-removal.** `uninstall.exe` copies itself to the per-user `%TEMP%`, runs the copy
  (which reports its exit code back through a pipe) and exits; the copy then deletes the
  original. Old copies are deleted by the next run.
- **Test seams are compiled out.** `--sandbox` (temp folders, the test registry root
  `HKCU\Software\opensmartersTest\<id>`, a fake keychain), `--preview`, the harness and
  `--slow` exist only with the `dev` cargo feature; release builds reject them.
- **Logs** (`%TEMP%\opensmarters-setup.log`) hold paths and steps only; the installer never
  reads a secret.

Residual: releases are **not Authenticode-signed** yet (SR-19), so SmartScreen warns and
only the published SHA-256 vouches for the download; `tools/package.ps1` signs both exes as
soon as a certificate is configured. "Delete my data" removes `%LOCALAPPDATA%\opensmarters`
as a whole: a recordings folder the user moved *inside* it would go too. A running app is
detected by its files being in use (the user is asked to close it; setup never closes or
kills it).

### 4.12 Web edition (`crates/web`, `opensmarters-web`)

The web edition ([WEB_PROMPT.md](WEB_PROMPT.md), deployment in
[WEB_DEPLOY.md](WEB_DEPLOY.md)) moves the provider connection onto a server that several
people sign in to over the internet. **New assets:** the instance secret key
(`OPENSMARTERS_SECRET_KEY`), people's passwords and sessions, every person's provider
secrets in one database, and the server's network position (it can reach the Docker
host, the LAN, cloud metadata). **New actors:** anyone on the internet (the sign-in page
is public), a signed-in household member trying to reach someone else's data or the
server's network, a hostile provider or playlist that now makes the *server* fetch
addresses, and a hostile web page open in the viewer's browser.

| Threat | Mitigation | Code |
|---|---|---|
| Provider secrets read from the disk or a backup | Passwords, M3U and EPG addresses sealed with XChaCha20-Poly1305 (random 192-bit nonces) under a key derived (keyed BLAKE2b) from the instance secret, with the user id and account id as associated data (a row can't be moved to another person); never returned by the API or put in a URL the browser sees; favourite / progress snapshots (names, artwork addresses) sealed too. A test plants the fake panel's `special` password and an M3U address and scans the whole data folder | `crypto.rs`, `provider.rs` (`seal`, `open`), `tests/web/accounts.rs` |
| Password guessing | Argon2id (19 MiB, t = 2, p = 1; at most two at once; stored parameters bounded); per-address and per-name lockout after 5 failures, doubling to 15 min (429 + `Retry-After`); one message for every failure; unknown names cost a dummy verification | `auth.rs` |
| Session theft or fixation | 256-bit random tokens stored only as keyed hashes; cookie `HttpOnly; Secure; SameSite=Strict; Path=/`, 30-day sliding expiry; a new token at every sign-in (the browser's old one ends); every session of a person ends on password change, disable and removal | `auth.rs`, `api/session.rs`, `api/admin.rs` |
| Cross-site request forgery | Every non-GET API request must carry `X-Opensmarters: 1` (a custom header forces a CORS preflight, which is never granted) on top of `SameSite=Strict`; JSON bodies only (a form post can't reach a handler) | `api/mod.rs` (`guard`, `Body`) |
| Script injection, framing | Strict CSP on every page (`default-src 'self'`, no inline script or style, `frame-ancestors 'none'`, `base-uri 'none'`, `form-action 'self'`), `nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`; provider text is returned as capped plain text with controls and bidi overrides removed (the UI never renders it as HTML); artwork is re-served only as sniffed PNG / JPEG / GIF / WebP (never SVG) with `default-src 'none'; sandbox` | `ui.rs`, `text.rs`, `images.rs` |
| Taking over a new instance | The administrator is created with a one-time setup code (random, ≈ 60 bits, logged once at start, or `OPENSMARTERS_SETUP_CODE`), compared in constant time, rate-limited, accepted only while no user exists (checked again in the insert transaction); invitations are 256-bit, stored hashed, single use, 72 h, and the token travels in the link's fragment (never in a server log) | `state.rs`, `api/session.rs`, `api/admin.rs` |
| One person reaching another's data | Every account query is scoped by the signed-in user's id; favourites and progress are reached only through such an account; streams answer only their owner (404 otherwise). Tested for every endpoint | `db.rs`, `api/*.rs`, `tests/web/accounts.rs`, `tests/web/streams.rs` |
| Server-side request forgery | One outbound client for **everything** (API, playlists, EPG, artwork, streams, HLS playlists and segments, every redirect): `http`/`https` only, no credentials in URLs, the server resolves names itself and connects only to addresses that pass the policy (no DNS-rebinding window); IP literals and every redirect are checked too. Refused: loopback, private, CGNAT, link-local (cloud metadata), multicast, unspecified, reserved and documentation ranges, the container's default gateway and `host.docker.internal`, in IPv4 and IPv6 including IPv4-mapped, IPv4-compatible, NAT64 and 6to4 forms. `HTTP(S)_PROXY` is ignored. Connect / read / total timeouts and size caps per purpose. `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS=1` relaxes only the private ranges | `net.rs`, `tests/web/ssrf.rs` |
| The test / demo loopback exception | `Config::dev_loopback_allow` lists exact `ip:port` pairs on loopback; it is **never read from the environment**: only `--demo` (the in-process fake panel's address) and the tests set it, and it applies only to URLs that name that IP literally (a host name resolving to loopback is still refused) | `config.rs`, `net.rs` (`Policy::check_ip`), `main.rs` |
| An open image proxy | Image tokens are the provider address encrypted and authenticated with a key only this server has (a synthetic nonce keeps them stable for caching); a session is required; 5 MB, 20 s, ≤ 3 redirects; magic bytes and decoded size checked (≤ 8192 px per side, ≤ 40 MP); disk cache capped at 256 MiB with 7-day expiry; failures remembered for 5 min | `crypto.rs` (`image_token`), `images.rs`, `tests/web/images.rs` |
| Hostile streams parsed by FFmpeg | The server fetches every byte itself (through the SSRF client) and writes it to ffmpeg's **stdin**: `-protocol_whitelist pipe` for the input and a demuxer whitelist (`mpegts, matroska, mov, avi, flv, mpeg, aac, mp3`), so no playlist, redirect or file reference inside a stream can make ffmpeg open anything; output only into the stream's own folder under `<data>/streams`; ffprobe runs the same way. In the container ffmpeg runs as uid 10001 with no capabilities and a read-only root | `streams.rs` |
| Exhausting the server | Streams per person / per instance / per provider account; zapping stops the previous stream before opening a new provider connection; a stream nobody fetches for 30 s is stopped (ffmpeg killed, connection closed, folder deleted), a stalled one after 90 s, any after 12 h; films are read at most ~2 minutes ahead of the viewer; request bodies 64 KiB; response caps; bounded caches; ≤ 20 providers and ≤ 5 000 favourites per account | `streams.rs`, `provider.rs`, `images.rs`, `lib.rs` |
| Secrets in logs | Every log line goes through `RedactingWriter`; each decrypted password, user name, playlist and EPG address is registered with the `Redactor`; URLs are logged only redacted; errors carry no URL | `main.rs`, `provider.rs`, `net.rs` |
| Spoofed client addresses | `X-Forwarded-For` / `-Proto` / `-Host` are read only from peers in `OPENSMARTERS_TRUSTED_PROXY` (the right-most untrusted address is the client) | `auth.rs` (`client_ip`), `config.rs` |
| Container escape helpers | Non-root user (uid 10001), `tini` as PID 1 (ffmpeg children reaped), no shell or curl needed (`--healthcheck` is built in); the Compose file adds a read-only root, a small `/tmp` tmpfs, `cap_drop: ALL` and `no-new-privileges` | `deploy/web/Dockerfile`, `deploy/web/compose.yaml` |

Residual risks of the web edition:

- **FFmpeg memory-safety bugs** on hostile streams remain the largest risk (as with
  libmpv on the desktop), now inside the server's container instead of the viewer's PC.
- **The server holds what it relays.** While it runs it decrypts provider secrets to
  build requests and keeps parsed M3U playlists (with credential-bearing stream
  addresses) in memory for up to 6 hours; whoever controls the host, or has the secret
  key and the database, can read every provider secret.
- **A signed-in person can make the server download public addresses** (as their
  "provider"): bandwidth, not secrets. This is why instances must stay private
  (WEB_DEPLOY.md, the private-instance rule). With `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS=1`
  they can also make it `GET` HTTP services on the LAN (answers are parsed as JSON, M3U,
  XMLTV, images or media, never reflected raw).
- Plain-HTTP providers expose credentials between the server and the provider, as on the
  desktop.
- The sign-in limiter lives in memory (a restart resets it); there is no second factor.
- `--demo` starts without a secret key (it makes up a temporary one) and creates a
  known password: local testing only.

Verification: `cargo test -p opensmarters-web` (unit tests plus `tests/web`: setup,
sessions, invites, rate limit, CSRF, isolation between two people, every catalogue
endpoint, SSRF refusals for typed addresses, redirects, playlists, HLS manifests and
artwork, the image proxy's token and limits, and a real stream through ffmpeg whose
provider connection is counted by a TCP proxy; the stream tests are skipped with a
message when ffmpeg isn't on `PATH`).

## 5. Residual risks and out of scope

- **Memory-safety bugs in libmpv/FFmpeg/libcurl/OpenSSL/libass** on hostile streams are the
  largest remaining risk: they run in-process with the user's rights. Mitigations: pinned,
  recent build; minimal protocol surface; no scripts. Keep the pinned build current (it
  is a git snapshot; GitHub keeps ~30 days). Future hardening: run playback in a separate
  low-privilege process, and ship a libmpv built without unneeded features (libarchive,
  bluray/dvd, Lua/JS, VapourSynth, SDL, OpenAL).
- **Plain-HTTP panels** expose credentials and content to the network; we warn, we cannot
  fix it. Xtream panels can also tell the app to fetch *streams* over HTTP from an HTTPS
  account (`server_info.server_protocol`) — see SR-12.
- **Malware running as the user** can read Windows Credential Manager entries (any process
  of the same user can), edit the database (e.g. clear the PIN row), or read the plain
  image cache and history. Local file access is out of scope for parental control: deleting
  the data folder resets everything by design.
- **Clock manipulation**: changing the system time forward (needs admin rights on a standard
  Windows setup) shortens PIN lockouts; the failure counter still grows.
- **Provider-chosen category names** decide keyword locks: a provider can name an adult
  category with homoglyphs or zero-width characters so no keyword matches; use manual
  locks for such providers.
- **SSRF to LAN** via provider image/stream URLs (GET only) is accepted. libcurl in mpv
  follows a redirect to `ftp://` (SR-09).
- **Large-but-bounded resource use**: a 667 KiB gzip XMLTV within the default limits takes
  ~1.8 GiB and ~30 s (debug build) to parse with the core's default limits (SR-11; guide
  downloads now use far tighter ones); artwork decoding is capped at 2 × 64 MiB (SR-10).

## 6. Design rules for contributors

- Every provider string is data: render it with `ui::text::cap`, never interpret it.
- Every URL from a provider goes through `PlayableUrl` before mpv, `is_fetchable` before an
  image download, and nowhere near a shell.
- Never log a URL or an error that might contain one without the redactor; register every
  secret you obtain with `Redactor::add_secret` (URLs register their embedded credentials).
- New HTTP downloads go through `core::net::HttpClient` (caps, timeouts, redirect policy).
- Parental checks belong where a playback request or detail page is created, not only in
  the list that led there.
- `unsafe` only in `crates/player`, each block with a `// SAFETY:` comment and a test.

## 7. Security review 2026-10-20

Scope: all crates on `main` at `d1b8ae6` (Phases 0–4, including Guide/Search/Catch-up/
Favourites), the pinned libmpv, CI and tooling. Method: code review plus dynamic tests
against the fake panel and the real DLL (commands in §8).

| ID | Severity | Component | Finding | Status |
|---|---|---|---|---|
| SR-01 | High | core redact | M3U playlists using the short live form `http://host/{user}/{pass}/{id}` were not recognised by the redactor and only the whole playlist URL was registered, so mpv's warning lines ("stream: Failed to open <url>", logged at the default level on every 403/404) wrote the username and percent-encoded password to `opensmarters.log` (reproduced: 3 lines at default level, 4 with debug) | Fixed (`0953800`) |
| SR-02 | High | app | Locked content is reachable without the PIN: Home's *Continue watching*, *Live now* (favourites/recently watched) and *Recently added* rails, the Favourites screen (incl. zapping), the Detail page ("More like this" lists the locked category) and `Session::live_request`/`movie_request`/`episode_request` do not check locks | Fixed (fix batches B, C1, C2): request builders refuse locked items (`AppError::Locked`, all category ids); Home rails, Detail (+ "More like this"), browse, Search, Favourites and the player zap list filter or gate locked items; shared `UnlockFlow` (refuse → PIN → unlock → retry); unlocks re-lock after 30 min idle and on restart/profile switch |
| SR-03 | Medium | app | Dev-harness flags are available in release builds: `--route player:live=<id>` and `--route detail:movie=<id>` play locked items without the PIN (verified; zapping then covers the whole catalogue), `--route onboarding` adds a profile without the "add profile" PIN gate, which lets a child add an M3U playlist they know and use it for "Forgot PIN" | Fixed (fix batch C1): harness flags exist only with the `harness` cargo feature (rejected otherwise); onboarding/welcome routes go through the PIN-gated "Add profile"; Detail fails closed for locked items; forgot-PIN limiter persisted (SR-14) |
| SR-04 | Medium | core m3u | Attribute-heavy M3U lines grew ~21× in memory (64 MiB → 1.36 GiB peak; ~5 GiB at the 256 MiB cap) | Fixed (`9d0f8e6`, now 71 MiB) |
| SR-06 | Medium | player | Release builds searched every `PATH` directory for `libmpv-2.dll`, and the process DLL search order was not restricted for DLLs libmpv/drivers load by name | Fixed (`2cca850`); the app calls `harden_dll_search_path()` first thing in `main` (fix batch A) |
| SR-07 | Medium | core net | reqwest followed `https` → `http` redirects, resending `player_api.php?…password=…` in clear while the UI showed the account as encrypted | Fixed (`4562d00`) |
| SR-05 | Low | core m3u | Quadratic duplicate check on `url-tvg` lists: 1 MiB of header took 36 s; a few MiB hang the playlist load | Fixed (`9d0f8e6`) |
| SR-08 | Low | CI | `cargo audit` (CI `rustsec/audit-check`) fails: the five accepted advisories are ignored only in `deny.toml` | Fixed (`.cargo/audit.toml` mirrors the `deny.toml` ignores) |
| SR-09 | Low | libmpv | libcurl follows provider redirects to `ftp://`/`ftps://` (13 other protocols refused; never plays or reads files) | Accepted (test documents it) |
| SR-10 | Low | app images | Decode limits (8192 px, 256 MiB, 6 concurrent) allow ~1.5 GiB of transient memory from hostile artwork | Fixed (fix batch B): 4096 px, 64 MiB decoder budget, 2 decodes at a time, area-average downscale (no full-size float buffers); worst case ≈ 2 × 64 MiB |
| SR-11 | Low | core xmltv / app | Default XMLTV limits let a 667 KiB gzip cost ~1.8 GiB and ~30 s | Fixed for guide downloads (fix batch B): `parse_filtered` with 256 MiB decompressed, 1 MiB text nodes, 1 KiB texts, window −36 h…+7 d, ≤ 400 k programmes kept, ≤ 2 M looked at, titles 240 B / descriptions 360 B kept (core defaults unchanged) |
| SR-12 | Low | app | The "Unencrypted" warning reflects only the API/playlist URL; stream URLs built from `server_info` (or M3U entries) may be `http` with credentials | Fixed in the session (fix batch B): `is_insecure()` is also true when the stream base (`server_info`) or any playlist entry is `http`; `insecure_reason()` says which (UI wording pending) |
| SR-13 | Low | app | Artwork of unlocked adult categories is cached as plain PNGs in `images\` (viewable in Explorer); history/search history are plain SQLite | Artwork fixed (fix batch B): images of locked categories stay in memory (their cached files are deleted) and the disk cache is wiped when a PIN is set; history remains plain SQLite (accepted, §5) |
| SR-14 | Low | app | "Forgot PIN" rate limit lives in memory (restart = 3 new tries); unlocked categories stay unlocked for the whole session | Fixed (fix batch B): the limiter is persisted in the settings table (`app.parental.reset_limiter`, machine-local); unlocks expire after 30 min without input, and on profile switch / restart |
| SR-15 | Low | app | Search hides locked VOD only by the primary `category_id` and applies keyword locks only when the category list is cached | Fixed (fix batch C2): Search checks every category id and loads category lists before showing VOD/series results when parental control is on; Favourites shows locked items as "Locked" (no artwork/title) behind the PIN |
| SR-16 | Low | app | `explorer` is started by bare name (`platform.rs`, `trailer.rs`); no panic hook (panic text goes unredacted to stderr in debug builds) | Fixed (fix batches A, C1): `platform::launcher()` starts `%SystemRoot%\explorer.exe` / the URL opener by absolute path; a panic hook logs through the redacting `tracing` writer |
| SR-17 | Info | store | Lockout can be shortened by moving the clock forward (admin); DB edits remove the PIN | Accepted (§5) |
| SR-18 | Info | core parental | Keyword matching can be evaded by provider names using homoglyphs/zero-width characters | Accepted (§5) |
| SR-19 | Info | supply chain | ~~CI actions pinned by tag~~ (fixed in 0.2.0: every action is pinned to a commit SHA, and release files carry build-provenance attestations); `fetch-libmpv.ps1` trusts an existing DLL when `VERSION.txt` matches (no DLL hash re-check); the libmpv build carries many unused C features; releases are not Authenticode-signed | Open (hardening) |
| SR-20 | Info | platform | Credential Manager entries are readable by any process of the same user; exports contain server URLs and usernames | Accepted (§5) |

Verified with no finding: TLS verification in both stacks (rustls; mpv/libcurl refuses a
self-signed certificate and accepts a public one); no telemetry or embedded keys; no
`unsafe` outside `crates/player`; secrets never reach SQLite (leak test); no credential in
logs for an Xtream profile at `trace` level with mpv `debug`; remote playlists, EDLs,
`hex://` blobs, HLS segments and redirects cannot reach local files, lavfi or devices;
header values cannot carry CR/LF; Argon2id/lockout logic; encrypted cache design.

## 8. Verification checklist

Run from the repository root (PowerShell or Git Bash). Use your own target directory if
other builds run in parallel.

```bash
# Gates
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check                    # advisories, licences, bans, sources
cargo audit                         # needs .cargo/audit.toml mirroring deny.toml's ignores (SR-08)

# libmpv: pinned DLL and hostile-content tests against it
powershell -ExecutionPolicy Bypass -File tools/fetch-libmpv.ps1
powershell -ExecutionPolicy Bypass -File tools/gen-test-media.ps1
sha256sum vendor/libmpv/libmpv-2.dll   # 82bae3b6bf7bb737afc9b52d590b477983c715f3c63c1a361f9ee06523dc5dc2
cargo test -p opensmarters-player --test mpv -- --test-threads 1

# Secrets never on disk / never in logs
cargo test -p opensmarters-store --test leak
cargo test -p opensmarters-core redact

# unsafe only in the player crate
grep -rln "unsafe" crates --include=*.rs | grep -v "^crates/player/" # only comments/test names
grep -rn "forbid(unsafe_code)" crates --include=*.rs                # core, store, app, fake-panel

# Fuzzing (nightly; 5 minutes each)
cd fuzz && cargo +nightly fuzz run m3u -- -max_total_time=300
cargo +nightly fuzz run xmltv -- -max_total_time=300
cargo +nightly fuzz run xtream_json -- -max_total_time=300
cargo +nightly fuzz run hls -- -max_total_time=300

# Update check: strict tag parsing, locally built link, caps, exact request headers
# (against the fake panel's /__updates/releases/latest and a raw socket; never GitHub)
cargo test -p opensmarters --all-features --test updates
cargo test -p opensmarters --all-features --lib updates

# Recordings and downloads: Range resume, redirects, timeouts, HTML/playlist bodies refused
cargo test -p opensmarters-core --test download
cargo test -p opensmarters --all-features --test dvr

# Installer: install/update/repair/uninstall and rollback in sandboxes (test registry root
# HKCU\Software\opensmartersTest, temp folders); the CLI end to end; no network crates
cargo test -p opensmarters-installer --features dev
```

**End-to-end log check** (synthetic credentials only; the fake panel's `special` user has
the password `p@ss/w#rd %&+`):

```bash
cargo build -p fake-panel && cargo build -p opensmarters --features demo
target/debug/fake-panel --port 18765 &      # wait for "LISTENING http://127.0.0.1:18765"
# A fresh --ephemeral run starts on the welcome screen: Enter opens the Xtream form.
OPENSMARTERS_LOG=trace target/debug/opensmarters --ephemeral --data-dir "$TMP/os-check" \
  --mpv-log debug --route player:live=1001 \
  --keys "wait:3,Enter,wait:1,text:Test,Enter,text:http://127.0.0.1:18765,Enter,text:special,Enter,text:p@ss/w#rd %&+,Enter,wait:10,quit"
for p in 'p@ss/w#rd' 'p%40ss' 'w%23rd' '%26%2B' 'special'; do
  grep -c -F -e "$p" "$TMP/os-check/logs/opensmarters.log"   # every count must be 0
done
```

Repeat with an M3U profile whose playlist uses the short form
`http://127.0.0.1:18765/special/p%40ss%2Fw%23rd%20%25%26%2B/1001` (any static file server
can serve it as `get.php?username=special&password=…`), and with a missing channel id so
mpv logs a warning at the default level.

**Secrets on disk**: after a real login, search the data folder for the password; only the
encrypted cache may hold it, never in clear:

```powershell
Select-String -Path "$env:LOCALAPPDATA\opensmarters\opensmarters\data\*","$env:LOCALAPPDATA\opensmarters\opensmarters\data\logs\*" -Pattern '<your password>' -SimpleMatch
```

**Parental control** (`--demo --ephemeral` sets PIN `1234`; "Adult Night" and
"18+ After Dark" are keyword-locked): check that Live TV, Movies, Series, Guide, Catch-up,
Search, Home rails, Favourites, Detail and `--route` cannot show or play a locked item
without the PIN, and that a lockout survives a restart.

**TLS**: playing `https://` from a server with a self-signed certificate must fail
(`curl: TLS certificate verification failed`), a public HTTPS URL must connect.

## 9. Reporting a vulnerability

Please **do not open a public issue** for a security problem. Use GitHub's private
vulnerability reporting on the repository (Security → *Report a vulnerability*). If that
button isn't there, open an issue titled "Security contact" with no details, and a
maintainer will reply with a private channel. Include
the opensmarters version or commit, Windows version, what an attacker controls (provider,
playlist, network, local access) and the steps or a sample (synthetic data only — never
send real provider credentials). We aim to acknowledge within 7 days and to fix or publish
an advisory within 90 days; reporters are credited unless they ask not to be.

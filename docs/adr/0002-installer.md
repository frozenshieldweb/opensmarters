# ADR 0002: The Windows installer (`opensmarters-setup`)

- **Status:** accepted
- **Date:** 2026-10-20
- **Scope:** `crates/installer` (`opensmarters-installer`, binaries `opensmarters-setup` and
  `opensmarters-pack`), `crates/afterglow` (shared Afterglow theme), `tools/package.ps1`
- **Spec:** [LAUNCH_PROMPT.md, Part B](../LAUNCH_PROMPT.md#part-b-the-installer-rust-beautiful-simple)

## Context

opensmarters ships today as a portable zip (`tools/package.ps1`). The launch needs a
one-click, **per-user** Windows installer that looks like the app's first screen, never asks
for elevation, never touches the network, and also updates, repairs and uninstalls. It must
follow the project's security rules (`#![forbid(unsafe_code)]`, cargo-deny, lean
dependencies, no web engine) and the Afterglow design contract (`docs/DESIGN.md`).

## Decision 1: Rust + eframe/egui (glow), a workspace crate

The installer is `crates/installer`, a Rust binary built with **eframe/egui and the glow
renderer**, exactly like the app.

- It reuses the Afterglow look natively: the same fonts (the unmodified TTFs in
  `assets/fonts/`, variable-font coordinates included), tokens, focus ring and glow.
- One static exe, no runtime, no web engine (the product promises "no web engine").
- The project's rules apply unchanged: `unsafe` is forbidden (workspace lint), cargo-deny
  checks every new crate, and errors use `anyhow` (binary) / `thiserror` (library modules).

Rejected alternatives:

| Option | Why not (for v1) |
|---|---|
| **MSI / WiX** | Standard and good for enterprise deployment (GPO, Intune), but rigid: the UI is a dated dialog wizard, per-user MSIs are awkward, and custom UI means a custom bootstrapper anyway. Could be added **alongside** this installer later for IT. |
| **NSIS / Inno Setup** | Their own scripting languages, dated visuals, and a weaker security story (plug-in DLLs extracted to `%TEMP%`, a history of DLL-planting fixes). A second toolchain to audit. |
| **Tauri / WebView2** | A web engine, against the product's promise and the "no HTML rendering" spirit of BUILD_PROMPT §3.1. |
| **MSIX** | Needs a trusted signature (and in practice the Store model), sandboxed install locations and different update semantics. Possible later. |

## Decision 2: extract `crates/afterglow` (pure move)

The theme pieces both programs need move from `crates/app/src/ui` into a small shared crate
`crates/afterglow`, **unchanged**:

| Moved file | Contents |
|---|---|
| `theme.rs` | colour/space/radius/stroke/elevation/motion tokens, font installation with variable-font coords, egui style |
| `icons.rs`, `icons/glyphs.rs` | Phosphor Regular/Fill families and code points |
| `paint.rs` | focus ring/glow, gradients and scrims, cover-fit, progress bar, spinner |
| `motion.rs` | the reduced-motion flag and animation helpers |

The app keeps its module paths: `crates/app/src/ui/mod.rs` re-exports the moved modules
(`pub use afterglow::{icons, motion, paint, theme};`), so no call site changes and the app
renders identically (verified with before/after harness screenshots, pixel-identical, and
the app's test suite).

Not moved: the app's `Button` is built on the app's spatial focus engine
(`crates/app/src/focus`), which is app-specific; moving it would drag the focus engine
along and stop being a pure move. The installer has its own ~150-line button and checkbox
widgets, painted only with `afterglow` tokens and helpers (`paint::focus_ring`,
`paint::focus_glow`, `paint::progress_bar`, the same fill/text pairs as DESIGN §6.5).

## Decision 3: payload = one xz stream + SHA-256 manifest, embedded by a build script

**Format.** The packer (`opensmarters-pack`, a release-built tool in the installer crate)
reads the staged release folder that `tools/package.ps1` also zips (**one source of truth**)
and writes:

- `payload.xz`: one solid xz stream of every file's bytes, concatenated in **sorted path
  order** (byte order of the `/`-separated relative path). No timestamps, no permissions,
  no names inside the stream: identical inputs give a byte-identical archive.
- `payload.manifest`: plain text: format version, product version, the archive's size and
  SHA-256, the total installed size, then one line per file: `SHA-256 size path`.

**Compression: xz (LZMA2, preset 9), measured.** On the 0.1.0 release folder
(`libmpv-2.dll` 121 MB + `opensmarters.exe` 31 MB + licences):

| | `libmpv-2.dll` | whole payload | decode time |
|---|---|---|---|
| zip (deflate, today's portable zip) | | 59.9 MB | |
| zstd level 22 + long window | 36.6 MB | ≈ 47 MB | 0.3 s |
| xz preset 9 (`lzma-rust2`) | 33.6 MB | ≈ 43 MB | 1.5 s |
| **xz preset 9 + BCJ x86 filter (`lzma-rust2`), shipped** | **33.2 MB** | **42.1 MB** | **1.5 s** |
| xz preset 9e (liblzma, for reference) | 33.5 MB | | 1.7 s |

xz is ~5 MB smaller, and `lzma-rust2` is a **pure-Rust** encoder and decoder (Apache-2.0)
whose ratio matches liblzma, so neither the installer nor the build needs a C library. The
x86 branch filter (BCJ) helps the exe and the DLL a little more. Decoding the whole payload
takes about 2 s, which is real work the progress light follows. Measured on the 0.1.0
release folder (152.6 MB, 10 files): payload 42.1 MB, setup exe 51.4 MiB, portable zip
57.1 MiB.

**Embedding: a build script, not an appended archive.** `crates/installer/build.rs`
`include_bytes!`s `payload.xz` and the manifest:

- With `OPENSMARTERS_SETUP_PAYLOAD=<dir>` (set only by `tools/package.ps1`), it embeds the
  real payload from that folder and checks its hash at build time.
- Without it, it generates a small **synthetic payload** (a fake app exe and DLL made of
  deterministic bytes, plus licence texts) in `OUT_DIR`. So `cargo build --workspace` and
  `cargo test --workspace --all-features` never need a release build.

Why not an appended archive ("stub + payload + signed-length trailer")? Authenticode puts the
certificate table **after** any appended data, so the stub would have to parse its own PE
headers to find the trailer, and the data path would be self-parsing code with its own
attack surface. Embedded in `.rdata`, the payload is covered by the signature like any other
byte of the exe, and there is no self-parsing. The cost: the setup exe is relinked when the
payload changes (seconds), and `uninstall.exe` (a copy of the setup exe) carries the payload
too (≈ 50 MB on disk next to a ≈ 150 MB app). That copy is also what makes **Repair** from the
installed copy possible.

**Integrity.** Before extracting anything, the SHA-256 of the embedded archive is checked
against the manifest. Every file is hashed while it is written and checked again by reading
it back after extraction. Any mismatch stops with "The installer is damaged. Download it
again and check its SHA-256."

## Decision 4: atomic, recoverable install transaction

```text
validate target ─▶ archive hash ─▶ extract to <dir>.new (hash each file)
  ─▶ copy self as uninstall.exe, write install manifest ─▶ re-read and verify every file
  ─▶ swap: <dir> → <dir>.old, <dir>.new → <dir>
  ─▶ shortcuts ─▶ registry ─▶ commit: delete <dir>.old (manifest files only)
```

- Every step before the commit is undone on failure **or cancel**: the registry values are
  restored to their previous state (or the key deleted), shortcuts restored/removed,
  `<dir>.old` renamed back, `<dir>.new` removed. Registry and shortcuts are written last.
- Stale `<dir>.new` / `<dir>.old` folders from a crash are removed only if they contain
  nothing but files named in a payload/install manifest.
- **Install manifest** `<dir>\install-manifest.txt` is both the ownership marker and the
  list of installed files with hashes. The installer only installs into an **empty** folder
  or one with a valid marker. **Uninstall deletes only files listed in it** (relative paths
  validated, regular files only, never through links), then empty folders, never a whole
  arbitrary folder.
- **Running app:** before replacing files, the installer checks whether `opensmarters.exe`
  or `libmpv-2.dll` in the install folder is in use (opening a mapped image for writing
  fails with a sharing violation; plain `std`, no process enumeration). The UI says
  "opensmarters is open. Close it to continue." and continues by itself once it is closed;
  `--quiet` exits with code 3. The installer never closes or kills the app itself (a
  "close it for me" button would need process enumeration plus a graceful-close request;
  noted as a follow-up).

## Decision 5: Windows integration without `unsafe` or elevation

| Need | How |
|---|---|
| No elevation, DPI, long paths | Embedded application manifest: `asInvoker`, PerMonitorV2, `supportedOS` Windows 10/11, `longPathAware` (`build.rs`, via `embed-resource`, with the icon and a VERSIONINFO naming the product "opensmarters") |
| DLL planting (the exe runs from Downloads) | Static imports are resolved from System32 only (`/DEPENDENTLOADFLAG:0x800` link flag on the setup binary), and the first line of `main` calls `opensmarters_player::harden_dll_search_path()` (the player crate's audited `unsafe`, reused rather than duplicated) |
| Clean PCs without the Visual C++ Redistributable | The VC runtime is linked statically into the setup binary and into the app's `opensmarters.exe` (the same link arguments in `crates/installer/build.rs` and `crates/app/build.rs`); their imports are then Windows system DLLs only, the Universal CRT being part of Windows 10/11. `tools/package.ps1` fails if either exe still names `vcruntime140*.dll` |
| Known folders (Start menu, Desktop, `%LOCALAPPDATA%\Programs`, system folders) | `known-folders` (safe `SHGetKnownFolderPath` wrapper) |
| Apps & features entry | `winreg` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\opensmarters`; `URLInfoAbout`/`HelpLink` come from the workspace `repository` field (one place) |
| Shortcuts | `mslnk` (pure-Rust `.lnk` writer). The app defines no AppUserModelID, so none is set. |
| Folder picker | `rfd` (already used by the app) |
| Purging saved passwords (opt-in at uninstall) | `windows-native-keyring-store` + `keyring-core` (already in the tree through `keyring`): enumerate generic credentials whose service part is `opensmarters`, delete them |
| One installer at a time | `std::fs::File::try_lock` on `%TEMP%\opensmarters-setup.lock` |
| Open the app unelevated | started through `%SystemRoot%\explorer.exe <exe>` (absolute path), so it runs as the desktop user even if someone started the installer elevated |

**Self-removal.** `uninstall.exe` can't delete itself while it runs. When started from the
install folder it copies itself to `%TEMP%\opensmarters-uninstall-<pid>.exe`, releases its
lock, and runs the copy with the same arguments plus `--from-temp`, reading the copy's
result line from a pipe. The copy removes everything, reports the exit code, and then
deletes the original `uninstall.exe` (retrying until the parent has exited) and the empty
install folder. The parent exits with the copy's code, so tools that wait on
`QuietUninstallString` get a meaningful exit code. Copies left in `%TEMP%` are deleted by the
next run of the installer (or by Windows' temp clean-up).

**Names live in one place** (`crates/installer/src/brand.rs`): product name, exe names,
registry key, data folder (`%LOCALAPPDATA%\opensmarters`, the `ProjectDirs` root the store
uses) and the keychain service. A test pins them to the values the store crate actually uses
(`opensmarters_store::default_db_path`, `KeyringSecretStore::SERVICE`, dev-dependency only),
so a future rename can't drift silently.

**Data and passwords** are touched only by the two unticked uninstall options (or
`--purge-data` / `--purge-passwords`). Recordings and downloads (`Videos\opensmarters`) are
never deleted.

## Decision 6: test and dev seams, compiled out of release

- `installer::Environment` bundles every location the installer writes to: the default
  install root, Start menu and Desktop folders, the data folder, the temp folder, the
  registry root and the keychain backend. `Environment::real()` is what the shipped binary
  uses. `Environment::sandbox(dir)` points everything into a temp folder, uses the **test
  registry root** `HKCU\Software\opensmartersTest\<id>\Uninstall` and a file-backed fake
  keychain. Library tests use it directly.
- The **`dev` cargo feature** adds hidden command-line flags to the binary: `--sandbox <dir>`,
  the harness (`--keys`, `--screenshot-after/-out`, `--exit-after`, `--ppp`), `--preview
  <screen>` (render a screen without doing any work), `--reduced-motion` and `--slow`
  (throttle real work so the progress can be watched). Without the feature (release builds,
  `tools/package.ps1`) the parser rejects them, as the app does with its `harness` feature
  (SR-03).
- No test or harness run touches the real Uninstall key, real shortcuts, real
  `%LOCALAPPDATA%\Programs`, the real data folder or Credential Manager.

## Decision 7: release pipeline and signing

`tools/package.ps1` builds the app, stages the folder and zips it (unchanged), then runs
`opensmarters-pack` on the same staged folder, builds `opensmarters-setup` with
`OPENSMARTERS_SETUP_PAYLOAD`, and writes `dist/opensmarters-<v>-windows-x64-setup.exe` and
`dist/SHA256SUMS.txt` covering the zip and the setup exe. When `OPENSMARTERS_SIGN_CERT`
(a `.pfx` path, with `OPENSMARTERS_SIGN_PASSWORD`) or `OPENSMARTERS_SIGN_THUMBPRINT` (a
certificate in the user store) is set, `signtool` signs `opensmarters.exe` (before staging)
and the setup exe (before hashing), with an RFC 3161 timestamp; otherwise the build is
unsigned and the script says so. Until releases are signed, SmartScreen warns; the landing
page says so honestly.

## Decision 8: the window

- 720 × 460 pt, not resizable, no OS title bar: a custom header (symbol mark, drag region,
  close button). Windows 11 rounds the corners and draws the shadow of the undecorated
  window (eframe's `Decorations(false)` command asks for winit's undecorated shadow);
  Windows 10 shows square corners.
- One light: the **open-corners symbol** over the art panel is an empty frame of dark glass
  with a champagne hairline at rest; while working the art dims and the symbol fills with
  light along its diagonal (a triangle-strip mesh built from the SVG geometry, per-vertex
  gradient, soft leading edge, feathered glow). Removing lets the light drain. Reduced
  motion (Windows' *Animation effects*, via `afterglow::system_motion`) shows a plain bar
  inside the frame instead, with ≤ 100 ms fades and no slides.
- Focus is egui's own (Tab/Shift+Tab, arrows, Enter/Space, AccessKit roles and labels on
  every control); hover moves it; the ring and glow are Afterglow's. While the installer
  works nothing is focused by default, so the light is the one bright thing; Esc asks to
  cancel.
- If OpenGL can't start (a VM without a graphics driver), setup offers the default install
  (or the plain uninstall) through a message box instead of failing.

## Consequences and follow-ups

- The setup exe (51.4 MiB measured) stays under the portable zip (57.1 MiB).
- Follow-ups: an "all users" install into Program Files (needs elevation; out of scope for
  v1); a "close it for me" button for a running app; an MSI for enterprise deployment;
  Authenticode signing of releases; a clean-machine VM test as part of release QA.

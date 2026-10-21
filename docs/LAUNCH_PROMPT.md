# Prompt: opensmarters launch kit (landing page + installer)

> Paste everything below the line into a new Claude Code session opened in this repository, or say: *"Follow docs/LAUNCH_PROMPT.md."*
>
> There are two independent parts. Run them in one session, one after the other, or in two parallel sessions, one per part, each on its own branch:
> - **Part A:** a one-screen, interactive landing page that presents the product and offers the download.
> - **Part B:** a beautiful, simple Windows installer written in Rust.
>
> They share only the brand rules (§0) and the release file names (§A8 / §B8).

---

You are preparing the public launch of this project's desktop app, a secure, open-source TV player for the viewer's own Xtream Codes or M3U subscription, built in Rust with egui and libmpv. You will build a **landing page** (Part A) and an **installer** (Part B). Both follow the project's art direction, **"Afterglow"**, exactly, and both feel like one product with the app.

## 0. Shared: read first, name and brand

**Read first (required):**
- `docs/DESIGN.md`: the art direction and token contract for colours (§4.1–4.2), spacing, radii, elevation (§4.4–4.5), typography (§4.7), focus treatment (§5) and motion (§7). **It is the source of truth for the look.** Where this prompt and DESIGN.md disagree on a visual detail, DESIGN.md wins.
- `README.md` and `docs/SECURITY.md`: the source of truth for **what the product does**. Don't claim anything they don't support.
- `docs/PROGRESS.md` (what is released) and `tools/package.ps1` (how releases are built today).
- For Part B also: `docs/ENGINEERING.md` (gates, errors, logging and dependency rules) and `docs/BUILD_PROMPT.md` §3 (non-negotiable security rules).
- **`tools/assets/README.md`: the launch-asset pipeline.** The images and web fonts both parts need are already made, in `assets/launch/`; the list and provenance are in `assets/launch/PROVENANCE.md`. They come from:
  - Codex image generation for the art;
  - a typeset template for the social image;
  - derived favicons, responsive hero images and WOFF2 fonts.

  **Use these files.** When you need a new or changed asset, add it to `tools/assets/launch.toml`, with a Codex brief in `tools/assets/briefs/` if it's art, and rebuild with the pipeline. Never hand-make, hand-edit or AI-generate assets outside it, and never let an image generator render text.

**Name.** The product is **opensmarters**, always lowercase, in UI text and in prose (as in `README.md`), even at the start of a sentence. Keep it in **one constant** per part. It matches the exe, the data folder and the keychain entries the app already uses, so nothing needs renaming.

**Brand rules:**
- **Wordmark:** lowercase `opensmarters`, set live in Fraunces with `"opsz" 144, "wght" 560, "SOFT" 100, "WONK" 0`, tracking −1%, cream `#F4ECE4`. One colour only, never split into two colours or two words, and never "IPTV" next to it (DESIGN §2). Where live text isn't possible, use the pre-rendered `assets/brand/wordmark.png` / `lockup.png` (@2x, drawn at ≤ 50% of their pixel size).
- **Symbol "Open corners"** (`assets/brand/symbol-gradient.svg`, `symbol.svg`, `assets/icon/`): a 5:4 screen frame whose two diagonal corners are left open, with champagne flowing into muted rose. Clear space ≥ half its height.
- **Voice:** calm, plain, warm, honest. Short sentences. No hype words, no exclamation marks, no fake urgency.
- **Distance from IPTV Smarters:** the name is similar, so the product must never look or sound like it, and must never be presented as a version, fork or alternative of it.
  - Don't mention, compare with or allude to IPTV Smarters or any other player or service, with **one exception**: a single factual non-affiliation line in the footer (§A2), worded as in `README.md`.
  - Never use "Smarters" on its own, never style the name as "Open Smarters" or "OpenSmarters", and never use the words "smart" or "smarter" as a slogan.
  - "IPTV" never appears in a headline (it may appear once in body copy as a technical term).
  - No "free channels", no provider names, no TV-network logos, no images of real shows. Use only synthetic demo data.
- **Afterglow in short:** a deep warm near-black room (`bg #141013`, surfaces `#20191E #2C2228 #3A2D34 #4A3A43`). Text is cream `#F4ECE4` / `#BFAFB6` / `#A8989E`. There is **one** light source, champagne `#E6BE91`: the primary button, progress fills and the focused element. Focus is a 3 px `#F2D3AE` ring drawn outside the element with a 3 px gap, plus a soft champagne glow. Fonts are **Fraunces** for display and **Source Sans 3** for UI (both OFL, in `assets/fonts/`). Motion is 90 / 160 / 240 / 400 ms, eased out, with no bounces or springs, and it respects reduced motion.

---

# Part A: landing page (one screen, interactive)

## A1. Goal

Someone arrives and, **without scrolling** (any desktop viewport from 1280×720 up), understands what opensmarters is, *feels* how it works by driving a small live replica of the app with their keyboard or mouse, and downloads it with confidence. Version, size, checksum, licence and source are all one glance away.

**Assets**, from the pipeline (§0):
- `assets/launch/web/hero-{1920,1280,768}.{avif,webp,jpg}`: serve them with `<picture>`.
- `assets/launch/web/og-image.jpg`
- `assets/launch/web/favicon.svg`, `favicon-32.png`, `apple-touch-icon.png`, `icon-512-maskable.png`
- `assets/launch/web/ready.png`: the "ready" light illustration, e.g. for the download moment.
- `assets/launch/web/fonts/*.woff2`, with their OFL texts.
- `assets/brand/*.svg` (the symbol), `assets/illustrations/*.png`, `docs/screenshots/*.jpg` (synthetic data).

## A2. Content (it must all fit the one screen on desktop)

Put every string in one `content` object.

**Headline** (Fraunces, 2 lines max). Pick one, or propose better ones in the same voice:
- "Make room for good stories." (the app's own onboarding line)
- "Your TV, quietly better."
- "Your subscription. Nothing else."

**Subhead** (Source Sans 3, `text-2`, ≤ 2 lines): "A free, open-source player for your own Xtream Codes or M3U subscription. Native and fast, made for the sofa, and private by design."

**Download cluster:**
- The primary button **"Download for Windows"** (`accent` fill, `text-on-accent`, 52 px). It downloads the **installer** (Part B). Under it, in `caption` `text-3`: `v0.x.y · NN MB · Windows 10/11 x64 · No admin rights needed`, taken from the release config (§A8).
- A secondary link **"Portable zip"** for people who don't want an installer.
- A secondary **"View source"** button (opens in a new tab with `rel="noopener"`).
- A ghost **"SHA-256"** button that reveals the checksums in a small popover with copy buttons, plus a link to `SHA256SUMS.txt`.
- A one-line note: "Your passwords stay in Windows Credential Manager. Nothing is sent anywhere."
- **Unsigned builds:** if the release config says the installer is not code-signed, show a short honest note: "Windows may warn that the publisher is unknown. Check the SHA-256 above." Don't hide it.

**Trust row:** chips (DESIGN §6.6), each with a Phosphor icon, focusable, with a one-line explanation in a tooltip:
- `Open source · GPL-3.0`
- `No telemetry, no accounts`
- `Native app, no web engine`
- `Passwords in Credential Manager`
- `Smooth with 35,000 titles`
- `Keyboard, remote and mouse`

**Feature beats** (4–6 small items, icon + 3–5 words, each **linked to the replica**, §A4): Live TV and guide · Films and series with resume · Catch-up · Parental control. Add these **only when the release config says the version includes them**: Pause and rewind live TV · Record and download · Multiscreen · Sleep timer and reminders.

**Footer line** (`caption` `text-3`, always visible): "opensmarters ships with no channels, content or provider. Use it with a subscription you're entitled to. Independent open-source project, not affiliated with IPTV Smarters or WHMCS Smarters." Add links to Licence, Security (`docs/SECURITY.md`) and Source.

## A3. Layout: one screen

**Desktop (≥ 1024 px wide):** a full-viewport stage (`min-height: 100svh`) with no page scroll at 1280×720 and above. Verify at 1280×720, 1366×768, 1440×900, 1920×1080 and 2560×1440.
- **Background:** `bg`. Behind it, `hero.jpg` cover-fit, **anchored right**, at reduced brightness, with DESIGN's `scrim-left` so text never sits on raw imagery.
- **Left column** (≈ 40%, at the page margin, vertically centred): symbol + wordmark → headline → subhead → download cluster → trust row.
- **Right** (≈ 55%): **the interactive replica**, a 16:9 "screen" framed by the open-corners symbol (a hairline frame with two open diagonal corners and the champagne→rose gradient stroke), `r-lg`, `e3` shadow.
- **Bottom strip:** feature beats on the left, footer line on the right.

**Tablet and mobile:** stack: wordmark → headline → subhead → download → replica → chips → footer. Scrolling is allowed here. On phones the primary button becomes **"Get it on your Windows PC"**: it copies the page link and shows a toast ("Link copied. Open it on your PC.").

## A4. Interactivity (the heart of the page)

### Interactive replica

A small, faithful **HTML/CSS/JS** recreation of the app. It contains no screenshots and no video files. Use the app's synthetic demo names only: Glacier Live, Silverbay Politics, Larkspur Headlines, Oakhaven Politics, Harborlight News HD, Ravenmoor Politics, Zephyr Headlines, Copperfield Live. Logos are monogram tiles in DESIGN §6.4's muted hues.

Scenes, switched with small tabs: "Live TV", "Guide", "Player".
- **Live TV:** a channel list (number, monogram, name, now-title, thin progress bar) and a preview showing a **generated** picture (a slow warm gradient or test-pattern canvas, never real footage, no sound).
- **Guide:** a mini EPG grid with the `live` now-line.
- **Player:** an OSD with the LIVE badge, play/pause circle, ±10 and the rewind bar.

**Keyboard:**
- Arrows move a **single focus** with the exact Afterglow treatment. Cards lift to scale 1.06 in 160 ms; rows brighten to `surface-3` without scaling.
- **Enter** plays the focused channel in the preview.
- **PageUp/PageDown** zap and show the zap banner for 3 s.
- **Digits** open the number-entry box and zap after 1.5 s.
- **Space** shows the rewind state "−00:12 · Go live".
- **Z** flashes "Sleep in 15 min".
- Features the release doesn't include don't respond.

**Pointer:** hover sets focus (there is no separate hover style). On touch, tap to focus and tap again to activate.

**Accessibility of the replica:**
- It is a labelled region (`role="application"`, `aria-label="Interactive preview of opensmarters"`).
- A visible hint row reads `← → ↑ ↓ move · Enter play · PgUp/PgDn change channel`.
- Focus enters it on click or Tab, and **Esc** returns to the page. It never traps focus or captures keys while it isn't focused.

### Linked storytelling

Focusing or hovering a feature beat or trust chip drives the replica:
- "Live TV and guide" switches to the Guide scene.
- "Pause and rewind live TV" shows the −mm:ss badge and rewind bar.
- "Parental control" shows a locked category with the PIN pad.

Moving away restores the visitor's state.

The page's only ambient effect: a very soft champagne radial light (≤ 8% opacity) follows the focused element, eased over 400 ms.

### Download intelligence

- Detect the platform. On Windows the primary button downloads the installer.
- On macOS or Linux it reads "Windows only for now" and offers "Build from source" (the README's build section), never a dead button.
- After a click, show an Afterglow toast: "Downloading v0.x.y · check it with SHA-256: 3f2a…" with a copy button.

### Motion

- No bounces, parallax, autoplay or blinking.
- The replica's picture moves only after the visitor presses Play; before that it shows a still first frame and a quiet "Press Enter to play".
- `prefers-reduced-motion`: no scale, no light movement, no picture animation, fades ≤ 100 ms. The ring and glow stay.

## A5. Engineering constraints

- **Static site in `site/`:** `index.html`, `styles.css`, `main.js`, or TypeScript built with Vite if you need a build step. A framework isn't required; any framework must justify its weight.
- **Zero third-party requests**, matching the app's "no telemetry": no Google Fonts, CDNs, analytics, cookies, embeds or trackers.
- **Fonts:**
  - Use `assets/launch/web/fonts/fraunces-latin.woff2` for display and `afterglow-sans-latin.woff2` for UI.
  - `afterglow-sans-latin.woff2` **is** Source Sans 3, subset and renamed: Source Sans 3's licence reserves the name "Source" for unmodified copies.
  - Declare them with `@font-face` (`font-display: swap`; `font-weight: 200 900` for the UI font) and ship the two OFL texts next to them.
  - Never subset or convert `assets/fonts/SourceSans3-Variable.ttf` under its own name; regenerate through the pipeline instead.
- **Strict CSP** via a `<meta>` tag: `default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; font-src 'self'; connect-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`. No inline scripts or handlers, and no `innerHTML` with non-constant strings.
- **Budget:**
  - HTML + CSS + JS ≤ 120 KB gzipped;
  - fonts ≤ 150 KB;
  - hero AVIF/WebP with a JPEG fallback, ≤ 250 KB at 1920 px;
  - LCP < 1.5 s; CLS 0;
  - Lighthouse ≥ 95 in all four categories.
- **Without JavaScript:** headline, a static screenshot in place of the replica, and working download links.
- **SEO/sharing:**
  - title "opensmarters: a secure, open-source TV player for your own subscription";
  - a meta description;
  - Open Graph and Twitter cards using `assets/launch/web/og-image.jpg` (1200×630, already composed);
  - the favicon set from `assets/launch/web/` and a web manifest with the maskable icon;
  - `theme-color` `#141013`.
- **CSS:** design tokens as custom properties named after DESIGN.md (`--bg`, `--surface-1…4`, `--text`, `--text-2`, `--text-3`, `--accent`, `--focus`, `--live`, `--sage`, `--r-*`, `--e*`). Dark only.

## A6. Accessibility (WCAG 2.2 AA minimum)

- Keyboard-only use everywhere, with the Afterglow focus ring.
- A skip link to the download button.
- Semantic landmarks and a single `h1`.
- Only the token colour pairs from DESIGN §4.2.
- `aria-live="polite"` for the zap banner and toasts.
- Targets ≥ 44×44 px.
- Respect `prefers-reduced-motion` and `prefers-contrast: more`.
- `lang="en"`, with all strings in the `content` object.

## A7. Deliverables

1. The `site/` folder: page, styles, scripts, optimised assets, font licences, `release.js`, `scripts/update-release.ps1`, and a `site/README.md` covering preview and deployment as plain static files (GitHub Pages or any static host).
2. Screenshots you have **looked at**: 1280×720, 1440×900, 1920×1080, 390×844, and one with reduced motion. Also a keyboard-only sequence of stills: Tab to the replica → arrows → Enter → PageDown → Esc → Tab to download.
3. A report: what you built, Lighthouse scores, transferred bytes, what you didn't verify, and every `TODO` left for the owner.

## A8. Release config (single source for everything version-dependent)

```js
// site/release.js: rewritten by site/scripts/update-release.ps1 from tools/package.ps1's output
export const RELEASE = {
  product: "opensmarters",
  version: "TODO",            // e.g. "0.2.0"
  date: "TODO",
  signed: false,              // Authenticode: true once releases are signed
  downloads: {
    installer: { file: "TODO", url: "TODO", sizeMB: 0, sha256: "TODO" },  // Part B
    portable:  { file: "TODO", url: "TODO", sizeMB: 0, sha256: "TODO" },  // today: dist/opensmarters-<v>-windows-x64.zip (59 MB for 0.1.0)
    sumsUrl: "TODO",
  },
  requirements: "Windows 10/11 x64 with a current graphics driver",
  sourceUrl: "TODO (github.com/<org>/<repo>)",
  licence: "GPL-3.0-or-later",
  features: [
    { id: "live",     label: "Live TV and guide",            since: "0.1.0" },
    { id: "vod",      label: "Films and series with resume", since: "0.1.0" },
    { id: "catchup",  label: "Catch-up",                     since: "0.1.0" },
    { id: "parental", label: "Parental control",             since: "0.1.0" },
    { id: "rewind",   label: "Pause and rewind live TV",     since: "0.2.0" },
    { id: "record",   label: "Record and download",          since: "0.2.0" },
    { id: "multi",    label: "Multiscreen",                  since: "0.2.0" },
    { id: "sleep",    label: "Sleep timer and reminders",    since: "0.2.0" },
  ],
};
```

Show a feature only if `since ≤ version`. Leave visible `TODO`s rather than inventing URLs, sizes or hashes.

## A9. Don'ts

- [ ] No other app or provider names, except the one non-affiliation line in the footer. No "Smarters" on its own, no "Open Smarters"/"OpenSmarters".
- [ ] No claims beyond README.md, SECURITY.md and the release config.
- [ ] No fake download counts, testimonials, ratings or "trusted by" logos.
- [ ] No sound, no autoplay, nothing blinking; nothing moves before the visitor does.
- [ ] No third-party requests, cookies or trackers. Check the browser's network panel: only same-origin requests.

---

# Part B: the installer (Rust, beautiful, simple)

## B1. Goal

A **one-click, per-user Windows installer** that feels like the first screen of the app itself, not a generic setup wizard:
- Double-click `opensmarters-<version>-setup.exe`, see one calm screen, press **Install** (or Enter), watch a few seconds of tasteful progress, press **Launch**.
- No UAC prompt, no EULA wall, no "Next, Next, Next", no bundled offers, no network access.
- The same exe also handles **update**, **repair** and **uninstall**.

## B2. Technology decision (record it in `docs/adr/0002-installer.md`)

**Use Rust with eframe/egui** (glow renderer, like the app), as a new workspace crate `crates/installer` (binary `opensmarters-setup`). Why:
- It reuses the Afterglow look natively: same fonts, tokens and focus painting.
- One static exe, no runtime, no web engine (the product promises "no web engine").
- The project's security rules (`#![forbid(unsafe_code)]`, cargo-deny, lean dependencies) apply unchanged.

Record the alternatives you rejected and why:
- **MSI/WiX:** standard but rigid, with a dated UI. It could be added later for enterprise deployment alongside this one.
- **NSIS/Inno Setup:** scripting languages with dated visuals and a weaker security story.
- **Tauri/WebView2:** a web engine, against the product's promise.
- **MSIX:** needs signing and the Store model; possible later.

To avoid two copies of the design system drifting apart:
- **Preferred:** extract the theme pieces the installer needs (colour/space/radius tokens, font installation with variable-font coords, focus ring/glow painting, the primary button and progress bar) from `crates/app/src/ui` into a small shared crate, `crates/afterglow`. Make it a **pure move**: no behaviour changes, app tests and screenshots unchanged.
- **Fallback:** if that would be invasive, give the installer a small `theme.rs` mirroring DESIGN.md and say so in the ADR.

## B3. The experience (every screen, plain language)

Window: **720 × 460 pt**, not resizable. Centred, DPI-aware (per-monitor v2), dark. Custom-drawn header with the symbol, a drag region and a close button. It looks right on Windows 10 and 11, with rounded corners where Windows provides them. Everything works with the keyboard alone (Tab/arrows, Enter, Esc) and with the mouse, using the Afterglow focus ring.

1. **Install (welcome):** one screen.
   - Left: wordmark, then the title "Install opensmarters" (Fraunces), then one line in `text-2`: "Version 0.x.y · about NNN MB · no admin rights needed".
   - Then the primary **Install** button (focused, so Enter installs), and a quiet **Options** link.
   - A `caption` line: "Free software under GPL-3.0 · View licence". The licence is a link, not a wall: GPL doesn't need acceptance.
   - Right: the 300×460 pt art panel `assets/launch/installer/panel@2x.jpg` (an @1x is provided too). It was generated by Codex for exactly this panel, and its left edge already fades into `bg #141013`. Draw the open-corners symbol over it, at rest.
2. **Options** (slides in over 240 ms, same window):
   - Install location: default `%LOCALAPPDATA%\Programs\opensmarters`, with "Change…" (folder picker; `rfd` is already used by the app).
   - Start menu shortcut (on), desktop shortcut (off), "Open opensmarters when done" (on).
   - "Back" returns. Nothing else; no component trees.
3. **Installing:**
   - **The signature moment:** the open-corners symbol *fills with champagne light* along its diagonal as the progress, the afterglow made literal. Draw it as an animated egui mesh or gradient. Under reduced motion, use a plain progress bar inside the symbol frame.
   - One status line that changes calmly: "Unpacking…", "Checking files…", "Adding shortcuts…".
   - **Cancel** asks once, then rolls back completely.
4. **Done:** the "ready" light illustration (`assets/launch/installer/ready.png`, 512 px, shown at about 160 pt) above "opensmarters is ready.", with the primary **Open opensmarters** button and a secondary **Close**. Enter opens the app (launched **unelevated**, as the user), then the installer exits.
5. **Update** (an older version is installed):
   - Title "Update opensmarters to 0.x.y", line "Your accounts, favourites and settings stay as they are."
   - Same flow. Offer to close the running app if needed ("opensmarters is open. Close it to continue."), never kill it silently.
   - Installing an older version over a newer one asks first.
6. **Same version installed:** offer **Repair** (re-verify and rewrite files) and **Uninstall**.
7. **Uninstall** (from Windows Settings › Apps or `--uninstall`):
   - Title "Remove opensmarters?". Two unticked options:
     - "Also delete my data (favourites, history, settings)", i.e. the app data folder;
     - "Also remove saved passwords from Windows Credential Manager" (the app's keychain entries).
   - Say plainly: **recordings and downloads in your Videos folder are never deleted.**
   - Primary **Remove**; secondary **Keep it**.
8. **Error:** a plain sentence of what happened and what to try, a **Try again** button and **Copy details**, which copies the log path and the technical details. Never a raw OS error code on its own.

Copy rules: the voice from §0, sentence case, at most 2 lines per paragraph, no exclamation marks.

## B4. Visual design (unique, modern, still Afterglow)

- **One light source:** only the focused control and the progress light use champagne. Everything else is warm ink and cream text.
- **Typography:** Fraunces (DESIGN coords) only for the screen title; Source Sans 3 for everything else, with tabular digits for sizes and versions.
- **Depth:** the content card sits on `surface-1` with `e3`; the hero crop bleeds to the window edge on the right.
- **Motion:**
  - 240 ms crossfade or slide between screens;
  - 160 ms focus;
  - progress light eased so it never jumps backwards;
  - reduced motion follows Windows' *Animation effects* setting (the app already reads it: reuse `platform::SystemMotion`).
- **Iconography:** Phosphor (Regular at rest, Fill when on), as in the app.
- **Window icon and exe icon:** `assets/icon/` via `embed-resource`, as `crates/app/build.rs` does. Product name "opensmarters" in the version resource.
- Produce **concept renders first**: three screens at 1× and 2× (Install, Installing at ~60%, Done) using the egui build and `--screenshot`-style harness output. Look at them and iterate before polishing behaviour.

## B5. Windows integration (per-user, no elevation)

- **Manifest:** `asInvoker`, per-monitor DPI aware v2, `supportedOS` Windows 10/11, long-path aware. **Never request elevation.** (An "all users" install in Program Files is out of scope for v1; note it as a follow-up.)
- **Install folder** (default `%LOCALAPPDATA%\Programs\opensmarters`) contains:
  - the app exe;
  - `libmpv-2.dll`;
  - the licence files and `THIRD_PARTY_LICENSES.html`;
  - `README.txt`;
  - the uninstaller (a copy of the setup exe as `uninstall.exe`).
- **Shortcuts:** Start menu (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\opensmarters.lnk`) and an optional desktop shortcut. Write them with a safe crate (e.g. `mslnk`), and set the AppUserModelID if the app defines one.
- **Apps & features entry** under `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\opensmarters`: DisplayName, DisplayVersion, Publisher, DisplayIcon, InstallLocation, EstimatedSize, UninstallString, QuietUninstallString, NoModify=1, NoRepair=0, URLInfoAbout. Use the `winreg` crate (safe API).
- **Data folder and keychain:** the installer never touches the app's data folder or Credential Manager, except for the explicit, unticked-by-default uninstall options. When it does delete them, it uses the same paths and service names the app uses (`%LOCALAPPDATA%\opensmarters\…`, keychain entries ending in `.opensmarters` today). Keep those names in one place so the future rename changes them once.
- **Running app:** before replacing files, detect a running app exe from the install folder and ask to close it.
- **Self-removal:** the uninstaller can't delete its own running exe. Copy it to `%TEMP%` and relaunch from there with `--uninstall --from-temp`, then remove the install folder. Leave the temp copy for Windows to clean, or delete it on the next run. Document the approach.
- **Command line** (for power users and IT): `--quiet` (no UI, defaults), `--dir <path>`, `--no-shortcuts`, `--desktop-shortcut`, `--no-launch`, `--uninstall [--purge-data] [--purge-passwords]`, `--log <file>`. Exit codes: 0 ok, 1 error, 2 cancelled, 3 needs the app closed. `--help` prints them.

## B6. Payload and integrity

- At build time, embed the app files as **one compressed archive** in the setup exe: zstd or xz (pick by measured size), with a **SHA-256 manifest** of every file.
- **Integrity first:**
  - verify the archive's hash before extracting anything;
  - verify every file's hash after extraction.
  - A mismatch stops the install with "The installer is damaged. Download it again and check its SHA-256."
- **Atomic and recoverable:**
  - extract to a sibling temp folder (`<dir>.new`);
  - verify, then swap (rename the old folder to `<dir>.old`, the new one into place, delete `<dir>.old`);
  - roll back on any failure or cancel;
  - registry and shortcuts are written last, and removed on rollback.
- **No network access at all:** no downloads, update checks or telemetry. Add a test that the installer binary links no HTTP client crate (`cargo tree`).
- **Size budget:** the setup exe should be **no bigger than the portable zip** (59 MB for 0.1.0, dominated by `libmpv-2.dll`). Report the measured sizes.

## B7. Security

- **Code:**
  - `#![forbid(unsafe_code)]` (the workspace lint already forbids it).
  - If a Windows API truly has no safe wrapper, stop and justify it in the ADR before adding a tiny audited module with `// SAFETY:` comments, following the player crate's rules.
  - Prefer `winreg`, `mslnk` and `directories`/known-folder crates.
- **DLL planting:** harden the DLL search order at startup. The installer runs from Downloads, a classic plant location; reuse `opensmarters_player::harden_dll_search_path` or its approach. Don't load any DLL by bare name.
- **Paths:**
  - canonicalise the chosen install path;
  - refuse system folders, network paths, the root of a drive and paths through junctions/symlinks;
  - install only into an empty or opensmarters-owned folder (marker file + manifest);
  - **uninstall deletes only files listed in the installed manifest**, never a whole arbitrary folder.
- **Signing-ready:**
  - `tools/package.ps1` signs both exes with `signtool` when a certificate is configured (env vars); otherwise it builds unsigned and says so.
  - Until releases are signed, Windows SmartScreen will warn. The landing page explains this honestly (§A2).
- **Logs:** `%TEMP%\opensmarters-setup.log`, with plain text, no secrets (there are none in the installer) and paths shown as-is. `--log` overrides the location.

## B8. Build and release pipeline

- Extend `tools/package.ps1` to produce, in `dist/`:
  - `opensmarters-<version>-windows-x64-setup.exe`;
  - the portable zip;
  - `SHA256SUMS.txt` covering both.
- The setup exe is built **after** the app, from the same staged files as the zip (one source of truth). Embed them through a build script that reads the staged folder, or append the archive to a stub exe with a signed-length trailer; explain your choice in the ADR.
- Builds are reproducible: identical inputs give an identical archive (sorted entries, fixed timestamps).
- Add the release config generator for Part A (`site/scripts/update-release.ps1`), reading `dist/` (file names, sizes, SHA-256).
- **CI:** build the installer in the Windows job. Run its tests, plus a headless `--quiet` install/uninstall into a temp folder with a test registry root (see B10).

## B9. Accessibility and input

- Keyboard-first: initial focus on the primary button, a logical Tab order, Enter activates, Esc goes back/cancels (with confirmation during an install).
- AccessKit labels on every control (eframe's `accesskit` feature is already enabled in the app; give custom-painted widgets `widget_info`).
- Contrast: only DESIGN §4.2 token pairs.
- Respect Windows text scaling and DPI. Check at 100%, 150% and 200%.
- Reduced motion: no slides, a plain progress bar, fades ≤ 100 ms.

## B10. Tests and acceptance

- **Unit tests:** path validation (system folders, junctions, UNC paths, drive roots), manifest hashing, version comparison and the update/downgrade decision, uninstall touching only manifest files, command-line parsing.
- **Integration tests** (no real user state): install, update, repair and uninstall into a temp folder. Use a **configurable registry root** (e.g. `HKCU\Software\opensmartersTest\…` via a test-only option) and temp shortcut folders. Assert files, hashes, registry values and shortcuts, then assert complete removal.
- **Rollback tests:** make extraction fail half-way (a corrupted archive fixture, a read-only file) and assert the previous install is intact.
- **Screenshots you have looked at:** every screen in B3 at 100% and 200% scaling, plus reduced motion and the error screen.
- **Clean-machine check:** install and run on a clean Windows 11 VM (Windows Sandbox needs Pro/Enterprise; this machine runs Home, so use a VM if one is available, otherwise say it wasn't done). Check that the app starts and plays the demo, and that uninstall leaves nothing behind except, by default, user data and passwords.
- **Gates:** `cargo fmt`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace --all-features`; `cargo deny check` (new crates must pass the licence and advisory policy, with a note on why each is needed).
- **Report:** what you built, the measured sizes, the screenshots, what you didn't verify and every `TODO`.

## B11. Don'ts

- [ ] No elevation, no services, no scheduled tasks, no autorun entries, no browser changes, no bundled offers.
- [ ] No network access of any kind.
- [ ] Never delete user data or passwords unless the user ticked the box; never delete recordings in Videos.
- [ ] No other app names in the UI; the product name is always lowercase `opensmarters`.
- [ ] No EULA wall, no "Next, Next, Finish" wizard, no fake progress (the light follows real work).

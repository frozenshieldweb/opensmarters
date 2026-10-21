# Launch-asset pipeline (`tools/assets/`)

Makes every image and font the **landing page** and the **installer** need (see
`docs/LAUNCH_PROMPT.md`) in the project's "Afterglow" art direction (`docs/DESIGN.md`).
Outputs go to `assets/launch/`, with a generated `assets/launch/PROVENANCE.md`.

```
launch.toml          the single list of assets: how each is made, sizes, budgets
briefs/*.md          creative briefs for Codex's image generation (direction only)
templates/*.html     typeset / composited assets, rendered by headless Chrome or Edge
codex_generate.py    stage 1: Codex candidates (non-deterministic, curated by a person)
build.py             stage 2: deterministic build of every output + budgets + provenance
raw/                 Codex candidates and logs (git-ignored)
.build/              render scratch (git-ignored)
```

## Stage 1: generate with Codex, then choose

```powershell
python tools/assets/codex_generate.py generate                  # every [[codex]] asset
python tools/assets/codex_generate.py generate installer-panel --count 2
# look at tools/assets/raw/<id>/<stamp>-contact.jpg, then adopt one candidate:
python tools/assets/codex_generate.py choose installer-panel tools/assets/raw/installer-panel/<stamp>-2.png
```

- Runs `codex exec` with a **read-only sandbox in a scratch folder**, so Codex never sees or edits the repository. The prompt is the brief plus a short preamble: creative direction only, with no names of other products.
- **Which Codex:** the image tool runs through Codex's `codex-code-mode-host` helper. The script prefers a Codex CLI that has that helper next to it. The standalone `~\.codex\.sandbox-bin\codex.exe` lacks it, while the Codex app's `%LOCALAPPDATA%\OpenAI\Codex\bin\<hash>\codex.exe` has it. Override with `CODEX_BIN`, and pass extra flags with `CODEX_ARGS`.
- Images land in `~\.codex\generated_images\<session>\`. The script copies the new ones into `raw/<id>/` and writes a contact sheet and a `-meta.json` (Codex version, brief hash).
- `choose` copies the pick to the manifest's `chosen` path in `assets/launch/sources/` (**committed**, so stage 2 is reproducible). It also writes a sidecar JSON with the candidate name, SHA-256, Codex version, brief path and date; `build.py` puts that into the provenance table.
- Codex output is never used as-is: stage 2 crops, grades, adds grain and exports it.

## Stage 2: build

```powershell
python tools/assets/build.py              # everything; rewrites assets/launch/PROVENANCE.md
python tools/assets/build.py og-image     # only some ids (provenance not rewritten)
```

Each `[[output]]` has a `kind`:

| kind | What it does |
|---|---|
| `photo` | Cover-crop at an anchor, gain in linear light, optional edge fade into a token colour, seeded film grain, then JPEG (baseline, 4:4:4) / WebP / AVIF / PNG exports at the listed widths |
| `lightart` | Light-on-black line art → transparent PNG: alpha = max(R,G,B) with a 10/255 floor, colour un-premultiplied, trimmed, margin, premultiplied resize. Same method as the app's empty-state illustrations |
| `html` | Fills a template (fonts, symbol SVG and hero art inlined as data URIs), screenshots it with headless Chrome/Edge at the given scale (transparent background optional), trims, then downsamples |
| `font` | fontTools: optional axis pinning, Latin subset, WOFF2. A font with a Reserved Font Name is **renamed** (see below) |
| `copy` | Copies a file as-is (e.g. the SVG favicon) |

Every export has a `max_kb` budget. The build lists everything over budget and exits non-zero. It needs Python 3.11+, Pillow with AVIF/WebP, NumPy, fontTools + brotli, and Chrome or Edge (`CHROME` overrides the path).

## Rules

- **No AI-rendered text.** The social image is typeset from the OFL fonts in `templates/`. The wordmark and lockup are the existing `assets/brand/wordmark.png` / `lockup.png`, typeset the same way.
- **No real logos, brands, people, shows or vendor material** in briefs or outputs. Briefs carry creative direction only.
- **Fonts and the OFL:**
  - Fraunces has no Reserved Font Name, so subsetting is fine.
  - **Source Sans 3 reserves "Source"**, so a subset is a Modified Version and must be renamed. The build calls it "Afterglow Sans" and fails if "Source" survives in any name record other than the copyright, trademark and licence notices.
  - The OFL texts are copied next to the WOFF2 files.
  - The app itself keeps embedding the **unmodified** TTFs (`assets/fonts/`).
- **Quality settings that protect the art:**
  - AVIF below q80 and WebP below q90 smooth away the film grain, and the dark gradients band.
  - Grain doubles as dither for 8-bit output.
- Change an asset by editing `launch.toml`, a brief or a template and rebuilding. Never hand-edit files in `assets/launch/`.

## Current outputs

| Area | Files |
|---|---|
| Installer | `installer/panel@1x.jpg`, `panel@2x.jpg` (right-hand art for the 720×460 pt window), `installer/ready.png` |
| Web | `web/hero-{1920,1280,768}.{avif,webp,jpg}`, `web/og-image.jpg` (1200×630), `web/ready.png` (+ `web/ready-128.png` for the landing page's download toast), `web/favicon.svg`, `web/favicon-32.png`, `web/apple-touch-icon.png`, `web/icon-512-maskable.png` |
| Web fonts | `web/fonts/fraunces-latin.woff2`, `web/fonts/afterglow-sans-latin.woff2` (+ OFL texts) |
| Web icons | `web/fonts/phosphor-regular-subset.woff2`, `web/fonts/phosphor-fill-subset.woff2` (+ MIT licence): only the Phosphor glyphs the landing page uses |

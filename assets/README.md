# assets/

Design assets for opensmarters, art direction **"Afterglow"**. How to use them is in [`docs/DESIGN.md`](../docs/DESIGN.md).

- **Embedded** in the binary (via `include_bytes!` / `egui::include_image!`): `fonts/`, `icon/*.png`, `icon/*.ico`, `brand/*.png`, `backgrounds/`, `placeholders/`, `avatars/`, `illustrations/`. Total ≈ 2.1 MB, of which fonts are 0.98 MB. `icon/icon-1024.png` (82 KB) is for packaging and stores, so there's no need to embed it.
- **Not embedded** (sources and documentation): `*.svg`, `concepts/`, this README.

All raster artwork was created for this project. **No** asset contains text except the wordmark/lockup (set from an OFL font, not AI-rendered). No asset contains real logos, trademarks, brands, people or vendor material. Nothing was taken from IPTV Smarters or `_reference/`.

## Provenance

**AI image generation:** OpenAI **Codex CLI 0.162** (`codex exec`, built-in `image_generation` tool) on **2026-10-20**, art-directed and curated by Claude (Anthropic) acting as creative producer. Codex proposed three directions (Afterglow, Lumen Tide, Night Editions; boards in `concepts/directions/`); Afterglow was chosen. The briefs contained only creative direction. Raw generator output is 1672×941 (16:9), 1254×1254 (square) or 1024×1536 (2:3).

**Post-processing** (Python 3.14 + Pillow 12.3 + NumPy, headless Chrome for vector/type rendering). Scripts are summarised per file below:
- *cover-resize*: Lanczos scale to cover the target size, then centre crop.
- *gain*: brightness multiplier applied in linear light (sRGB ↔ γ2.2).
- *grain*: Gaussian luma noise (σ in 8-bit levels, fixed seed). It doubles as dither so dark gradients don't band in 8-bit/JPEG.
- *JPEG*: baseline (not progressive), 4:4:4 chroma, optimised Huffman.
- *black→alpha*: for light-on-pure-black art, alpha = max(R,G,B) with a 10/255 noise floor, colour = RGB/alpha (un-premultiply). The result is trimmed to content, padded and resized in premultiplied space (no dark fringes).
- *supersampled vector render*: SVG rendered by headless Chrome at 8× and box-downsampled in premultiplied space, giving exact pixel coverage for grid-aligned masters.

| File | Size | Source | Prompt summary | Post-processing |
|---|---|---|---|---|
| `backgrounds/hero.jpg` | 1920×1080, 180 KB | Codex image gen (hero v2, candidate A), a refinement of the approved Afterglow board | Softly bowed translucent planes with a luminous champagne rim light and muted-rose halation on the right ~45%; left 55% calm near-black; no mottling/specks; fine film grain; highlights ≤ 85% | cover-resize, grain σ1.6, JPEG q87. Left 55% measured: mean luma 12/255, p99 16. |
| `backgrounds/home-ambient.jpg` | 1920×1080, 85 KB | Codex image gen (backgrounds round) | ~90% deep warm near-black; faint warm glow and the barely visible edge of one bowed plane top-right; lower two thirds empty; max brightness ~30% | cover-resize, gain ×1.35, grain σ1.4, JPEG q84 |
| `backgrounds/player-idle.jpg` | 1920×1080, 109 KB | Codex image gen (backgrounds round) | Symmetric: two dim bowed light planes curving in from the left/right edges, calm dark centre; max ~45% | cover-resize, grain σ1.4, JPEG q84 |
| `placeholders/poster.jpg` | 400×600, 12 KB | Codex image gen (placeholders round) | Portrait 2:3, very dim warm near-black with one faint champagne-to-rose bowed plane from the lower left, empty centre | cover-resize, gain ×1.25, grain σ1.2, JPEG q86 |
| `placeholders/backdrop.jpg` | 1280×720, 49 KB | Codex image gen (placeholders round) | Same treatment, landscape, light from the lower right | cover-resize, gain ×1.25, grain σ1.2, JPEG q86 |
| `placeholders/channel-logo.png` | 256×256 RGBA, 2 KB | Vector (the brand symbol) | n/a | `brand/symbol.svg` recoloured `#A8989E`, scaled to 62%, supersampled vector render |
| `avatars/avatar-1.jpg` … `avatar-6.jpg` | 256×256, 6-11 KB each | Codex image gen (avatars round; avatar 1 used as style reference for 2-6) | Abstract emblem of bowed translucent light on warm near-black, one hue each: 1 champagne rising arc, 2 rose overlapping petals, 3 sage wave, 4 dusk-blue tilted disc, 5 lilac folded ribbon, 6 amber-terracotta stacked bowed bars | full-frame Lanczos resize, JPEG q88 4:4:4. Checked as circles at 128 and 48 px. |
| `illustrations/empty-favourites.png` | 512×512 RGBA | Codex image gen (empty states v2; v1 used as reference) | Luminous line art "drawn with light", champagne→cream→rose strokes with close halation, shared bowed horizon arc, pure black background. Motif: empty heart + small four-point spark | black→alpha, 6% margin, 512 px |
| `illustrations/empty-search.png` | 512×512 RGBA | same set | Magnifying lens resting on the horizon, empty inside | same |
| `illustrations/error-offline.png` | 512×512 RGBA | same set | Two unplugged cable ends reaching toward each other above the horizon | same |
| `illustrations/empty-guide.png` | 512×512 RGBA | same set | Small clock dial above four rounded bars; the third is a faint dashed rose outline | same |
| `illustrations/parental-lock.png` | 512×512 RGBA | same set | Rounded padlock with a small rose keyhole | same |
| `icon/icon.svg` | vector | Design by Codex ("Open corners", chosen from 6 concepts and refined in a second round, variant v1); clean parametric redraw by Claude | 5:4 frame, constant 70-unit band (14% of width), concentric radii 130/60, openings top-right and bottom-left, 180° symmetry, champagne `#E6BE91` → rose `#C9868A`; Windows-11-style plate 896² r224, gradient `#2A2026`→`#141013`, 10% white hairline | generated by a small Python script (straight bands instead of Codex's 2% bow, so the terminals are square and it matches the pixel masters) |
| `icon/icon-16.svg`, `icon/icon-32.svg` | vector | Codex, hand-authored pixel-grid masters (v1-16, v1-32), unchanged | Full-bleed/1 px-inset plate `#1E171B`, flat champagne symbol on whole/half pixels | none |
| `icon/icon-24.svg` | vector | Claude, the same construction on a 24 px grid | 3 px bands, concentric 4/1 radii | none |
| `icon/icon-1024.png`, `icon/icon-256.png` | 1024², 256² RGBA | renders of `icon.svg` | n/a | headless Chrome |
| `icon/opensmarters.ico` | 7 frames, 19 KB | 16/24/32 from the pixel masters, 48/64 from `icon.svg` (supersampled vector render), 128/256 direct render | n/a | Pillow ICO writer, PNG-compressed frames, each read back and verified byte-identical to its source frame |
| `brand/symbol.svg` / `symbol-gradient.svg` / `symbol.png` | vector / 512² RGBA | same geometry as `icon.svg`, without the plate | n/a | Chrome render of `symbol-gradient.svg` |
| `brand/wordmark.png` | 713×117 RGBA, 17 KB | **Typeset, not generated:** "opensmarters" in Fraunces (opsz 144, wght 560, SOFT 100, WONK 0), 128 px, tracking -1%, `#F4ECE4` | n/a | headless Chrome (HarfBuzz shaping, GPOS kerning), trimmed with 8 px padding |
| `brand/lockup.png` | 856×129 RGBA, 20 KB | `symbol-gradient.svg` (132 px box) + the wordmark, 22 px gap, optically centred | n/a | as above |
| `fonts/SourceSans3-Variable.ttf` | 631 KB | [google/fonts `ofl/sourcesans3/SourceSans3[wght].ttf`](https://github.com/google/fonts/tree/main/ofl/sourcesans3) (upstream [adobe-fonts/source-sans](https://github.com/adobe-fonts/source-sans) @ `272b22b`), downloaded 2026-10-20 | n/a | **unmodified** (file renamed only). SHA-256 `042fe2cc0b933e328410d7acbd0aa6a1873dca5aef81875f4bc214b08825c7b9`. Licence: SIL OFL 1.1 with Reserved Font Name "Source", see `fonts/OFL-SourceSans3.txt`. |
| `fonts/Fraunces-Variable.ttf` | 352 KB | [google/fonts `ofl/fraunces/Fraunces[SOFT,WONK,opsz,wght].ttf`](https://github.com/google/fonts/tree/main/ofl/fraunces) (upstream [undercasetype/Fraunces](https://github.com/undercasetype/Fraunces) @ `d6d3857`), downloaded 2026-10-20 | n/a | **unmodified** (file renamed only). SHA-256 `177ff6c0f14e5550a3c624247cd1189611d4eb65d000b14944c63d967958abbb`. Licence: SIL OFL 1.1, see `fonts/OFL-Fraunces.txt`. |

Because Source Sans 3 has a Reserved Font Name, **do not subset or otherwise modify** the font file. Ship it as is, or rename the family if you ever need a modified build.

### `concepts/` (documentation only, never embedded)

| File | What |
|---|---|
| `directions/1-afterglow-chosen.jpg`, `2-lumen-tide.jpg`, `3-night-editions.jpg` | The three direction key visuals Codex proposed (image gen), downscaled to 1280 px |
| `icon-exploration-round1.jpg`, `icon-exploration-round2.jpg` | QA renders of Codex's 6 symbol concepts and of the 4 refined "Open corners" variants, at 256/48/32/24/16 px on dark and light taskbars |
| `login.jpg`, `home.jpg`, `home-scrolled.jpg`, `live-tv.jpg`, `detail.jpg`, `player.jpg` (TV density, zoom 1.75) | 1920×1080 concept mockups (JPEG q82, 1.9 MB for the whole folder): HTML/CSS authored by Codex from `docs/DESIGN.md`, using the real fonts, assets and Phosphor icons (MIT), rendered with headless Chrome. Film/series/channel names are fictional; the content art is fictional Codex image-gen artwork (landscapes, architecture, no people or text) made only for these mockups. |

## Icons used in the UI
Not stored here. Use **Phosphor** (MIT) via the `egui-phosphor` crate, Regular + Fill (see DESIGN.md §4.8).

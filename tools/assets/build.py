#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Stage 2 of the launch-asset pipeline: build every [[output]] in launch.toml.

Deterministic from committed sources: the Codex picks in `assets/launch/sources/`, the
existing brand art in `assets/`, the OFL fonts, and the HTML templates in
`tools/assets/templates/` (rendered by headless Chrome/Edge). Writes `assets/launch/` and
`assets/launch/PROVENANCE.md`, and fails when an output exceeds its size budget.

    python tools/assets/build.py            # everything
    python tools/assets/build.py og-image   # only some ids

Requires Python 3.11+, Pillow (with AVIF/WebP), NumPy, fontTools + brotli, and Chrome or
Edge (set CHROME to override the path).
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BUILD = HERE / ".build"

# Latin + the punctuation and symbols the page and installer use (· − … › ← → ↑ ↓ ✓ ★ €).
LATIN = (
    "U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+2000-206F,"
    "U+2074,U+20AC,U+2122,U+2190-2193,U+2212,U+2215,U+2713,U+2605,U+FEFF,U+FFFD"
)


# ------------------------------------------------------------------------------ helpers

def rel(p: Path) -> str:
    return p.resolve().relative_to(ROOT).as_posix()


def hex_rgb(h: str) -> tuple[int, int, int]:
    h = h.lstrip("#")
    return int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)


def to_linear(a: np.ndarray) -> np.ndarray:
    return np.power(a / 255.0, 2.2)


def to_srgb(a: np.ndarray) -> np.ndarray:
    return np.clip(np.power(np.clip(a, 0, 1), 1 / 2.2) * 255.0, 0, 255)


def cover(im: Image.Image, size: tuple[int, int], anchor: tuple[float, float]) -> Image.Image:
    """Scale to cover `size`, then crop at `anchor` (0..1 in x and y)."""
    tw, th = size
    s = max(tw / im.width, th / im.height)
    w, h = max(tw, round(im.width * s)), max(th, round(im.height * s))
    im = im.resize((w, h), Image.Resampling.LANCZOS)
    x = round((w - tw) * anchor[0])
    y = round((h - th) * anchor[1])
    return im.crop((x, y, x + tw, y + th))


def save(im: Image.Image, path: Path, quality: int | None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    ext = path.suffix.lower()
    if ext in (".jpg", ".jpeg"):
        im.convert("RGB").save(path, "JPEG", quality=quality or 86, optimize=True,
                               progressive=False, subsampling=0)
    elif ext == ".webp":
        im.save(path, "WEBP", quality=quality or 80, method=6)
    elif ext == ".avif":
        im.save(path, "AVIF", quality=quality or 62, speed=4)
    elif ext == ".png":
        im.save(path, "PNG", optimize=True)
    else:
        raise ValueError(f"unknown format {path}")


def resize_premultiplied(im: Image.Image, size: tuple[int, int]) -> Image.Image:
    """Resize RGBA without dark fringes (premultiply, resize, un-premultiply)."""
    if im.mode != "RGBA":
        return im.resize(size, Image.Resampling.LANCZOS)
    a = np.asarray(im).astype(np.float64)
    rgb, alpha = a[..., :3], a[..., 3:4] / 255.0
    pm = np.concatenate([rgb * alpha, alpha * 255.0], axis=-1)
    chans = [Image.fromarray(pm[..., i].astype(np.float32), "F").resize(size, Image.Resampling.LANCZOS)
             for i in range(4)]
    out = np.stack([np.asarray(c) for c in chans], axis=-1)
    al = np.clip(out[..., 3:4], 0, 255)
    with np.errstate(divide="ignore", invalid="ignore"):
        col = np.where(al > 0, out[..., :3] * 255.0 / al, 0)
    res = np.concatenate([np.clip(col, 0, 255), al], axis=-1).round().astype(np.uint8)
    return Image.fromarray(res, "RGBA")


def data_uri(path: Path, mime: str) -> str:
    return f"data:{mime};base64," + base64.b64encode(path.read_bytes()).decode("ascii")


def chrome() -> str:
    for cand in (
        os.environ.get("CHROME"),
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        shutil.which("chrome"), shutil.which("google-chrome"), shutil.which("chromium"),
    ):
        if cand and Path(cand).exists():
            return cand
    sys.exit("Chrome/Edge not found: set CHROME")


# ------------------------------------------------------------------------------ kinds

def build_photo(o: dict, out: Path) -> tuple[list[Path], str]:
    src = ROOT / o["src"]
    im = Image.open(src).convert("RGB")
    im = cover(im, tuple(o["size"]), tuple(o.get("anchor", [0.5, 0.5])))
    a = to_linear(np.asarray(im).astype(np.float64)) * float(o.get("gain", 1.0))
    if fade := o.get("edge_fade"):
        w = a.shape[1]
        ramp = np.clip(np.arange(w) / max(1.0, fade["width"] * w), 0, 1)
        ramp = ramp * ramp * (3 - 2 * ramp)            # smoothstep
        if fade["side"] == "right":
            ramp = ramp[::-1]
        col = to_linear(np.array(hex_rgb(fade["color"]), dtype=np.float64))
        a = a * ramp[None, :, None] + col[None, None, :] * (1 - ramp[None, :, None])
    img = to_srgb(a)
    if (sigma := float(o.get("grain", 0))) > 0:
        rng = np.random.default_rng(int(o.get("seed", 0)))
        noise = rng.normal(0.0, sigma, img.shape[:2])[..., None]
        img = np.clip(img + noise, 0, 255)
    base = Image.fromarray(img.round().astype(np.uint8), "RGB")
    paths = []
    for e in o["exports"]:
        w = int(e.get("width", base.width))
        h = round(base.height * w / base.width)
        im2 = base if w == base.width else base.resize((w, h), Image.Resampling.LANCZOS)
        p = out / e["path"]
        save(im2, p, e.get("quality"))
        paths.append(p)
    steps = [f"cover {o['size'][0]}×{o['size'][1]} anchor {o.get('anchor', [0.5, 0.5])}"]
    if o.get("gain", 1.0) != 1.0:
        steps.append(f"gain ×{o['gain']}")
    if o.get("edge_fade"):
        f = o["edge_fade"]
        steps.append(f"{f['side']} fade {int(f['width'] * 100)}% → {f['color']}")
    if o.get("grain"):
        steps.append(f"grain σ{o['grain']} seed {o.get('seed', 0)}")
    return paths, "; ".join(steps)


def build_lightart(o: dict, out: Path) -> tuple[list[Path], str]:
    """Light-on-black art → transparent PNG: alpha = max(R,G,B) with a 10/255 floor,
    colour un-premultiplied, trimmed, padded and resized in premultiplied space."""
    a = np.asarray(Image.open(ROOT / o["src"]).convert("RGB")).astype(np.float64)
    alpha = a.max(axis=-1)
    alpha = np.clip((alpha - 10.0) / (255.0 - 10.0), 0, 1) * 255.0
    with np.errstate(divide="ignore", invalid="ignore"):
        col = np.where(alpha[..., None] > 0, a * 255.0 / np.maximum(a.max(axis=-1, keepdims=True), 1), 0)
    rgba = np.concatenate([np.clip(col, 0, 255), alpha[..., None]], axis=-1).round().astype(np.uint8)
    im = Image.fromarray(rgba, "RGBA")
    bbox = im.getchannel("A").point(lambda v: 255 if v > 2 else 0).getbbox()
    if bbox:
        im = im.crop(bbox)
    side = max(im.size)
    pad = round(side * float(o.get("margin", 0.06)))
    canvas = Image.new("RGBA", (side + 2 * pad, side + 2 * pad), (0, 0, 0, 0))
    canvas.paste(im, ((canvas.width - im.width) // 2, (canvas.height - im.height) // 2))
    paths = []
    for e in o["exports"]:
        w = int(e.get("width", o["size"]))
        p = out / e["path"]
        save(resize_premultiplied(canvas, (w, w)), p, None)
        paths.append(p)
    return paths, f"black→alpha (floor 10/255), trim, margin {int(o.get('margin', 0.06) * 100)}%, premultiplied resize"


def html_tokens(m: dict) -> dict[str, str]:
    a = ROOT / "assets"
    return {
        "{{WORDMARK}}": m["wordmark"],
        "{{FONT_DISPLAY}}": data_uri(a / "fonts/Fraunces-Variable.ttf", "font/ttf"),
        "{{FONT_UI}}": data_uri(a / "fonts/SourceSans3-Variable.ttf", "font/ttf"),
        "{{SYMBOL_SVG}}": (a / "brand/symbol-gradient.svg").read_text(encoding="utf-8"),
        "{{ICON32_SVG}}": (a / "icon/icon-32.svg").read_text(encoding="utf-8"),
        "{{HERO}}": data_uri(a / "backgrounds/hero.jpg", "image/jpeg"),
    }


def build_html(o: dict, out: Path, m: dict) -> tuple[list[Path], str]:
    html = (HERE / o["template"]).read_text(encoding="utf-8")
    for k, v in html_tokens(m).items():
        html = html.replace(k, v)
    BUILD.mkdir(parents=True, exist_ok=True)
    page = BUILD / f"{o['id']}.html"
    page.write_text(html, encoding="utf-8", newline="\n")
    shot = BUILD / f"{o['id']}.png"
    if shot.exists():
        shot.unlink()
    vw, vh = o["viewport"]
    scale = int(o.get("scale", 1))
    args = [
        chrome(), "--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run",
        "--no-default-browser-check", "--disable-extensions", "--force-color-profile=srgb",
        f"--force-device-scale-factor={scale}", f"--window-size={vw},{vh}",
        f"--screenshot={shot}", "--virtual-time-budget=4000",
    ]
    if o.get("transparent"):
        args.append("--default-background-color=00000000")
    args.append(page.as_uri())
    subprocess.run(args, check=True, capture_output=True, timeout=120)
    im = Image.open(shot).convert("RGBA" if o.get("transparent") else "RGB")
    if (pad := o.get("trim_pad")) is not None:
        bbox = im.getchannel("A").getbbox()
        if bbox:
            im = im.crop(bbox)
        canvas = Image.new("RGBA", (im.width + 2 * pad, im.height + 2 * pad), (0, 0, 0, 0))
        canvas.paste(im, (pad, pad))
        im = canvas
    paths = []
    for e in o["exports"]:
        im2 = im
        if (w := e.get("width")) and w != im.width:
            h = round(im.height * w / im.width)
            im2 = resize_premultiplied(im, (w, h)) if im.mode == "RGBA" else im.resize((w, h), Image.Resampling.LANCZOS)
        p = out / e["path"]
        save(im2, p, e.get("quality"))
        paths.append(p)
    return paths, f"headless Chrome render of `{o['template']}` at {scale}× ({vw}×{vh})"


def unicode_ranges(spec: str) -> list[int]:
    if spec == "latin":
        spec = LATIN
    cps: list[int] = []
    for part in spec.split(","):
        part = part.strip().removeprefix("U+")
        if "-" in part:
            lo, hi = part.split("-")
            cps.extend(range(int(lo, 16), int(hi, 16) + 1))
        else:
            cps.append(int(part, 16))
    return cps


def build_font(o: dict, out: Path) -> tuple[list[Path], str]:
    from fontTools import subset
    from fontTools.ttLib import TTFont
    from fontTools.varLib import instancer

    # recalcTimestamp=False keeps head.modified from the source: reproducible output.
    font = TTFont(ROOT / o["src"], recalcTimestamp=False)
    steps = []
    if pin := o.get("pin"):
        font = instancer.instantiateVariableFont(font, dict(pin), inplace=False)
        font.recalcTimestamp = False
        steps.append("pinned " + ", ".join(f"{k}={v}" for k, v in pin.items()))
    opts = subset.Options()
    opts.layout_features = ["*"]
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.hinting = False
    opts.notdef_outline = True
    opts.flavor = "woff2"
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=unicode_ranges(o.get("unicodes", "latin")))
    sub.subset(font)
    steps.append(f"subset {o.get('unicodes', 'latin')}, hinting removed")
    if old := o.get("rename_from"):
        rename_family(font, o["family"], old)
        steps.append(f"renamed to “{o['family']}” (Reserved Font Name “{old}” removed)")
    paths = []
    for e in o["exports"]:
        p = out / e["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        font.flavor = "woff2"
        font.save(p)
        paths.append(p)
        if lic := o.get("licence"):
            shutil.copy2(ROOT / lic, p.parent / Path(lic).name)
    steps.append("WOFF2")
    return paths, "; ".join(steps)


# Name records that may keep the original name: copyright, trademark, licence, licence URL.
KEEP_NAME_IDS = {0, 7, 13, 14}


def rename_family(font, family: str, reserved: str) -> None:
    """OFL 1.1 §3: a Modified Version must not use a Reserved Font Name. Replace the family
    in every name record that carries it (ids 1, 3, 4, 6, 16, 18, 21, 25 and fvar instance
    PostScript names) and fail if the reserved name is left anywhere it may not be."""
    name = font["name"]
    ps_family = family.replace(" ", "")
    olds = sorted({r.toUnicode() for r in name.names if r.nameID in (1, 16, 21)}, key=len, reverse=True)
    ps_olds = sorted({r.toUnicode().split("-")[0] for r in name.names if r.nameID in (6, 25)}, key=len, reverse=True)
    for r in name.names:
        if r.nameID in KEEP_NAME_IDS:
            continue
        s = r.toUnicode()
        for o in olds:
            s = s.replace(o, family)
        for o in ps_olds:
            s = s.replace(o, ps_family)
        r.string = s
    left = [(r.nameID, r.toUnicode()) for r in name.names
            if r.nameID not in KEEP_NAME_IDS and reserved.lower() in r.toUnicode().lower()]
    if left:
        raise SystemExit(f"reserved name still present after renaming: {left}")
    if "CFF " in font:
        font["CFF "].cff.fontNames = [f"{ps_family}-Regular"]


def build_copy(o: dict, out: Path) -> tuple[list[Path], str]:
    paths = []
    for e in o["exports"]:
        p = out / e["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / o["src"], p)
        paths.append(p)
    return paths, f"copy of `{o['src']}`"


# ------------------------------------------------------------------------------ main

def describe(p: Path) -> str:
    try:
        with Image.open(p) as im:
            return f"{im.width}×{im.height}"
    except Exception:  # fonts, SVG
        return "n/a"


def source_note(o: dict) -> str:
    src = o.get("src") or o.get("template", "")
    side = (ROOT / src).with_suffix(".json") if src else None
    if side and side.exists():
        s = json.loads(side.read_text(encoding="utf-8"))
        return (f"Codex image gen ({s.get('codex')}, generated {s.get('generated')}, candidate "
                f"`{s.get('candidate')}`, brief `tools/assets/{s.get('brief')}`), source `{src}`")
    if o["kind"] == "html":
        return f"typeset/composited from `tools/assets/{o['template']}`"
    return f"`{src}`"


def main() -> None:
    # Windows consoles default to a legacy code page; our messages use arrows and ×.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("ids", nargs="*")
    args = ap.parse_args()
    with open(HERE / "launch.toml", "rb") as f:
        m = tomllib.load(f)
    out = ROOT / m["out_dir"]
    kinds = {"photo": build_photo, "lightart": build_lightart, "font": build_font, "copy": build_copy}
    rows, over, missing = [], [], []
    for o in m["output"]:
        if args.ids and o["id"] not in args.ids:
            continue
        if "src" in o and not (ROOT / o["src"]).exists():
            missing.append(f"{o['id']}: {o['src']} (run codex_generate.py, then choose)")
            continue
        fn = kinds.get(o["kind"])
        paths, how = build_html(o, out, m) if o["kind"] == "html" else fn(o, out)
        for p, e in zip(paths, o["exports"], strict=True):
            kib = p.stat().st_size / 1024
            if kib > e["max_kb"]:
                over.append(f"{rel(p)}: {kib:.0f} KiB > {e['max_kb']} KiB")
            rows.append((rel(p), describe(p), f"{kib:.0f}", source_note(o), how))
            print(f"  {rel(p):48} {describe(p):>11} {kib:7.0f} KiB")
    if not args.ids:
        write_provenance(out, m, rows)
    for line in missing:
        print("missing source:", line)
    if over:
        print("\nover budget:\n  " + "\n  ".join(over))
        sys.exit(1)
    if missing:
        sys.exit(2)


def write_provenance(out: Path, m: dict, rows: list[tuple[str, ...]]) -> None:
    lines = [
        "# Launch assets: provenance",
        "",
        f"Generated by `tools/assets/build.py` from `tools/assets/launch.toml` (product: "
        f"{m['product']}). **Don't edit by hand**: change the manifest, a brief or a template "
        "and rebuild. How the pipeline works: `tools/assets/README.md`.",
        "",
        "No asset contains text except the typeset social image (set from the OFL fonts, never "
        "AI-rendered). No real logos, brands, people or vendor material.",
        "",
        "| File | Size | KiB | Source | Processing |",
        "|---|---|---|---|---|",
    ]
    lines += [f"| `{r[0]}` | {r[1]} | {r[2]} | {r[3]} | {r[4]} |" for r in rows]
    lines += [
        "",
        "Fonts: `fraunces-latin.woff2` is a subset of Fraunces (SIL OFL 1.1, no Reserved Font "
        "Name). `afterglow-sans-latin.woff2` is a **Modified Version of Source Sans 3** (SIL OFL "
        "1.1): subset and renamed, because Source Sans 3 reserves the name “Source”. Both "
        "licence texts ship next to the WOFF2 files.",
        "",
    ]
    (out / "PROVENANCE.md").write_text("\n".join(lines), encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()

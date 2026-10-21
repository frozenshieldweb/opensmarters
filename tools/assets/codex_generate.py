#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Stage 1 of the launch-asset pipeline: generate image candidates with Codex CLI.

Codex's built-in image generation tool is non-deterministic, so this stage only produces
*candidates* in `tools/assets/raw/<id>/` (git-ignored) plus a contact sheet. A person picks
one with the `choose` command, which copies it to the manifest's `chosen` path (committed)
and writes a provenance sidecar next to it. Stage 2 (`build.py`) is deterministic from there.

    python tools/assets/codex_generate.py generate [ID ...] [--count N]
    python tools/assets/codex_generate.py choose ID tools/assets/raw/ID/<file>.png

Codex runs with a read-only sandbox in a scratch folder: it only uses its image tool and
never sees or edits the repository. Set CODEX_BIN to override the executable.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
RAW = HERE / "raw"
CODEX_HOME = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
TIMEOUT_S = 30 * 60
# Extra `codex exec` arguments (space-separated), e.g. CODEX_ARGS="--disable code_mode_host"
# when the CLI's code-mode host helper isn't installed (the image tool then fails to start).
EXTRA_ARGS = os.environ.get("CODEX_ARGS", "").split()

PREAMBLE = """You are the image generator for a small open-source desktop app's launch kit.
Use your built-in image generation tool. Do not run shell commands, do not read, create or
edit files, and do not browse. Follow the brief exactly; it is creative direction only.

"""


def manifest() -> dict:
    with open(HERE / "launch.toml", "rb") as f:
        return tomllib.load(f)


def codex_bin() -> str:
    """CODEX_BIN, else a Codex CLI that has its code-mode host next to it (the image tool
    runs through that helper and fails without it), else any Codex CLI found."""
    if env := os.environ.get("CODEX_BIN"):
        return env
    local = Path(os.environ.get("LOCALAPPDATA", ""))
    cands = [Path(p) for p in (shutil.which("codex"),) if p]
    cands += sorted((local / "OpenAI" / "Codex" / "bin").glob("*/codex.exe"),
                    key=lambda p: p.stat().st_mtime, reverse=True)
    cands.append(CODEX_HOME / ".sandbox-bin" / "codex.exe")
    cands = [c for c in cands if c.exists()]
    with_host = [c for c in cands if any(c.parent.glob("codex-code-mode-host*"))]
    if with_host or cands:
        return str((with_host or cands)[0])
    sys.exit("Codex CLI not found: set CODEX_BIN to codex(.exe)")


def codex_version(exe: str) -> str:
    out = subprocess.run([exe, "--version"], capture_output=True, text=True, check=False)
    return (out.stdout or out.stderr).strip()


def new_images(since: float) -> list[Path]:
    root = CODEX_HOME / "generated_images"
    if not root.is_dir():
        return []
    found = [p for p in root.rglob("*.png") if p.stat().st_mtime >= since]
    return sorted(found, key=lambda p: p.stat().st_mtime)


def contact_sheet(images: list[Path], out: Path, thumb_h: int = 420) -> None:
    thumbs = []
    for p in images:
        im = Image.open(p).convert("RGB")
        w = round(im.width * thumb_h / im.height)
        thumbs.append(im.resize((w, thumb_h), Image.Resampling.LANCZOS))
    gap, label_h = 16, 28
    width = sum(t.width for t in thumbs) + gap * (len(thumbs) + 1)
    sheet = Image.new("RGB", (max(width, 200), thumb_h + label_h + gap * 2), (20, 16, 19))
    draw = ImageDraw.Draw(sheet)
    x = gap
    for i, t in enumerate(thumbs, start=1):
        sheet.paste(t, (x, gap))
        draw.text((x + 4, gap + thumb_h + 6), f"{i}: {images[i - 1].name}", fill=(244, 236, 228))
        x += t.width + gap
    sheet.save(out, quality=88)


def generate(ids: list[str], count: int | None) -> None:
    m = manifest()
    exe = codex_bin()
    version = codex_version(exe)
    jobs = [c for c in m["codex"] if not ids or c["id"] in ids]
    if not jobs:
        sys.exit(f"no [[codex]] entries match {ids}")
    scratch = RAW / ".codex-cwd"
    scratch.mkdir(parents=True, exist_ok=True)
    for job in jobs:
        n = count or int(job.get("candidates", 3))
        brief = (HERE / job["brief"]).read_text(encoding="utf-8")
        prompt = (
            PREAMBLE
            + brief
            + f"\n\nGenerate {n} distinct variations, each at {job['gen_size']}. "
            "Reply with one short line per variation describing how it differs."
        )
        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        out_dir = RAW / job["id"]
        out_dir.mkdir(parents=True, exist_ok=True)
        log = out_dir / f"{stamp}-codex.log"
        print(f"[{job['id']}] codex exec ({version}), {n} candidates…", flush=True)
        start = time.time()
        with open(log, "w", encoding="utf-8") as lf:
            proc = subprocess.run(
                [exe, "exec", "--skip-git-repo-check", "--sandbox", "read-only",
                 *EXTRA_ARGS, "-C", str(scratch), "-"],
                input=prompt, stdout=lf, stderr=subprocess.STDOUT, text=True,
                encoding="utf-8", timeout=TIMEOUT_S, check=False,
            )
        images = new_images(start - 1)
        copies = []
        for i, src in enumerate(images, start=1):
            dst = out_dir / f"{stamp}-{i}.png"
            shutil.copy2(src, dst)
            copies.append(dst)
        meta = {
            "id": job["id"], "codex": version, "date": stamp, "exit": proc.returncode,
            "brief": job["brief"],
            "brief_sha256": hashlib.sha256(brief.encode("utf-8")).hexdigest(),
            "candidates": [c.name for c in copies],
            "originals": [str(p) for p in images],
        }
        (out_dir / f"{stamp}-meta.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
        if copies:
            sheet = out_dir / f"{stamp}-contact.jpg"
            contact_sheet(copies, sheet)
            print(f"[{job['id']}] {len(copies)} image(s) → {out_dir}\n  contact sheet: {sheet}")
        else:
            print(f"[{job['id']}] no images (exit {proc.returncode}); see {log}")


def choose(asset_id: str, candidate: str) -> None:
    m = manifest()
    job = next((c for c in m["codex"] if c["id"] == asset_id), None)
    if job is None:
        sys.exit(f"unknown codex id {asset_id}")
    src = Path(candidate).resolve()
    if not src.is_file():
        sys.exit(f"no such file: {src}")
    stamp = src.stem.rsplit("-", 1)[0]
    meta_path = src.parent / f"{stamp}-meta.json"
    meta = json.loads(meta_path.read_text(encoding="utf-8")) if meta_path.exists() else {}
    dst = ROOT / job["chosen"]
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
    with Image.open(dst) as im:
        size = list(im.size)
    sidecar = {
        "id": asset_id,
        "file": dst.name,
        "candidate": src.name,
        "size": size,
        "sha256": hashlib.sha256(dst.read_bytes()).hexdigest(),
        "codex": meta.get("codex"),
        "generated": meta.get("date"),
        "brief": job["brief"],
        "brief_sha256": meta.get("brief_sha256"),
        "chosen": dt.date.today().isoformat(),
    }
    dst.with_suffix(".json").write_text(json.dumps(sidecar, indent=2), encoding="utf-8")
    print(f"chose {src.name} → {dst.relative_to(ROOT)} (+ provenance sidecar)")


def main() -> None:
    # Windows consoles default to a legacy code page; our messages use arrows and ×.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("generate", help="generate candidates with Codex")
    g.add_argument("ids", nargs="*", help="[[codex]] ids (default: all)")
    g.add_argument("--count", type=int, help="candidates per asset (default: manifest)")
    c = sub.add_parser("choose", help="adopt a candidate as the committed source")
    c.add_argument("id")
    c.add_argument("candidate")
    a = ap.parse_args()
    if a.cmd == "generate":
        generate(a.ids, a.count)
    else:
        choose(a.id, a.candidate)


if __name__ == "__main__":
    main()

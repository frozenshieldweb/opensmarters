# SPDX-License-Identifier: GPL-3.0-or-later
"""Builds the web UI's WOFF2 fonts into web/public/fonts/ from the OFL masters in assets/fonts/.

The landing page's Latin subsets (tools/assets/launch.toml) cover the UI's own text. Provider
text (channel and film names) is often Greek, Cyrillic, Vietnamese or accented Latin, which
Source Sans 3 also covers (DESIGN.md §4.7), so the UI adds a second, unicode-range-split file
for those scripts. Same pipeline functions as tools/assets/build.py: subset, hinting removed,
and Source Sans 3 renamed to "Afterglow Sans" (OFL 1.1 §3, Reserved Font Name "Source").

Run from the repository root: python web/scripts/fonts.py   (needs fontTools + brotli)
"""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "assets"))

import build  # noqa: E402  (tools/assets/build.py)

OUT = ROOT / "web" / "public" / "fonts"

# Everything Source Sans 3 covers beyond build.LATIN: Latin Extended-A/B and Additional
# (Vietnamese), combining marks, Greek and Coptic, Cyrillic and Cyrillic Supplement.
EXTENDED = "U+0100-024F,U+0259,U+0300-036F,U+0370-03FF,U+0400-052F,U+1E00-1EFF,U+20A0-20C0"

OUTPUTS = [
    {
        "src": "assets/fonts/SourceSans3-Variable.ttf",
        "family": "Afterglow Sans",
        "rename_from": "Source",
        "unicodes": "latin",
        "licence": "assets/fonts/OFL-SourceSans3.txt",
        "exports": [{"path": "afterglow-sans-latin.woff2"}],
    },
    {
        "src": "assets/fonts/SourceSans3-Variable.ttf",
        "family": "Afterglow Sans",
        "rename_from": "Source",
        "unicodes": EXTENDED,
        "licence": "assets/fonts/OFL-SourceSans3.txt",
        "exports": [{"path": "afterglow-sans-ext.woff2"}],
    },
    {
        "src": "assets/fonts/Fraunces-Variable.ttf",
        "family": "Fraunces",
        "pin": {"SOFT": 100, "WONK": 0},
        "unicodes": "latin",
        "licence": "assets/fonts/OFL-Fraunces.txt",
        "exports": [{"path": "fraunces-latin.woff2"}],
    },
]


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for o in OUTPUTS:
        paths, how = build.build_font(o, OUT)
        for p in paths:
            print(f"{p.relative_to(ROOT)}  {p.stat().st_size / 1024:.1f} KB  ({how})")


if __name__ == "__main__":
    main()

# Third-party components bundled with opensmarters

opensmarters is free software under the **GNU General Public License v3.0 or later**. See `LICENSE`.
Its full source code is at <https://github.com/opensmarters/opensmarters>.

## libmpv (`libmpv-2.dll`): mpv, FFmpeg and libraries

`libmpv-2.dll` is an unmodified build of **mpv** with **FFmpeg** and their dependencies. It comes from
[shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake), the Windows builds linked
from <https://mpv.io/installation/>.

- This build is distributed under the **GNU GPL**. mpv's `gpl` configuration is GPL-2.0-or-later, and the build statically includes GPL components such as x264.
- FFmpeg is LGPL-2.1-or-later, and the bundled libraries carry their own permissive licences: libass, libplacebo, zlib, libcurl and others.

Corresponding source, as required by the GPL:

| Component | Version | Source |
|---|---|---|
| mpv | `v0.41.0-1205-g01e2533f3` | <https://github.com/mpv-player/mpv/tree/01e2533f31> |
| FFmpeg | `N-127617-g94cba3150` | <https://github.com/FFmpeg/FFmpeg/tree/94cba3150> |
| Build recipe and all other library versions | tag `20261018` | <https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261018> |
| Archive used | `mpv-dev-x86_64-20261018-git-01e2533f31.7z`, SHA-256 `D7F5621F42B6E161FE032DF914A10BDECE28A89DF3E57C6C3E16A5A250929B5E` | see `tools/fetch-libmpv.ps1` |

If those links ever become unavailable, the opensmarters maintainers will provide the corresponding source on
request for at least three years from the date of this release (open an issue on the repository).

## Fonts (SIL Open Font License 1.1)

- **Source Sans 3**, © Adobe. Licence: `licenses/OFL-SourceSans3.txt`.
- **Fraunces**, © The Fraunces Project Authors. Licence: `licenses/OFL-Fraunces.txt`.

The fonts are embedded unmodified.

## Icons

- **Phosphor Icons** (font build), © Phosphor Icons. MIT licence: `licenses/LICENSE-Phosphor.txt`.

## Rust crates

See `THIRD_PARTY_LICENSES.html` for every Rust crate compiled into `opensmarters.exe` and its licence text.

## Artwork

The app icon, wordmark, backgrounds, placeholders, avatars and illustrations were created for opensmarters
(generated with Codex image generation, then curated and post-processed). They are distributed under the
same licence as opensmarters. See `assets/README.md` in the source repository for provenance.

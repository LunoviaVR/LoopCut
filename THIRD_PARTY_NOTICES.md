# Third-party notices

LoopCut is free software licensed under the **GNU General Public License v3.0** (see [LICENSE](LICENSE)).
It is built on, and ships with, the third-party software listed below. Each component keeps its own license.

The installer copies this file, LoopCut's `LICENSE` and the font licenses into the `licenses/` folder of the
installation; FFmpeg's license stays next to its binaries.

## Bundled programs

### FFmpeg

LoopCut runs FFmpeg as a separate program (`ffmpeg.exe`, `ffprobe.exe` and their DLLs in the installation's
`ffmpeg/<version>/` folder). The bundled build is **BtbN FFmpeg-Builds, `win64-gpl-shared`**, FFmpeg
`n9.0.2-25-g67b60c310b`, configured with `--enable-gpl --enable-version3`, and is therefore licensed under the
**GNU General Public License v3.0 or later**. Its full license text ships next to the binaries as `LICENSE.txt`.

The build includes, among others: x264 and x265 (GPL-2.0-or-later), SVT-AV1 (BSD-3-Clause-Clear with patent grant),
libvpx (BSD-3-Clause), dav1d (BSD-2-Clause), libwebp (BSD-3-Clause), zimg (WTFPL), libass (ISC), the AMD AMF
headers (MIT), nv-codec-headers (MIT) and libvpl (MIT).

**Corresponding source.** The exact FFmpeg source (commit `67b60c310b` of https://github.com/FFmpeg/FFmpeg) and the
build scripts (https://github.com/BtbN/FFmpeg-Builds) used for the bundled binaries are published, together with
the binary archive, on LoopCut's `deps-ffmpeg-9.0.2-25-g67b60c310b` GitHub release:
https://github.com/LunoviaVR/LoopCut/releases/tag/deps-ffmpeg-9.0.2-25-g67b60c310b. This offer is valid for at
least three years from the release of any LoopCut version that ships this build.

## Rust libraries linked into LoopCut

| Component | License | Notes |
| --- | --- | --- |
| [gifski](https://github.com/ImageOptim/gifski) 1.34 | AGPL-3.0-or-later | The GIF encoder. GPLv3 section 13 permits combining a GPLv3 work with an AGPLv3 work; the gifski part keeps the AGPL terms, including its section 13 network-interaction clause, which applies only when users interact with the program over a network (LoopCut is a local desktop application). gifski's source is available from its repository and LoopCut's full source is public. |
| [libimagequant / imagequant](https://github.com/ImageOptim/libimagequant) 4.4 | GPL-3.0-or-later | Palette quantization for the GPU-dither GIF engine. |
| [Tauri](https://tauri.app) 2 and plugins | MIT OR Apache-2.0 | Application shell, updater, dialogs, logging. |
| [wgpu](https://wgpu.rs) 30 | MIT OR Apache-2.0 | GPU compositor (DirectX 12; WARP software fallback). |
| [cosmic-text](https://github.com/pop-os/cosmic-text), [swash](https://github.com/dfrg/swash) | MIT OR Apache-2.0 | Caption shaping and glyph outlines. |
| [tiny-skia](https://github.com/RazrFalcon/tiny-skia) | BSD-3-Clause | Caption rasterization. |
| [gif](https://github.com/image-rs/image-gif) | MIT OR Apache-2.0 | Indexed GIF writing for the GPU-dither engine. |
| [windows-rs](https://github.com/microsoft/windows-rs) | MIT OR Apache-2.0 | DXGI adapter enumeration, Job Objects. |

The complete Rust dependency tree (about 600 crates) is checked with `cargo deny check licenses` in CI. Licenses in
the tree: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0,
Unlicense, MPL-2.0, CC0-1.0, MIT-0, 0BSD, CDLA-Permissive-2.0, GPL-3.0-or-later (imagequant) and AGPL-3.0-or-later
(gifski). Dual-licensed crates that also offer GPL-2.0-only or LGPL-2.1-or-later (`self_cell`, `r-efi`) are used
under their permissive option. Run `cargo deny list -l crate` for the per-crate list.

## JavaScript libraries in the user interface

React and React DOM (MIT), Radix UI (MIT), lucide-react (ISC), zustand (MIT), immer (MIT), zundo (MIT),
tinykeys (MIT), i18next and react-i18next (MIT), react-colorful (MIT), clsx (MIT), tailwind-merge (MIT),
class-variance-authority (Apache-2.0), the Tauri JavaScript API and plugins (MIT OR Apache-2.0), and Tailwind CSS
(MIT, build time). Their license texts are included in the npm packages listed in `package-lock.json`.

## Fonts

| Font | License | Use |
| --- | --- | --- |
| Anton | SIL Open Font License 1.1 (`assets/fonts/OFL-Anton.txt`) | Bundled fallback for the meme caption style when Impact is not installed. |
| Inter | SIL Open Font License 1.1 (`assets/fonts/OFL-Inter.txt`) | Bundled caption font. |

Impact and other system fonts are read from the user's Windows installation and are not redistributed.

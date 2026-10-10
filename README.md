# LoopCut

**Turn any video into a great-looking GIF — with a real editor, honest file sizes and hardware acceleration.**

LoopCut is a free, open-source desktop GIF maker for Windows. Drop in a clip, trim it on a timeline, shape the loop,
crop, color-grade, add captions, and export a GIF (or a looping MP4/WebM) that fits the platform you're posting to.
Everything runs locally: no uploads, no accounts, no watermarks.

## Features

- **Your videos in.** MP4, MOV, MKV, WebM, AVI, M4V, WMV, FLV, MPEG-TS, MXF, OGV, 3GP and animated GIF/APNG, including
  ProRes, HEVC 10-bit, HDR (PQ/HLG, tone-mapped), variable-frame-rate phone video and rotated portrait clips.
- **Editor workflow.** Timeline with filmstrip, in/out handles, splits and multiple kept segments, frame-accurate
  scrubbing and a live preview that renders exactly what will be exported.
- **Loop shaping.** Speed 0.25×–4×, reverse, ping-pong, seamless-loop crossfade, frame-rate snapping to GIF-exact
  rates, loop count for GIFs.
- **Crop and transform.** Aspect presets (1:1, 4:5, 9:16, 16:9, 4:3, free), rotate and flip.
- **Color.** Exposure, contrast, highlights/shadows, temperature/tint, saturation/vibrance, lift/gamma/gain wheels,
  curves, per-hue HSL, `.cube` LUTs, sharpen and vignette, presets, before/after.
- **Captions.** Classic meme text (Impact with outline) and free text layers with fonts, outline, shadow, background box,
  fade/pop/slide/typewriter animations, timeline editing and SRT import.
- **Exports.** GIF with three engines — *Quality* (gifski), *Fast* (FFmpeg palette) and *GPU dither* — plus looping
  MP4 (H.264/HEVC) and WebM (VP9/AV1). Live size estimate, **target-size mode** and platform presets (Discord, X,
  Telegram, WhatsApp, GitHub, Slack, Reddit, GIPHY, email).
- **Hardware acceleration** for NVIDIA (NVDEC/NVENC/CUDA), AMD (AMF/D3D11VA) and Intel (Quick Sync) GPUs, toggleable per
  stage, with automatic CPU fallback.
- **Automatic updates** through signed GitHub Releases (you're notified and install with one click).

### About hardware acceleration and GIFs

No GPU can encode GIF files. In LoopCut the GPU accelerates **decoding, scaling, color, captions and effects** for
every format, and the GPU's video encoders (NVENC, AMF, Quick Sync) encode **MP4** and **WebM AV1** exports and
timeline proxies (WebM VP9 always uses the CPU).
GIF palette generation and compression always run on the CPU (the *GPU dither* engine moves palette mapping to the
GPU). On first launch LoopCut tests what your GPU can do; anything that fails or is slower than the CPU falls back
automatically, and you can change every choice in **Settings → Performance**.

## System requirements

- Windows 11 or Windows 10, 64-bit (tested on Windows 11)
- Microsoft Edge WebView2 Runtime (installed automatically if missing)
- Optional: a DirectX 12 GPU from NVIDIA, AMD or Intel for hardware acceleration

## Install

Download the latest `LoopCut_x.y.z_x64-setup.exe` from
[Releases](https://github.com/LunoviaVR/LoopCut/releases/latest) and run it. LoopCut checks for updates on startup
(configurable in Settings → Updates).

## Build from source

Prerequisites: [Node.js 24](https://nodejs.org), [Rust 1.90+](https://rustup.rs) with the MSVC toolchain, and the
WebView2 runtime.

```bash
npm ci
npm run ffmpeg:fetch     # downloads the pinned, checksum-verified FFmpeg build
npm run tauri dev        # run the app in development mode
npm run tauri build      # produce the NSIS installer
```

Tests: `cargo test --workspace`, `npm test`, `npm run typecheck`, `npm run lint`. `npm run media:gen` creates the test
media corpus used by the Rust integration tests.

Project layout and design decisions are documented in [docs/architecture.md](docs/architecture.md); the frontend/Rust
contract is in [docs/ipc.md](docs/ipc.md); releasing is described in [docs/releasing.md](docs/releasing.md).

## License

LoopCut is licensed under the [GNU General Public License v3.0](LICENSE). It bundles FFmpeg (GPL-3.0-or-later) and uses
gifski (AGPL-3.0-or-later) and other open-source components; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

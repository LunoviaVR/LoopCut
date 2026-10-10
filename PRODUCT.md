# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

(Tauri 2 desktop app rendered in WebView2 on Windows 10/11; Windows-first, portable to Linux/macOS later.
Desktop pointer + keyboard; not a phone layout.)

## Users

- **Gamers and streamers** clipping highlights from gameplay and stream recordings (often long, high
  resolution, high frame rate, sometimes HDR) into GIFs and short loops for Discord, X and Reddit.
- **Meme and social creators** turning clips from shows, videos and phone recordings (portrait, VFR) into
  captioned reaction GIFs and memes.

Both work fast, repeatedly, and judge results by how the GIF looks and whether it fits a platform's
size limit.

## Product Purpose

LoopCut turns any video into a high-quality GIF (or looping MP4/WebM) through a video-editor workflow:
import, trim on a timeline, adjust speed/direction/loop, crop, color-grade, add captions, export. Success
means a clip becomes a great-looking, correctly sized GIF in under a minute, with the preview matching the
output.

## Positioning

A real timeline editor dedicated to GIFs, running locally with hardware acceleration: exact WYSIWYG preview
(the same renderer draws preview and export), honest file-size control (live estimate and target-size
mode), gifski-grade quality, no watermark, no upload, no hidden caps.

## Operating Context

- Inputs: any FFmpeg-readable video (MP4, MKV, MOV, WebM, AVI, GIF, ProRes, HEVC 10-bit, HDR, VFR phone video).
- Outputs: GIF (hero), MP4 H.264/HEVC and WebM VP9/AV1 loops; platform presets (Discord, X, Telegram,
  WhatsApp, GitHub, Slack…).
- Hardware: NVIDIA/AMD/Intel GPUs with user-togglable acceleration and CPU fallback.
- Updates: signed GitHub Releases, notify + one-click install.

## Capabilities and Constraints

- One source clip per project in v1 (multiple kept segments); multi-clip in v1.1.
- GIF limits: integer-centisecond delays, ≤ 50 fps, 256-color palettes; the GPU cannot encode GIF itself.
- Projects are `.loopcut` files (JSON) with autosave and crash recovery.
- Terminology: *clip*, *segment*, *in/out*, *loop*, *ping-pong*, *crossfade*, *caption*, *grade*, *LUT*,
  *export preset*, *hardware acceleration*.

## Brand Commitments

Name: **LoopCut**. Identity is open (no existing logo, palette or typeface). Personality: a serious,
fast, precise pro tool with a playful edge around the loop/GIF identity.

Visual direction (user-chosen 2026-10-10, standing preference): the video-editor category standard,
executed without irony at **CapCut desktop's craft level** — dark graphite editor chrome, one accent,
preview-dominant layout. Explored alternatives (riso meme press, replay suite, sampler deck, console
studio) were declined.

## Evidence on Hand

No testimonials, user counts, press or benchmarks exist. Do not fabricate any.

## Product Principles

1. What you see is what you export — preview, size estimate and output never disagree.
2. Speed of the loop: import → trim → export in seconds; keyboard-first editing.
3. Honest output: show real sizes, real fps after GIF rounding, and what the GPU does and doesn't speed up.
4. Pro controls, sane defaults: presets get people there; depth is available, never in the way.
5. Local and private: no uploads, no accounts, no watermarks.

## Accessibility & Inclusion

Keyboard operability for all editing actions, visible focus, WCAG AA text contrast on dark surfaces,
reduced-motion support. No product-specific requirement beyond these was stated.

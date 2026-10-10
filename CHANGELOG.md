# Changelog

All notable changes to LoopCut are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Project scaffold: Tauri 2 + React 19 + TypeScript 7 app with a Rust workspace
  (`loopcut-core`, `loopcut-media`, `loopcut-render`, `loopcut-export`, `loopcut-cli`).
- Core project model (`.loopcut` files with versioned migrations), Sequencer (speed, reverse,
  ping-pong, seamless-loop crossfade, holds), crop/rotation geometry and GIF timing rules.
- Bundled, checksum-pinned FFmpeg 9.0.2 build and verification scripts.
- Sandboxed FFmpeg process runner (Job Object, no console windows, protocol whitelist) and ffprobe model.
- wgpu GPU context with WARP software fallback; gifski GIF encoder adapter; caption rasterizer prototype.
- CI and signed-release workflows with GitHub Releases auto-update support.

### Changed
- Design finish pass: the tool rail shows labels; sliders fill in a neutral colour and from the centre for values
  that go below zero; the inspector ends in a one-line export status, and the size target is set only in the Export
  tool; the timeline handles are grip tabs, distinct from the playhead, with the trim keys shown in the dock header.
- The Media tool opens with the clip itself (poster frame, name, duration and size) and keeps technical details in
  a collapsed section; the Trim tool shows the kept range.
- Settings use full-width section tabs in a fixed-height dialog with aligned rows.
- Accessibility: control edges reach 3:1 contrast on every surface, pointer targets are at least 24px, sliders
  announce their value with its unit, and selected states stay visible in Windows High Contrast.

### Fixed
- GPU dither and Fast GIF exports no longer fail on long clips or while the preview is busy.
- The timeline keeps painting while the window is in the background.
- Dragging a caption on the timeline or the preview commits once on release instead of updating the project on
  every frame.
- Rotate buttons turn the clip the right way on screen when the clip has exactly one flip, and the rotation readout
  shows the turn seen on screen.
- The Media panel shows friendly container names (MP4, MOV, MKV, WebM) instead of FFmpeg's format list.
- Exports and estimates that fail say why, with a Try again button for the estimate; a GPU dithering fallback is
  named as such instead of as a hardware encode failure.
- Opening a video shows its progress at once instead of waiting on a size estimate first.
- Kept ranges and caption bars can be selected, nudged and deleted from the keyboard; curve points can be added with
  Insert.
- Applying a color preset resets the basic and effect controls it does not set, so the result matches the preset.
- GPU library logging (wgpu, naga) is limited to warnings, so the frontend log no longer grows by tens of MB a
  minute.

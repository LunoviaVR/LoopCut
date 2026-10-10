//! Types that cross the preview boundary: the source and viewport the app passes in, and the frames and
//! metadata it gets back.

use std::path::PathBuf;

use loopcut_core::{DecodeChain, MediaInfo, Rational};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A source video as the app layer resolved it: the local file, its probe and the decode chain to use.
#[derive(Debug, Clone)]
pub struct PreviewSource {
    /// The local media file (never a URL).
    pub path: PathBuf,
    /// Probe result for the file, including the edited video stream.
    pub media: MediaInfo,
    /// Decode chain requested by the settings. The engine falls back to the CPU chain for the rest of the source
    /// when a hardware chain fails.
    pub decode: DecodeChain,
}

/// The panel the preview is drawn into: its size in CSS pixels and the device pixel ratio.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub dpr: f32,
}

/// Render quality of an output frame.
///
/// `Full` renders at the panel size (times the device pixel ratio). `Reduced` is what playback uses: the same frame
/// capped at 1280×720 pixels, so playback stays within the transport budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PreviewQuality {
    Full,
    Reduced,
}

/// Metadata of the planned sequence for the current project and viewport.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PreviewInfo {
    /// Output size from the project (display orientation, even dimensions). This is the size export produces.
    pub output_width: u32,
    pub output_height: u32,
    /// Size of full-quality frames: the output fitted into the viewport, never upscaled.
    pub render_width: u32,
    pub render_height: u32,
    /// Output frames in the plan.
    pub frames: u32,
    /// Output frame rate of the plan (after GIF clamping).
    pub fps: Rational,
    /// Quality the info describes. Always `full`; playback uses `reduced` frames of the same plan.
    pub quality: PreviewQuality,
    /// GIF delay of each output frame in centiseconds. Playback paces frames by these.
    pub delays_cs: Vec<u16>,
}

/// One rendered frame: straight-alpha RGBA8, tightly packed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, alpha 255.
    pub rgba: Vec<u8>,
}

impl RenderedFrame {
    /// Encodes the frame for the webview: `u32` little-endian width, `u32` little-endian height, then the RGBA bytes.
    pub fn to_wire(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.rgba.len());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&self.rgba);
        out
    }
}

/// A hardware decode chain that failed and was replaced by the CPU chain for the rest of the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DecodeFallback {
    /// Name of the chain that failed (`amf`, `d3d11va`, ...).
    pub from: String,
    /// Name of the chain that replaced it (`cpu`).
    pub to: String,
    /// The error that made the hardware chain fail.
    pub reason: String,
}

/// What the app shows about the preview's background work, from [`crate::PreviewEngine::status`].
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PreviewStatus {
    /// Fraction of the planned source time decoded, from 0 to 1. Ranges that failed are left out of both sides.
    pub decode_progress: f32,
    /// Planned ranges with a decode that failed. They stay failed until the source or the decode inputs change.
    pub failed_ranges: u32,
    /// Hardware chains replaced by the CPU chain that [`crate::PreviewEngine::take_fallbacks`] has not yet returned.
    pub fallbacks: Vec<DecodeFallback>,
    /// True while a playback is running.
    pub playing: bool,
    /// The most recent decode or playback error, as text.
    pub last_error: Option<String>,
}

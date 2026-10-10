//! The `.loopcut` project document.
//!
//! Rust is the canonical definition; TypeScript types are generated with ts-rs. Every field has a
//! serde default so older or partial documents load, and unknown fields are preserved on save.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::geometry::{NormRect, OutputSize};
use crate::time::{Rational, TimeRange};

/// Current project schema version. Bump together with a migration in `crate::migrate`.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Project {
    pub schema_version: u32,
    pub name: String,
    /// v1 edits exactly one clip; the list keeps multi-clip (v1.1) a non-breaking change.
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default)]
    pub output: Output,
    #[serde(default)]
    pub grade: Grade,
    #[serde(default)]
    pub captions: Vec<Caption>,
    #[serde(default)]
    pub export: ExportSettings,
    /// Fields written by newer LoopCut versions that this version does not understand.
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Map<String, Value>,
}

impl Project {
    pub fn new(name: impl Into<String>, clip: Clip) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: name.into(),
            clips: vec![clip],
            timing: Timing::default(),
            transform: Transform::default(),
            output: Output::default(),
            grade: Grade::default(),
            captions: Vec::new(),
            export: ExportSettings::default(),
            extra: Map::new(),
        }
    }

    pub fn clip(&self) -> Option<&Clip> {
        self.clips.first()
    }
}

/// A source video and the parts of it that are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Clip {
    pub id: String,
    /// Path to the media file. Stored relative to the project file when saved; absolute in memory.
    pub path: String,
    /// Container stream index of the edited video stream.
    pub stream_index: u32,
    /// Kept ranges in source time, ascending and non-overlapping. Trim = one segment; cuts split it.
    pub segments: Vec<TimeRange>,
    /// Freeze-frames inserted at a source time.
    #[serde(default)]
    pub holds: Vec<Hold>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Hold {
    /// Source time of the frozen frame.
    pub at: f64,
    /// How long the frame is held, in output seconds.
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum Direction {
    #[default]
    Forward,
    Reverse,
    /// Forward then backward ("boomerang").
    PingPong,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Timing {
    /// Playback speed multiplier, 0.25–4.
    pub speed: f64,
    pub direction: Direction,
    /// Seamless-loop crossfade: blend the last N output seconds into the start.
    pub crossfade: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            speed: 1.0,
            direction: Direction::Forward,
            crossfade: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Transform {
    /// Crop in display space (after source + user rotation and flips), normalized.
    pub crop: NormRect,
    /// Locked aspect ratio for the crop tool (`None` = free).
    pub crop_aspect: Option<Rational>,
    /// User rotation, clockwise: 0, 90, 180, 270.
    pub rotation: u16,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            crop: NormRect::FULL,
            crop_aspect: None,
            rotation: 0,
            flip_h: false,
            flip_v: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Output {
    pub fps: Rational,
    pub size: OutputSize,
    /// How many times the GIF plays: 0 = forever, 1 = once (no NETSCAPE loop block), N = N plays in total
    /// (written as N - 1 repeats).
    pub loop_count: u16,
    /// Background used when the source has alpha (RGBA, 0–255).
    pub background: [u8; 4],
}

impl Default for Output {
    fn default() -> Self {
        Self {
            fps: Rational::integer(20),
            size: OutputSize::Width { width: 480 },
            loop_count: 0,
            background: [0, 0, 0, 255],
        }
    }
}

/// Color grade applied by the compositor in preview and export (identical math in both).
///
/// Ranges (UI and validation): `exposure` -4..4 stops; `contrast`, `highlights`, `shadows`, `saturation`,
/// `vibrance`, `temperature`, `tint` -1..1 (0 = neutral); `lift` -1..1, `gamma` 0.1..4 (1 = neutral) and `gain`
/// 0..4 (1 = neutral) per RGB channel; `sharpen` 0..1; `vignette` 0..1 (darkening); `lut_strength` 0..1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Grade {
    pub enabled: bool,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub temperature: f32,
    pub tint: f32,
    pub lift: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
    pub sharpen: f32,
    pub vignette: f32,
    pub curves: Curves,
    /// Per-hue adjustments for the bands red, orange, yellow, green, aqua, blue, purple, magenta.
    pub hsl: [HslAdjust; 8],
    /// Path to a `.cube` LUT (relative to the project when saved).
    pub lut_path: Option<String>,
    pub lut_strength: f32,
    /// Name of the preset the grade started from (informational).
    pub preset: Option<String>,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            enabled: true,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            temperature: 0.0,
            tint: 0.0,
            lift: [0.0; 3],
            gamma: [1.0; 3],
            gain: [1.0; 3],
            sharpen: 0.0,
            vignette: 0.0,
            curves: Curves::default(),
            hsl: [HslAdjust::default(); 8],
            lut_path: None,
            lut_strength: 1.0,
            preset: None,
        }
    }
}

impl Grade {
    /// True when the grade leaves pixels unchanged (disabled, or every control neutral and no LUT).
    pub fn is_identity(&self) -> bool {
        if !self.enabled {
            return true;
        }
        let neutral = Grade {
            enabled: true,
            preset: self.preset.clone(),
            lut_strength: self.lut_strength,
            ..Grade::default()
        };
        *self == neutral
    }
}

/// Tone curves as control points in `[0, 1]`, ascending `x`. An empty list is the identity curve.
/// Curves are interpolated with monotone cubic (Fritsch-Carlson) splines and baked into 1D LUTs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Curves {
    pub master: Vec<CurvePoint>,
    pub red: Vec<CurvePoint>,
    pub green: Vec<CurvePoint>,
    pub blue: Vec<CurvePoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

/// Adjustment for one hue band: hue shift in degrees (-180..180), saturation and luminance (-1..1).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct HslAdjust {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
}

/// A caption (or text layer) drawn over the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Caption {
    pub id: String,
    pub text: String,
    /// Visible range in source time.
    pub range: TimeRange,
    /// Anchor: center of the text box in normalized output space (0..1).
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub style: CaptionStyle,
    #[serde(default)]
    pub animation: CaptionAnimation,
}

/// Text style. Sizes are relative so captions scale with the output size: `font_size` is a fraction of the
/// output height, stroke and shadow sizes are fractions of the font size, `max_width` a fraction of the output width.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct CaptionStyle {
    pub font_family: String,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub uppercase: bool,
    /// RGBA, straight alpha.
    pub fill: [u8; 4],
    /// Outer stroke width as a fraction of the font size (0 = none).
    pub stroke_width: f32,
    pub stroke_color: [u8; 4],
    pub shadow: Option<CaptionShadow>,
    /// Box behind the text (RGBA), `None` for no box.
    pub background: Option<[u8; 4]>,
    pub align: TextAlign,
    pub max_width: f32,
    pub line_height: f32,
}

impl Default for CaptionStyle {
    /// The classic meme caption: Impact, white fill, black outline, uppercase, centered.
    fn default() -> Self {
        Self {
            font_family: "Impact".into(),
            font_size: 0.12,
            bold: false,
            italic: false,
            uppercase: true,
            fill: [255, 255, 255, 255],
            stroke_width: 0.08,
            stroke_color: [0, 0, 0, 255],
            shadow: None,
            background: None,
            align: TextAlign::Center,
            max_width: 0.92,
            line_height: 1.1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CaptionShadow {
    /// Offsets and blur as fractions of the font size.
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

/// How a caption enters and leaves. Durations are in output seconds.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum CaptionAnimation {
    #[default]
    None,
    Fade {
        fade_in: f32,
        fade_out: f32,
    },
    /// Scales from 60% to 100% with a slight overshoot.
    Pop {
        duration: f32,
    },
    Slide {
        from: SlideEdge,
        duration: f32,
    },
    /// Reveals characters progressively at `chars_per_second`.
    Typewriter {
        chars_per_second: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SlideEdge {
    Left,
    Right,
    Top,
    #[default]
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum GifEngine {
    /// gifski: best quality (cross-frame palettes, temporal dithering).
    #[default]
    Quality,
    /// FFmpeg palettegen/paletteuse.
    Fast,
    /// imagequant palette + GPU ordered dithering.
    GpuDither,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExportFormat {
    #[default]
    Gif,
    Mp4H264,
    Mp4Hevc,
    WebmVp9,
    WebmAv1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct ExportSettings {
    pub format: ExportFormat,
    pub gif_engine: GifEngine,
    /// 1–100.
    pub quality: u8,
    /// gifski lossy quality (1–100, 100 = lossless LZW).
    pub lossy_quality: u8,
    /// gifski motion quality (1–100).
    pub motion_quality: u8,
    /// Palette size for the Fast / GPU engines (2–256).
    pub colors: u16,
    pub dither: Dither,
    /// Optional size budget in bytes (target-size mode).
    pub target_bytes: Option<u64>,
    /// Platform preset id (see `crate::presets`) the settings were taken from.
    pub preset: Option<String>,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            format: ExportFormat::Gif,
            gif_engine: GifEngine::Quality,
            quality: 90,
            lossy_quality: 100,
            motion_quality: 100,
            colors: 256,
            dither: Dither::Sierra2_4a,
            target_bytes: None,
            preset: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum Dither {
    None,
    Bayer,
    FloydSteinberg,
    #[default]
    #[serde(rename = "sierra2_4a")]
    Sierra2_4a,
    BlueNoise,
}

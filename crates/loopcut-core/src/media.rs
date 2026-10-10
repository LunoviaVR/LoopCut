//! Media description shared by every crate (populated from ffprobe by `loopcut-media`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::time::Rational;

/// YUV→RGB matrix coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
    /// Source is already RGB (PNG/GIF/WebP input, ProRes 4444 decoded to RGBA, …).
    Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ColorRange {
    /// "tv" / MPEG range (16–235).
    Limited,
    /// "pc" / JPEG range (0–255).
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TransferFunction {
    /// BT.709 / sRGB-like SDR gamma.
    Sdr,
    /// SMPTE ST 2084 (PQ) HDR.
    Pq,
    /// ARIB STD-B67 (HLG) HDR.
    Hlg,
}

/// Color description of a decoded video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ColorInfo {
    pub matrix: YuvMatrix,
    pub range: ColorRange,
    pub transfer: TransferFunction,
    /// BT.2020 primaries (wide gamut) that need gamut mapping to BT.709.
    pub wide_gamut: bool,
}

impl ColorInfo {
    pub const SDR_BT709: Self = Self {
        matrix: YuvMatrix::Bt709,
        range: ColorRange::Limited,
        transfer: TransferFunction::Sdr,
        wide_gamut: false,
    };

    pub fn is_hdr(&self) -> bool {
        !matches!(self.transfer, TransferFunction::Sdr)
    }

    /// Derives color info from ffprobe tags, falling back to resolution-based defaults when the
    /// stream is untagged (SD → BT.601, otherwise BT.709).
    pub fn from_ffprobe_tags(
        color_space: Option<&str>,
        color_transfer: Option<&str>,
        color_primaries: Option<&str>,
        color_range: Option<&str>,
        height: u32,
        is_rgb: bool,
    ) -> Self {
        let matrix = if is_rgb {
            YuvMatrix::Rgb
        } else {
            match color_space.unwrap_or("") {
                "bt709" => YuvMatrix::Bt709,
                "bt2020nc" | "bt2020c" | "bt2020_ncl" | "bt2020_cl" => YuvMatrix::Bt2020,
                "bt470bg" | "smpte170m" | "bt601" | "fcc" => YuvMatrix::Bt601,
                _ if height <= 576 => YuvMatrix::Bt601,
                _ => YuvMatrix::Bt709,
            }
        };
        let transfer = match color_transfer.unwrap_or("") {
            "smpte2084" => TransferFunction::Pq,
            "arib-std-b67" => TransferFunction::Hlg,
            _ => TransferFunction::Sdr,
        };
        let range = match color_range {
            Some("pc") | Some("jpeg") => ColorRange::Full,
            _ if is_rgb => ColorRange::Full,
            _ => ColorRange::Limited,
        };
        let wide_gamut = matches!(color_primaries, Some("bt2020"));
        Self {
            matrix,
            range,
            transfer,
            wide_gamut,
        }
    }
}

/// Memory layout of a decoded frame exchanged between FFmpeg, the frame store and the compositor.
///
/// All layouts are tightly packed (no row padding):
/// * `Nv12`: 8-bit Y plane (`w*h` bytes) followed by interleaved Cb/Cr at half resolution (`w*h/2`).
/// * `P010`: 16-bit little-endian samples with the 10 significant bits in the high bits; Y plane
///   (`w*h*2` bytes) followed by interleaved Cb/Cr at half resolution (`w*h` bytes).
/// * `Rgba8`: 8-bit RGBA, straight (non-premultiplied) alpha, sRGB-encoded (`w*h*4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PixelLayout {
    Nv12,
    P010,
    Rgba8,
}

impl PixelLayout {
    /// Bytes for one frame of `width`×`height` (both must be even for the YUV layouts).
    pub fn frame_bytes(self, width: u32, height: u32) -> usize {
        let px = width as usize * height as usize;
        match self {
            Self::Nv12 => px * 3 / 2,
            Self::P010 => px * 3,
            Self::Rgba8 => px * 4,
        }
    }

    /// FFmpeg `-pix_fmt` name.
    pub fn ffmpeg_pix_fmt(self) -> &'static str {
        match self {
            Self::Nv12 => "nv12",
            Self::P010 => "p010le",
            Self::Rgba8 => "rgba",
        }
    }
}

/// Description of the selected video stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VideoStreamInfo {
    /// Absolute stream index inside the container (ffmpeg `-map 0:<index>`).
    pub index: u32,
    pub codec: String,
    pub profile: Option<String>,
    /// Coded (stored) dimensions, before applying `rotation`.
    pub width: u32,
    pub height: u32,
    /// Sample aspect ratio (1/1 for square pixels).
    pub sample_aspect: Rational,
    pub pix_fmt: String,
    pub bit_depth: u8,
    pub has_alpha: bool,
    /// Display rotation in degrees clockwise (0, 90, 180, 270), from the display matrix.
    pub rotation: u16,
    /// Nominal frame rate (`r_frame_rate`).
    pub frame_rate: Rational,
    /// Average frame rate; differs from `frame_rate` for variable-frame-rate sources.
    pub avg_frame_rate: Option<Rational>,
    pub variable_frame_rate: bool,
    pub color: ColorInfo,
    /// Stream start time in seconds (source time 0 corresponds to this PTS).
    pub start_time: f64,
    pub duration: f64,
    pub frame_count: Option<u64>,
    pub interlaced: bool,
    /// True if WebView2's `<video>` can likely play this stream directly (H.264/VP8/VP9/AV1 8-bit SDR in MP4/WebM).
    pub web_playable: bool,
}

impl VideoStreamInfo {
    /// Dimensions as displayed (after rotation and sample aspect), used for crop and output sizing.
    pub fn display_size(&self) -> (u32, u32) {
        let sar = if self.sample_aspect.is_valid() {
            self.sample_aspect.as_f64()
        } else {
            1.0
        };
        let w = (f64::from(self.width) * sar).round().max(1.0) as u32;
        let h = self.height;
        if self.rotation % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Everything LoopCut needs to know about an imported file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MediaInfo {
    pub container: String,
    pub duration: f64,
    pub size_bytes: u64,
    /// The stream LoopCut edits (first video stream that is not an attached picture, unless chosen).
    pub video: VideoStreamInfo,
    /// Other usable video streams, if the user wants to pick a different one.
    pub alternate_video_streams: Vec<u32>,
    pub has_audio: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_defaults_follow_resolution() {
        let sd = ColorInfo::from_ffprobe_tags(None, None, None, None, 480, false);
        assert_eq!(sd.matrix, YuvMatrix::Bt601);
        let hd = ColorInfo::from_ffprobe_tags(None, None, None, None, 1080, false);
        assert_eq!(hd, ColorInfo::SDR_BT709);
    }

    #[test]
    fn hdr_tags_are_detected() {
        let pq = ColorInfo::from_ffprobe_tags(
            Some("bt2020nc"),
            Some("smpte2084"),
            Some("bt2020"),
            Some("tv"),
            2160,
            false,
        );
        assert!(pq.is_hdr());
        assert_eq!(pq.matrix, YuvMatrix::Bt2020);
        assert!(pq.wide_gamut);
        let hlg = ColorInfo::from_ffprobe_tags(
            Some("bt2020nc"),
            Some("arib-std-b67"),
            Some("bt2020"),
            None,
            2160,
            false,
        );
        assert_eq!(hlg.transfer, TransferFunction::Hlg);
    }
}

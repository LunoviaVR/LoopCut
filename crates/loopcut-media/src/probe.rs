//! ffprobe-backed media probing and the pure conversion from ffprobe JSON to [`MediaInfo`].
//!
//! Stream selection and rotation rules (plan decision M1):
//!
//! * The edited video stream is the first video stream whose `disposition.attached_pic` is not 1 (cover art is
//!   skipped). Other non-cover video streams are listed in `alternate_video_streams`.
//! * `rotation` is the clockwise correction needed to display the stored frame upright, normalized to 0/90/180/270.
//!   ffprobe's `side_data_list[].rotation` is the counter-clockwise angle that autorotate applies, so the value is
//!   negated. The sign is verified in CI by `tests/probe_media.rs::rotation_matches_what_autorotate_displays`, which
//!   compares the reported value with the quadrant FFmpeg's autorotate actually shows.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use loopcut_core::{ColorInfo, MediaInfo, Rational, VideoStreamInfo};
use serde::Deserialize;

use crate::error::MediaError;
use crate::paths::FfmpegPaths;
use crate::runner::{FfmpegCommand, InputOptions, run_capture};

/// Upper bound for one ffprobe call. Header parsing normally takes well under a second.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Containers that WebView2 can usually play when the streams inside are also playable.
const WEB_CONTAINERS: [&str; 8] = ["mov", "mp4", "m4a", "3gp", "3g2", "mj2", "matroska", "webm"];

/// Codecs that WebView2 can usually decode directly.
const WEB_CODECS: [&str; 4] = ["h264", "vp8", "vp9", "av1"];

/// Single-image codecs. A stream of one of these is cover art or a still, never the clip, while another video stream
/// exists. GIF is deliberately absent: an animated GIF is a real clip.
const STILL_IMAGE_CODECS: [&str; 5] = ["mjpeg", "png", "bmp", "tiff", "webp"];

/// Probes `file` with ffprobe and returns the editing-oriented description of its streams.
///
/// # Errors
///
/// [`MediaError::Failed`] if ffprobe rejects the file, [`MediaError::NoVideoStream`] for audio-only files, and
/// [`MediaError::ProbeParse`] for output that does not describe a usable video stream.
pub fn probe(paths: &FfmpegPaths, file: &Path) -> Result<MediaInfo, MediaError> {
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(file, &InputOptions::default())?
        .output_args(["-print_format", "json", "-show_format", "-show_streams"])?;
    let size_bytes = std::fs::metadata(file)?.len();
    let stdout = run_capture(command, PROBE_TIMEOUT)?;
    let json = std::str::from_utf8(&stdout)
        .map_err(|err| MediaError::ProbeParse(format!("output is not UTF-8: {err}")))?;
    parse_ffprobe_json(json, size_bytes)
}

/// The parts of `ffprobe -show_format -show_streams -print_format json` that LoopCut reads.
#[derive(Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

/// One entry of `streams`. ffprobe encodes most numbers as strings; only dimensions and indices are numeric.
#[derive(Debug, Deserialize)]
struct RawStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<String>,
    sample_aspect_ratio: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    nb_frames: Option<String>,
    start_time: Option<String>,
    duration: Option<String>,
    field_order: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    color_range: Option<String>,
    #[serde(default)]
    disposition: RawDisposition,
    #[serde(default)]
    side_data_list: Vec<RawSideData>,
    /// Container tags. Matroska writes the per-stream duration here (`DURATION`); WebM marks VP9 alpha (`alpha_mode`).
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDisposition {
    #[serde(default)]
    attached_pic: i64,
}

/// Side data entry. Only the display matrix carries `rotation`; other kinds are ignored.
#[derive(Debug, Deserialize)]
struct RawSideData {
    #[serde(default)]
    rotation: Option<serde_json::Value>,
}

impl RawStream {
    fn is_cover_art(&self) -> bool {
        self.disposition.attached_pic == 1
    }

    fn is_video(&self) -> bool {
        self.codec_type.as_deref() == Some("video")
    }

    /// A container tag by name, matched case-insensitively (Matroska writes `DURATION`, WebM may write `duration`).
    fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    }

    /// Frame count from the stream header, when the demuxer knows it (`N/A` and zero count as unknown).
    fn frame_count(&self) -> Option<u64> {
        self.nb_frames
            .as_deref()
            .and_then(|count| count.trim().parse::<u64>().ok())
            .filter(|count| *count > 0)
    }

    /// Stream duration in seconds, from the header or, for Matroska, from the `DURATION` tag.
    fn duration_seconds(&self) -> Option<f64> {
        parse_seconds(self.duration.as_deref())
            .or_else(|| parse_clock_seconds(self.tag("DURATION")?))
            .filter(|seconds| *seconds > 0.0)
    }

    /// True when the stream is a single still picture rather than a clip: a still codec with one frame, or with no
    /// frame count but a duration no longer than about one frame. Streams that say nothing about their length are not
    /// still, so an ordinary clip is never mistaken for one.
    fn is_still_image(&self) -> bool {
        let Some(codec) = self.codec_name.as_deref() else {
            return false;
        };
        if !STILL_IMAGE_CODECS.contains(&codec) {
            return false;
        }
        if let Some(count) = self.frame_count() {
            return count <= 1;
        }
        let frame_seconds = self
            .r_frame_rate
            .as_deref()
            .and_then(Rational::parse)
            .filter(|rate| rate.is_valid())
            .map_or(1.0 / 30.0, Rational::frame_duration);
        self.duration_seconds()
            .is_some_and(|seconds| seconds <= frame_seconds * 1.5)
    }

    /// VP9 alpha in WebM: FFmpeg's native VP9 decoder drops the alpha plane, and the muxer marks such streams with
    /// `alpha_mode=1`. Decoding them needs the libvpx-vp9 decoder.
    fn has_vp9_alpha_tag(&self) -> bool {
        self.codec_name.as_deref() == Some("vp9") && self.tag("alpha_mode") == Some("1")
    }

    /// Display rotation from the first side data entry that has one, in ffprobe's counter-clockwise convention.
    fn display_rotation(&self) -> Option<f64> {
        self.side_data_list
            .iter()
            .filter_map(|side| side.rotation.as_ref().and_then(serde_json::Value::as_f64))
            .find(|angle| angle.is_finite())
    }
}

/// Converts ffprobe JSON (from `-print_format json -show_format -show_streams`) into [`MediaInfo`].
///
/// `size_bytes` is the on-disk size, which ffprobe does not report reliably for every container. The function is
/// pure, so every rule here is covered by fixture-based unit tests.
///
/// # Errors
///
/// [`MediaError::Json`] for malformed JSON, [`MediaError::ProbeParse`] for a missing format section or a video
/// stream without dimensions, and [`MediaError::NoVideoStream`] when no non-cover video stream exists.
pub fn parse_ffprobe_json(json: &str, size_bytes: u64) -> Result<MediaInfo, MediaError> {
    let output: ProbeOutput = serde_json::from_str(json)?;
    let format = output
        .format
        .ok_or_else(|| MediaError::ProbeParse("missing format section".to_owned()))?;

    let has_audio = output
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("audio"));
    let usable: Vec<&RawStream> = output
        .streams
        .iter()
        .filter(|stream| stream.is_video() && !stream.is_cover_art())
        .collect();
    // The clip is the first usable stream that is not a still picture. A file whose only video is a still (a PNG
    // input) still edits that stream, so the fallback is the first usable stream.
    let chosen_at = usable
        .iter()
        .position(|stream| !stream.is_still_image())
        .unwrap_or(0);
    let chosen = usable
        .get(chosen_at)
        .copied()
        .ok_or(MediaError::NoVideoStream)?;
    // Cover art and stills are not offered as alternates when a clip was chosen, matching the cover-art rule above.
    let chosen_is_still = chosen.is_still_image();
    let alternate_video_streams = usable
        .iter()
        .enumerate()
        .filter(|(at, stream)| *at != chosen_at && (chosen_is_still || !stream.is_still_image()))
        .map(|(_, stream)| stream.index)
        .collect();

    let format_duration = parse_seconds(format.duration.as_deref());
    let container = format.format_name.unwrap_or_else(|| "unknown".to_owned());
    let video = video_stream_info(chosen, format_duration.unwrap_or(0.0), &container)?;

    Ok(MediaInfo {
        container,
        duration: format_duration.unwrap_or(video.duration),
        size_bytes,
        video,
        alternate_video_streams,
        has_audio,
    })
}

/// Builds the description of one usable video stream.
fn video_stream_info(
    stream: &RawStream,
    fallback_duration: f64,
    container: &str,
) -> Result<VideoStreamInfo, MediaError> {
    let (width, height) = match (stream.width, stream.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => {
            return Err(MediaError::ProbeParse(format!(
                "video stream {} has no dimensions",
                stream.index
            )));
        }
    };

    let pix_fmt = stream.pix_fmt.clone().unwrap_or_default();
    let codec = stream
        .codec_name
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let bit_depth = pix_fmt_bit_depth(&pix_fmt)
        .or_else(|| {
            stream
                .bits_per_raw_sample
                .as_deref()
                .and_then(|bits| bits.parse::<u8>().ok())
                .filter(|bits| *bits > 0)
        })
        .unwrap_or(8);
    let has_alpha = pix_fmt_has_alpha(&pix_fmt) || stream.has_vp9_alpha_tag();
    let is_rgb = pix_fmt_is_rgb(&pix_fmt);
    let color = ColorInfo::from_ffprobe_tags(
        stream.color_space.as_deref(),
        stream.color_transfer.as_deref(),
        stream.color_primaries.as_deref(),
        stream.color_range.as_deref(),
        height,
        is_rgb,
    );

    let frame_rate_nominal = stream.r_frame_rate.as_deref().and_then(Rational::parse);
    let frame_rate_average = stream.avg_frame_rate.as_deref().and_then(Rational::parse);
    // Still-image video streams sometimes report 0/0; 30 keeps timing finite until a caller overrides it.
    let frame_rate = frame_rate_nominal
        .or(frame_rate_average)
        .unwrap_or(Rational::integer(30));
    let variable_frame_rate = match (frame_rate_nominal, frame_rate_average) {
        (Some(nominal), Some(average)) => {
            let nominal = nominal.as_f64();
            (nominal - average.as_f64()).abs() > 0.01 * nominal
        }
        _ => false,
    };

    let interlaced = matches!(
        stream.field_order.as_deref(),
        Some("tt" | "bb" | "tb" | "bt")
    );
    let web_playable = is_web_playable(
        &codec,
        bit_depth,
        &pix_fmt,
        color.is_hdr(),
        container,
        interlaced,
    );

    Ok(VideoStreamInfo {
        index: stream.index,
        codec,
        profile: stream.profile.clone(),
        width,
        height,
        sample_aspect: stream
            .sample_aspect_ratio
            .as_deref()
            .and_then(|sar| Rational::parse(&sar.replace(':', "/")))
            .unwrap_or(Rational::integer(1)),
        pix_fmt,
        bit_depth,
        has_alpha,
        rotation: stream.display_rotation().map_or(0, clockwise_correction),
        frame_rate,
        avg_frame_rate: frame_rate_average,
        variable_frame_rate,
        color,
        start_time: parse_seconds(stream.start_time.as_deref()).unwrap_or(0.0),
        duration: parse_seconds(stream.duration.as_deref())
            .filter(|seconds| *seconds > 0.0)
            .unwrap_or(fallback_duration),
        frame_count: stream
            .nb_frames
            .as_deref()
            .and_then(|count| count.parse::<u64>().ok())
            .filter(|count| *count > 0),
        interlaced,
        web_playable,
    })
}

/// Parses a seconds value that ffprobe prints as a decimal string. `N/A` and non-finite values yield `None`.
fn parse_seconds(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite())
}

/// Parses a Matroska `DURATION` tag such as `00:00:01.000000000` (hours:minutes:seconds, fraction optional).
fn parse_clock_seconds(text: &str) -> Option<f64> {
    let mut parts = text.trim().splitn(3, ':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    let total = hours * 3600.0 + minutes * 60.0 + seconds;
    (total.is_finite() && total >= 0.0).then_some(total)
}

/// Converts ffprobe's counter-clockwise display rotation into the clockwise correction for upright display.
///
/// Autorotate turns the stored frame counter-clockwise by `R` (ffprobe's value). Showing it upright therefore needs a
/// clockwise turn of `-R`. The result is normalized to 0, 90, 180 or 270 by rounding to the nearest quarter turn.
pub(crate) fn clockwise_correction(display_rotation: f64) -> u16 {
    let clockwise = (-display_rotation).rem_euclid(360.0);
    let quarter_turns = (clockwise / 90.0).round() as i64 % 4;
    u16::try_from(quarter_turns * 90).unwrap_or(0)
}

/// Bit depth per component implied by an FFmpeg pixel format name, when the name encodes it.
///
/// Names that do not encode their depth (such as `yuv420p`) return `None`, and the caller falls back to ffprobe's
/// `bits_per_raw_sample`.
pub(crate) fn pix_fmt_bit_depth(pix_fmt: &str) -> Option<u8> {
    let base = pix_fmt
        .strip_suffix("le")
        .or_else(|| pix_fmt.strip_suffix("be"))
        .unwrap_or(pix_fmt);
    // Names where the trailing digits are not a depth (nv12 is 8-bit, rgb48 is 16-bit per channel, x2rgb10 is 10-bit
    // packed RGB, and float formats end in their bit width).
    match base {
        "rgb48" | "bgr48" | "rgba64" | "bgra64" | "ayuv64" | "xv48" => return Some(16),
        "p010" | "p210" | "p410" | "nv20" | "x2rgb10" | "x2bgr10" | "xv30" | "y210" | "v210" => {
            return Some(10);
        }
        "p012" | "p212" | "p412" => return Some(12),
        "p016" | "p216" | "p416" => return Some(16),
        "nv12" | "nv21" | "nv16" | "nv24" | "nv42" | "pal8" | "rgb8" | "bgr8" | "rgb24"
        | "bgr24" | "rgba" | "bgra" | "argb" | "abgr" | "gray8" | "ya8" | "yuyv422" | "uyvy422" => {
            return Some(8);
        }
        _ => {}
    }
    if base.ends_with("f32") {
        return Some(32);
    }
    if base.ends_with("f16") {
        return Some(16);
    }
    let stem = base.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &base[stem.len()..];
    let planar_family = stem.ends_with('p') || stem.ends_with("gray") || stem.ends_with("ya");
    if digits.is_empty() || !planar_family {
        return None;
    }
    digits
        .parse::<u8>()
        .ok()
        .filter(|depth| (1..=16).contains(depth))
}

/// True for pixel formats that carry an alpha plane (or, for `pal8`, may carry alpha in the palette).
pub(crate) fn pix_fmt_has_alpha(pix_fmt: &str) -> bool {
    [
        "yuva", "rgba", "bgra", "argb", "abgr", "gbrap", "ya", "ayuv", "pal8",
    ]
    .iter()
    .any(|prefix| pix_fmt.starts_with(prefix))
}

/// True for RGB-family pixel formats, which take the identity YUV matrix in [`ColorInfo`].
pub(crate) fn pix_fmt_is_rgb(pix_fmt: &str) -> bool {
    [
        "rgb", "bgr", "gbr", "argb", "abgr", "rgba", "bgra", "x2rgb", "x2bgr", "pal8",
    ]
    .iter()
    .any(|prefix| pix_fmt.starts_with(prefix))
}

/// The web-playability rule from the plan: a codec WebView2 decodes, 8-bit 4:2:0 SDR, a web-friendly container, and
/// progressive scan.
fn is_web_playable(
    codec: &str,
    bit_depth: u8,
    pix_fmt: &str,
    is_hdr: bool,
    container: &str,
    interlaced: bool,
) -> bool {
    let container_ok = container
        .split(',')
        .any(|token| WEB_CONTAINERS.contains(&token.trim()));
    WEB_CODECS.contains(&codec)
        && bit_depth == 8
        && matches!(pix_fmt, "yuv420p" | "yuvj420p")
        && !is_hdr
        && container_ok
        && !interlaced
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_320X240: &str = include_str!("../tests/fixtures/ffprobe/h264_320x240.json");
    const H264_ROTATED_90: &str =
        include_str!("../tests/fixtures/ffprobe/h264_display_rotation_90.json");
    const H264_ROTATED_MINUS_90: &str =
        include_str!("../tests/fixtures/ffprobe/h264_display_rotation_minus90.json");
    const HEVC_10BIT_PQ: &str = include_str!("../tests/fixtures/ffprobe/hevc10_pq_matroska.json");
    const HLG_MP4: &str = include_str!("../tests/fixtures/ffprobe/hevc10_hlg_mp4.json");
    const PRORES_ALPHA: &str = include_str!("../tests/fixtures/ffprobe/prores4444_alpha_mov.json");
    const PNG_RGBA: &str = include_str!("../tests/fixtures/ffprobe/png_rgba_mov.json");
    const COVER_ART_MP4: &str = include_str!("../tests/fixtures/ffprobe/h264_with_cover_mp4.json");
    const AUDIO_ONLY: &str = include_str!("../tests/fixtures/ffprobe/audio_only_m4a.json");
    const AUDIO_VIDEO: &str = include_str!("../tests/fixtures/ffprobe/h264_aac_mp4.json");
    const VFR_SYNTHETIC: &str =
        include_str!("../tests/fixtures/ffprobe/synthetic_vfr_interlaced.json");
    const COVER_FIRST_SYNTHETIC: &str =
        include_str!("../tests/fixtures/ffprobe/synthetic_cover_first.json");

    fn parse(json: &str) -> MediaInfo {
        parse_ffprobe_json(json, 12_345).expect("fixture parses")
    }

    #[test]
    fn plain_h264_is_web_playable() {
        let info = parse(H264_320X240);
        let video = &info.video;
        assert_eq!(info.size_bytes, 12_345);
        assert_eq!((video.width, video.height), (320, 240));
        assert_eq!(video.codec, "h264");
        assert_eq!(video.pix_fmt, "yuv420p");
        assert_eq!(video.bit_depth, 8);
        assert!(!video.has_alpha);
        assert_eq!(video.rotation, 0);
        assert_eq!(video.frame_rate, Rational::integer(10));
        assert!(!video.variable_frame_rate);
        assert!(!video.interlaced);
        assert_eq!(video.frame_count, Some(10));
        // The fixture is untagged SD (240 lines), which the core rule maps to BT.601.
        assert_eq!(video.color.matrix, loopcut_core::YuvMatrix::Bt601);
        assert!(!video.color.is_hdr());
        assert!(video.web_playable);
        assert!(info.container.contains("mp4"));
        assert!(!info.has_audio);
        assert!(info.alternate_video_streams.is_empty());
        assert!((info.duration - 1.0).abs() < 1e-6);
    }

    #[test]
    fn display_rotation_plus_90_is_clockwise_270_correction() {
        // ffprobe reports rotation 90 (counter-clockwise), so autorotate shows the stored frame turned CCW 90 and the
        // clockwise correction that gives an upright picture is 270.
        let info = parse(H264_ROTATED_90);
        assert_eq!(info.video.rotation, 270);
        assert_eq!(info.video.display_size(), (240, 320));
    }

    #[test]
    fn display_rotation_minus_90_is_clockwise_90_correction() {
        let info = parse(H264_ROTATED_MINUS_90);
        assert_eq!(info.video.rotation, 90);
        assert_eq!(info.video.display_size(), (240, 320));
    }

    #[test]
    fn clockwise_correction_normalizes_quarter_turns() {
        assert_eq!(clockwise_correction(0.0), 0);
        assert_eq!(clockwise_correction(90.0), 270);
        assert_eq!(clockwise_correction(-90.0), 90);
        assert_eq!(clockwise_correction(180.0), 180);
        assert_eq!(clockwise_correction(-180.0), 180);
        assert_eq!(clockwise_correction(270.0), 90);
        assert_eq!(clockwise_correction(-270.0), 270);
        assert_eq!(
            clockwise_correction(-89.7),
            90,
            "near-quarter matrices round to the quarter turn"
        );
    }

    #[test]
    fn hdr10_matroska_is_detected_and_not_web_playable() {
        let info = parse(HEVC_10BIT_PQ);
        let video = &info.video;
        assert_eq!(video.codec, "hevc");
        assert_eq!(video.pix_fmt, "yuv420p10le");
        assert_eq!(video.bit_depth, 10);
        assert!(video.color.is_hdr());
        assert_eq!(video.color.transfer, loopcut_core::TransferFunction::Pq);
        assert_eq!(video.color.matrix, loopcut_core::YuvMatrix::Bt2020);
        assert!(video.color.wide_gamut);
        assert!(!video.web_playable);
        assert!(info.container.contains("matroska"));
    }

    #[test]
    fn hlg_mp4_uses_hlg_transfer() {
        let info = parse(HLG_MP4);
        assert_eq!(
            info.video.color.transfer,
            loopcut_core::TransferFunction::Hlg
        );
        assert!(!info.video.web_playable);
    }

    #[test]
    fn prores_4444_reports_alpha_and_twelve_bits() {
        // Encoding yuva444p10le with prores_ks 4444 yields yuva444p12le on decode; the table follows the decoder.
        let info = parse(PRORES_ALPHA);
        let video = &info.video;
        assert_eq!(video.codec, "prores");
        assert_eq!(video.pix_fmt, "yuva444p12le");
        assert!(video.has_alpha);
        assert_eq!(video.bit_depth, 12);
        assert!(!video.web_playable);
    }

    #[test]
    fn rgba_png_in_mov_is_rgb_with_alpha() {
        let info = parse(PNG_RGBA);
        assert!(info.video.has_alpha);
        assert_eq!(info.video.color.matrix, loopcut_core::YuvMatrix::Rgb);
        assert!(!info.video.web_playable);
    }

    #[test]
    fn cover_art_is_skipped_for_the_edited_stream() {
        let info = parse(COVER_ART_MP4);
        assert_eq!(info.video.index, 0);
        assert_eq!(info.video.codec, "h264");
        assert!(
            info.alternate_video_streams.is_empty(),
            "cover art is not a usable alternate"
        );
    }

    #[test]
    fn cover_art_listed_first_is_still_skipped() {
        let info = parse(COVER_FIRST_SYNTHETIC);
        assert_eq!(info.video.index, 1);
        assert_eq!(info.video.codec, "h264");
        assert_eq!(info.alternate_video_streams, Vec::<u32>::new());
    }

    #[test]
    fn audio_and_video_are_both_reported() {
        let info = parse(AUDIO_VIDEO);
        assert!(info.has_audio);
        assert_eq!(info.video.codec, "h264");
    }

    #[test]
    fn audio_only_file_has_no_video_stream() {
        let err = parse_ffprobe_json(AUDIO_ONLY, 1).expect_err("no video stream");
        assert!(matches!(err, MediaError::NoVideoStream), "{err:?}");
    }

    #[test]
    fn variable_frame_rate_and_interlacing_are_detected() {
        let info = parse(VFR_SYNTHETIC);
        assert!(info.video.variable_frame_rate);
        assert!(info.video.interlaced);
        assert!(
            !info.video.web_playable,
            "interlaced streams are never web playable"
        );
        assert_eq!(info.video.frame_rate, Rational::new(30000, 1001));
        assert_eq!(info.video.avg_frame_rate, Some(Rational::integer(25)));
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        let err = parse_ffprobe_json("{not json", 0).expect_err("bad json");
        assert!(matches!(err, MediaError::Json(_)), "{err:?}");
        let err = parse_ffprobe_json("{}", 0).expect_err("no format");
        assert!(matches!(err, MediaError::ProbeParse(_)), "{err:?}");
    }

    #[test]
    fn pixel_format_depths_follow_the_table() {
        assert_eq!(pix_fmt_bit_depth("yuv420p10le"), Some(10));
        assert_eq!(pix_fmt_bit_depth("p010le"), Some(10));
        assert_eq!(pix_fmt_bit_depth("p016le"), Some(16));
        assert_eq!(pix_fmt_bit_depth("yuva444p12le"), Some(12));
        assert_eq!(pix_fmt_bit_depth("gbrap16le"), Some(16));
        assert_eq!(pix_fmt_bit_depth("gray10le"), Some(10));
        assert_eq!(pix_fmt_bit_depth("rgba64le"), Some(16));
        assert_eq!(pix_fmt_bit_depth("rgb24"), Some(8));
        assert_eq!(pix_fmt_bit_depth("nv12"), Some(8));
        assert_eq!(
            pix_fmt_bit_depth("yuv420p"),
            None,
            "falls back to bits_per_raw_sample"
        );
    }

    #[test]
    fn alpha_and_rgb_families_are_recognised() {
        for alpha in [
            "yuva420p",
            "yuva444p10le",
            "rgba",
            "bgra",
            "argb",
            "abgr",
            "gbrap",
            "ya8",
            "pal8",
        ] {
            assert!(pix_fmt_has_alpha(alpha), "{alpha}");
        }
        for opaque in ["yuv420p", "nv12", "gbrp", "rgb24", "gray"] {
            assert!(!pix_fmt_has_alpha(opaque), "{opaque}");
        }
        for rgb in ["rgb24", "bgr24", "gbrp", "gbrap", "pal8", "rgba"] {
            assert!(pix_fmt_is_rgb(rgb), "{rgb}");
        }
        assert!(!pix_fmt_is_rgb("yuv420p"));
    }

    #[test]
    fn web_playability_requires_every_condition() {
        assert!(is_web_playable(
            "h264",
            8,
            "yuv420p",
            false,
            "mov,mp4,m4a,3gp,3g2,mj2",
            false
        ));
        assert!(is_web_playable(
            "vp9",
            8,
            "yuvj420p",
            false,
            "matroska,webm",
            false
        ));
        assert!(!is_web_playable("hevc", 8, "yuv420p", false, "mp4", false));
        assert!(!is_web_playable(
            "h264",
            10,
            "yuv420p10le",
            false,
            "mp4",
            false
        ));
        assert!(!is_web_playable("h264", 8, "yuv422p", false, "mp4", false));
        assert!(!is_web_playable("h264", 8, "yuv420p", true, "mp4", false));
        assert!(!is_web_playable("h264", 8, "yuv420p", false, "avi", false));
        assert!(!is_web_playable("h264", 8, "yuv420p", false, "mp4", true));
    }

    const MKV_COVER_FIRST: &str =
        include_str!("../tests/fixtures/ffprobe/synthetic_mkv_cover_mjpeg_first.json");
    const VP9_ALPHA_WEBM: &str =
        include_str!("../tests/fixtures/ffprobe/synthetic_vp9_alpha_webm.json");

    #[test]
    fn matroska_cover_art_listed_first_is_not_the_clip() {
        // An MJPEG stream with no frame count and a one-frame DURATION tag (what ffmpeg writes for cover art in MKV)
        // is still, so the H.264 clip after it is chosen.
        let info = parse(MKV_COVER_FIRST);
        assert_eq!(info.video.codec, "h264");
        assert_eq!(info.video.index, 1);
        assert!(info.alternate_video_streams.is_empty());
    }

    #[test]
    fn vp9_alpha_tag_marks_the_stream_as_alpha() {
        let info = parse(VP9_ALPHA_WEBM);
        assert_eq!(info.video.codec, "vp9");
        assert_eq!(
            info.video.pix_fmt, "yuv420p",
            "the native decoder hides the alpha plane"
        );
        assert!(info.video.has_alpha);
        assert_eq!(info.video.bit_depth, 8);
    }

    fn json_with_streams(streams: &str) -> String {
        format!(
            r#"{{"streams":[{streams}],"format":{{"format_name":"matroska,webm","duration":"1.000000"}}}}"#
        )
    }

    #[test]
    fn an_animated_mjpeg_clip_is_not_mistaken_for_a_still() {
        let json = json_with_streams(
            r#"{"index":0,"codec_name":"mjpeg","codec_type":"video","width":320,"height":240,"r_frame_rate":"25/1","nb_frames":"25","duration":"1.0"},
               {"index":1,"codec_name":"h264","codec_type":"video","width":640,"height":480,"r_frame_rate":"30/1","nb_frames":"30"}"#,
        );
        let info = parse_ffprobe_json(&json, 1).expect("parses");
        assert_eq!(info.video.index, 0, "the motion MJPEG is the clip");
        assert_eq!(info.alternate_video_streams, vec![1]);
    }

    #[test]
    fn a_still_is_chosen_when_it_is_the_only_video() {
        let json = json_with_streams(
            r#"{"index":0,"codec_name":"png","codec_type":"video","width":64,"height":64,"r_frame_rate":"25/1","nb_frames":"1","pix_fmt":"rgba"}"#,
        );
        let info = parse_ffprobe_json(&json, 1).expect("parses");
        assert_eq!(info.video.index, 0);
        assert!(info.video.has_alpha);
    }

    #[test]
    fn a_still_without_frame_count_or_duration_is_not_assumed_to_be_cover_art() {
        // Without any evidence of length the stream is kept as a candidate, so a motion MJPEG is never dropped.
        let json = json_with_streams(
            r#"{"index":0,"codec_name":"mjpeg","codec_type":"video","width":320,"height":240,"r_frame_rate":"25/1"},
               {"index":1,"codec_name":"h264","codec_type":"video","width":320,"height":240,"r_frame_rate":"25/1"}"#,
        );
        let info = parse_ffprobe_json(&json, 1).expect("parses");
        assert_eq!(info.video.index, 0);
    }

    #[test]
    fn extended_bit_depth_names_are_recognised() {
        for (name, depth) in [
            ("x2rgb10le", 10),
            ("x2bgr10le", 10),
            ("nv20le", 10),
            ("p210le", 10),
            ("p410le", 10),
            ("p216le", 16),
            ("rgbaf32le", 32),
            ("gbrapf32le", 32),
            ("grayf32le", 32),
            ("yuv444p16le", 16),
        ] {
            assert_eq!(pix_fmt_bit_depth(name), Some(depth), "{name}");
        }
    }

    #[test]
    fn clock_style_durations_parse() {
        assert_eq!(parse_clock_seconds("00:00:00.040000000"), Some(0.04));
        assert_eq!(parse_clock_seconds("01:02:03.5"), Some(3723.5));
        assert_eq!(parse_clock_seconds("N/A"), None);
    }
}

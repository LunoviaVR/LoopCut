//! Timeline proxy generation (short-GOP H.264, SDR/BT.709) with hardware encoders and CPU fallback.
//!
//! A proxy is what the webview plays while editing, so it must be upright, 8-bit 4:2:0 BT.709, and seekable frame by
//! frame. [`make_proxy`] writes to `<output>.partial` and renames it only after the encoder exits cleanly, so a crash
//! or cancellation never leaves a truncated file under the final name. Encoder failures are returned unchanged; the
//! caller decides whether to retry with [`EncoderBackend::Cpu`].
//!
//! The color chain is shared with the filmstrip thumbnails (see [`crate::thumbnails`]): HDR sources are tonemapped to
//! SDR and wide-gamut sources are converted to BT.709 by [`sdr_conversion_filter`], and every YUV conversion pins the
//! matrix and range from the stream's [`ColorInfo`] instead of relying on swscale's defaults for untagged streams.

use std::fs;
use std::path::{Path, PathBuf};

use loopcut_core::{
    ColorInfo, ColorRange, EncoderBackend, Rational, TransferFunction, VideoCodec, YuvMatrix,
};

use crate::error::{MediaError, MediaResult};
use crate::paths::FfmpegPaths;
use crate::probe::{PROBE_TIMEOUT, probe};
use crate::runner::{CancelToken, FfmpegCommand, InputOptions, Progress, SpawnIo, run_capture};
use crate::thumbnails::rotation_filter;

/// Keyframe interval in frames. Short GOPs make every frame a cheap seek target in the webview.
pub const PROXY_GOP: u32 = 15;

/// Default height cap for proxies.
pub const DEFAULT_PROXY_MAX_HEIGHT: u32 = 720;

/// Stamps BT.709 primaries, transfer, matrix and limited range on the frames, which the encoders write into the VUI.
const BT709_TAGS: &str =
    "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv";

/// YUV tags of a frame stream, as the names swscale and `setparams` expect them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct YuvTags {
    /// `in_color_matrix` name for the `scale` filter.
    pub(crate) matrix: &'static str,
    /// `colorspace` name for `setparams` and `zscale` (`min`).
    pub(crate) colorspace: &'static str,
    /// `range` name, `tv` or `pc`.
    pub(crate) range: &'static str,
}

/// Matrix and range names for a YUV stream, or `None` for RGB sources.
fn yuv_tags(color: ColorInfo) -> Option<YuvTags> {
    let (matrix, colorspace) = match color.matrix {
        YuvMatrix::Bt601 => ("bt601", "bt470bg"),
        YuvMatrix::Bt709 => ("bt709", "bt709"),
        YuvMatrix::Bt2020 => ("bt2020", "bt2020nc"),
        YuvMatrix::Rgb => return None,
    };
    let range = match color.range {
        ColorRange::Limited => "tv",
        ColorRange::Full => "pc",
    };
    Some(YuvTags {
        matrix,
        colorspace,
        range,
    })
}

/// Tags of the frames a `scale` filter receives straight from the decoder: the stream's own color description. HDR
/// streams return `None` because their tonemap chain reads the tags itself; RGB sources have no YUV matrix.
pub(crate) fn source_yuv_tags(color: ColorInfo) -> Option<YuvTags> {
    if color.is_hdr() {
        None
    } else {
        yuv_tags(color)
    }
}

/// Tags of the frames after [`sdr_conversion_filter`]. HDR output is RGB (no YUV tags), and wide-gamut output is
/// BT.709 limited range.
pub(crate) fn converted_yuv_tags(color: ColorInfo) -> Option<YuvTags> {
    if color.is_hdr() {
        None
    } else if color.wide_gamut {
        Some(YuvTags {
            matrix: "bt709",
            colorspace: "bt709",
            range: "tv",
        })
    } else {
        yuv_tags(color)
    }
}

/// `:in_color_matrix=...:in_range=...` options for a `scale` filter, or an empty string when `tags` is `None`.
pub(crate) fn scale_input_options(tags: Option<YuvTags>) -> String {
    match tags {
        Some(tags) => format!(":in_color_matrix={}:in_range={}", tags.matrix, tags.range),
        None => String::new(),
    }
}

/// A `setparams` filter that tags the frames with `tags`, so that the following RGB conversion uses the pinned
/// matrix and range. `None` when there are no YUV tags to set.
pub(crate) fn tag_filter(tags: Option<YuvTags>) -> Option<String> {
    tags.map(|tags| {
        format!(
            "setparams=colorspace={}:range={}",
            tags.colorspace, tags.range
        )
    })
}

/// The zscale input description (`tin`, `min`, `pin`) for a source, from its color tags.
fn zscale_inputs(color: ColorInfo) -> String {
    let tin = match color.transfer {
        TransferFunction::Sdr => "bt709",
        TransferFunction::Pq => "smpte2084",
        TransferFunction::Hlg => "arib-std-b67",
    };
    let pin = if color.wide_gamut || color.is_hdr() {
        "bt2020"
    } else {
        "bt709"
    };
    match yuv_tags(color) {
        Some(tags) => format!("tin={tin}:min={}:pin={pin}", tags.colorspace),
        None => format!("tin={tin}:pin={pin}"),
    }
}

/// The chain that brings a source into SDR BT.709 before the final scale and conversion. HDR (PQ or HLG) is tonemapped
/// with a Hable curve in linear light, and wide-gamut SDR is converted from BT.2020 primaries. Returns `None` for
/// sources that already are SDR BT.709-compatible.
///
/// The HDR chain is verified on `hdr_pq.mp4` and `hdr_hlg.mp4`: the output is 8-bit BT.709 with mean luma near
/// mid-grey and no clipped highlights. Pinning the input tags leaves the output of tagged streams unchanged.
pub(crate) fn sdr_conversion_filter(color: ColorInfo) -> Option<String> {
    let inputs = zscale_inputs(color);
    if color.is_hdr() {
        Some(format!(
            "zscale={inputs}:t=linear:npl=100,format=gbrpf32le,zscale=pin=bt2020:p=bt709,\
             tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv"
        ))
    } else if color.wide_gamut {
        Some(format!("zscale={inputs}:p=bt709:t=bt709:m=bt709:r=tv"))
    } else {
        None
    }
}

/// What to encode and how. Paths are local files; the output is replaced atomically on success.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyRequest {
    /// Source media file.
    pub input: PathBuf,
    /// Absolute container index of the video stream to proxy (`-map 0:<index>`).
    pub stream_index: u32,
    /// Final proxy path. The encoder writes `<output>.partial` first.
    pub output: PathBuf,
    /// Upper bound for the proxy height in pixels; the width follows the displayed aspect ratio.
    pub max_height: u32,
    /// Frame rate cap. `None` keeps the source timing.
    pub fps_cap: Option<Rational>,
    /// Encoder to use. Hardware encoders can fail on sizes they do not support; see the module docs.
    pub encoder: EncoderBackend,
    /// Color description of the source stream, from [`loopcut_core::VideoStreamInfo::color`].
    pub color: ColorInfo,
    /// Clockwise rotation (0, 90, 180 or 270) that makes the stored frames upright.
    pub rotation_cw: u16,
}

/// Result of a successful proxy encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyInfo {
    /// Upright width in pixels (even).
    pub width: u32,
    /// Upright height in pixels (even).
    pub height: u32,
    /// Size of the finished file on disk.
    pub bytes: u64,
}

/// Encodes the proxy described by `req` and returns its geometry.
///
/// `on_progress` receives the fraction of the source duration encoded so far (0.0 to 1.0) on FFmpeg's stderr thread,
/// and 1.0 when the encoder reports completion. The fraction stays at 0.0 if the source duration is unknown.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] for an unusable request (including an output path equal to the input),
/// [`MediaError::Unsupported`] when the encoder backend has no H.264 encoder, [`MediaError::Cancelled`] when `cancel` is
/// set, and [`MediaError::Failed`] when the encoder exits with an error. The `.partial` file is removed on every failure.
pub fn make_proxy(
    paths: &FfmpegPaths,
    req: &ProxyRequest,
    on_progress: impl FnMut(f64) + Send + 'static,
    cancel: CancelToken,
) -> MediaResult<ProxyInfo> {
    validate(req)?;
    let partial = partial_path(&req.output)?;
    let _ = fs::remove_file(&partial);

    // The duration probe takes no token, so a cancel that is already set is checked before each process starts.
    let unless_cancelled = || {
        if cancel.is_cancelled() {
            Err(MediaError::Cancelled)
        } else {
            Ok(())
        }
    };
    let outcome = unless_cancelled()
        .and_then(|()| source_duration(paths, &req.input, req.stream_index))
        .and_then(|duration| {
            unless_cancelled()?;
            encode(paths, req, &partial, duration, on_progress, &cancel)
        });
    let result = outcome.and_then(|()| finish(paths, &partial, &req.output));
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

/// Checks the request before any process starts.
fn validate(req: &ProxyRequest) -> MediaResult<()> {
    if req.max_height < 2 {
        return Err(MediaError::InvalidArgument(format!(
            "proxy max_height must be at least 2, got {}",
            req.max_height
        )));
    }
    if !matches!(req.rotation_cw, 0 | 90 | 180 | 270) {
        return Err(MediaError::InvalidArgument(format!(
            "rotation must be 0, 90, 180 or 270 degrees, got {}",
            req.rotation_cw
        )));
    }
    if let Some(fps) = req.fps_cap
        && !fps.is_valid()
    {
        return Err(MediaError::InvalidArgument(
            "proxy fps cap must be a positive rational".to_owned(),
        ));
    }
    if req.output == req.input {
        return Err(MediaError::InvalidArgument(
            "proxy output must not overwrite its input".to_owned(),
        ));
    }
    // Fails before any probe or encode when the backend cannot produce H.264.
    encoder_codec(req.encoder)?;
    Ok(())
}

/// `<output>.partial`, in the same folder so the final rename never crosses volumes.
fn partial_path(output: &Path) -> MediaResult<PathBuf> {
    let name = output.file_name().ok_or_else(|| {
        MediaError::InvalidArgument(format!(
            "proxy output has no file name: {}",
            output.display()
        ))
    })?;
    let mut partial_name = name.to_os_string();
    partial_name.push(".partial");
    Ok(output.with_file_name(partial_name))
}

/// Reads the duration of one stream in seconds, falling back to the container duration.
///
/// Returns `0.0` when neither is known. The progress callback then reports only the final 1.0.
fn source_duration(paths: &FfmpegPaths, input: &Path, stream_index: u32) -> MediaResult<f64> {
    let args = [
        "-select_streams".to_owned(),
        stream_index.to_string(),
        "-show_entries".to_owned(),
        "format=duration:stream=duration".to_owned(),
        "-of".to_owned(),
        "json".to_owned(),
    ];
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(input, &InputOptions::default())?
        .output_args(args)?;
    let stdout = run_capture(command, PROBE_TIMEOUT)?;
    let value: serde_json::Value = serde_json::from_slice(&stdout)?;
    let parse = |node: Option<&serde_json::Value>| {
        node.and_then(serde_json::Value::as_str)
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    };
    let stream = value.get("streams").and_then(|streams| streams.get(0));
    let format = value.get("format");
    Ok(parse(stream.and_then(|s| s.get("duration")))
        .or_else(|| parse(format.and_then(|f| f.get("duration"))))
        .unwrap_or(0.0))
}

/// Runs the encoder into `partial`, reporting progress against `duration`.
fn encode(
    paths: &FfmpegPaths,
    req: &ProxyRequest,
    partial: &Path,
    duration: f64,
    mut on_progress: impl FnMut(f64) + Send + 'static,
    cancel: &CancelToken,
) -> MediaResult<()> {
    let total_us = duration * 1_000_000.0;
    let video_filter = video_filter(req);
    // The source's display matrix would be copied into the output and rotate the proxy a second time in the browser.
    // An input-side `-display_rotation` of 0 replaces it, so the proxy carries no rotation and the transposes above
    // are the only rotation.
    let input_options = InputOptions {
        extra: vec![
            format!("-display_rotation:{}", req.stream_index),
            "0".to_owned(),
        ],
        ..InputOptions::default()
    };
    let command = FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")?
        .input(&req.input, &input_options)?
        .output_args(output_args(req, &video_filter)?)?
        .output(partial)?
        .on_progress(move |progress: Progress| {
            if progress.done {
                on_progress(1.0);
            } else if let Some(us) = progress.out_time_us
                && total_us > 0.0
            {
                // Lossy conversion is fine: a fraction for a progress bar.
                on_progress((us as f64 / total_us).clamp(0.0, 1.0));
            }
        });
    command
        .spawn(SpawnIo::default())?
        .wait_cancellable(cancel, None)
        .map(|_| ())
}

/// The complete `-vf` chain: rotation, optional frame-rate cap, conversion to SDR BT.709, scale, and 4:2:0 output.
fn video_filter(req: &ProxyRequest) -> String {
    let mut filters: Vec<String> = Vec::with_capacity(7);
    if let Some(rotation) = rotation_filter(req.rotation_cw) {
        filters.push(rotation.to_owned());
    }
    if let Some(fps) = req.fps_cap {
        filters.push(format!("fps={fps}"));
    }
    if let Some(conversion) = sdr_conversion_filter(req.color) {
        filters.push(conversion);
    }
    // An even height no larger than the cap, and a width of -2 (the aspect ratio, kept even).
    let max_height = req.max_height;
    let pins = scale_input_options(converted_yuv_tags(req.color));
    filters.push(format!(
        "scale=-2:'2*trunc(min({max_height},ih)/2)':flags=lanczos{pins}:out_color_matrix=bt709:out_range=tv"
    ));
    filters.push("format=yuv420p".to_owned());
    // Tag the frames, not the codec: libx264 and the AMF encoder take their VUI from frame properties, and output options
    // such as `-color_primaries` are ignored by libx264 when the frames carry their own (untagged) values.
    filters.push(BT709_TAGS.to_owned());
    filters.join(",")
}

/// Stream selection, encoder settings and container options for the proxy file.
fn output_args(req: &ProxyRequest, video_filter: &str) -> MediaResult<Vec<String>> {
    let mut args: Vec<String> = vec![
        "-map".to_owned(),
        format!("0:{}", req.stream_index),
        "-map_metadata".to_owned(),
        "-1".to_owned(),
        "-an".to_owned(),
        "-sn".to_owned(),
        "-vf".to_owned(),
        video_filter.to_owned(),
    ];
    args.extend(encoder_args(req.encoder)?);
    args.extend(
        [
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ]
        .map(str::to_owned),
    );
    Ok(args)
}

/// The FFmpeg H.264 encoder name for a backend. Every backend has one today, but a missing name is an error rather than
/// a silent substitution, so a backend without H.264 can never quietly produce libx264 output.
fn encoder_codec(encoder: EncoderBackend) -> MediaResult<&'static str> {
    encoder.ffmpeg_encoder(VideoCodec::H264).ok_or_else(|| {
        MediaError::Unsupported(format!("{encoder:?} has no H.264 encoder for proxies"))
    })
}

/// Encoder name and quality settings for a backend. Every backend gets the same GOP and no B-frames.
fn encoder_args(encoder: EncoderBackend) -> MediaResult<Vec<String>> {
    let codec = encoder_codec(encoder)?;
    let mut args = vec!["-c:v".to_owned(), codec.to_owned()];
    let quality: &[&str] = match encoder {
        EncoderBackend::Cpu => &["-preset", "veryfast", "-crf", "23", "-sc_threshold", "0"],
        EncoderBackend::Amf { .. } => &[
            "-quality", "speed", "-rc", "cqp", "-qp_i", "23", "-qp_p", "23",
        ],
        EncoderBackend::Nvenc { .. } => &["-preset", "p3", "-rc", "vbr", "-cq", "23", "-b:v", "0"],
        EncoderBackend::Qsv { .. } => &["-preset", "veryfast", "-global_quality", "23"],
    };
    args.extend(quality.iter().map(|arg| (*arg).to_owned()));
    args.extend([
        "-g".to_owned(),
        PROXY_GOP.to_string(),
        "-bf".to_owned(),
        "0".to_owned(),
    ]);
    Ok(args)
}

/// Probes the finished partial file for its geometry, then moves it into place.
fn finish(paths: &FfmpegPaths, partial: &Path, output: &Path) -> MediaResult<ProxyInfo> {
    let info = probe(paths, partial)?;
    let (width, height) = info.video.display_size();
    fs::rename(partial, output)?;
    let bytes = fs::metadata(output)?.len();
    Ok(ProxyInfo {
        width,
        height,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(rotation_cw: u16, color: ColorInfo) -> ProxyRequest {
        ProxyRequest {
            input: PathBuf::from("in.mp4"),
            stream_index: 0,
            output: PathBuf::from("out.mp4"),
            max_height: DEFAULT_PROXY_MAX_HEIGHT,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color,
            rotation_cw,
        }
    }

    fn hdr(transfer: TransferFunction) -> ColorInfo {
        ColorInfo {
            matrix: YuvMatrix::Bt2020,
            range: ColorRange::Limited,
            transfer,
            wide_gamut: true,
        }
    }

    #[test]
    fn a_cancelled_token_starts_no_process() {
        // Executables that do not exist: a started process would fail with a spawn error, not Cancelled.
        let paths = FfmpegPaths {
            ffmpeg: PathBuf::from("missing-ffmpeg.exe"),
            ffprobe: PathBuf::from("missing-ffprobe.exe"),
        };
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = make_proxy(&paths, &request(0, ColorInfo::SDR_BT709), |_| {}, cancel);
        assert!(matches!(result, Err(MediaError::Cancelled)), "{result:?}");
    }

    #[test]
    fn partial_file_sits_next_to_the_output() {
        let partial = partial_path(Path::new("cache/proxy/clip.mp4")).expect("partial");
        assert_eq!(partial, Path::new("cache/proxy/clip.mp4.partial"));
        assert!(partial_path(Path::new("")).is_err());
    }

    #[test]
    fn sdr_chain_scales_and_converts_to_420() {
        let chain = video_filter(&request(0, ColorInfo::SDR_BT709));
        assert!(
            chain.starts_with("scale=-2:'2*trunc(min(720,ih)/2)'"),
            "{chain}"
        );
        assert!(chain.contains(",format=yuv420p,"), "{chain}");
        assert!(chain.ends_with(BT709_TAGS), "{chain}");
        assert!(!chain.contains("tonemap"));
        assert!(!chain.contains("transpose"));
    }

    #[test]
    fn sdr_scale_pins_the_source_matrix_and_range() {
        let untagged_sd = ColorInfo {
            matrix: YuvMatrix::Bt601,
            range: ColorRange::Full,
            transfer: TransferFunction::Sdr,
            wide_gamut: false,
        };
        let chain = video_filter(&request(0, untagged_sd));
        assert!(
            chain
                .contains(":in_color_matrix=bt601:in_range=pc:out_color_matrix=bt709:out_range=tv"),
            "{chain}"
        );
        // RGB sources have no YUV matrix to pin.
        let rgb = ColorInfo {
            matrix: YuvMatrix::Rgb,
            range: ColorRange::Full,
            transfer: TransferFunction::Sdr,
            wide_gamut: false,
        };
        assert!(!video_filter(&request(0, rgb)).contains("in_color_matrix"));
    }

    #[test]
    fn rotation_and_hdr_are_applied_in_order() {
        let mut req = request(270, hdr(TransferFunction::Pq));
        req.fps_cap = Some(Rational::new(30000, 1001));
        let chain = video_filter(&req);
        let rotate = chain.find("transpose=2").expect("rotation");
        let fps = chain.find("fps=30000/1001").expect("fps");
        let tonemap = chain.find("tonemap=tonemap=hable").expect("tonemap");
        assert!(rotate < fps && fps < tonemap, "{chain}");
    }

    #[test]
    fn hdr_inputs_are_pinned_from_the_color_description() {
        let pq = sdr_conversion_filter(hdr(TransferFunction::Pq)).expect("hdr chain");
        assert!(
            pq.starts_with("zscale=tin=smpte2084:min=bt2020nc:pin=bt2020:t=linear:npl=100"),
            "{pq}"
        );
        let hlg = sdr_conversion_filter(hdr(TransferFunction::Hlg)).expect("hlg chain");
        assert!(hlg.starts_with("zscale=tin=arib-std-b67:"), "{hlg}");
        assert!(sdr_conversion_filter(ColorInfo::SDR_BT709).is_none());
    }

    #[test]
    fn encoder_settings_keep_a_short_gop_without_b_frames() {
        for encoder in [
            EncoderBackend::Cpu,
            EncoderBackend::Amf { dxgi_index: 0 },
            EncoderBackend::Nvenc { cuda_index: 0 },
            EncoderBackend::Qsv { dxgi_index: 0 },
        ] {
            let args = encoder_args(encoder).expect("H.264 encoder");
            let gop = args.iter().position(|arg| arg == "-g").expect("-g");
            assert_eq!(args[gop + 1], "15");
            let bf = args.iter().position(|arg| arg == "-bf").expect("-bf");
            assert_eq!(args[bf + 1], "0");
        }
        assert_eq!(
            encoder_args(EncoderBackend::Amf { dxgi_index: 0 }).expect("amf")[1],
            "h264_amf"
        );
    }

    #[test]
    fn invalid_requests_fail_before_encoding() {
        let mut same = request(0, ColorInfo::SDR_BT709);
        same.output = same.input.clone();
        assert!(validate(&same).is_err());
        let mut sideways = request(45, ColorInfo::SDR_BT709);
        assert!(validate(&sideways).is_err());
        sideways.rotation_cw = 90;
        sideways.max_height = 1;
        assert!(validate(&sideways).is_err());
    }
}

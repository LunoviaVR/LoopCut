//! Decode command builder and raw-frame decode sessions for every hardware backend.
//!
//! [`build_decode_command`] turns a [`DecodeRequest`] into an FFmpeg invocation that writes raw frames (NV12, P010 or
//! RGBA) to stdout. [`DecodeSession`] runs that invocation and yields the frames in source-time order.
//!
//! # Timing
//!
//! Frames are decoded once at a constant rate (`fps`, or the stream's `r_frame_rate` when the request leaves it
//! unset). Output frame `i` is at source time `range.start + i / fps`, where source time 0 is the start of the selected
//! video stream.
//!
//! Resampling uses `fps=round=up:start_time=0`. Output frame `i` is the last source frame at or before its instant
//! `range.start + i / fps`, so it is never more than one source frame early. The grid starts at the seek point. When no
//! frame precedes the seek point, the first decoded frame stands in for the first output frame. That is exact
//! when the source rate is a whole multiple of the output rate, and within one source frame early otherwise.
//!
//! The input seek is absolute (`-seek_timestamp 1`), so it is measured from the stream's own start rather than from the
//! container's, which can differ when audio starts earlier.
//!
//! # Filter chains
//!
//! The graph runs in this order: the GPU scaler (if any, on the hardware frames), `hwdownload` (hardware backends only),
//! CPU crop, CPU scale (lanczos), `fps`, and finally `format` for the requested layout.
//!
//! GPU scalers that cannot crop (`vpp_amf`, `scale_cuda`, `scale_d3d11`) scale the whole frame so that the crop region
//! lands on the final size, then crop on the CPU at the scaled offset with `exact=1`. The GPU does the scaling and the
//! crop stays exact. `vpp_qsv` crops on the GPU directly.
//!
//! # Pixel formats
//!
//! Hardware frames are downloaded as `nv12` for 8-bit sources and `p010le` for higher bit depths. RGBA output is CPU
//! only, and a VP9 source with alpha is decoded with `libvpx-vp9`, because FFmpeg's native VP9 decoder drops the alpha
//! plane.

use std::io;
use std::path::PathBuf;
use std::process::ChildStdout;
use std::time::Duration;

use loopcut_core::{
    ColorInfo, DecodeBackend, DecodeChain, GpuScaler, PixelLayout, PixelRect, Rational, TimeRange,
};

use crate::error::MediaError;
use crate::paths::FfmpegPaths;
use crate::probe::PROBE_TIMEOUT;
use crate::proxy::{source_yuv_tags, tag_filter};
use crate::runner::{
    CancelToken, FfmpegCommand, FfmpegProcess, InputOptions, RawFrameReader, SpawnIo, run_capture,
};

/// How long a failed read waits for FFmpeg to exit, so that its own error can be reported.
const EXIT_WAIT: Duration = Duration::from_secs(10);

/// Everything needed to decode one video stream into raw frames.
///
/// Geometry is in stored (coded) pixels, before the display rotation that the compositor applies.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeRequest {
    /// The local media file.
    pub input: PathBuf,
    /// Absolute stream index inside the container (`-map 0:<index>`).
    pub stream_index: u32,
    /// Coded width of the stream, from the probe.
    pub coded_width: u32,
    /// Coded height of the stream, from the probe.
    pub coded_height: u32,
    /// Bits per component of the source (8 for NV12 sources, 10 or 12 for HDR and ProRes).
    pub bit_depth: u8,
    /// True if the stream carries an alpha plane.
    pub has_alpha: bool,
    /// Codec name from the probe (`h264`, `hevc`, `vp9`, ...).
    pub codec: String,
    /// Color description of the stream.
    pub color: ColorInfo,
    /// Absolute start time of the selected video stream in seconds (ffprobe `start_time`). Source time 0 is this
    /// instant.
    pub start_time: f64,
    /// Source-time range to decode, `[start, end)` in seconds from the stream start. `None` decodes the whole stream.
    pub range: Option<TimeRange>,
    /// Crop rectangle in coded pixels. Every coordinate and size must be even, and the rectangle must fit the frame.
    pub crop: Option<PixelRect>,
    /// Final frame size after the crop, in stored orientation. Both values must be even. `None` keeps the crop size, or
    /// the coded size when there is no crop.
    pub size: Option<(u32, u32)>,
    /// Constant output frame rate. `None` uses the stream's `r_frame_rate`, which [`DecodeSession::start`] reads with
    /// ffprobe before building the command.
    pub fps: Option<Rational>,
    /// The decode backend and GPU scaler.
    pub chain: DecodeChain,
    /// Layout of the frames delivered to the caller.
    pub layout: PixelLayout,
}

impl DecodeRequest {
    /// Size of each delivered frame, after crop and scale.
    pub fn output_size(&self) -> (u32, u32) {
        if let Some(size) = self.size {
            return size;
        }
        match self.crop {
            Some(crop) => (crop.w, crop.h),
            None => (self.coded_width, self.coded_height),
        }
    }

    /// Bytes in one delivered frame.
    pub fn frame_bytes(&self) -> usize {
        let (width, height) = self.output_size();
        self.layout.frame_bytes(width, height)
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFrame {
    /// Position in the decoded sequence, starting at 0.
    pub index: u64,
    /// Source time of the frame in seconds: `range.start + index / fps`.
    pub time: f64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Layout of `data`.
    pub layout: PixelLayout,
    /// Tightly packed pixels in `layout`.
    pub data: Vec<u8>,
}

/// The validated pieces of one decode command.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    /// Global options (creation of the hardware device the decoder uses).
    global: Vec<String>,
    /// Hardware decode options, placed before `-i`.
    hwaccel: Vec<String>,
    /// Other input options, placed before `-i` (the VP9 alpha decoder override).
    input_extra: Vec<String>,
    /// The complete video filter graph.
    filter: String,
}

/// Builds the decode command for `req`: frames of the selected stream on stdout as raw `req.layout` pixels.
///
/// The command reads only `req.input` and writes only to stdout. The caller spawns it with
/// [`FfmpegCommand::spawn`] (stdout piped), or uses [`DecodeSession`], which also resolves an unset frame rate.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] for geometry that is zero, odd or outside the frame, an invalid time range, a scaler
/// that does not belong to the backend, and combinations that cannot work (RGBA output or VP9 alpha on a hardware
/// backend). [`MediaError::UrlNotAllowed`] if the input path is URL-like.
pub fn build_decode_command(
    paths: &FfmpegPaths,
    req: &DecodeRequest,
) -> Result<FfmpegCommand, MediaError> {
    let plan = plan(req)?;

    // The seek is absolute (`-seek_timestamp 1`), measured from the stream start. Adding `start_time` makes source
    // time 0 the video's first instant even when the container starts earlier.
    let seek_at = req.start_time + req.range.map_or(0.0, |range| range.start);
    let mut input_extra = Vec::new();
    let seek = if seek_at > 0.0 {
        input_extra.extend(words(&["-seek_timestamp", "1"]));
        Some(seek_at)
    } else {
        None
    };
    input_extra.extend(plan.input_extra);
    let input = InputOptions {
        seek,
        hwaccel_args: plan.hwaccel,
        extra: input_extra,
        ..InputOptions::default()
    };

    let mut output = vec![
        "-map".to_owned(),
        format!("0:{}", req.stream_index),
        "-an".to_owned(),
        "-sn".to_owned(),
        "-dn".to_owned(),
    ];
    if let Some(range) = req.range {
        output.extend(["-t".to_owned(), format_seconds(range.duration())]);
    }
    output.extend(words(&["-vf"]));
    output.push(plan.filter);
    output.extend(words(&["-fps_mode", "cfr", "-f", "rawvideo", "-pix_fmt"]));
    output.push(req.layout.ffmpeg_pix_fmt().to_owned());

    FfmpegCommand::new_ffmpeg(paths)
        .global_args(plan.global)?
        .input(&req.input, &input)?
        .output_args(output)
        .map(FfmpegCommand::output_stdout)
}

/// A running decode: an iterator over the frames of one request.
///
/// Iteration yields [`DecodedFrame`]s in order. If FFmpeg fails, the iterator yields one `Err` carrying the exit code and
/// the last stderr lines, then ends. Dropping the session kills FFmpeg, so stopping early is cheap.
pub struct DecodeSession {
    process: Option<FfmpegProcess>,
    reader: Option<RawFrameReader<ChildStdout>>,
    cancel: CancelToken,
    fps: Rational,
    range_start: f64,
    width: u32,
    height: u32,
    layout: PixelLayout,
    frame_bytes: usize,
    next_index: u64,
}

impl DecodeSession {
    /// Starts decoding `req`. When `req.fps` is `None`, the stream's `r_frame_rate` is read with ffprobe first.
    ///
    /// Cancelling `cancel` makes the next call to `next` return [`MediaError::Cancelled`] and kills FFmpeg. Note that
    /// `next` blocks while FFmpeg produces the next frame, so cancellation takes effect between frames.
    ///
    /// # Errors
    ///
    /// Any error from [`build_decode_command`], from the frame-rate probe, or from starting the process. FFmpeg failures
    /// after the start (hardware setup that fails, corrupt data) arrive through the iterator instead.
    pub fn start(
        paths: &FfmpegPaths,
        req: &DecodeRequest,
        cancel: CancelToken,
    ) -> Result<Self, MediaError> {
        let fps = match req.fps {
            Some(fps) => fps,
            None => source_frame_rate(paths, req)?,
        };
        let resolved = DecodeRequest {
            fps: Some(fps),
            ..req.clone()
        };
        let command = build_decode_command(paths, &resolved)?;
        let mut process = command.spawn(SpawnIo {
            stdin: false,
            stdout: true,
        })?;
        let stdout = process
            .take_stdout()
            .ok_or_else(|| MediaError::Io(io::Error::other("decoder stdout pipe is missing")))?;
        let (width, height) = resolved.output_size();
        let frame_bytes = resolved.frame_bytes();
        Ok(Self {
            process: Some(process),
            reader: Some(RawFrameReader::new(stdout, frame_bytes)),
            cancel,
            fps,
            range_start: req.range.map_or(0.0, |range| range.start),
            width,
            height,
            layout: req.layout,
            frame_bytes,
            next_index: 0,
        })
    }

    /// The frame rate the frames were decoded at: `req.fps`, or the stream rate resolved by [`Self::start`].
    pub fn fps(&self) -> Rational {
        self.fps
    }

    /// Stops the decoder. Dropping the process kills FFmpeg and reaps it.
    fn abort(&mut self) {
        self.reader = None;
        self.process = None;
    }

    /// Closes the pipe and waits for FFmpeg, reporting a non-zero exit as an error.
    fn finish(&mut self) -> Option<Result<DecodedFrame, MediaError>> {
        self.reader = None;
        let process = self.process.take()?;
        process.wait().err().map(Err)
    }
}

impl Iterator for DecodeSession {
    type Item = Result<DecodedFrame, MediaError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.reader.as_ref()?;
        if self.cancel.is_cancelled() {
            self.abort();
            return Some(Err(MediaError::Cancelled));
        }
        let mut data = vec![0u8; self.frame_bytes];
        let outcome = self.reader.as_mut()?.read_frame(&mut data);
        match outcome {
            Ok(true) => {
                let index = self.next_index;
                self.next_index += 1;
                let time = self.range_start + index as f64 / self.fps.as_f64();
                Some(Ok(DecodedFrame {
                    index,
                    time,
                    width: self.width,
                    height: self.height,
                    layout: self.layout,
                    data,
                }))
            }
            Ok(false) => self.finish(),
            Err(err) => {
                // A short frame usually means FFmpeg failed. Closing the pipe makes it exit, so report its stderr tail
                // when it did, and the I/O error otherwise.
                self.reader = None;
                let failure = self
                    .process
                    .take()
                    .and_then(|process| process.wait_timeout(EXIT_WAIT).err());
                match failure {
                    Some(failed @ MediaError::Failed { .. }) => Some(Err(failed)),
                    _ => Some(Err(MediaError::Io(err))),
                }
            }
        }
    }
}

/// Reads the selected stream's `r_frame_rate` with ffprobe.
fn source_frame_rate(paths: &FfmpegPaths, req: &DecodeRequest) -> Result<Rational, MediaError> {
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(&req.input, &InputOptions::default())?
        .output_args([
            "-select_streams".to_owned(),
            req.stream_index.to_string(),
            "-show_entries".to_owned(),
            "stream=r_frame_rate".to_owned(),
            "-of".to_owned(),
            "csv=p=0".to_owned(),
        ])?;
    let stdout = run_capture(command, PROBE_TIMEOUT)?;
    let text = String::from_utf8_lossy(&stdout);
    let line = text.lines().next().unwrap_or_default().trim();
    Rational::parse(line).ok_or_else(|| {
        MediaError::ProbeParse(format!(
            "no frame rate for stream {}: {line:?}",
            req.stream_index
        ))
    })
}

/// Checks the request and derives the command pieces.
fn plan(req: &DecodeRequest) -> Result<Plan, MediaError> {
    validate(req)?;
    let (width, height) = req.output_size();
    let coded = (req.coded_width, req.coded_height);
    let (scale_format, download_format) = if req.bit_depth > 8 {
        ("p010", "p010le")
    } else {
        ("nv12", "nv12")
    };
    let (global, hwaccel) = backend_args(req.chain.backend);
    // Without a crop the region is the whole frame.
    let crop = req.crop.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        w: req.coded_width,
        h: req.coded_height,
    });

    // `gpu` runs on the hardware frames before the download; the CPU steps run after it.
    let mut gpu = None;
    let mut cpu_crop = None;
    let mut cpu_scale = None;
    match req.chain.scaler {
        GpuScaler::None => {
            cpu_crop = req.crop;
            if (width, height) != (crop.w, crop.h) {
                cpu_scale = Some((width, height));
            }
        }
        GpuScaler::Qsv => {
            // vpp_qsv crops and scales on the GPU, so the CPU has nothing to do.
            if req.crop.is_some() || (width, height) != coded {
                gpu = Some(qsv_filter(req.crop, width, height, scale_format));
            }
        }
        GpuScaler::Amf | GpuScaler::Cuda | GpuScaler::D3d11 => {
            let (gw, gh) = match req.crop {
                Some(_) => {
                    let gw = round_even(f64::from(coded.0) * f64::from(width) / f64::from(crop.w))
                        .max(width);
                    let gh = round_even(f64::from(coded.1) * f64::from(height) / f64::from(crop.h))
                        .max(height);
                    let x = round_even(f64::from(crop.x) * f64::from(gw) / f64::from(coded.0))
                        .min(gw - width);
                    let y = round_even(f64::from(crop.y) * f64::from(gh) / f64::from(coded.1))
                        .min(gh - height);
                    cpu_crop = Some(PixelRect {
                        x,
                        y,
                        w: width,
                        h: height,
                    });
                    (gw, gh)
                }
                None => (width, height),
            };
            if (gw, gh) != coded {
                gpu = Some(scaled_filter(req.chain.scaler, gw, gh, scale_format));
            }
        }
    }

    let mut filters = Vec::new();
    filters.extend(gpu);
    if req.chain.backend.is_hardware() {
        filters.push(format!("hwdownload,format={download_format}"));
    }
    if let Some(c) = cpu_crop {
        filters.push(format!("crop={}:{}:{}:{}:exact=1", c.w, c.h, c.x, c.y));
    }
    if let Some((w, h)) = cpu_scale {
        filters.push(format!("scale={w}:{h}:flags=lanczos"));
    }
    if let Some(fps) = req.fps {
        // round=up: each output frame is the last source frame at or before its instant. When the source rate is a
        // whole multiple of the output rate, which covers the usual loop settings, every sample is exact. The default
        // (near) can pick a frame after the instant, and down is one source frame or more early.
        //
        // start_time=0 anchors the grid at the seek point. After an absolute seek the filter graph's timestamps start
        // at the seek target, so frame i is then at `range.start + i / fps` as documented. Without it the grid would
        // anchor on the first decoded frame; measured after a seek between frames, that put output frames two source frames late.
        filters.push(format!("fps={fps}:round=up:start_time=0"));
    }
    // RGBA is YUV converted by FFmpeg, so the stream's matrix and range are tagged explicitly. Otherwise swscale would
    // fall back to its BT.601 default for untagged streams, while the probe describes them as BT.709 for HD sources.
    if req.layout == PixelLayout::Rgba8
        && let Some(tag) = tag_filter(source_yuv_tags(req.color))
    {
        filters.push(tag);
    }
    filters.push(format!("format={}", req.layout.ffmpeg_pix_fmt()));

    let input_extra = if req.has_alpha && req.codec.eq_ignore_ascii_case("vp9") {
        words(&["-c:v", "libvpx-vp9"])
    } else {
        Vec::new()
    };

    Ok(Plan {
        global,
        hwaccel,
        input_extra,
        filter: filters.join(","),
    })
}

/// Rejects requests that cannot be decoded, before any process starts.
fn validate(req: &DecodeRequest) -> Result<(), MediaError> {
    let invalid = |message: String| Err(MediaError::InvalidArgument(message));
    if req.coded_width == 0 || req.coded_height == 0 {
        return invalid("coded frame size must be non-zero".to_owned());
    }
    if !(req.start_time.is_finite() && req.start_time >= 0.0) {
        return invalid(format!(
            "start time must be finite and >= 0, got {}",
            req.start_time
        ));
    }
    if let Some(range) = req.range
        && !range.is_valid()
    {
        return invalid(format!("invalid decode range {range:?}"));
    }
    if let Some(fps) = req.fps
        && !fps.is_valid()
    {
        return invalid(format!("invalid frame rate {fps}"));
    }
    if let Some(crop) = req.crop {
        let fits = crop
            .x
            .checked_add(crop.w)
            .is_some_and(|right| right <= req.coded_width)
            && crop
                .y
                .checked_add(crop.h)
                .is_some_and(|bottom| bottom <= req.coded_height);
        if crop.w == 0 || crop.h == 0 || !fits {
            return invalid(format!(
                "crop {crop:?} is empty or outside the {}x{} frame",
                req.coded_width, req.coded_height
            ));
        }
        if [crop.x, crop.y, crop.w, crop.h]
            .iter()
            .any(|value| !value.is_multiple_of(2))
        {
            return invalid(format!("crop {crop:?} must be even-aligned"));
        }
    }
    if let Some((w, h)) = req.size
        && (w == 0 || h == 0)
    {
        return invalid(format!("size {w}x{h} must be non-zero"));
    }
    let (width, height) = req.output_size();
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return invalid(format!("output size {width}x{height} must be even"));
    }
    let backend_ok = matches!(
        (req.chain.backend, req.chain.scaler),
        (DecodeBackend::Cpu, GpuScaler::None)
            | (
                DecodeBackend::D3d11va { .. },
                GpuScaler::None | GpuScaler::D3d11
            )
            | (DecodeBackend::Amf { .. }, GpuScaler::None | GpuScaler::Amf)
            | (
                DecodeBackend::Cuda { .. },
                GpuScaler::None | GpuScaler::Cuda
            )
            | (DecodeBackend::Qsv { .. }, GpuScaler::None | GpuScaler::Qsv)
    );
    if !backend_ok {
        return invalid(format!(
            "scaler {:?} cannot run on backend {:?}",
            req.chain.scaler, req.chain.backend
        ));
    }
    if req.layout == PixelLayout::Rgba8 && req.chain.backend.is_hardware() {
        return Err(MediaError::Unsupported(
            "RGBA output is CPU-only; use the CPU chain".to_owned(),
        ));
    }
    if req.has_alpha && req.codec.eq_ignore_ascii_case("vp9") && req.chain.backend.is_hardware() {
        return Err(MediaError::Unsupported(
            "VP9 with alpha decodes in software only; use the CPU chain".to_owned(),
        ));
    }
    Ok(())
}

/// Global and input options that create and select the hardware decoder for a backend.
fn backend_args(backend: DecodeBackend) -> (Vec<String>, Vec<String>) {
    match backend {
        DecodeBackend::Cpu => (Vec::new(), Vec::new()),
        DecodeBackend::D3d11va { dxgi_index } => (
            vec![
                "-init_hw_device".to_owned(),
                format!("d3d11va=dx:{dxgi_index}"),
            ],
            words(&[
                "-hwaccel",
                "d3d11va",
                "-hwaccel_device",
                "dx",
                "-hwaccel_output_format",
                "d3d11",
            ]),
        ),
        // FFmpeg's AMF decoder takes no device argument (`-hwaccel_device` fails), so decode always uses the default
        // AMD device. The DXGI index is recorded for the probe but cannot select the adapter here.
        DecodeBackend::Amf { .. } => (
            Vec::new(),
            words(&["-hwaccel", "amf", "-hwaccel_output_format", "amf"]),
        ),
        DecodeBackend::Cuda { cuda_index } => (
            Vec::new(),
            vec![
                "-hwaccel".to_owned(),
                "cuda".to_owned(),
                "-hwaccel_device".to_owned(),
                cuda_index.to_string(),
                "-hwaccel_output_format".to_owned(),
                "cuda".to_owned(),
            ],
        ),
        DecodeBackend::Qsv { dxgi_index } => (
            vec![
                "-init_hw_device".to_owned(),
                format!("qsv=hw,child_device_type=d3d11va,child_device={dxgi_index}"),
            ],
            words(&[
                "-hwaccel",
                "qsv",
                "-hwaccel_device",
                "hw",
                "-hwaccel_output_format",
                "qsv",
            ]),
        ),
    }
}

/// The GPU scale filter for scalers that cannot crop: scales the frame to `width`x`height`.
fn scaled_filter(scaler: GpuScaler, width: u32, height: u32, format: &str) -> String {
    match scaler {
        GpuScaler::Cuda => {
            format!("scale_cuda={width}:{height}:interp_algo=lanczos:format={format}")
        }
        GpuScaler::D3d11 => {
            format!("scale_d3d11=width={width}:height={height}:format={format}")
        }
        GpuScaler::Amf => format!("vpp_amf=w={width}:h={height}:format={format}"),
        // Only reached for Qsv through `qsv_filter`; the arm keeps the match exhaustive.
        GpuScaler::Qsv | GpuScaler::None => {
            format!("scale={width}:{height}:flags=lanczos,format={format}")
        }
    }
}

/// The `vpp_qsv` filter: crops `crop` (when present) and scales to `width`x`height` on the GPU.
fn qsv_filter(crop: Option<PixelRect>, width: u32, height: u32, format: &str) -> String {
    match crop {
        Some(c) => format!(
            "vpp_qsv=cw={}:ch={}:cx={}:cy={}:w={width}:h={height}:format={format}",
            c.w, c.h, c.x, c.y
        ),
        None => format!("vpp_qsv=w={width}:h={height}:format={format}"),
    }
}

/// Rounds to the nearest even integer (chroma subsampling needs even offsets and sizes).
fn round_even(value: f64) -> u32 {
    ((value / 2.0).round() * 2.0).max(0.0) as u32
}

/// Formats seconds for an FFmpeg time option with fixed precision, so command lines are deterministic.
fn format_seconds(seconds: f64) -> String {
    format!("{seconds:.6}")
}

/// Splits a list of literal arguments into owned strings.
fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DecodeRequest {
        DecodeRequest {
            input: PathBuf::from("clip.mp4"),
            stream_index: 0,
            coded_width: 1920,
            coded_height: 1080,
            bit_depth: 8,
            has_alpha: false,
            codec: "h264".to_owned(),
            color: ColorInfo::SDR_BT709,
            start_time: 0.0,
            range: None,
            crop: None,
            size: None,
            fps: Some(Rational::integer(30)),
            chain: DecodeChain::CPU,
            layout: PixelLayout::Nv12,
        }
    }

    #[test]
    fn output_size_follows_size_then_crop_then_coded() {
        let mut req = request();
        assert_eq!(req.output_size(), (1920, 1080));
        req.crop = Some(PixelRect {
            x: 0,
            y: 0,
            w: 640,
            h: 360,
        });
        assert_eq!(req.output_size(), (640, 360));
        req.size = Some((320, 180));
        assert_eq!(req.output_size(), (320, 180));
        assert_eq!(req.frame_bytes(), 320 * 180 * 3 / 2);
    }

    #[test]
    fn round_even_keeps_offsets_on_even_pixels() {
        assert_eq!(round_even(0.0), 0);
        assert_eq!(round_even(159.4), 160);
        assert_eq!(round_even(161.0), 162);
        assert_eq!(round_even(1.0), 2);
    }

    #[test]
    fn scale_then_crop_puts_the_crop_on_the_scaled_grid() {
        // Crop (320,180,1280,720) of 1920x1080 to 640x360: the whole frame goes to 960x540 on the GPU, and the CPU
        // crops (160,90) of size 640x360.
        let mut req = request();
        req.chain = DecodeChain {
            backend: DecodeBackend::Amf { dxgi_index: 0 },
            scaler: GpuScaler::Amf,
        };
        req.crop = Some(PixelRect {
            x: 320,
            y: 180,
            w: 1280,
            h: 720,
        });
        req.size = Some((640, 360));
        let plan = plan(&req).expect("valid plan");
        assert_eq!(
            plan.filter,
            "vpp_amf=w=960:h=540:format=nv12,hwdownload,format=nv12,crop=640:360:160:90:exact=1,fps=30:round=up:start_time=0,format=nv12"
        );
    }

    #[test]
    fn scale_then_crop_offsets_never_leave_the_scaled_frame() {
        // A crop flush against the bottom-right corner must still fit after rounding.
        let mut req = request();
        req.coded_width = 1000;
        req.coded_height = 600;
        req.chain = DecodeChain {
            backend: DecodeBackend::Cuda { cuda_index: 0 },
            scaler: GpuScaler::Cuda,
        };
        req.crop = Some(PixelRect {
            x: 500,
            y: 300,
            w: 500,
            h: 300,
        });
        req.size = Some((250, 150));
        let plan = plan(&req).expect("valid plan");
        // Scaled to 500x300 on the GPU, so the crop sits at (250,150) of size 250x150.
        assert!(
            plan.filter.contains("crop=250:150:250:150:exact=1"),
            "{}",
            plan.filter
        );
    }

    #[test]
    fn cpu_chain_has_no_download_and_orders_crop_scale_fps_format() {
        let mut req = request();
        req.crop = Some(PixelRect {
            x: 0,
            y: 0,
            w: 640,
            h: 360,
        });
        req.size = Some((320, 180));
        let plan = plan(&req).expect("valid plan");
        assert_eq!(
            plan.filter,
            "crop=640:360:0:0:exact=1,scale=320:180:flags=lanczos,fps=30:round=up:start_time=0,format=nv12"
        );
    }

    #[test]
    fn high_bit_depth_downloads_p010() {
        let mut req = request();
        req.bit_depth = 10;
        req.layout = PixelLayout::P010;
        req.chain = DecodeChain {
            backend: DecodeBackend::D3d11va { dxgi_index: 0 },
            scaler: GpuScaler::None,
        };
        let plan = plan(&req).expect("valid plan");
        assert_eq!(
            plan.filter,
            "hwdownload,format=p010le,fps=30:round=up:start_time=0,format=p010le"
        );
    }

    #[test]
    fn invalid_requests_are_refused_before_any_process_starts() {
        let mut req = request();
        req.crop = Some(PixelRect {
            x: 1,
            y: 0,
            w: 640,
            h: 360,
        });
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));

        let mut req = request();
        req.crop = Some(PixelRect {
            x: 1900,
            y: 0,
            w: 640,
            h: 360,
        });
        assert!(
            matches!(validate(&req), Err(MediaError::InvalidArgument(_))),
            "crop past the edge"
        );

        let mut req = request();
        req.size = Some((321, 180));
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));

        let mut req = request();
        req.range = Some(TimeRange::new(1.0, 1.0));
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));

        let mut req = request();
        req.chain = DecodeChain {
            backend: DecodeBackend::Cpu,
            scaler: GpuScaler::Amf,
        };
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));

        let mut req = request();
        req.layout = PixelLayout::Rgba8;
        req.chain = DecodeChain {
            backend: DecodeBackend::Amf { dxgi_index: 0 },
            scaler: GpuScaler::None,
        };
        assert!(matches!(validate(&req), Err(MediaError::Unsupported(_))));

        let mut req = request();
        req.codec = "vp9".to_owned();
        req.has_alpha = true;
        req.chain = DecodeChain {
            backend: DecodeBackend::D3d11va { dxgi_index: 0 },
            scaler: GpuScaler::None,
        };
        assert!(matches!(validate(&req), Err(MediaError::Unsupported(_))));
    }

    #[test]
    fn rgba_output_tags_the_stream_matrix_before_the_conversion() {
        let mut req = request();
        req.layout = PixelLayout::Rgba8;
        req.color = ColorInfo {
            matrix: loopcut_core::YuvMatrix::Bt709,
            range: loopcut_core::ColorRange::Full,
            transfer: loopcut_core::TransferFunction::Sdr,
            wide_gamut: false,
        };
        let built = plan(&req).expect("valid plan");
        assert!(
            built
                .filter
                .ends_with("setparams=colorspace=bt709:range=pc,format=rgba"),
            "{}",
            built.filter
        );
        // RGB sources have no YUV matrix to tag.
        req.color.matrix = loopcut_core::YuvMatrix::Rgb;
        let built = plan(&req).expect("valid plan");
        assert!(!built.filter.contains("setparams"), "{}", built.filter);
    }

    #[test]
    fn stream_start_is_added_to_the_absolute_seek() {
        let paths = FfmpegPaths {
            ffmpeg: PathBuf::from("ffmpeg.exe"),
            ffprobe: PathBuf::from("ffprobe.exe"),
        };
        let mut req = request();
        req.start_time = 10.0;
        req.range = Some(TimeRange::new(0.5, 1.0));
        let cmd = build_decode_command(&paths, &req).expect("command");
        let args: Vec<String> = cmd
            .args()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|arg| arg == "-ss").expect("-ss");
        assert_eq!(args[at + 1], "10.500000");
        assert!(args.windows(2).any(|pair| pair == ["-seek_timestamp", "1"]));
    }
}

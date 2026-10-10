//! Video encode command builder (RGBA frames on stdin) and encode sessions for every encoder backend.
//!
//! [`build_encode_command`] makes an FFmpeg invocation that reads raw RGBA frames from stdin, converts them to 8-bit
//! BT.709 limited-range video, and encodes them to an MP4 or WebM file. [`EncodeSession`] runs that invocation and
//! feeds it frames.
//!
//! # Conversion and tagging
//!
//! Frames are converted with `scale=out_color_matrix=bt709:out_range=tv` and then tagged with `setparams`. The tag
//! reaches the bitstream for every encoder tested (libx264, libvpx-vp9, libsvtav1, h264_amf, hevc_amf and av1_amf). The
//! `-color_*` output options are also set, but on their own they did not reach the stream for H.264 or VP9 in tests.
//! CPU encoders take `yuv420p` and hardware encoders take `nv12`. AMF and QSV upload the frames to a D3D11 device with
//! `hwupload`.
//!
//! # Quality
//!
//! The 1-100 quality scale maps onto each encoder's own rate control (see [`quality_qp`] and [`quality_crf63`]).
//!
//! # Hardware limits
//!
//! Hardware encoders have minimum frame sizes that depend on the codec and driver. On the RX 9060 XT, `hevc_amf` fails at
//! 320x240 and below, and `av1_amf` fails at 256x256. An encode below the minimum fails with [`MediaError::Failed`] and the stderr
//! tail, so the caller can fall back to a CPU encoder.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ChildStdin;

use loopcut_core::{EncoderBackend, Rational, VideoCodec};

use crate::error::MediaError;
use crate::paths::FfmpegPaths;
use crate::runner::{FfmpegCommand, FfmpegProcess, InputOptions, Progress, SpawnIo};

/// Container format of the encoded file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MP4 (H.264, HEVC and AV1).
    Mp4,
    /// WebM (VP9 and AV1).
    Webm,
}

/// Everything needed to encode one video.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodeRequest {
    /// Output file. FFmpeg writes it directly, so the caller passes a temporary `<name>.partial` path and renames it when
    /// finished. The builder passes `-y`, so only such a path may be given: it is the only file FFmpeg may overwrite.
    pub output: PathBuf,
    /// Frame width in pixels. Must be even.
    pub width: u32,
    /// Frame height in pixels. Must be even.
    pub height: u32,
    /// Frame rate of the frames written with [`EncodeSession::write_frame`].
    pub fps: Rational,
    /// Video codec.
    pub codec: VideoCodec,
    /// Encoder implementation and, for hardware, its device index.
    pub backend: EncoderBackend,
    /// Quality from 1 (smallest) to 100 (best). Mapped to each encoder's rate control.
    pub quality: u8,
    /// Container. H.264 and HEVC need MP4, VP9 needs WebM, and AV1 takes either.
    pub container: Container,
    /// Keyframe interval in frames. `None` uses one keyframe per second.
    pub keyint: Option<u32>,
}

/// Totals of a finished encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeStats {
    /// Frames written to the encoder.
    pub frames: u64,
    /// Size of the output file in bytes.
    pub bytes: u64,
}

/// Quality knots for the 1-100 scale mapped onto the 0-51 QP/CRF range of H.264, HEVC and the hardware encoders.
/// Each entry is `(quality, value)`; values between knots are interpolated linearly.
const QP_KNOTS: [(f64, f64); 3] = [(1.0, 40.0), (50.0, 26.0), (100.0, 16.0)];

/// Quality knots for the 0-63 CRF range of libsvtav1 and libvpx-vp9.
const CRF63_KNOTS: [(f64, f64); 3] = [(1.0, 50.0), (50.0, 36.0), (100.0, 24.0)];

/// Piecewise-linear interpolation through `knots`, which are sorted by their first component.
fn interpolate(knots: &[(f64, f64)], x: f64) -> f64 {
    for segment in knots.windows(2) {
        if let &[(x0, y0), (x1, y1)] = segment
            && x <= x1
        {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    knots.last().map_or(0.0, |&(_, y)| y)
}

/// The H.264, HEVC, NVENC, AMF and QSV quantizer for a quality from 1 to 100. Quality 100 gives 16 and quality 50 gives
/// 26, which tracks x264's visually lossless range. Quality 1 gives 40.
pub fn quality_qp(quality: u8) -> u32 {
    interpolate(&QP_KNOTS, f64::from(quality)).round() as u32
}

/// The libsvtav1 and libvpx-vp9 CRF for a quality from 1 to 100 (0 to 63 scale). Quality 100 gives 24 and quality 1
/// gives 50.
pub fn quality_crf63(quality: u8) -> u32 {
    interpolate(&CRF63_KNOTS, f64::from(quality)).round() as u32
}

/// Builds the encode command for `req`: RGBA frames on stdin, one encoded file at `req.output`.
///
/// # Errors
///
/// [`MediaError::Unsupported`] when the backend has no encoder for the codec, or the container cannot hold the codec.
/// [`MediaError::InvalidArgument`] for odd or zero dimensions, an invalid frame rate, a quality outside 1-100, or a zero
/// keyframe interval, or an output that is not a `.partial` file. [`MediaError::UrlNotAllowed`] if the output path is URL-like.
pub fn build_encode_command(
    paths: &FfmpegPaths,
    req: &EncodeRequest,
) -> Result<FfmpegCommand, MediaError> {
    let plan = plan(req)?;
    let input = InputOptions {
        format: Some("rawvideo".to_owned()),
        extra: vec![
            "-pix_fmt".to_owned(),
            "rgba".to_owned(),
            "-s".to_owned(),
            format!("{}x{}", req.width, req.height),
            "-r".to_owned(),
            req.fps.to_string(),
        ],
        ..InputOptions::default()
    };
    FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")?
        .global_args(plan.global)?
        .input_stdin(&input)?
        .output_args(plan.output)?
        .output(&req.output)
}

/// A running encode. Write frames with [`Self::write_frame`], then call [`Self::finish`] or [`Self::cancel`].
///
/// Dropping a session that did not finish successfully kills the encoder and deletes the partial output, so a failed
/// or abandoned encode never leaves a file behind.
pub struct EncodeSession {
    process: Option<FfmpegProcess>,
    stdin: Option<ChildStdin>,
    output: PathBuf,
    frame_bytes: usize,
    frames: u64,
    /// Set once the encoder has exited successfully; only then is the output kept.
    complete: bool,
}

impl EncodeSession {
    /// Starts an encode. `progress` receives FFmpeg's progress snapshots on a background thread.
    ///
    /// # Errors
    ///
    /// Any error from [`build_encode_command`], or [`MediaError::Spawn`] if FFmpeg cannot start. Failures inside the
    /// encoder (an unsupported frame size, a missing device) appear from [`Self::write_frame`] or [`Self::finish`].
    pub fn start(
        paths: &FfmpegPaths,
        req: &EncodeRequest,
        progress: Option<Box<dyn FnMut(Progress) + Send>>,
    ) -> Result<Self, MediaError> {
        let mut command = build_encode_command(paths, req)?;
        if let Some(callback) = progress {
            command = command.on_progress(callback);
        }
        let mut process = command.spawn(SpawnIo {
            stdin: true,
            stdout: false,
        })?;
        let stdin = process
            .take_stdin()
            .ok_or_else(|| MediaError::Io(io::Error::other("encoder stdin pipe is missing")))?;
        Ok(Self {
            process: Some(process),
            stdin: Some(stdin),
            output: req.output.clone(),
            frame_bytes: rgba_frame_bytes(req.width, req.height),
            frames: 0,
            complete: false,
        })
    }

    /// Frames accepted so far.
    pub fn frames_written(&self) -> u64 {
        self.frames
    }

    /// Writes one RGBA frame (`width * height * 4` bytes, straight alpha, sRGB).
    ///
    /// # Errors
    ///
    /// [`MediaError::InvalidArgument`] if `rgba` has the wrong length. If the encoder has stopped (for example because
    /// the frame size is below its minimum), returns the encoder's error with its stderr tail.
    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<(), MediaError> {
        if rgba.len() != self.frame_bytes {
            return Err(MediaError::InvalidArgument(format!(
                "frame is {} bytes, expected {}",
                rgba.len(),
                self.frame_bytes
            )));
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(MediaError::Io(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the encoder input is already closed",
            )));
        };
        match stdin.write_all(rgba) {
            Ok(()) => {
                self.frames += 1;
                Ok(())
            }
            Err(err) => Err(self.stopped(err)),
        }
    }

    /// Ends the input, waits for the encoder and returns the totals.
    ///
    /// # Errors
    ///
    /// [`MediaError::Failed`] (with the stderr tail) if the encoder exits unsuccessfully, or an I/O error if the output
    /// cannot be read back.
    pub fn finish(mut self) -> Result<EncodeStats, MediaError> {
        // Closing stdin tells the encoder the stream has ended. It must be closed before waiting.
        self.stdin = None;
        let process = self
            .process
            .take()
            .ok_or_else(|| MediaError::Io(io::Error::other("encoder was already finished")))?;
        // On failure the session is dropped on the way out, which deletes the partial output.
        process.wait()?;
        let bytes = fs::metadata(&self.output)?.len();
        self.complete = true;
        Ok(EncodeStats {
            frames: self.frames,
            bytes,
        })
    }

    /// Stops the encode, kills FFmpeg and deletes the partial output file.
    pub fn cancel(self) {
        // Dropping the session kills the encoder, waits for it, and deletes the partial output.
        drop(self);
    }

    /// The encoder stopped while a frame was being written. Waits for it to report why.
    fn stopped(&mut self, err: io::Error) -> MediaError {
        self.stdin = None;
        match self.process.take() {
            Some(process) => match process.wait() {
                Err(failure) => failure,
                Ok(_) => MediaError::Io(err),
            },
            None => MediaError::Io(err),
        }
    }
}

impl Drop for EncodeSession {
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(mut process) = self.process.take() {
            process.kill();
            // Waiting makes sure FFmpeg has released the file before it is deleted.
            let _ = process.wait();
        }
        if !self.complete {
            // Only the caller's partial file is ever removed, and only when the encode did not complete.
            if let Err(err) = fs::remove_file(&self.output)
                && err.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(path = %self.output.display(), %err, "could not delete partial encode");
            }
        }
    }
}

/// Bytes in one RGBA frame.
fn rgba_frame_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// Default keyframe interval: one per second, at least one frame.
fn default_keyint(fps: Rational) -> u32 {
    (fps.as_f64().round() as u32).max(1)
}

/// The encoder command pieces for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    /// Global options (device creation for AMF and QSV, and `-y` is added by the builder).
    global: Vec<String>,
    /// Output options: the filter graph, the encoder and the container.
    output: Vec<String>,
}

/// Checks the request and derives the command pieces.
fn plan(req: &EncodeRequest) -> Result<Plan, MediaError> {
    validate(req)?;
    let encoder = req.backend.ffmpeg_encoder(req.codec).ok_or_else(|| {
        MediaError::Unsupported(format!(
            "{:?} has no encoder for {:?}",
            req.backend, req.codec
        ))
    })?;
    check_container(req.codec, req.container)?;
    let keyint = req.keyint.unwrap_or_else(|| default_keyint(req.fps));
    let qp = quality_qp(req.quality);
    let crf63 = quality_crf63(req.quality);
    let mp4 = req.container == Container::Mp4;

    let mut global = Vec::new();
    let (filter, mut args) = match req.backend {
        EncoderBackend::Cpu => {
            let filter = convert_filter("yuv420p", "");
            let args = match req.codec {
                VideoCodec::H264 => words(&["-crf", &qp.to_string(), "-bf", "0"]),
                VideoCodec::Hevc => {
                    let mut args = words(&["-crf", &qp.to_string(), "-x265-params", "bframes=0"]);
                    args.extend(tag_hvc1(mp4));
                    args
                }
                VideoCodec::Av1 => words(&["-crf", &crf63.to_string(), "-preset", "8"]),
                VideoCodec::Vp9 => words(&[
                    "-crf",
                    &crf63.to_string(),
                    "-b:v",
                    "0",
                    "-row-mt",
                    "1",
                    "-deadline",
                    "good",
                    "-cpu-used",
                    "2",
                ]),
            };
            (filter, args)
        }
        EncoderBackend::Nvenc { cuda_index } => {
            let filter = convert_filter("nv12", "");
            let mut args = words(&[
                "-preset",
                "p7",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-cq",
                &qp.to_string(),
                "-b:v",
                "0",
                "-bf",
                "0",
                "-gpu",
                &cuda_index.to_string(),
            ]);
            if req.codec == VideoCodec::Hevc {
                args.extend(tag_hvc1(mp4));
            }
            (filter, args)
        }
        EncoderBackend::Amf { dxgi_index } => {
            // AMF has no device option, so frames are uploaded to a D3D11 device chosen by index.
            global = words(&[
                "-init_hw_device",
                &format!("d3d11va=dx:{dxgi_index}"),
                "-filter_hw_device",
                "dx",
            ]);
            let filter = convert_filter("nv12", ",hwupload");
            let args = match req.codec {
                VideoCodec::H264 => amf_args(qp, 51, true),
                VideoCodec::Hevc => {
                    let mut args = amf_args(qp, 51, false);
                    args.extend(tag_hvc1(mp4));
                    args
                }
                // AV1 quantizers run 0-255 at about five times the H.264 scale.
                _ => amf_args((qp * 5).min(255), 255, true),
            };
            (filter, args)
        }
        EncoderBackend::Qsv { dxgi_index } => {
            global = words(&[
                "-init_hw_device",
                &format!("qsv=hw,child_device_type=d3d11va,child_device={dxgi_index}"),
                "-filter_hw_device",
                "hw",
            ]);
            let filter = convert_filter("nv12", ",hwupload=extra_hw_frames=64,format=qsv");
            // AV1 global quality runs on the 0-255 scale, like AMF's AV1 QP. Unverified on this machine (no Intel GPU).
            let value = if req.codec == VideoCodec::Av1 {
                (qp * 5).min(255)
            } else {
                qp
            };
            let mut args = words(&["-preset", "veryslow", "-global_quality", &value.to_string()]);
            match req.codec {
                VideoCodec::H264 => args.extend(words(&["-look_ahead", "1", "-bf", "0"])),
                VideoCodec::Hevc => args.extend(tag_hvc1(mp4)),
                VideoCodec::Av1 | VideoCodec::Vp9 => {}
            }
            (filter, args)
        }
    };

    let mut output = vec![
        "-vf".to_owned(),
        filter,
        "-c:v".to_owned(),
        encoder.to_owned(),
    ];
    output.append(&mut args);
    output.extend(words(&["-g", &keyint.to_string()]));
    // The container and stream tags. The filter's `setparams` already tags the frames, which is what reaches the
    // bitstream for every encoder, so these options only add the muxer-level description.
    output.extend(words(&[
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-colorspace",
        "bt709",
        "-color_range",
        "tv",
        "-an",
    ]));
    match req.container {
        Container::Mp4 => output.extend(words(&["-f", "mp4", "-movflags", "+faststart"])),
        Container::Webm => output.extend(words(&["-f", "webm"])),
    }
    Ok(Plan { global, output })
}

/// The shared conversion stage: RGBA to `format`, tagged BT.709 limited range, then `tail` (for example `hwupload`).
fn convert_filter(format: &str, tail: &str) -> String {
    format!(
        "scale=out_color_matrix=bt709:out_range=tv,format={format},\
         setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv{tail}"
    )
}

/// AMF constant-QP options. `max_qp` is the largest quantizer the codec accepts. `b_frames` says whether `-bf 0` is
/// valid, which it is not for `hevc_amf`.
fn amf_args(qp: u32, max_qp: u32, b_frames: bool) -> Vec<String> {
    let p = (qp + 2).min(max_qp);
    let mut args = words(&[
        "-quality",
        "quality",
        "-usage",
        "transcoding",
        "-rc",
        "cqp",
        "-qp_i",
        &qp.to_string(),
        "-qp_p",
        &p.to_string(),
    ]);
    if b_frames {
        args.extend(words(&["-bf", "0"]));
    }
    args
}

/// `-tag:v hvc1` for HEVC in MP4, so that Apple and browser players accept it.
fn tag_hvc1(mp4: bool) -> Vec<String> {
    if mp4 {
        words(&["-tag:v", "hvc1"])
    } else {
        Vec::new()
    }
}

/// Rejects codec and container pairs that cannot be muxed.
fn check_container(codec: VideoCodec, container: Container) -> Result<(), MediaError> {
    match (codec, container) {
        (VideoCodec::H264 | VideoCodec::Hevc, Container::Webm) => Err(MediaError::Unsupported(
            format!("{codec:?} needs an MP4 container, not WebM"),
        )),
        (VideoCodec::Vp9, Container::Mp4) => Err(MediaError::Unsupported(
            "VP9 needs a WebM container, not MP4".to_owned(),
        )),
        _ => Ok(()),
    }
}

/// Rejects requests that cannot be encoded, before any process starts.
fn validate(req: &EncodeRequest) -> Result<(), MediaError> {
    if !is_partial_output(&req.output) {
        return Err(MediaError::InvalidArgument(format!(
            "encode output {} must be a .partial file: the encoder overwrites its output with -y",
            req.output.display()
        )));
    }
    if req.width == 0
        || req.height == 0
        || !req.width.is_multiple_of(2)
        || !req.height.is_multiple_of(2)
    {
        return Err(MediaError::InvalidArgument(format!(
            "frame size {}x{} must be non-zero and even",
            req.width, req.height
        )));
    }
    if !req.fps.is_valid() {
        return Err(MediaError::InvalidArgument(format!(
            "invalid frame rate {}",
            req.fps
        )));
    }
    if !(1..=100).contains(&req.quality) {
        return Err(MediaError::InvalidArgument(format!(
            "quality must be 1-100, got {}",
            req.quality
        )));
    }
    if req.keyint == Some(0) {
        return Err(MediaError::InvalidArgument(
            "keyframe interval must be at least 1 frame".to_owned(),
        ));
    }
    Ok(())
}

/// True for the caller's temporary output names: `<name>.partial` or `<name>.partial.<ext>`.
///
/// The builder passes `-y`, so FFmpeg replaces whatever is at the output path. Only a file the caller has set aside as
/// partial may be that path. A final destination is never overwritten silently.
fn is_partial_output(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".partial"))
}

/// Literal arguments as owned strings. Values computed at run time are passed as `&String`s.
fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> EncodeRequest {
        EncodeRequest {
            output: PathBuf::from("out.partial.mp4"),
            width: 640,
            height: 360,
            fps: Rational::integer(30),
            codec: VideoCodec::H264,
            backend: EncoderBackend::Cpu,
            quality: 80,
            container: Container::Mp4,
            keyint: None,
        }
    }

    #[test]
    fn quality_mapping_hits_the_documented_points() {
        assert_eq!(quality_qp(100), 16);
        assert_eq!(quality_qp(50), 26);
        assert_eq!(quality_qp(1), 40);
        assert_eq!(quality_crf63(100), 24);
        assert_eq!(quality_crf63(1), 50);
        // Monotonic: better quality never means a larger quantizer.
        let mut last = u32::MAX;
        for q in 1..=100 {
            let qp = quality_qp(q);
            assert!(qp <= last, "quality {q}");
            last = qp;
        }
    }

    #[test]
    fn interpolation_is_linear_between_knots() {
        assert!((interpolate(&QP_KNOTS, 25.5) - 33.0).abs() < 1e-9);
        assert_eq!(interpolate(&QP_KNOTS, 500.0), 16.0);
    }

    #[test]
    fn default_keyint_is_one_second() {
        assert_eq!(default_keyint(Rational::new(30000, 1001)), 30);
        assert_eq!(default_keyint(Rational::new(1, 2)), 1);
    }

    #[test]
    fn codec_and_container_must_match() {
        let mut req = request();
        req.codec = VideoCodec::Hevc;
        req.container = Container::Webm;
        assert!(matches!(plan(&req), Err(MediaError::Unsupported(_))));

        let mut req = request();
        req.codec = VideoCodec::Vp9;
        assert!(matches!(plan(&req), Err(MediaError::Unsupported(_))));

        let mut req = request();
        req.codec = VideoCodec::Av1;
        req.container = Container::Webm;
        assert!(plan(&req).is_ok());
    }

    #[test]
    fn encoders_that_lack_a_codec_are_unsupported() {
        let mut req = request();
        req.codec = VideoCodec::Vp9;
        req.backend = EncoderBackend::Nvenc { cuda_index: 0 };
        req.container = Container::Webm;
        assert!(matches!(plan(&req), Err(MediaError::Unsupported(_))));
    }

    #[test]
    fn invalid_requests_are_refused() {
        let mut req = request();
        req.width = 641;
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));

        let mut req = request();
        req.quality = 0;
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));

        let mut req = request();
        req.keyint = Some(0);
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));
    }

    #[test]
    fn only_partial_outputs_may_be_overwritten() {
        let mut req = request();
        req.output = PathBuf::from("C:/clips/final.mp4");
        assert!(matches!(
            validate(&req),
            Err(MediaError::InvalidArgument(_))
        ));
        req.output = PathBuf::from("C:/clips/final.mp4.partial");
        assert!(validate(&req).is_ok());
        req.output = PathBuf::from("C:/clips/final.partial.mp4");
        assert!(validate(&req).is_ok());
    }

    #[test]
    fn conversion_tags_bt709_limited_range() {
        let filter = convert_filter("yuv420p", "");
        assert!(filter.starts_with("scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,"));
        assert!(
            filter.contains(
                "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
            )
        );
    }
}

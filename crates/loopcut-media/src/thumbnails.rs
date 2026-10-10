//! Filmstrip thumbnail extraction as small RGBA frames.
//!
//! Sample `k` is the frame at `k * duration / count` on the stream's timeline, which starts at
//! [`ThumbnailRequest::start_time`]. Two strategies produce the samples:
//!
//! * Requests up to [`SINGLE_PASS_MAX_SECONDS`] long decode once. `setpts=PTS-STARTPTS` moves the stream start to zero,
//!   an `fps` filter on the exact grid `count / duration` (anchored at zero, `round=up`) picks the samples, and
//!   `-frames:v` stops FFmpeg after the last one. Each sample is the last frame at or before its instant.
//! * Longer requests run one accurate absolute seek per thumbnail, with at most [`SEEK_WORKERS`] FFmpeg processes at a
//!   time. Each sample is the first frame at or after its target.
//!
//! The two rules agree when a sample falls exactly on a frame, which covers every sample of an integer frame step. In
//! between they can differ by one frame interval.
//!
//! Both paths scale with lanczos, convert YUV to RGBA with the stream's matrix and range pinned (HDR and wide-gamut
//! sources go through the proxy's color chain first), and apply the clockwise rotation themselves. The builder always
//! passes `-noautorotate`, so the stored frames are never rotated implicitly.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use loopcut_core::ColorInfo;

use crate::error::{MediaError, MediaResult};
use crate::paths::FfmpegPaths;
use crate::proxy::{
    converted_yuv_tags, scale_input_options, sdr_conversion_filter, source_yuv_tags, tag_filter,
};
use crate::runner::{CancelToken, FfmpegCommand, InputOptions, SpawnIo};

/// Inputs at most this long are sampled in one decoding pass; longer ones use parallel seeks.
pub const SINGLE_PASS_MAX_SECONDS: f64 = 120.0;

/// Maximum number of seek processes running at once for long inputs.
pub const SEEK_WORKERS: usize = 4;

/// Largest number of thumbnails one request may ask for.
pub const MAX_THUMBNAILS: u32 = 512;

/// Largest displayed thumbnail width in pixels.
pub const MAX_THUMBNAIL_WIDTH: u32 = 8192;

/// Largest displayed thumbnail height in pixels.
pub const MAX_THUMBNAIL_HEIGHT: u32 = 4320;

/// Largest total RGBA size of one request. Every frame is held in memory until the call returns.
pub const MAX_THUMBNAIL_BYTES: u64 = 256 * 1024 * 1024;

/// Upper bound for the single decoding pass.
const SINGLE_PASS_TIMEOUT: Duration = Duration::from_secs(900);

/// Upper bound for one seek process, which decodes at most one GOP plus one frame.
const SEEK_TIMEOUT: Duration = Duration::from_secs(120);

/// How often the seek coordinator forwards the caller's cancellation to the in-flight seeks.
const CANCEL_POLL: Duration = Duration::from_millis(10);

/// One sampled frame, scaled and rotated for display.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    /// Source seconds at which the frame was sampled (`index * duration / count`), measured from the stream start.
    pub time: f64,
    /// Displayed width in pixels (always even).
    pub width: u32,
    /// Displayed height in pixels (always even).
    pub height: u32,
    /// Tightly packed RGBA8 pixels, `width * height * 4` bytes, upright.
    pub rgba: Vec<u8>,
}

/// What to sample and how to scale it.
#[derive(Debug, Clone, PartialEq)]
pub struct ThumbnailRequest {
    /// The local media file.
    pub input: PathBuf,
    /// Absolute container index of the video stream (`-map 0:<index>`).
    pub stream_index: u32,
    /// Stream start in seconds (absolute PTS), as used by [`crate::keyframes::keyframe_index`]. Any finite value is
    /// accepted; seeks before the start of the file clamp to zero.
    pub start_time: f64,
    /// Seconds spanned by the samples, measured from the stream start.
    pub duration: f64,
    /// Number of samples, at most [`MAX_THUMBNAILS`].
    pub count: u32,
    /// Displayed thumbnail height. The width is `height * display_aspect`, rounded to an even number.
    pub height: u32,
    /// Clockwise rotation (0, 90, 180 or 270) that makes the stored frames upright.
    pub rotation_cw: u16,
    /// Upright width divided by upright height (see [`loopcut_core::VideoStreamInfo::display_size`]).
    pub display_aspect: f64,
    /// Color description of the stream, from [`loopcut_core::VideoStreamInfo::color`].
    pub color: ColorInfo,
}

/// Generates the thumbnails described by `req`, evenly spaced over `req.duration` seconds of the stream.
///
/// Thumbnails are returned in time order. A sample past the end of the stream is omitted, so the result can be shorter
/// than `count`.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] for a non-positive or non-finite `duration`, a non-finite `start_time`, a zero or
/// too large `height`, a non-finite or non-positive `display_aspect`, an unsupported rotation, more than
/// [`MAX_THUMBNAILS`] samples, a frame wider than [`MAX_THUMBNAIL_WIDTH`] or taller than [`MAX_THUMBNAIL_HEIGHT`], or
/// a total larger than [`MAX_THUMBNAIL_BYTES`]. [`MediaError::Cancelled`] when `cancel` is set, and
/// [`MediaError::Failed`] or [`MediaError::Timeout`] from FFmpeg.
pub fn generate_thumbnails(
    paths: &FfmpegPaths,
    req: &ThumbnailRequest,
    cancel: CancelToken,
) -> MediaResult<Vec<Thumbnail>> {
    run(paths, req, SINGLE_PASS_MAX_SECONDS, &cancel)
}

/// Output geometry and sampling step for one request.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Plan {
    /// Displayed (upright) width.
    width: u32,
    /// Displayed (upright) height.
    height: u32,
    rotation_cw: u16,
    /// Seconds between consecutive samples (`duration / count`), or zero when there are no samples.
    step: f64,
    /// Number of samples.
    count: u32,
}

impl Plan {
    fn frame_bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// Shared implementation with the single-pass threshold as a parameter, so tests can force the seek path.
fn run(
    paths: &FfmpegPaths,
    req: &ThumbnailRequest,
    single_pass_max: f64,
    cancel: &CancelToken,
) -> MediaResult<Vec<Thumbnail>> {
    let plan = plan(req)?;
    if plan.count == 0 {
        return Ok(Vec::new());
    }
    if req.duration <= single_pass_max {
        single_pass(paths, req, plan, cancel)
    } else {
        seek_pass(paths, req, plan, cancel)
    }
}

/// Checks the request and derives its geometry. Every limit is checked before any allocation or process starts.
fn plan(req: &ThumbnailRequest) -> MediaResult<Plan> {
    let invalid = |message: String| MediaError::InvalidArgument(message);
    if !(req.duration.is_finite() && req.duration > 0.0) {
        return Err(invalid(format!(
            "thumbnail duration must be finite and positive, got {}",
            req.duration
        )));
    }
    if !req.start_time.is_finite() {
        return Err(invalid(format!(
            "thumbnail start_time must be finite, got {}",
            req.start_time
        )));
    }
    if req.height == 0 {
        return Err(invalid("thumbnail height must be positive".to_owned()));
    }
    if !(req.display_aspect.is_finite() && req.display_aspect > 0.0) {
        return Err(invalid(format!(
            "display aspect must be finite and positive, got {}",
            req.display_aspect
        )));
    }
    if !matches!(req.rotation_cw, 0 | 90 | 180 | 270) {
        return Err(invalid(format!(
            "rotation must be 0, 90, 180 or 270 degrees, got {}",
            req.rotation_cw
        )));
    }
    if req.count > MAX_THUMBNAILS {
        return Err(invalid(format!(
            "at most {MAX_THUMBNAILS} thumbnails per request, got {}",
            req.count
        )));
    }
    let height = (req.height & !1).max(2);
    if height > MAX_THUMBNAIL_HEIGHT {
        return Err(invalid(format!(
            "thumbnail height {height} exceeds {MAX_THUMBNAIL_HEIGHT}"
        )));
    }
    let width_exact = (f64::from(height) * req.display_aspect).round();
    if !(width_exact.is_finite() && width_exact <= f64::from(MAX_THUMBNAIL_WIDTH)) {
        return Err(invalid(format!(
            "thumbnail width for aspect {} exceeds {MAX_THUMBNAIL_WIDTH}",
            req.display_aspect
        )));
    }
    // `as` saturates for out-of-range values, and the bound above keeps this in range.
    let width = ((width_exact as u32) & !1).max(2);
    let total = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| bytes.checked_mul(u64::from(req.count)))
        .ok_or_else(|| invalid("thumbnail size overflows".to_owned()))?;
    if total > MAX_THUMBNAIL_BYTES {
        return Err(invalid(format!(
            "thumbnails total {total} bytes, more than the {MAX_THUMBNAIL_BYTES} byte limit"
        )));
    }
    let step = if req.count > 0 {
        req.duration / f64::from(req.count)
    } else {
        0.0
    };
    Ok(Plan {
        width,
        height,
        rotation_cw: req.rotation_cw,
        step,
        count: req.count,
    })
}

/// The FFmpeg filter that rotates a stored frame clockwise, or `None` for no rotation.
///
/// `transpose=1` is 90° clockwise, `transpose=2` is 90° counter-clockwise (270° clockwise), and 180° is a flip on both
/// axes. Callers validate the angle first, so any other value yields no filter.
pub(crate) fn rotation_filter(rotation_cw: u16) -> Option<&'static str> {
    match rotation_cw {
        90 => Some("transpose=1"),
        180 => Some("hflip,vflip"),
        270 => Some("transpose=2"),
        _ => None,
    }
}

/// The per-frame chain after sampling: scale to the stored orientation, convert to RGBA with the stream's color, rotate
/// and emit RGBA.
///
/// The scale reads the source's matrix and range, so the YUV input is interpreted correctly even when the stream is
/// untagged. Wide-gamut and HDR sources then go through the proxy's SDR conversion (tagged BT.709 afterwards), and the
/// RGBA conversion is explicit, so it cannot pick up whatever tags the decoder left behind. The `scale` goes first:
/// HDR tonemapping then works on the small frame.
fn color_chain(req: &ThumbnailRequest, plan: Plan) -> String {
    let (scale_w, scale_h) = if matches!(plan.rotation_cw, 90 | 270) {
        (plan.height, plan.width)
    } else {
        (plan.width, plan.height)
    };
    let mut filters: Vec<String> = Vec::with_capacity(5);
    filters.push(format!(
        "scale={scale_w}:{scale_h}:flags=lanczos{}",
        scale_input_options(source_yuv_tags(req.color))
    ));
    if let Some(conversion) = sdr_conversion_filter(req.color) {
        filters.push(conversion);
    }
    if let Some(tag) = tag_filter(converted_yuv_tags(req.color)) {
        filters.push(tag);
    }
    if let Some(rotation) = rotation_filter(plan.rotation_cw) {
        filters.push(rotation.to_owned());
    }
    filters.push("format=rgba".to_owned());
    filters.join(",")
}

/// Output options that select the stream and emit `frames` RGBA frames on stdout.
fn output_args(stream_index: u32, video_filter: &str, frames: u32) -> Vec<String> {
    vec![
        "-map".to_owned(),
        format!("0:{stream_index}"),
        "-vf".to_owned(),
        video_filter.to_owned(),
        "-frames:v".to_owned(),
        frames.to_string(),
        "-an".to_owned(),
        "-sn".to_owned(),
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "-pix_fmt".to_owned(),
        "rgba".to_owned(),
    ]
}

/// The sampler: the stream starts at zero, and one grid point per sample lies `duration / count` apart.
///
/// The rate is the exact rational `count * 1e6 / round(duration * 1e6)`, so a request for 4 samples over 2.8 s uses
/// 10/7 fps and lands on frames 0, 7, 14 and 21 of a 10 fps stream. `round=up` gives each grid point the last frame at or
/// before it. The seek path takes the first frame at or after its target, so the two rules differ between frames.
fn sampler(req: &ThumbnailRequest) -> MediaResult<String> {
    let numerator = u64::from(req.count) * 1_000_000;
    let denominator = (req.duration * 1_000_000.0).round() as u64;
    // Only requests within the single-pass limit reach here; FFmpeg reads the rational as 32-bit integers.
    if denominator == 0 || denominator > i32::MAX as u64 {
        return Err(MediaError::InvalidArgument(format!(
            "duration {} is outside the single-pass range",
            req.duration
        )));
    }
    Ok(format!(
        "setpts=PTS-STARTPTS,fps={numerator}/{denominator}:round=up:start_time=0"
    ))
}

/// Decodes the stream once from the start, sampling `count` frames with the `fps` filter.
fn single_pass(
    paths: &FfmpegPaths,
    req: &ThumbnailRequest,
    plan: Plan,
    cancel: &CancelToken,
) -> MediaResult<Vec<Thumbnail>> {
    let video_filter = format!("{},{}", sampler(req)?, color_chain(req, plan));
    let mut process = FfmpegCommand::new_ffmpeg(paths)
        .input(&req.input, &InputOptions::default())?
        .output_args(output_args(req.stream_index, &video_filter, plan.count))?
        .output_stdout()
        .spawn(SpawnIo {
            stdin: false,
            stdout: true,
        })?;
    let mut stdout = process
        .take_stdout()
        .ok_or_else(|| MediaError::Io(io::Error::other("stdout pipe is missing")))?;
    // Drain on a helper thread so FFmpeg never blocks on a full pipe while the wait below polls for exit.
    let drain = thread::Builder::new()
        .name("thumbnail-decode".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })?;
    process.wait_cancellable(cancel, Some(SINGLE_PASS_TIMEOUT))?;
    let bytes = drain
        .join()
        .map_err(|_| MediaError::Io(io::Error::other("thumbnail reader panicked")))??;
    let frame_bytes = plan.frame_bytes();
    Ok(bytes
        .chunks_exact(frame_bytes)
        .take(plan.count as usize)
        .enumerate()
        .map(|(index, rgba)| Thumbnail {
            time: index as f64 * plan.step,
            width: plan.width,
            height: plan.height,
            rgba: rgba.to_vec(),
        })
        .collect())
}

/// Samples each thumbnail with its own input seek, running up to [`SEEK_WORKERS`] processes at once.
///
/// The first worker that fails cancels a per-call token, which stops every in-flight seek. The caller's token is
/// forwarded to that per-call token while the workers run, so cancellation takes effect within one poll interval.
fn seek_pass(
    paths: &FfmpegPaths,
    req: &ThumbnailRequest,
    plan: Plan,
    cancel: &CancelToken,
) -> MediaResult<Vec<Thumbnail>> {
    let total = plan.count as usize;
    let workers = SEEK_WORKERS.min(total).max(1);
    let next = AtomicUsize::new(0);
    let call = CancelToken::new();
    let outcomes = thread::scope(|scope| {
        let handles: Vec<io::Result<thread::ScopedJoinHandle<'_, _>>> = (0..workers)
            .map(|worker| {
                thread::Builder::new()
                    .name(format!("thumbnail-seek-{worker}"))
                    .spawn_scoped(scope, || {
                        seek_worker(paths, req, plan, &next, &call, cancel)
                    })
            })
            .collect();
        if handles.iter().any(Result::is_err) {
            call.cancel();
        }
        while handles
            .iter()
            .any(|handle| handle.as_ref().is_ok_and(|handle| !handle.is_finished()))
        {
            if cancel.is_cancelled() {
                call.cancel();
            }
            thread::sleep(CANCEL_POLL);
        }
        handles
            .into_iter()
            .map(|handle| match handle {
                Ok(handle) => handle.join().unwrap_or_else(|_| {
                    Err(MediaError::Io(io::Error::other(
                        "thumbnail worker panicked",
                    )))
                }),
                Err(err) => Err(MediaError::Io(err)),
            })
            .collect::<Vec<_>>()
    });

    let mut frames: Vec<Option<Vec<u8>>> = vec![None; total];
    let mut first_error = None;
    let mut cancelled = false;
    for outcome in outcomes {
        match outcome {
            Ok(done) => {
                for (index, rgba) in done {
                    frames[index] = rgba;
                }
            }
            // Cancelled workers were stopped by a failure elsewhere or by the caller; the real failure wins.
            Err(MediaError::Cancelled) => cancelled = true,
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    if let Some(err) = first_error {
        return Err(err);
    }
    if cancelled {
        return Err(MediaError::Cancelled);
    }
    Ok(frames
        .into_iter()
        .enumerate()
        .filter_map(|(index, rgba)| {
            rgba.map(|rgba| Thumbnail {
                time: index as f64 * plan.step,
                width: plan.width,
                height: plan.height,
                rgba,
            })
        })
        .collect())
}

/// Claims sample indices until none remain, seeking for each one. Stops early when `call` is cancelled.
///
/// Returns each claimed index with its frame, or `None` when the sample lies past the end of the stream.
fn seek_worker(
    paths: &FfmpegPaths,
    req: &ThumbnailRequest,
    plan: Plan,
    next: &AtomicUsize,
    call: &CancelToken,
    cancel: &CancelToken,
) -> MediaResult<Vec<(usize, Option<Vec<u8>>)>> {
    let total = plan.count as usize;
    let mut done = Vec::new();
    loop {
        if cancel.is_cancelled() || call.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let index = next.fetch_add(1, Ordering::Relaxed);
        if index >= total {
            return Ok(done);
        }
        match seek_one(paths, req, plan, index, call) {
            Ok(rgba) => done.push((index, rgba)),
            Err(err) => {
                call.cancel();
                return Err(err);
            }
        }
    }
}

/// Seeks to sample `index` and returns its single frame, or `None` if the stream ends before the sample.
///
/// `-seek_timestamp` makes the seek absolute, so `start_time + time` is a PTS and not an offset from the container start.
/// The seek is accurate (FFmpeg's default): `-noaccurate_seek` would return a frame up to a whole GOP before the sample
/// time, which would misplace the thumbnail's time label. A target before the start of the file clamps to zero.
fn seek_one(
    paths: &FfmpegPaths,
    req: &ThumbnailRequest,
    plan: Plan,
    index: usize,
    cancel: &CancelToken,
) -> MediaResult<Option<Vec<u8>>> {
    let time = index as f64 * plan.step;
    let target = req.start_time + time;
    // Also normalizes -0.0, which FFmpeg would print as a negative seek.
    let seek = if target > 0.0 { target } else { 0.0 };
    let input_options = InputOptions {
        seek: Some(seek),
        extra: vec!["-seek_timestamp".to_owned(), "1".to_owned()],
        ..InputOptions::default()
    };
    let mut process = FfmpegCommand::new_ffmpeg(paths)
        .input(&req.input, &input_options)?
        .output_args(output_args(req.stream_index, &color_chain(req, plan), 1))?
        .output_stdout()
        .spawn(SpawnIo {
            stdin: false,
            stdout: true,
        })?;
    let mut stdout = process
        .take_stdout()
        .ok_or_else(|| MediaError::Io(io::Error::other("stdout pipe is missing")))?;
    // Drain on a separate thread so FFmpeg never blocks on a full pipe while the wait below polls for exit.
    let reader = thread::Builder::new()
        .name("thumbnail-frame".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })?;
    process.wait_cancellable(cancel, Some(SEEK_TIMEOUT))?;
    let mut bytes = reader
        .join()
        .map_err(|_| MediaError::Io(io::Error::other("thumbnail reader panicked")))??;
    let frame_bytes = plan.frame_bytes();
    if bytes.len() < frame_bytes {
        return Ok(None);
    }
    bytes.truncate(frame_bytes);
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use loopcut_core::{ColorRange, TransferFunction, YuvMatrix};

    use super::*;

    fn sample_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/media/generated")
            .join(name)
    }

    fn request(count: u32, duration: f64, rotation_cw: u16) -> ThumbnailRequest {
        ThumbnailRequest {
            input: PathBuf::from("unused.mp4"),
            stream_index: 0,
            start_time: 0.0,
            duration,
            count,
            height: 90,
            rotation_cw,
            display_aspect: 16.0 / 9.0,
            color: ColorInfo::SDR_BT709,
        }
    }

    fn hdr_pq() -> ColorInfo {
        ColorInfo {
            matrix: YuvMatrix::Bt2020,
            range: ColorRange::Limited,
            transfer: TransferFunction::Pq,
            wide_gamut: true,
        }
    }

    #[test]
    fn dimensions_are_even_and_follow_the_aspect() {
        let mut sized = request(4, 10.0, 0);
        let geometry = plan(&sized).expect("plan");
        assert_eq!((geometry.width, geometry.height), (160, 90));

        sized.height = 121;
        sized.display_aspect = 4.0 / 3.0;
        let geometry = plan(&sized).expect("plan");
        assert_eq!((geometry.width, geometry.height), (160, 120));

        sized.display_aspect = 0.5625; // portrait 9:16
        let geometry = plan(&sized).expect("plan");
        assert_eq!((geometry.width, geometry.height), (68, 120));
    }

    #[test]
    fn quarter_turns_scale_before_transposing() {
        let req = request(2, 2.0, 90);
        let geometry = plan(&req).expect("plan");
        assert_eq!(
            color_chain(&req, geometry),
            "scale=90:160:flags=lanczos:in_color_matrix=bt709:in_range=tv,\
             setparams=colorspace=bt709:range=tv,transpose=1,format=rgba"
        );
        let req = request(2, 2.0, 270);
        let geometry = plan(&req).expect("plan");
        assert!(color_chain(&req, geometry).contains("transpose=2,format=rgba"));
    }

    #[test]
    fn sampler_uses_the_exact_grid_from_stream_start() {
        // Four samples over 2.8 s: a 10/7 fps grid anchored at zero after the stream start is moved to zero.
        let req = request(4, 2.8, 0);
        assert_eq!(
            sampler(&req).expect("sampler"),
            "setpts=PTS-STARTPTS,fps=4000000/2800000:round=up:start_time=0"
        );
    }

    #[test]
    fn hdr_sources_tonemap_after_the_small_scale_and_skip_the_tags() {
        let mut req = request(1, 2.0, 0);
        req.color = hdr_pq();
        let geometry = plan(&req).expect("plan");
        let chain = color_chain(&req, geometry);
        assert!(
            chain.starts_with("scale=160:90:flags=lanczos,zscale=tin=smpte2084"),
            "{chain}"
        );
        assert!(chain.contains("tonemap=tonemap=hable"), "{chain}");
        assert!(!chain.contains("setparams"), "{chain}");
        assert!(chain.ends_with(",format=rgba"), "{chain}");
    }

    #[test]
    fn rgb_sources_convert_without_yuv_pins() {
        let mut req = request(1, 2.0, 0);
        req.color = ColorInfo {
            matrix: YuvMatrix::Rgb,
            range: ColorRange::Full,
            transfer: TransferFunction::Sdr,
            wide_gamut: false,
        };
        let geometry = plan(&req).expect("plan");
        assert_eq!(
            color_chain(&req, geometry),
            "scale=160:90:flags=lanczos,format=rgba"
        );
    }

    #[test]
    fn invalid_arguments_are_rejected_before_any_process_starts() {
        assert!(plan(&request(4, 0.0, 0)).is_err());
        assert!(plan(&request(4, f64::NAN, 0)).is_err());
        assert!(plan(&request(4, 10.0, 45)).is_err());
        let mut zero_height = request(4, 10.0, 0);
        zero_height.height = 0;
        assert!(plan(&zero_height).is_err());
        assert!(plan(&request(4, 10.0, 90)).is_ok());

        // Count and size caps.
        assert!(plan(&request(MAX_THUMBNAILS + 1, 10.0, 0)).is_err());
        assert!(plan(&request(MAX_THUMBNAILS, 10.0, 0)).is_ok());
        let mut tall = request(1, 10.0, 0);
        tall.height = MAX_THUMBNAIL_HEIGHT + 2;
        assert!(plan(&tall).is_err());
        let mut wide = request(1, 10.0, 0);
        wide.height = 4;
        wide.display_aspect = 3000.0;
        assert!(plan(&wide).is_err());
        // 512 frames at 8192x4320 is far beyond the byte cap.
        let mut huge = request(MAX_THUMBNAILS, 10.0, 0);
        huge.height = MAX_THUMBNAIL_HEIGHT;
        huge.display_aspect = 16.0 / 9.0;
        assert!(plan(&huge).is_err());
        let mut non_finite = request(4, 10.0, 0);
        non_finite.display_aspect = f64::INFINITY;
        assert!(plan(&non_finite).is_err());
    }

    #[test]
    fn negative_start_times_are_accepted() {
        let mut req = request(4, 10.0, 0);
        req.start_time = -2.0;
        assert!(plan(&req).is_ok());
        req.start_time = f64::NEG_INFINITY;
        assert!(plan(&req).is_err());
    }

    /// Writes a clip whose stream starts at 5 s: 30 fps, `seconds` long, with a keyframe every 10 frames.
    fn offset_clip(paths: &FfmpegPaths, dir: &Path, seconds: u32) -> PathBuf {
        let out = dir.join("offset.mp4");
        let graph = format!("testsrc2=size=160x120:rate=30:duration={seconds}");
        let lavfi = InputOptions {
            lavfi: true,
            ..InputOptions::default()
        };
        FfmpegCommand::new_ffmpeg(paths)
            .global_arg("-y")
            .expect("global")
            .input(graph.as_str(), &lavfi)
            .expect("input")
            .output_args([
                "-vf",
                "setpts=PTS+5/TB",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "10",
                "-keyint_min",
                "10",
                "-sc_threshold",
                "0",
            ])
            .expect("output args")
            .output(&out)
            .expect("output")
            .spawn(SpawnIo::default())
            .expect("spawn encoder")
            .wait()
            .expect("encode clip");
        out
    }

    /// Every frame of `clip` scaled like a thumbnail (lanczos, RGBA), as a reference for the sampled frames.
    fn reference_frames(paths: &FfmpegPaths, clip: &Path, width: u32, height: u32) -> Vec<u8> {
        let scale = format!("scale={width}:{height}:flags=lanczos");
        let output = std::process::Command::new(&paths.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(clip)
            .args([
                "-map",
                "0:0",
                "-vf",
                scale.as_str(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .output()
            .expect("run ffmpeg");
        assert!(output.status.success(), "reference decode failed");
        output.stdout
    }

    #[test]
    fn offset_start_stream_samples_the_frames_at_the_grid_points() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        // 30 fps for 2 s starting at PTS 5 s. Two samples over 2 s sit at 0 s and 1 s, which are frames 0 and 30.
        let clip = offset_clip(&paths, dir.path(), 2);
        // The reference below decodes with FFmpeg defaults, so the request uses the probed color of the untagged stream.
        let color = crate::probe::probe(&paths, &clip)
            .expect("probe")
            .video
            .color;
        let req = ThumbnailRequest {
            input: clip.clone(),
            stream_index: 0,
            start_time: 5.0,
            duration: 2.0,
            count: 2,
            height: 60,
            rotation_cw: 0,
            display_aspect: 4.0 / 3.0,
            color,
        };
        let thumbs = generate_thumbnails(&paths, &req, CancelToken::new()).expect("thumbnails");
        assert_eq!(thumbs.len(), 2);
        let every = reference_frames(&paths, &clip, thumbs[0].width, thumbs[0].height);
        let frame_bytes = (thumbs[0].width * thumbs[0].height * 4) as usize;
        assert_eq!(every.len() % frame_bytes, 0);
        for (k, (thumb, index)) in thumbs.iter().zip([0usize, 30]).enumerate() {
            assert!(
                (thumb.time - k as f64).abs() < 1e-9,
                "thumb {k} at {}",
                thumb.time
            );
            let reference = &every[index * frame_bytes..(index + 1) * frame_bytes];
            assert!(
                thumb.rgba == reference,
                "thumb {k} is not frame {index} of the stream"
            );
        }
    }

    #[test]
    fn hdr_thumbnails_use_the_proxy_tonemap_chain() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            return;
        };
        let input = sample_path("hdr_pq.mp4");
        if !input.is_file() {
            eprintln!("skipping: corpus missing");
            return;
        }
        let info = crate::probe::probe(&paths, &input).expect("probe");
        assert!(info.video.color.is_hdr(), "fixture is PQ HDR");
        let (w, h) = info.video.display_size();
        let req = ThumbnailRequest {
            input: input.clone(),
            stream_index: info.video.index,
            start_time: info.video.start_time,
            duration: info.duration,
            count: 1,
            height: 90,
            rotation_cw: info.video.rotation,
            display_aspect: f64::from(w) / f64::from(h),
            color: info.video.color,
        };
        let thumb = generate_thumbnails(&paths, &req, CancelToken::new())
            .expect("HDR thumbnail")
            .remove(0);
        // Reference: the proxy's HDR-to-SDR chain applied to the first frame at the thumbnail size. Thumbnails must match
        // it, not a plain RGB conversion, which keeps the PQ curve and comes out visibly different.
        let chain = crate::proxy::sdr_conversion_filter(info.video.color).expect("HDR chain");
        let filter = format!(
            "scale={}:{}:flags=lanczos,{chain},format=rgba",
            thumb.width, thumb.height
        );
        let reference = std::process::Command::new(&paths.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(&input)
            .args(["-frames:v", "1", "-vf", filter.as_str()])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .output()
            .expect("run ffmpeg");
        assert!(reference.status.success(), "reference chain failed");
        assert_eq!(reference.stdout.len(), thumb.rgba.len());
        let total: u64 = thumb
            .rgba
            .iter()
            .zip(&reference.stdout)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        let mean = total as f64 / thumb.rgba.len() as f64;
        assert!(
            mean < 1.0,
            "HDR thumbnail differs from the proxy chain by {mean:.2}"
        );
    }

    #[test]
    fn between_frames_the_single_pass_and_the_seek_path_pick_opposite_neighbours() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let clip = offset_clip(&paths, dir.path(), 2);
        let color = crate::probe::probe(&paths, &clip)
            .expect("probe")
            .video
            .color;
        // Four samples over 1 s at 30 fps sit at frames 0, 7.5, 15 and 22.5 of the stream.
        let req = ThumbnailRequest {
            input: clip.clone(),
            stream_index: 0,
            start_time: 5.0,
            duration: 1.0,
            count: 4,
            height: 60,
            rotation_cw: 0,
            display_aspect: 4.0 / 3.0,
            color,
        };
        let cancel = CancelToken::new();
        let single = run(&paths, &req, f64::INFINITY, &cancel).expect("single pass");
        let seeked = run(&paths, &req, 0.0, &cancel).expect("seek path");
        let every = reference_frames(&paths, &clip, single[0].width, single[0].height);
        let frame_bytes = (single[0].width * single[0].height * 4) as usize;
        let frame = |index: usize| &every[index * frame_bytes..(index + 1) * frame_bytes];
        // The single pass keeps the last frame at or before each sample; the seek keeps the first frame at or after.
        for (k, (before, after)) in [(0usize, 0usize), (7, 8), (15, 15), (22, 23)]
            .into_iter()
            .enumerate()
        {
            assert!(single[k].rgba == frame(before), "single pass sample {k}");
            assert!(seeked[k].rgba == frame(after), "seek sample {k}");
        }
    }

    #[test]
    fn seeks_use_absolute_stream_time_when_the_stream_does_not_start_at_zero() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let clip = offset_clip(&paths, dir.path(), 3);
        // Samples at 0 s, 1 s and 2 s of the stream, which start at PTS 5 s. Each sample lands on a keyframe, so the
        // seek and the decoding pass must return identical pixels.
        let timed = ThumbnailRequest {
            input: clip,
            stream_index: 0,
            start_time: 5.0,
            duration: 3.0,
            count: 3,
            height: 60,
            rotation_cw: 0,
            display_aspect: 4.0 / 3.0,
            color: ColorInfo::SDR_BT709,
        };
        let cancel = CancelToken::new();
        let seeked = run(&paths, &timed, 0.0, &cancel).expect("seek path");
        let single = run(&paths, &timed, f64::INFINITY, &cancel).expect("single pass");
        assert_eq!(seeked.len(), 3);
        assert_eq!(single.len(), 3);
        for (a, b) in seeked.iter().zip(&single) {
            assert_eq!(
                a.rgba, b.rgba,
                "seek at {} s differs from the decoded frame",
                a.time
            );
        }
    }

    #[test]
    fn forced_seek_path_matches_single_pass_geometry() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            return;
        };
        let input = sample_path("h264_320x240.mp4");
        if !input.is_file() {
            eprintln!("skipping: corpus missing");
            return;
        }
        // A 2 s clip sampled four times. A zero threshold forces the parallel seek path; an infinite one forces the
        // single pass, so both strategies run on the same input and can be compared.
        let forced = ThumbnailRequest {
            input,
            ..request(4, 2.0, 0)
        };
        let cancel = CancelToken::new();
        let seeked = run(&paths, &forced, 0.0, &cancel).expect("seek path");
        let single = run(&paths, &forced, f64::INFINITY, &cancel).expect("single pass");
        assert_eq!(seeked.len(), 4, "every seek lands inside the clip");
        assert_eq!(single.len(), 4);
        for (a, b) in seeked.iter().zip(&single) {
            assert_eq!((a.width, a.height), (b.width, b.height));
            assert_eq!(a.rgba.len(), (a.width * a.height * 4) as usize);
        }
        assert!((seeked[1].time - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_cancelled_call_returns_cancelled_without_frames() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            return;
        };
        let input = sample_path("h264_320x240.mp4");
        if !input.is_file() {
            return;
        }
        let req = ThumbnailRequest {
            input,
            ..request(4, 2.0, 0)
        };
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            run(&paths, &req, 0.0, &cancel),
            Err(MediaError::Cancelled)
        ));
        assert!(matches!(
            run(&paths, &req, f64::INFINITY, &cancel),
            Err(MediaError::Cancelled)
        ));
    }
}

//! Export orchestration.
//!
//! [`run_export`] turns a project into one output file. It plans the output frames, decodes the source ranges the plan
//! uses, composites each frame on the GPU (with the project's grade and captions) and hands the pixels to a GIF engine
//! or a video encoder. Output is written to `<output>.partial` and renamed into place only after the encoder has
//! finished, so a failed or cancelled export never leaves a file that looks complete.
//!
//! Engines: the GIF engine is [`GifEngine::Quality`] (gifski), [`GifEngine::Fast`] (FFmpeg palettegen and paletteuse) or
//! [`GifEngine::GpuDither`] (one imagequant palette and GPU ordered dithering). A project with a size budget runs the
//! target-size search in [`crate::target`] instead of one encode.
//!
//! Look: the grade is prepared once per export and set on the compositor. Captions are drawn per output frame at the
//! output size, at the source time of the frame's primary sample.
//!
//! Fallbacks:
//! * A hardware decode chain that fails on a source range restarts that range on the CPU. Only that range stays on
//!   the CPU; later ranges try the requested chain again. The event has stage `"decode"`, once per failed range.
//! * A hardware video encoder that fails to start or fails mid-way is replaced by libx264, libx265, libsvtav1 or
//!   libvpx-vp9, and the whole encode restarts. The event has stage `"encode"`.
//! * The GPU-dither pass that fails on a frame is replaced by the CPU mapping, which gives the same indices. The event
//!   has stage `"encode"` and `from` `"gpu-dither"`, once for each encode (a target search encodes once per step).
//! * Both decode and encoder fallbacks can be forced for testing with `LOOPCUT_FORCE_HW_FAIL` (see
//!   [`loopcut_core::hw`]).
//! * A lost GPU device is reported as [`ExportError::GpuLost`]. Recreating the device is the app's job.
//!
//! Output safety: an output that names the clip's source file or a `.loopcut` project is refused before any work starts.
//!
//! Frames: a plan that plays its source in order is streamed when a source range does not fit the frame store. A plan
//! that does not (reverse, ping-pong, crossfade) fails with [`ExportError::TooLarge`] when a range does not fit. See
//! [`crate::frames`].

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use loopcut_core::geometry::{self, NormRect, Orientation, PixelRect};
use loopcut_core::gif_timing::clamp_gif_fps;
use loopcut_core::hw::{FORCE_HW_FAIL_ENV, HwStage, forced_failure_from_env};
use loopcut_core::media::VideoStreamInfo;
use loopcut_core::project::{Caption, ExportSettings, Grade, Output, Project, Transform};
use loopcut_core::sequencer::{self, Sample, SequenceError, SequencePlan};
use loopcut_core::{
    Clip, ColorInfo, DecodeChain, EncoderBackend, ExportFormat, GifEngine, MediaInfo, VideoCodec,
};
use loopcut_media::encode::{Container, EncodeRequest, EncodeSession, EncodeStats};
use loopcut_media::framestore::{FrameStore, StoredFrame};
use loopcut_media::runner::{FfmpegCommand, FfmpegProcess, SpawnIo};
use loopcut_media::{CancelToken, FfmpegPaths, MediaError};
use loopcut_render::caption_layers::{CaptionRenderer, OwnedOverlay};
use loopcut_render::compositor::{
    CompositeParams, Compositor, CompositorError, InputFrame, Overlay,
};
use loopcut_render::gpu::{GpuContext, GpuError};
use loopcut_render::grade::{GradeSetup, prepare_grade};
use loopcut_render::lut::{self, Lut, LutError};
use thiserror::Error;

use crate::frames::{Access, DecodeGeometry, DecodeOptions, SourceFrames};
use crate::gif::dither::{self, DitherSpec};
use crate::gif::fast::{self, FastOptions};
use crate::gif::gpu_dither::{DitherMapper, DitherPipeline};
use crate::gif::indexed::IndexedWriter;
use crate::gif::palette::PaletteBuilder;
use crate::gif::{GifError, GifStats, GifskiEncoder, GifskiOptions, pts_for_exact_delays};
use crate::target;
use crate::video;

/// Shortest interval between two progress events. The final progress event is always sent.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Tolerance when comparing a sample time with a plan's source times.
pub(crate) const TIME_EPSILON: f64 = 1e-9;

/// Most frames sampled to build a palette (GPU dither, and the palette pass of the Fast engine).
pub(crate) const PALETTE_SAMPLES: usize = 30;

/// Most parsed LUTs kept between exports. The oldest entry is dropped first.
const LUT_CACHE_ENTRIES: usize = 8;

/// Everything an export needs that outlives it: the FFmpeg executables, the GPU, the frame cache and, optionally, the
/// caption rasterizer.
pub struct ExportContext {
    /// The `ffmpeg` and `ffprobe` executables.
    pub paths: FfmpegPaths,
    /// The GPU device the compositor runs on. Shared with the preview.
    pub gpu: Arc<GpuContext>,
    /// Decoded source frames. Frames of one export are removed when it ends.
    pub framestore: Arc<FrameStore>,
    /// Caption rasterizer shared with the app, so the installed fonts are scanned once for every export. `None` makes
    /// each run build its own, and only when the project has captions.
    pub captions: Option<Arc<Mutex<CaptionRenderer>>>,
}

/// One export: the project, its probed media, the output file and the decode and encoder choices.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportRequest {
    /// The project to export. Its clip's `path` and its grade's `lut_path` must be resolved to absolute paths.
    pub project: Project,
    /// The probed media of the project's clip. Its video stream must be the clip's `stream_index`.
    pub media: MediaInfo,
    /// Final output path. The format comes from `project.export.format`, not from the extension. Must not be the
    /// clip's source file or a `.loopcut` project file.
    pub output: PathBuf,
    /// Decode chain for the source. Ignored where the layout needs the CPU (RGBA sources).
    pub decode: DecodeChain,
    /// Video encoder for MP4 and WebM. Ignored for GIF, and VP9 always uses the CPU.
    pub encoder: EncoderBackend,
}

/// Progress and status of a running export.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportEvent {
    /// Output frames done so far, and the bytes written to the partial file. Sent at most every 100 ms, and once at
    /// the end. The count starts again from zero after an encoder fallback, because that encode restarts. It also
    /// starts again from zero on every target-size pass, because each pass encodes the plan from its first frame.
    /// Sampled size estimates inside the target search do not send progress.
    Progress {
        frames_done: u64,
        frames_total: u64,
        bytes: u64,
    },
    /// A hardware path failed and was replaced. `stage` is `"decode"` or `"encode"`.
    Fallback {
        stage: String,
        from: String,
        to: String,
        reason: String,
    },
    /// One attempt of the target-size search. `pass` counts from 0 (the project's own settings). `bytes` is the size
    /// that attempt produced: a real encode when one ran, otherwise the estimate that ruled it out.
    TargetPass {
        pass: u32,
        description: String,
        bytes: u64,
    },
    /// The output file is complete.
    Done {
        path: PathBuf,
        bytes: u64,
        seconds: f64,
    },
}

/// The result of a finished export.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportResult {
    /// The output file.
    pub path: PathBuf,
    /// Size of the output file in bytes.
    pub bytes: u64,
    /// Frames in the output file. For GIF, gifski can merge identical frames, so this can be below the plan's count.
    pub frames: u64,
    /// Wall-clock time of the export in seconds.
    pub seconds: f64,
}

/// Why an export failed.
#[derive(Debug, Error)]
pub enum ExportError {
    /// The request does not describe an export that can run.
    #[error("invalid export request: {0}")]
    InvalidRequest(String),
    /// The request asks for something this build does not implement yet, or a state this build cannot reach.
    #[error("not supported yet: {0}")]
    Unsupported(String),
    /// The clip's frame plan is invalid.
    #[error(transparent)]
    Plan(#[from] SequenceError),
    /// FFmpeg failed to decode or to encode.
    #[error(transparent)]
    Media(MediaError),
    /// The GIF encoder failed.
    #[error(transparent)]
    Gif(GifError),
    /// The GPU compositor failed.
    #[error(transparent)]
    Render(CompositorError),
    /// The GPU device was lost during the export. Retry the export with the software renderer.
    #[error("the GPU device was lost during the export; retry with the software renderer")]
    GpuLost,
    /// A source range decoded to no frames at all.
    #[error("the decoder produced no frames from source time {start} s")]
    DecodeEmpty {
        /// Start of the range that came out empty, in source seconds.
        start: f64,
    },
    /// The frame for a source time is not available, even after decoding its range again.
    #[error("no decoded frame is available for source time {time} s")]
    FrameUnavailable {
        /// The source time that was requested.
        time: f64,
    },
    /// The plan needs source frames in an order the frame cache cannot serve, and they do not fit the cache. The
    /// message says how much memory is needed and what to change.
    #[error("too large for the frame cache: {0}")]
    TooLarge(String),
    /// The colour LUT named by the grade could not be loaded or parsed.
    #[error("cannot use the LUT {path}: {reason}")]
    Lut {
        /// The LUT path the grade names.
        path: String,
        /// Why it could not be used.
        reason: String,
    },
    /// Target-size mode tried every setting and none fitted the budget. `best_bytes` is the smallest size of a real
    /// encode the search made, or 0 when no setting was encoded (every one was ruled out by its estimate). Estimates
    /// are never reported here, because they run high.
    #[error(
        "the output cannot fit in {target_bytes} bytes; the smallest size reached was {best_bytes} bytes"
    )]
    TargetUnreachable {
        /// The size budget that was asked for.
        target_bytes: u64,
        /// The smallest size of a real encode, or 0 when none ran.
        best_bytes: u64,
    },
    /// Reading or writing a file failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The export was cancelled. The partial output has been deleted.
    #[error("export cancelled")]
    Cancelled,
}

/// Maps an FFmpeg error. A cancelled process is a cancelled export, not a failure.
pub(crate) fn map_media(err: MediaError) -> ExportError {
    match err {
        MediaError::Cancelled => ExportError::Cancelled,
        other => ExportError::Media(other),
    }
}

fn map_render(err: CompositorError) -> ExportError {
    match err {
        CompositorError::Gpu(GpuError::DeviceLost) => ExportError::GpuLost,
        other => ExportError::Render(other),
    }
}

/// Maps a GIF error. Once cancellation has been requested, the writer's abort is a cancelled export.
fn map_gif(err: GifError, cancel: &CancelToken) -> ExportError {
    if cancel.is_cancelled() {
        ExportError::Cancelled
    } else {
        ExportError::Gif(err)
    }
}

/// Runs one export to completion.
///
/// `on_event` receives progress, fallbacks, target passes and the final `Done` event on the calling thread. `cancel`
/// stops the export at the next frame or decoder read. A cancelled export returns [`ExportError::Cancelled`] and
/// removes its partial file.
///
/// With `project.export.target_bytes` set, the export tries settings in order until the output fits the budget (see
/// [`crate::target`]).
///
/// # Errors
///
/// [`ExportError::InvalidRequest`] for requests that cannot run (including an output that would overwrite the source
/// clip or a `.loopcut` project), [`ExportError::Lut`] when the grade's LUT cannot be loaded,
/// [`ExportError::TargetUnreachable`] when no setting fits the size budget, [`ExportError::TooLarge`] when the plan
/// cannot be served from the frame cache, the frame-plan, decode, render and encode errors described on
/// [`ExportError`], and [`ExportError::Io`] for file errors.
pub fn run_export(
    ctx: &ExportContext,
    req: &ExportRequest,
    on_event: &mut dyn FnMut(ExportEvent),
    cancel: &CancelToken,
) -> Result<ExportResult, ExportError> {
    let started = Instant::now();
    let clip = validated_clip(ctx, req)?;
    check_output(req, clip)?;
    if req.project.export.target_bytes.is_some() {
        return target::run_target(ctx, req, started, on_event, cancel);
    }
    let mut kit = RenderKit::new(ctx, &req.project.grade)?;
    let (frames, bytes) = encode_to_partial(&mut kit, ctx, req, on_event, cancel)?;
    finish_output(req, frames, bytes, started, on_event)
}

/// Renames the finished partial file to the output and reports `Done`.
pub(crate) fn finish_output(
    req: &ExportRequest,
    frames: u64,
    bytes: u64,
    started: Instant,
    on_event: &mut dyn FnMut(ExportEvent),
) -> Result<ExportResult, ExportError> {
    let partial = partial_path(&req.output);
    if let Err(err) = fs::rename(&partial, &req.output) {
        let _ = fs::remove_file(&partial);
        return Err(ExportError::Io(err));
    }
    let seconds = started.elapsed().as_secs_f64();
    on_event(ExportEvent::Done {
        path: req.output.clone(),
        bytes,
        seconds,
    });
    Ok(ExportResult {
        path: req.output.clone(),
        bytes,
        frames,
        seconds,
    })
}

/// Encodes the whole plan of `req` to `<output>.partial` and returns its frame and byte counts. On failure or
/// cancellation the partial file is removed. On success it is left in place for [`finish_output`] or a discard.
///
/// `kit` is the run's GPU state (see [`RenderKit`]). The caller builds it once and passes it to every encode of the run.
pub(crate) fn encode_to_partial(
    kit: &mut RenderKit,
    ctx: &ExportContext,
    req: &ExportRequest,
    on_event: &mut dyn FnMut(ExportEvent),
    cancel: &CancelToken,
) -> Result<(u64, u64), ExportError> {
    let clip = validated_clip(ctx, req)?;
    let planned = plan_export(req, clip)?;
    let access = if is_monotonic(&planned.plan) {
        Access::Forward
    } else {
        Access::Random
    };
    let frames = SourceFrames::decode(
        &ctx.paths,
        &req.media,
        clip,
        DecodeOptions {
            chain: req.decode,
            geometry: planned.dims.geometry,
            access,
        },
        Arc::clone(&ctx.framestore),
        cancel,
    )?;
    let mut renderer = Renderer::new(
        kit,
        frames,
        planned.composite_params(&req.project.output),
        &req.project,
    );
    tracing::info!(
        frames = planned.plan.len(),
        width = planned.dims.width,
        height = planned.dims.height,
        gif = planned.format == ExportFormat::Gif,
        engine = ?req.project.export.gif_engine,
        "starting export"
    );

    let partial = partial_path(&req.output);
    let _ = fs::remove_file(&partial);
    let encoded = encode_planned(
        ctx,
        req,
        &planned,
        &mut renderer,
        on_event,
        cancel,
        &partial,
    );
    if encoded.is_err() {
        let _ = fs::remove_file(&partial);
    }
    encoded
}

/// Runs the planned frames through the encoder for the output format.
fn encode_planned(
    ctx: &ExportContext,
    req: &ExportRequest,
    planned: &Planned,
    renderer: &mut Renderer<'_>,
    on_event: &mut dyn FnMut(ExportEvent),
    cancel: &CancelToken,
    partial: &Path,
) -> Result<(u64, u64), ExportError> {
    let mut reporter = Reporter::new(on_event, planned.plan.len() as u64);
    renderer.report_fallbacks(&mut reporter);
    reporter.progress(0, || 0);

    let gif_palette = if planned.format == ExportFormat::Gif
        && req.project.export.gif_engine == GifEngine::GpuDither
    {
        let samples: Vec<Sample> = planned
            .plan
            .frames
            .iter()
            .map(|frame| frame.sample)
            .collect();
        Some(sample_palette(
            renderer,
            &samples,
            (planned.dims.width, planned.dims.height),
            req.project.export.colors,
            &mut reporter,
            cancel,
        )?)
    } else {
        None
    };
    let job = Job {
        format: planned.format,
        settings: &req.project.export,
        loop_count: req.project.output.loop_count,
        encoder: req.encoder,
        plan: &planned.plan,
        width: planned.dims.width,
        height: planned.dims.height,
        gif_palette: gif_palette.as_deref(),
        partial,
        cancel,
        paths: &ctx.paths,
    };
    if planned.format == ExportFormat::Gif {
        encode_gif_file(&job, renderer, &mut reporter)
            .map(|stats| (u64::try_from(stats.frames).unwrap_or(u64::MAX), stats.bytes))
    } else {
        encode_video(&job, renderer, &mut reporter).map(|stats| (stats.frames, stats.bytes))
    }
}

/// The partial file an export writes before it is renamed to the output: `<output>.partial`.
pub fn partial_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// The clip of a request, checked against the probed media and the GPU state.
pub(crate) fn validated_clip<'a>(
    ctx: &ExportContext,
    req: &'a ExportRequest,
) -> Result<&'a Clip, ExportError> {
    let clip = req
        .project
        .clip()
        .ok_or_else(|| ExportError::InvalidRequest("the project has no clip".to_owned()))?;
    if clip.stream_index != req.media.video.index {
        return Err(ExportError::InvalidRequest(format!(
            "the clip uses stream {} but the probed video stream is {}",
            clip.stream_index, req.media.video.index
        )));
    }
    if ctx.gpu.is_lost() {
        return Err(ExportError::GpuLost);
    }
    Ok(clip)
}

/// Refuses an output that would overwrite the clip's source or a `.loopcut` project file, and an output whose folder
/// does not exist.
fn check_output(req: &ExportRequest, clip: &Clip) -> Result<(), ExportError> {
    check_output_folder(&req.output)?;
    if req
        .output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("loopcut"))
    {
        return Err(ExportError::InvalidRequest(format!(
            "{} is a LoopCut project file; choose another output name",
            req.output.display()
        )));
    }
    if same_file(&req.output, Path::new(&clip.path)) {
        return Err(ExportError::InvalidRequest(format!(
            "the output would overwrite the source clip {}",
            clip.path
        )));
    }
    Ok(())
}

/// True when `a` and `b` name the same file. Both are canonicalized when both exist, which resolves relative paths,
/// `..`, symbolic links and letter case on Windows. Otherwise their absolute forms are compared, ignoring case on
/// Windows.
fn same_file(a: &Path, b: &Path) -> bool {
    if let (Ok(first), Ok(second)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return first == second;
    }
    match (std::path::absolute(a), std::path::absolute(b)) {
        (Ok(first), Ok(second)) => {
            if cfg!(windows) {
                first
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&second.to_string_lossy())
            } else {
                first == second
            }
        }
        _ => false,
    }
}

/// The frame plan and output geometry of one export, before any frame is decoded.
pub(crate) struct Planned {
    pub(crate) format: ExportFormat,
    pub(crate) plan: SequencePlan,
    pub(crate) dims: Dimensions,
}

impl Planned {
    pub(crate) fn composite_params(&self, output: &Output) -> CompositeParams {
        CompositeParams {
            output_width: self.dims.width,
            output_height: self.dims.height,
            crop: NormRect::FULL,
            orientation: self.dims.orientation,
            background: output.background,
        }
    }
}

/// Plans the output frames of `req` for its clip.
pub(crate) fn plan_export(req: &ExportRequest, clip: &Clip) -> Result<Planned, ExportError> {
    let format = req.project.export.format;
    let is_gif = format == ExportFormat::Gif;
    let fps = if is_gif {
        clamp_gif_fps(req.project.output.fps)
    } else {
        req.project.output.fps
    };
    let plan = sequencer::plan(clip, &req.project.timing, fps)?;
    let dims = dimensions(
        &req.media.video,
        &req.project.transform,
        &req.project.output,
    );
    Ok(Planned { format, plan, dims })
}

/// Parsed LUTs kept between exports, each with the file length and modification time it was read at.
static LUT_CACHE: Mutex<Vec<CachedLut>> = Mutex::new(Vec::new());

/// One parsed `.cube` file in [`LUT_CACHE`].
struct CachedLut {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    lut: Arc<Lut>,
}

/// Loads the `.cube` file at `path`, or returns the parsed copy from an earlier call when the file still has the same
/// length and modification time. The flag is true for a cache hit.
///
/// # Errors
///
/// The parser's [`LutError`] when the file cannot be read or does not parse. Errors are not cached.
pub(crate) fn load_lut_cached(path: &Path) -> Result<(Arc<Lut>, bool), LutError> {
    let stamp = fs::metadata(path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()));
    if let Some((len, modified)) = stamp {
        let cache = LUT_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = cache
            .iter()
            .find(|entry| entry.path == path && entry.len == len && entry.modified == modified)
        {
            return Ok((Arc::clone(&hit.lut), true));
        }
    }
    let lut = Arc::new(lut::load_cube(path)?);
    if let Some((len, modified)) = stamp {
        let mut cache = LUT_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        cache.retain(|entry| entry.path != path);
        if cache.len() >= LUT_CACHE_ENTRIES {
            cache.remove(0);
        }
        cache.push(CachedLut {
            path: path.to_path_buf(),
            len,
            modified,
            lut: Arc::clone(&lut),
        });
    }
    Ok((lut, false))
}

/// Prepares the grade of a run: loads its LUT when it names one (through [`load_lut_cached`]), then bakes the grade.
///
/// # Errors
///
/// [`ExportError::Lut`] when the LUT file cannot be read or parsed.
pub(crate) fn prepare_project_grade(grade: &Grade) -> Result<GradeSetup, ExportError> {
    let lut = match grade.lut_path.as_deref() {
        Some(path) => Some(
            load_lut_cached(Path::new(path))
                .map_err(|err| ExportError::Lut {
                    path: path.to_owned(),
                    reason: err.to_string(),
                })?
                .0,
        ),
        None => None,
    };
    Ok(prepare_grade(grade, lut.as_deref()))
}

/// True when the plan shows its source in non-decreasing time and has no crossfade blend, so a streamed range can
/// serve it in order.
pub(crate) fn is_monotonic(plan: &SequencePlan) -> bool {
    let mut last = f64::NEG_INFINITY;
    plan.frames.iter().all(|frame| match frame.sample {
        Sample::At { t } if t >= last - TIME_EPSILON => {
            last = t;
            true
        }
        _ => false,
    })
}

/// Output size and decode geometry of one export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dimensions {
    pub(crate) orientation: Orientation,
    /// Output width in display orientation.
    pub(crate) width: u32,
    /// Output height in display orientation.
    pub(crate) height: u32,
    pub(crate) geometry: DecodeGeometry,
}

/// Works out the output size and the decode geometry for a stream, its transform and the output settings.
///
/// The crop is given in display space, so it is sized against the display size, which includes the source rotation
/// and the user's rotation. FFmpeg crops and scales in stored space, so the decoded frame size is the output size
/// swapped back when the orientation swaps the axes.
pub(crate) fn dimensions(
    video: &VideoStreamInfo,
    transform: &Transform,
    output: &Output,
) -> Dimensions {
    let orientation = Orientation::new(
        video.rotation,
        transform.rotation,
        transform.flip_h,
        transform.flip_v,
    );
    let (display_w, display_h) = geometry::display_size(
        video.width,
        video.height,
        video.sample_aspect.as_f64(),
        orientation,
    );
    let crop = transform.crop.clamped();
    let (width, height) = geometry::output_dimensions(
        f64::from(display_w) * crop.w,
        f64::from(display_h) * crop.h,
        output.size,
    );
    let size = if orientation.swaps_axes() {
        (height, width)
    } else {
        (width, height)
    };
    let crop_px: Option<PixelRect> = if transform.crop.is_full() {
        None
    } else {
        Some(geometry::stored_crop_px(
            video.width,
            video.height,
            orientation,
            crop,
        ))
    };
    Dimensions {
        orientation,
        width,
        height,
        geometry: DecodeGeometry {
            crop: crop_px,
            size,
        },
    }
}

/// Checks that the folder for the output exists, so the error names the folder rather than a failed file create.
fn check_output_folder(output: &Path) -> Result<(), ExportError> {
    match output.parent() {
        Some(folder) if !folder.as_os_str().is_empty() && !folder.is_dir() => Err(
            ExportError::InvalidRequest(format!("the folder {} does not exist", folder.display())),
        ),
        _ => Ok(()),
    }
}

/// Everything the encoders need about one run. Borrowed from the caller, so the same encoders serve an export and a
/// size estimate.
pub(crate) struct Job<'a> {
    /// The output format. GIF is handled by the GIF engines, everything else by a video encoder.
    pub(crate) format: ExportFormat,
    pub(crate) settings: &'a ExportSettings,
    pub(crate) loop_count: u16,
    /// The requested video encoder. VP9 ignores it.
    pub(crate) encoder: EncoderBackend,
    pub(crate) plan: &'a SequencePlan,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The global palette of the GPU-dither engine, RGB triples. `None` for the other engines.
    pub(crate) gif_palette: Option<&'a [[u8; 3]]>,
    /// Where an encoder writes. The export renames it to the output when complete.
    pub(crate) partial: &'a Path,
    pub(crate) cancel: &'a CancelToken,
    pub(crate) paths: &'a FfmpegPaths,
}

/// Throttles progress and forwards events to the caller.
pub(crate) struct Reporter<'a> {
    on_event: &'a mut dyn FnMut(ExportEvent),
    total: u64,
    last_progress: Option<Instant>,
}

impl<'a> Reporter<'a> {
    pub(crate) fn new(on_event: &'a mut dyn FnMut(ExportEvent), total: u64) -> Self {
        Self {
            on_event,
            total,
            last_progress: None,
        }
    }

    pub(crate) fn emit(&mut self, event: ExportEvent) {
        (self.on_event)(event);
    }

    /// Sends a progress event unless one was sent less than [`PROGRESS_INTERVAL`] ago. The final frame always sends.
    /// `bytes` runs only when the event is sent, so a file size is never read for a throttled frame.
    pub(crate) fn progress(&mut self, frames_done: u64, bytes: impl FnOnce() -> u64) {
        let now = Instant::now();
        let due = frames_done >= self.total
            || self
                .last_progress
                .is_none_or(|last| now.duration_since(last) >= PROGRESS_INTERVAL);
        if due {
            self.last_progress = Some(now);
            let bytes = bytes();
            self.emit(ExportEvent::Progress {
                frames_done,
                frames_total: self.total,
                bytes,
            });
        }
    }
}

/// GPU and look state of one run: the compositor with the grade already set, the caption rasterizer, the output
/// buffer and the GPU-dither pipeline. A run (an export, a target-size search, or an estimate) builds it once and reuses
/// it for every encode, so the shaders, the grade and the font system are not rebuilt per encode.
pub(crate) struct RenderKit {
    gpu: Arc<GpuContext>,
    compositor: Compositor,
    captions: CaptionSource,
    /// The composite of the last frame rendered. Its allocation persists across frames and encodes.
    out: Vec<u8>,
    /// The GPU-dither pipeline, built by the first GPU-dither encode of the run and reused by the rest.
    dither_pipeline: Option<DitherPipeline>,
}

/// Where the caption rasterizer of a run comes from.
enum CaptionSource {
    /// The app's rasterizer, shared through [`ExportContext::captions`].
    Shared(Arc<Mutex<CaptionRenderer>>),
    /// Built on the first frame that has captions, so a project without captions never loads fonts.
    Local(Option<Box<CaptionRenderer>>),
}

impl RenderKit {
    /// Builds the compositor for `grade` and sets the grade on it.
    ///
    /// # Errors
    ///
    /// [`ExportError::Lut`] when the grade's LUT cannot be loaded, [`ExportError::GpuLost`] or [`ExportError::Render`]
    /// when the compositor cannot be built.
    pub(crate) fn new(ctx: &ExportContext, grade: &Grade) -> Result<Self, ExportError> {
        let setup = prepare_project_grade(grade)?;
        let mut compositor = Compositor::new(Arc::clone(&ctx.gpu)).map_err(map_render)?;
        compositor.set_grade(Some(&setup)).map_err(map_render)?;
        let captions = match &ctx.captions {
            Some(shared) => CaptionSource::Shared(Arc::clone(shared)),
            None => CaptionSource::Local(None),
        };
        Ok(Self {
            gpu: Arc::clone(&ctx.gpu),
            compositor,
            captions,
            out: Vec::new(),
            dither_pipeline: None,
        })
    }

    /// The caption overlays visible at `time`, or none when `list` is empty.
    fn caption_layers(
        &mut self,
        list: &[Caption],
        time: f64,
        speed: f64,
        width: u32,
        height: u32,
    ) -> Vec<OwnedOverlay> {
        if list.is_empty() {
            return Vec::new();
        }
        match &mut self.captions {
            CaptionSource::Shared(shared) => shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .layers_at(list, time, speed, width, height),
            CaptionSource::Local(slot) => slot
                .get_or_insert_with(|| Box::new(CaptionRenderer::new()))
                .layers_at(list, time, speed, width, height),
        }
    }
}

/// Turns source times into composited RGBA frames for one encode. The GPU state is borrowed from the run's
/// [`RenderKit`].
pub(crate) struct Renderer<'k> {
    kit: &'k mut RenderKit,
    frames: SourceFrames,
    params: CompositeParams,
    color: ColorInfo,
    /// The project's captions, drawn per frame at the output size.
    captions: Vec<Caption>,
    speed: f64,
    /// The sample whose composite is in the kit's output buffer.
    shown: Option<Sample>,
}

impl<'k> Renderer<'k> {
    /// Starts one encode: frames come from `frames`, and `params` gives the output size. The captions of `project` are
    /// drawn per frame.
    pub(crate) fn new(
        kit: &'k mut RenderKit,
        frames: SourceFrames,
        params: CompositeParams,
        project: &Project,
    ) -> Self {
        let color = frames.color();
        Self {
            kit,
            frames,
            params,
            color,
            captions: project.captions.clone(),
            speed: project.timing.speed,
            shown: None,
        }
    }

    /// The GPU device the compositor runs on.
    pub(crate) fn gpu(&self) -> Arc<GpuContext> {
        Arc::clone(&self.kit.gpu)
    }

    /// The composited RGBA pixels (`width * height * 4` bytes) for one planned frame. The slice is valid until the next
    /// call.
    pub(crate) fn frame(
        &mut self,
        sample: Sample,
        reporter: &mut Reporter<'_>,
    ) -> Result<&[u8], ExportError> {
        if self.shown == Some(sample) {
            return Ok(&self.kit.out);
        }
        self.shown = None;
        let rendered = self.render(sample);
        self.report_fallbacks(reporter);
        rendered?;
        self.shown = Some(sample);
        Ok(&self.kit.out)
    }

    /// Emits the decode fallbacks recorded since the last call. The eager decode records its fallbacks before the first
    /// frame, so the export reports them right after it is set up, as well as after each frame.
    pub(crate) fn report_fallbacks(&mut self, reporter: &mut Reporter<'_>) {
        for fallback in self.frames.take_fallbacks() {
            reporter.emit(ExportEvent::Fallback {
                stage: "decode".to_owned(),
                from: fallback.from,
                to: fallback.to,
                reason: fallback.reason,
            });
        }
    }

    /// Composites `sample` into the kit's output buffer, with the captions visible at its primary source time.
    fn render(&mut self, sample: Sample) -> Result<(), ExportError> {
        let primary = match sample {
            Sample::At { t } => t,
            Sample::Blend { a, .. } => a,
        };
        let layers = self.kit.caption_layers(
            &self.captions,
            primary,
            self.speed,
            self.params.output_width,
            self.params.output_height,
        );
        let overlays: Vec<Overlay<'_>> = layers.iter().map(OwnedOverlay::as_overlay).collect();
        let kit = &mut *self.kit;
        match sample {
            Sample::At { t } => {
                let frame = self.frames.frame_at(t)?;
                let input = input_frame(&frame, self.color);
                kit.compositor
                    .render_into(&input, None, &overlays, &self.params, &mut kit.out)
                    .map_err(map_render)
            }
            Sample::Blend { a, b, weight_b } => {
                let first = self.frames.frame_at(a)?;
                let second = self.frames.frame_at(b)?;
                let input_a = input_frame(&first, self.color);
                let input_b = input_frame(&second, self.color);
                kit.compositor
                    .render_into(
                        &input_a,
                        Some((&input_b, weight_b)),
                        &overlays,
                        &self.params,
                        &mut kit.out,
                    )
                    .map_err(map_render)
            }
        }
    }
}

fn input_frame(frame: &StoredFrame, color: ColorInfo) -> InputFrame<'_> {
    InputFrame {
        layout: frame.layout,
        width: frame.width,
        height: frame.height,
        data: &frame.data[..],
        color,
    }
}

/// Encodes every planned frame as a GIF with the job's engine, into `job.partial`.
pub(crate) fn encode_gif_file(
    job: &Job<'_>,
    renderer: &mut Renderer<'_>,
    reporter: &mut Reporter<'_>,
) -> Result<GifStats, ExportError> {
    match job.settings.gif_engine {
        GifEngine::Quality => {
            let file = File::create(job.partial)?;
            encode_gif_into(job, file, renderer, reporter)
        }
        GifEngine::Fast => encode_fast(job, renderer, reporter),
        GifEngine::GpuDither => encode_gpu_dither(job, renderer, reporter),
    }
}

/// Encodes every planned frame with the gifski engine into `sink`. The sink is complete only when this returns `Ok`.
pub(crate) fn encode_gif_into<W: Write + Send + 'static>(
    job: &Job<'_>,
    sink: W,
    renderer: &mut Renderer<'_>,
    reporter: &mut Reporter<'_>,
) -> Result<GifStats, ExportError> {
    let settings = job.settings;
    let options = GifskiOptions {
        width: job.width,
        height: job.height,
        quality: clamp_percent(settings.quality),
        lossy_quality: clamp_percent(settings.lossy_quality),
        motion_quality: clamp_percent(settings.motion_quality),
        fast: false,
        extra_effort: false,
        loop_count: job.loop_count,
    };
    let delays: Vec<u16> = job.plan.frames.iter().map(|frame| frame.delay_cs).collect();
    let pts = pts_for_exact_delays(&delays);

    let written = Arc::new(AtomicU64::new(0));
    let progress_written = Arc::clone(&written);
    let progress_cancel = job.cancel.clone();
    let encoder = GifskiEncoder::start(
        &options,
        sink,
        Box::new(move |progress| {
            progress_written.store(progress.bytes_written, Ordering::Relaxed);
            !progress_cancel.is_cancelled()
        }),
    )
    .map_err(|err| map_gif(err, job.cancel))?;

    for (index, (planned, pts)) in job.plan.frames.iter().zip(pts).enumerate() {
        if job.cancel.is_cancelled() {
            encoder.cancel();
            return Err(ExportError::Cancelled);
        }
        // gifski takes ownership of each frame, so this is the one copy the frame makes on its way out.
        let rgba = renderer.frame(planned.sample, reporter)?.to_vec();
        if let Err(err) = encoder.add_frame(index, rgba, job.width, job.height, pts) {
            // The writer refused the frame. Its own error is more specific than the refusal, so report that.
            return match encoder.finish() {
                Err(cause) => Err(map_gif(cause, job.cancel)),
                Ok(_) => Err(map_gif(err, job.cancel)),
            };
        }
        reporter.progress(index as u64 + 1, || written.load(Ordering::Relaxed));
    }
    encoder.finish().map_err(|err| map_gif(err, job.cancel))
}

/// The Fast engine, in two FFmpeg passes.
///
/// The palette pass pipes up to [`PALETTE_SAMPLES`] evenly spaced composited frames into `palettegen` and writes the
/// palette to a PNG in a temporary folder. The encode pass then streams every frame of the plan into `paletteuse`,
/// which applies that fixed palette. Neither pass holds more than one composited frame, so memory does not grow with
/// the length of the plan.
fn encode_fast(
    job: &Job<'_>,
    renderer: &mut Renderer<'_>,
    reporter: &mut Reporter<'_>,
) -> Result<GifStats, ExportError> {
    let options = FastOptions {
        width: job.width,
        height: job.height,
        fps: job.plan.fps,
        colors: job.settings.colors,
        dither: job.settings.dither,
        loop_count: job.loop_count,
    };
    let folder = tempfile::Builder::new()
        .prefix("loopcut-palette-")
        .tempdir()?;
    let palette = folder.path().join("palette.png");

    // Pass 1: the palette, from a short sample of the plan. Progress is not reported for this pass.
    let command = fast::palette_command(job.paths, &options, &palette).map_err(map_media)?;
    let mut process = spawn_piped(command)?;
    let mut stdin = take_stdin(&mut process)?;
    for index in palette_picks(job.plan.len(), PALETTE_SAMPLES) {
        if job.cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let rgba = renderer.frame(job.plan.frames[index].sample, reporter)?;
        if let Err(err) = stdin.write_all(rgba) {
            return Err(broken_pipe(process, stdin, err, job.cancel));
        }
    }
    drop(stdin);
    process
        .wait_cancellable(job.cancel, None)
        .map_err(map_media)?;

    // Pass 2: every frame through paletteuse with the palette from pass 1.
    let command =
        fast::encode_command(job.paths, &options, &palette, job.partial).map_err(map_media)?;
    let mut process = spawn_piped(command)?;
    let mut stdin = take_stdin(&mut process)?;
    for (index, planned) in job.plan.frames.iter().enumerate() {
        if job.cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let rgba = renderer.frame(planned.sample, reporter)?;
        if let Err(err) = stdin.write_all(rgba) {
            return Err(broken_pipe(process, stdin, err, job.cancel));
        }
        reporter.progress(index as u64 + 1, || file_len(job.partial));
    }
    drop(stdin);
    process
        .wait_cancellable(job.cancel, None)
        .map_err(map_media)?;
    Ok(GifStats {
        bytes: file_len(job.partial),
        frames: job.plan.len(),
    })
}

/// Starts an FFmpeg child whose stdin receives raw frames.
fn spawn_piped(command: FfmpegCommand) -> Result<FfmpegProcess, ExportError> {
    command
        .spawn(SpawnIo {
            stdin: true,
            stdout: false,
        })
        .map_err(map_media)
}

/// The stdin pipe of a child started by [`spawn_piped`].
fn take_stdin(process: &mut FfmpegProcess) -> Result<ChildStdin, ExportError> {
    process.take_stdin().ok_or_else(|| {
        ExportError::Gif(GifError::Internal(
            "the FFmpeg stdin pipe is missing".to_owned(),
        ))
    })
}

/// The error for a write that FFmpeg refused. FFmpeg stops reading when it rejects input and its stderr says why, so the
/// child is waited for before the broken pipe is reported. A cancelled export stops the child instead.
fn broken_pipe(
    process: FfmpegProcess,
    stdin: ChildStdin,
    err: io::Error,
    cancel: &CancelToken,
) -> ExportError {
    drop(stdin);
    match process.wait_cancellable(cancel, None) {
        Err(media) => map_media(media),
        Ok(_) => ExportError::Io(err),
    }
}

/// The GPU-dither engine: one global palette, ordered dithering per frame, and the indexed GIF writer.
fn encode_gpu_dither(
    job: &Job<'_>,
    renderer: &mut Renderer<'_>,
    reporter: &mut Reporter<'_>,
) -> Result<GifStats, ExportError> {
    let palette = job.gif_palette.ok_or_else(|| {
        ExportError::Gif(GifError::Internal(
            "the GPU-dither engine needs a palette".to_owned(),
        ))
    })?;
    let spec: DitherSpec = dither::spec_for(job.settings.dither, palette.len());
    let gpu = renderer.gpu();
    let (mut mapper, reason) = DitherMapper::new(
        Some(gpu),
        &mut renderer.kit.dither_pipeline,
        job.width,
        job.height,
        palette,
        spec,
    );
    if let Some(reason) = reason {
        reporter.emit(gpu_fallback(reason));
    }
    let file = File::create(job.partial)?;
    let mut writer = IndexedWriter::new(
        BufWriter::new(file),
        job.width,
        job.height,
        palette,
        job.loop_count,
    )
    .map_err(ExportError::Gif)?;
    let mut indices = Vec::new();
    for (index, planned) in job.plan.frames.iter().enumerate() {
        if job.cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let rgba = renderer.frame(planned.sample, reporter)?;
        if let Some(reason) = mapper.map(rgba, &mut indices) {
            reporter.emit(gpu_fallback(reason));
        }
        writer
            .push(&indices, planned.delay_cs)
            .map_err(ExportError::Gif)?;
        reporter.progress(index as u64 + 1, || file_len(job.partial));
    }
    let mut inner = writer.finish().map_err(ExportError::Gif)?;
    inner.flush()?;
    drop(inner);
    Ok(GifStats {
        bytes: file_len(job.partial),
        frames: job.plan.len(),
    })
}

/// The fallback event for the GPU-dither pass.
fn gpu_fallback(reason: String) -> ExportEvent {
    ExportEvent::Fallback {
        stage: "encode".to_owned(),
        from: "gpu-dither".to_owned(),
        to: "cpu-dither".to_owned(),
        reason,
    }
}

/// Builds the global palette of the GPU-dither engine from up to [`PALETTE_SAMPLES`] evenly spaced frames of
/// `samples`. The frames are composited at `size` and never all held at once. Cancellation is checked before each
/// frame.
pub(crate) fn sample_palette(
    renderer: &mut Renderer<'_>,
    samples: &[Sample],
    size: (u32, u32),
    colors: u16,
    reporter: &mut Reporter<'_>,
    cancel: &CancelToken,
) -> Result<Vec<[u8; 3]>, ExportError> {
    let mut builder = PaletteBuilder::new(colors.min(crate::gif::palette::MAX_PALETTE_COLORS))
        .map_err(ExportError::Gif)?;
    for index in palette_picks(samples.len(), PALETTE_SAMPLES) {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let rgba = renderer.frame(samples[index], reporter)?;
        builder
            .add_frame(rgba, size.0, size.1)
            .map_err(ExportError::Gif)?;
    }
    builder.finish().map_err(ExportError::Gif)
}

/// Indices of at most `count` evenly spaced items out of `len`, first and last included. With a single pick the
/// middle item is taken, since one item cannot be both the first and the last.
pub(crate) fn palette_picks(len: usize, count: usize) -> Vec<usize> {
    if len == 0 || count == 0 {
        return Vec::new();
    }
    if len <= count {
        return (0..len).collect();
    }
    if count == 1 {
        return vec![(len - 1) / 2];
    }
    (0..count).map(|i| i * (len - 1) / (count - 1)).collect()
}

/// Why one video encode attempt failed. Encoder failures can be retried on the CPU; the rest cannot.
enum AttemptError {
    /// The encoder failed to start, failed on a frame, or failed to finish.
    Encoder(MediaError),
    /// Something else failed, or the export was cancelled.
    Export(ExportError),
}

impl AttemptError {
    fn into_export(self) -> ExportError {
        match self {
            Self::Encoder(err) => map_media(err),
            Self::Export(err) => err,
        }
    }
}

fn encoder_failure(err: MediaError) -> AttemptError {
    match err {
        MediaError::Cancelled => AttemptError::Export(ExportError::Cancelled),
        other => AttemptError::Encoder(other),
    }
}

/// Encodes every planned frame to an MP4 or WebM file at `job.partial`. A failed hardware encoder restarts the encode on
/// the CPU.
pub(crate) fn encode_video(
    job: &Job<'_>,
    renderer: &mut Renderer<'_>,
    reporter: &mut Reporter<'_>,
) -> Result<EncodeStats, ExportError> {
    let target = video::video_target(job.format)
        .ok_or_else(|| ExportError::InvalidRequest("GIF is not encoded as a video".to_owned()))?;
    let (codec, _) = target;
    let requested = video::encoder_for(codec, job.encoder);
    match encode_attempt(job, renderer, reporter, target, requested) {
        Ok(stats) => Ok(stats),
        Err(AttemptError::Encoder(err)) if requested.is_hardware() => {
            let from = requested
                .ffmpeg_encoder(codec)
                .unwrap_or("hardware encoder");
            let to = EncoderBackend::Cpu
                .ffmpeg_encoder(codec)
                .unwrap_or("cpu encoder");
            tracing::warn!(from, to, error = %err, "hardware encoder failed; restarting on the CPU");
            reporter.emit(ExportEvent::Fallback {
                stage: "encode".to_owned(),
                from: from.to_owned(),
                to: to.to_owned(),
                reason: err.to_string(),
            });
            let _ = fs::remove_file(job.partial);
            encode_attempt(job, renderer, reporter, target, EncoderBackend::Cpu)
                .map_err(AttemptError::into_export)
        }
        Err(err) => Err(err.into_export()),
    }
}

/// One complete encode with `backend`, from the first frame to the trailer.
fn encode_attempt(
    job: &Job<'_>,
    renderer: &mut Renderer<'_>,
    reporter: &mut Reporter<'_>,
    (codec, container): (VideoCodec, Container),
    backend: EncoderBackend,
) -> Result<EncodeStats, AttemptError> {
    if forced_failure_from_env(HwStage::Encode, backend.name()) {
        return Err(AttemptError::Encoder(MediaError::Unsupported(format!(
            "{FORCE_HW_FAIL_ENV} forces the {} encoder to fail",
            backend.name()
        ))));
    }
    let request = EncodeRequest {
        output: job.partial.to_path_buf(),
        width: job.width,
        height: job.height,
        fps: job.plan.fps,
        codec,
        backend,
        quality: clamp_percent(job.settings.quality),
        container,
        keyint: Some(video::keyint_for_loop(job.plan.len())),
    };
    let mut session = EncodeSession::start(job.paths, &request, None).map_err(encoder_failure)?;
    for (index, planned) in job.plan.frames.iter().enumerate() {
        if job.cancel.is_cancelled() {
            session.cancel();
            return Err(AttemptError::Export(ExportError::Cancelled));
        }
        let rgba = match renderer.frame(planned.sample, reporter) {
            Ok(rgba) => rgba,
            Err(err) => {
                session.cancel();
                return Err(AttemptError::Export(err));
            }
        };
        session.write_frame(rgba).map_err(encoder_failure)?;
        reporter.progress(index as u64 + 1, || file_len(job.partial));
    }
    let stats = session.finish().map_err(encoder_failure)?;
    reporter.progress(job.plan.len() as u64, || stats.bytes);
    Ok(stats)
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// Clamps a 1–100 setting from the project into the range the encoders accept.
fn clamp_percent(value: u8) -> u8 {
    value.clamp(1, 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcut_core::geometry::{NormRect, OutputSize};
    use loopcut_core::media::{ColorInfo, VideoStreamInfo};
    use loopcut_core::{Rational, project::Output};

    fn video(width: u32, height: u32, rotation: u16) -> VideoStreamInfo {
        VideoStreamInfo {
            index: 0,
            codec: "h264".to_owned(),
            profile: None,
            width,
            height,
            sample_aspect: Rational::integer(1),
            pix_fmt: "yuv420p".to_owned(),
            bit_depth: 8,
            has_alpha: false,
            rotation,
            frame_rate: Rational::integer(30),
            avg_frame_rate: None,
            variable_frame_rate: false,
            color: ColorInfo::SDR_BT709,
            start_time: 0.0,
            duration: 2.0,
            frame_count: None,
            interlaced: false,
            web_playable: true,
        }
    }

    fn output_width(width: u32) -> Output {
        Output {
            size: OutputSize::Width { width },
            ..Output::default()
        }
    }

    fn sample_at(t: f64) -> Sample {
        Sample::At { t }
    }

    fn plan_of(samples: &[Sample]) -> SequencePlan {
        SequencePlan {
            fps: Rational::integer(10),
            frames: samples
                .iter()
                .map(|&sample| sequencer::PlannedFrame {
                    sample,
                    delay_cs: 10,
                })
                .collect(),
            duration: samples.len() as f64 / 10.0,
        }
    }

    #[test]
    fn portrait_source_outputs_portrait_and_decodes_swapped() {
        // Stored 320x240 with a 90° display matrix: displayed as 240x320.
        let dims = dimensions(
            &video(320, 240, 90),
            &Transform::default(),
            &output_width(160),
        );
        assert_eq!((dims.width, dims.height), (160, 214));
        assert_eq!(dims.geometry.size, (214, 160));
        assert_eq!(dims.geometry.crop, None);
        assert!(dims.orientation.swaps_axes());
    }

    #[test]
    fn centre_crop_follows_the_crop_aspect() {
        // A tall crop of the middle half width of a 320x240 source: 160x240 in display space.
        let transform = Transform {
            crop: NormRect {
                x: 0.25,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            ..Transform::default()
        };
        let dims = dimensions(&video(320, 240, 0), &transform, &output_width(160));
        assert_eq!((dims.width, dims.height), (160, 240));
        assert_eq!(dims.geometry.size, (160, 240));
        assert_eq!(
            dims.geometry.crop,
            Some(PixelRect {
                x: 80,
                y: 0,
                w: 160,
                h: 240
            })
        );
    }

    #[test]
    fn user_rotation_adds_to_the_source_rotation() {
        // Source rotation 90 plus user rotation 90 is 180: no axis swap, and the crop stays in stored space.
        let transform = Transform {
            rotation: 90,
            ..Transform::default()
        };
        let dims = dimensions(&video(320, 240, 90), &transform, &output_width(160));
        assert!(!dims.orientation.swaps_axes());
        assert_eq!((dims.width, dims.height), (160, 120));
        assert_eq!(dims.geometry.size, (160, 120));
    }

    #[test]
    fn partial_file_sits_next_to_the_output() {
        assert_eq!(
            partial_path(Path::new("C:/out/loop.gif")),
            PathBuf::from("C:/out/loop.gif.partial")
        );
    }

    #[test]
    fn missing_output_folder_is_reported_by_name() {
        let missing = Path::new("D:/definitely/not/a/folder/out.gif");
        assert!(matches!(
            check_output_folder(missing),
            Err(ExportError::InvalidRequest(message)) if message.contains("does not exist")
        ));
        assert!(check_output_folder(Path::new("out.gif")).is_ok());
    }

    #[test]
    fn same_file_compares_existing_files_by_identity() {
        let dir = tempfile::tempdir().expect("temporary folder");
        let clip = dir.path().join("clip.mp4");
        fs::write(&clip, b"x").expect("write clip");
        let other = dir.path().join("other.mp4");
        // A spelling through `.` names the same existing file.
        let respelled = dir.path().join(".").join("clip.mp4");
        assert!(same_file(&clip, &respelled));
        assert!(!same_file(&clip, &other));
        // A file that does not exist yet cannot be the clip.
        assert!(!same_file(&other, &clip));
    }

    #[test]
    fn monotonic_plans_stream_and_blends_do_not() {
        assert!(is_monotonic(&plan_of(&[
            sample_at(0.0),
            sample_at(0.0),
            sample_at(0.1),
            sample_at(2.0),
        ])));
        assert!(!is_monotonic(&plan_of(&[sample_at(0.5), sample_at(0.4)])));
        assert!(!is_monotonic(&plan_of(&[
            sample_at(0.0),
            Sample::Blend {
                a: 0.9,
                b: 0.0,
                weight_b: 0.5,
            },
        ])));
    }

    #[test]
    fn progress_is_throttled_and_sizes_are_read_only_when_sent() {
        let mut events = Vec::new();
        let mut sink = |event: ExportEvent| events.push(event);
        let mut reporter = Reporter::new(&mut sink, 100);
        let mut reads = 0;
        reporter.progress(0, || {
            reads += 1;
            0
        });
        // Within the interval after the first event, a size is not read.
        reporter.progress(1, || {
            reads += 1;
            0
        });
        // The final frame is always sent, and only then is its size read again.
        reporter.progress(100, || {
            reads += 1;
            7
        });
        assert_eq!(reads, 2);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            ExportEvent::Progress {
                frames_done: 100,
                bytes: 7,
                ..
            }
        ));
    }

    #[test]
    fn clamp_keeps_settings_in_range() {
        assert_eq!(clamp_percent(0), 1);
        assert_eq!(clamp_percent(90), 90);
        assert_eq!(clamp_percent(250), 100);
    }

    #[test]
    fn palette_picks_are_evenly_spaced_and_include_both_ends() {
        assert_eq!(palette_picks(0, 30), Vec::<usize>::new());
        assert_eq!(palette_picks(4, 30), vec![0, 1, 2, 3]);
        let picks = palette_picks(100, 5);
        assert_eq!(picks, vec![0, 24, 49, 74, 99]);
        let many = palette_picks(1000, 30);
        assert_eq!(many.len(), 30);
        assert_eq!((many[0], many[29]), (0, 999));
        assert!(many.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_missing_lut_is_an_export_error_naming_the_file() {
        let grade = Grade {
            lut_path: Some("D:/definitely/not/here/look.cube".to_owned()),
            ..Grade::default()
        };
        assert!(matches!(
            prepare_project_grade(&grade),
            Err(ExportError::Lut { path, .. }) if path.contains("look.cube")
        ));
        // A grade without a LUT prepares without reading any file.
        assert!(prepare_project_grade(&Grade::default()).is_ok());
    }

    #[test]
    fn a_single_palette_pick_is_the_middle_frame() {
        assert_eq!(palette_picks(1, 1), vec![0]);
        assert_eq!(palette_picks(100, 1), vec![49]);
        assert_eq!(palette_picks(3, 2), vec![0, 2]);
    }

    #[test]
    fn a_long_plan_gives_the_palette_at_most_the_sample_count() {
        // The Fast engine's palette pass and the GPU-dither palette both take their frames from this list, so a plan of
        // any length feeds the palette this many frames and no more.
        let picks = palette_picks(100_000, PALETTE_SAMPLES);
        assert_eq!(picks.len(), PALETTE_SAMPLES);
        assert_eq!((picks[0], picks[PALETTE_SAMPLES - 1]), (0, 99_999));
        assert!(picks.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// A 2x2x2 `.cube` file.
    const CUBE: &str = "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";

    #[test]
    fn a_lut_is_parsed_once_until_its_file_changes() {
        let dir = tempfile::tempdir().expect("temporary folder");
        let path = dir.path().join("look.cube");
        fs::write(&path, CUBE).expect("write cube");
        let (first, hit) = load_lut_cached(&path).expect("first load");
        assert!(!hit, "the first load parses the file");
        let (second, hit) = load_lut_cached(&path).expect("second load");
        assert!(hit, "an unchanged file is served from the cache");
        assert!(Arc::ptr_eq(&first, &second));
        // A longer file has a new length, so it is parsed again.
        fs::write(&path, format!("TITLE \"changed\"\n{CUBE}")).expect("rewrite cube");
        let (_, hit) = load_lut_cached(&path).expect("third load");
        assert!(!hit, "a changed file is parsed again");
    }
}

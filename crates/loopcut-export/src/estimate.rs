//! Output size estimates from real encodes of sampled windows of the export's frame plan.
//!
//! [`estimate_size_with_cancel`] encodes up to three windows of the plan with the export's own encoder settings, then
//! extrapolates the bytes per frame to the whole plan. A plan no longer than three windows is encoded in full instead,
//! and that estimate is the exact export size. Windows are written to a temporary folder, which is removed before the
//! function returns. The windows use the export's grade, captions and GIF engine. The GPU-dither palette is built from
//! the frames of the windows, not from the whole plan, so it is an approximation of the export's palette.
//!
//! The cancel token is checked before each window and before each frame, so a cancelled estimate returns
//! [`ExportError::Cancelled`] within one encoded frame.
//!
//! Sampled estimates are approximate, and the ±15 % band is not a guarantee. Each window is encoded on its own, so it
//! starts with a full GIF frame or a keyframe that the real export has only once. On short plans that overhead is a
//! large share of each window, and a video estimate runs high: on the test clip (a 2 s H.264 range at 10 fps, 0.5 s
//! windows) the estimate was 33 % above the MP4 that the export wrote. GIF estimates landed 4 to 18 % high on the test
//! clips, the Fast and GPU-dither engines highest. The integration tests check that the estimate lands within 35 % of
//! the real export. The target-size search sets its rule-out margins from the measured ratios.

use std::sync::Arc;

use loopcut_core::project::GifEngine;
use loopcut_core::sequencer::{Sample, SequencePlan, required_source_ranges};
use loopcut_core::{ExportFormat, Rational, TimeRange};
use loopcut_media::CancelToken;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::frames::{Access, DecodeOptions, SourceFrames};
use crate::job::{
    ExportContext, ExportError, ExportEvent, ExportRequest, Job, RenderKit, Renderer, Reporter,
    TIME_EPSILON, encode_gif_file, encode_video, is_monotonic, plan_export, sample_palette,
    validated_clip,
};

/// Number of windows encoded for a sampled estimate.
const WINDOWS: usize = 3;

/// Relative band of a sampled estimate: `low` and `high` are the estimate minus and plus this fraction.
const SAMPLE_TOLERANCE: f64 = 0.15;

/// How an estimate was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum EstimateBasis {
    /// Extrapolated from sampled windows. `low` and `high` are the estimate ±15 %.
    Sample,
    /// The whole plan was encoded, so `bytes` is the export's size and `low` and `high` equal it.
    Full,
}

/// Predicted size of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SizeEstimate {
    /// Estimated size of the output file in bytes.
    pub bytes: u64,
    /// Lower bound of the estimate in bytes.
    pub low: u64,
    /// Upper bound of the estimate in bytes.
    pub high: u64,
    /// Output frames in the plan.
    pub frames: u64,
    /// Whether the estimate is sampled or exact.
    pub basis: EstimateBasis,
}

/// Estimates the size of the export `req` describes, using windows of `sample_seconds` of output.
///
/// If the plan is no longer than three windows, the whole plan is encoded and the result is exact. Otherwise three
/// windows are encoded: at the start, the middle and the end of the plan. Only the source ranges those windows touch
/// are decoded. The estimate cannot be cancelled; use [`estimate_size_with_cancel`] for that.
///
/// # Errors
///
/// The errors of [`estimate_size_with_cancel`].
pub fn estimate_size(
    ctx: &ExportContext,
    req: &ExportRequest,
    sample_seconds: f64,
) -> Result<SizeEstimate, ExportError> {
    estimate_size_with_cancel(ctx, req, sample_seconds, &CancelToken::new())
}

/// [`estimate_size`] that stops when `cancel` fires, checked before each window and each frame.
///
/// # Errors
///
/// [`ExportError::InvalidRequest`] for a sample length that is not a positive finite number of seconds,
/// [`ExportError::Cancelled`] when `cancel` fires, and the same errors as [`crate::run_export`] for the request itself,
/// except that the output path is not checked: nothing is written to it.
pub fn estimate_size_with_cancel(
    ctx: &ExportContext,
    req: &ExportRequest,
    sample_seconds: f64,
    cancel: &CancelToken,
) -> Result<SizeEstimate, ExportError> {
    check_sample_seconds(sample_seconds)?;
    let mut kit = RenderKit::new(ctx, &req.project.grade)?;
    estimate_in(&mut kit, ctx, req, sample_seconds, cancel)
}

/// The estimate of [`estimate_size_with_cancel`], run on the GPU state of a larger run (the target-size search builds
/// one [`RenderKit`] and estimates every step with it).
pub(crate) fn estimate_in(
    kit: &mut RenderKit,
    ctx: &ExportContext,
    req: &ExportRequest,
    sample_seconds: f64,
    cancel: &CancelToken,
) -> Result<SizeEstimate, ExportError> {
    check_sample_seconds(sample_seconds)?;
    let clip = validated_clip(ctx, req)?;
    let planned = plan_export(req, clip)?;
    let plan = &planned.plan;
    let total = plan.len();
    let window = sample_window(sample_seconds, plan.fps);
    let spans = window_spans(total, window);
    let full = is_full_plan(total, window);

    let times: Vec<f64> = spans
        .iter()
        .flat_map(|&(start, len)| {
            plan.frames[start..start + len]
                .iter()
                .flat_map(|frame| frame.sample.source_times())
        })
        .collect();
    let ranges: Vec<TimeRange> = required_source_ranges(clip)
        .into_iter()
        .filter(|range| {
            times
                .iter()
                .any(|&t| (range.start - TIME_EPSILON..=range.end + TIME_EPSILON).contains(&t))
        })
        .collect();

    let access = if is_monotonic(plan) {
        Access::Forward
    } else {
        Access::Random
    };
    let frames = SourceFrames::decode_ranges(
        &ctx.paths,
        &req.media,
        clip,
        &ranges,
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
    let mut discard = |_: ExportEvent| {};
    let mut reporter = Reporter::new(&mut discard, total as u64);

    let is_gif = planned.format == ExportFormat::Gif;
    let palette = if is_gif && req.project.export.gif_engine == GifEngine::GpuDither {
        let samples: Vec<Sample> = spans
            .iter()
            .flat_map(|&(start, len)| plan.frames[start..start + len].iter().map(|f| f.sample))
            .collect();
        Some(sample_palette(
            &mut renderer,
            &samples,
            (planned.dims.width, planned.dims.height),
            req.project.export.colors,
            &mut reporter,
            cancel,
        )?)
    } else {
        None
    };

    // Windows are written to a folder that is removed when this function returns.
    let scratch = tempfile::Builder::new()
        .prefix("loopcut-estimate-")
        .tempdir()?;
    #[cfg(test)]
    test_hooks::scratch_created(scratch.path());
    let mut bytes_total = 0u64;
    let mut frames_total = 0u64;
    for (number, &(start, len)) in spans.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let window_plan = SequencePlan {
            fps: plan.fps,
            frames: plan.frames[start..start + len].to_vec(),
            duration: len as f64 / plan.fps.as_f64(),
        };
        let partial = scratch.path().join(format!("window-{number}.partial"));
        let job = Job {
            format: planned.format,
            settings: &req.project.export,
            loop_count: req.project.output.loop_count,
            encoder: req.encoder,
            plan: &window_plan,
            width: planned.dims.width,
            height: planned.dims.height,
            gif_palette: palette.as_deref(),
            partial: &partial,
            cancel,
            paths: &ctx.paths,
        };
        let bytes = if is_gif {
            encode_gif_file(&job, &mut renderer, &mut reporter)?.bytes
        } else {
            encode_video(&job, &mut renderer, &mut reporter)?.bytes
        };
        bytes_total += bytes;
        frames_total += len as u64;
        #[cfg(test)]
        test_hooks::after_window(number, cancel);
    }

    let frames = total as u64;
    if full {
        return Ok(SizeEstimate {
            bytes: bytes_total,
            low: bytes_total,
            high: bytes_total,
            frames,
            basis: EstimateBasis::Full,
        });
    }
    let per_frame = bytes_total as f64 / frames_total as f64;
    let bytes = (per_frame * total as f64).round() as u64;
    let low = (bytes as f64 * (1.0 - SAMPLE_TOLERANCE)).floor() as u64;
    let high = (bytes as f64 * (1.0 + SAMPLE_TOLERANCE)).ceil() as u64;
    Ok(SizeEstimate {
        bytes,
        low,
        high,
        frames,
        basis: EstimateBasis::Sample,
    })
}

/// Rejects a sample length that is not a positive, finite number of seconds.
fn check_sample_seconds(sample_seconds: f64) -> Result<(), ExportError> {
    if sample_seconds.is_finite() && sample_seconds > 0.0 {
        Ok(())
    } else {
        Err(ExportError::InvalidRequest(format!(
            "the sample length {sample_seconds} s must be a positive number of seconds"
        )))
    }
}

/// Window length in frames for `sample_seconds` of output at `fps`. Never zero.
pub(crate) fn sample_window(sample_seconds: f64, fps: Rational) -> usize {
    ((sample_seconds * fps.as_f64()).round() as usize).max(1)
}

/// True when a plan of `total` frames is no longer than [`WINDOWS`] windows of `window` frames, so the whole plan is
/// encoded and no sampled estimate is needed.
pub(crate) fn is_full_plan(total: usize, window: usize) -> bool {
    total <= window.saturating_mul(WINDOWS)
}

/// The `(start, length)` frame spans an estimate encodes: the whole plan when [`is_full_plan`], otherwise windows at the
/// start, the middle and the end.
pub(crate) fn window_spans(total: usize, window: usize) -> Vec<(usize, usize)> {
    if is_full_plan(total, window) {
        vec![(0, total)]
    } else {
        let last = total - window;
        vec![(0, window), (last / 2, window), (last, window)]
    }
}

/// Test hooks for the window loop. A test can cancel an estimate after a chosen window, and find that estimate's scratch
/// folder afterwards. The hooks are per thread, so tests that run in parallel do not see each other.
#[cfg(test)]
mod test_hooks {
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    use loopcut_media::CancelToken;

    thread_local! {
        static CANCEL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
        static SCRATCH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// Makes the estimate on this thread cancel once window `number` has been encoded.
    pub(crate) fn cancel_after_window(number: usize) {
        CANCEL_AFTER.with(|slot| slot.set(Some(number)));
    }

    /// The scratch folder of the estimate that ran last on this thread.
    pub(crate) fn scratch_folder() -> Option<PathBuf> {
        SCRATCH.with(|slot| slot.borrow().clone())
    }

    pub(crate) fn scratch_created(path: &Path) {
        SCRATCH.with(|slot| *slot.borrow_mut() = Some(path.to_path_buf()));
    }

    pub(crate) fn after_window(number: usize, cancel: &CancelToken) {
        if CANCEL_AFTER.with(Cell::get) == Some(number) {
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcut_core::geometry::OutputSize;
    use loopcut_core::{Clip, DecodeChain, EncoderBackend, Project};
    use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
    use loopcut_media::{FfmpegPaths, probe};
    use loopcut_render::gpu::{AdapterChoice, GpuContext};
    use std::path::{Path, PathBuf};

    #[test]
    fn short_plans_are_encoded_whole() {
        assert_eq!(window_spans(10, 4), vec![(0, 10)]);
        assert!(is_full_plan(12, 4));
        assert!(!is_full_plan(13, 4));
    }

    #[test]
    fn long_plans_sample_the_start_middle_and_end() {
        assert_eq!(
            window_spans(30, 4),
            vec![(0, 4), (13, 4), (26, 4)],
            "the middle window starts half way to the last window"
        );
    }

    #[test]
    fn windows_are_at_least_one_frame() {
        assert_eq!(sample_window(0.01, Rational::integer(10)), 1);
        assert_eq!(sample_window(0.5, Rational::integer(20)), 10);
    }

    #[test]
    fn a_non_positive_sample_length_is_refused() {
        assert!(check_sample_seconds(0.0).is_err());
        assert!(check_sample_seconds(f64::NAN).is_err());
        assert!(check_sample_seconds(0.5).is_ok());
    }

    /// The staged FFmpeg build and the software adapter, or `None` (after a note) when either is missing. Under
    /// `LOOPCUT_REQUIRE_FFMPEG=1` or `LOOPCUT_REQUIRE_GPU_TESTS=1` a missing one fails the test.
    fn export_rig() -> Option<(FfmpegPaths, Arc<GpuContext>)> {
        let required = |name: &str| std::env::var(name).is_ok_and(|value| value.trim() == "1");
        let Some(paths) = FfmpegPaths::discover_dev() else {
            assert!(
                !required("LOOPCUT_REQUIRE_FFMPEG"),
                "LOOPCUT_REQUIRE_FFMPEG=1 but no staged FFmpeg build was found"
            );
            eprintln!("skipping: no staged FFmpeg build");
            return None;
        };
        match GpuContext::new(&AdapterChoice::Software) {
            Ok(gpu) => Some((paths, Arc::new(gpu))),
            Err(err) => {
                assert!(
                    !required("LOOPCUT_REQUIRE_GPU_TESTS"),
                    "LOOPCUT_REQUIRE_GPU_TESTS=1 but there is no software GPU adapter: {err}"
                );
                eprintln!("skipping: no software GPU adapter: {err}");
                None
            }
        }
    }

    #[test]
    fn a_cancel_after_the_first_window_stops_the_estimate_and_removes_its_folder() {
        let Some((paths, gpu)) = export_rig() else {
            return;
        };
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("loopcut-media")
            .join("tests")
            .join("media")
            .join("generated")
            .join("h264_320x240.mp4");
        let media = probe(&paths, &source).expect("probe the test clip");
        let clip = Clip {
            id: "clip-1".to_owned(),
            path: source.to_string_lossy().into_owned(),
            stream_index: media.video.index,
            segments: vec![TimeRange::new(0.0, 2.0)],
            holds: Vec::new(),
        };
        let mut project = Project::new("cancel", clip);
        project.output.fps = Rational::integer(10);
        project.output.size = OutputSize::Width { width: 160 };
        // Half speed makes a 4 s plan of 40 frames, more than three 1 s windows, so the estimate is sampled.
        project.timing.speed = 0.5;
        let req = ExportRequest {
            project,
            media,
            output: PathBuf::from("never.gif"),
            decode: DecodeChain::CPU,
            encoder: EncoderBackend::Cpu,
        };
        let folder = tempfile::tempdir().expect("temporary folder");
        let ctx = ExportContext {
            paths,
            gpu,
            framestore: Arc::new(
                FrameStore::new(FrameStoreConfig {
                    ram_budget_bytes: 256 << 20,
                    disk_budget_bytes: 2 << 30,
                    spill_dir: folder.path().join("spill"),
                })
                .expect("frame store"),
            ),
            captions: None,
        };

        test_hooks::cancel_after_window(0);
        let result = estimate_size_with_cancel(&ctx, &req, 1.0, &CancelToken::new());
        assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
        let scratch = test_hooks::scratch_folder().expect("the estimate made a scratch folder");
        assert!(
            !scratch.exists(),
            "the scratch folder {} is removed",
            scratch.display()
        );
    }
}

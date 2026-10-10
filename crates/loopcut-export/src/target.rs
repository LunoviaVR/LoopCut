//! Target-size search: the export tries settings in a fixed order until the output fits a byte budget.
//!
//! The order is the product's. A GIF with the Quality engine degrades in this order: gifski quality 90 → 80 → 70 → 60,
//! then fps (25, 20, 15, 12.5, 10, only the values below the project's fps), then width (90 %, 80 %, … 30 % of the output
//! width), then gifski lossy quality 80, 50, 30. The Fast and GPU-dither engines have no gifski quality, so the first
//! steps are their palette sizes instead: 128, 64, 32 colours. Video degrades its quality (80, 60, 40, 20) and then its
//! width. Every step applies on top of the steps before it.
//!
//! Each step is first checked with a sampled estimate over short windows, unless the whole plan is short enough to be
//! encoded once for real. A step whose estimate is above the budget times a margin is ruled out without a real encode.
//! Each engine has its own margin, set from the estimate-to-export ratios measured on the test clips (see the constants).
//! Every other step is encoded for real. It is kept when the real output fits, and the search moves on when it does not.
//!
//! The whole search shares one [`RenderKit`]: the compositor, the grade and the caption rasterizer are built once, not
//! once per encode.

use std::fs;
use std::time::Instant;

use loopcut_core::Rational;
use loopcut_core::geometry::OutputSize;
use loopcut_core::project::{ExportFormat, ExportSettings, GifEngine, Output};
use loopcut_media::CancelToken;

use crate::estimate::{estimate_in, is_full_plan, sample_window};
use crate::job::{
    ExportContext, ExportError, ExportEvent, ExportRequest, ExportResult, RenderKit,
    encode_to_partial, finish_output, partial_path, plan_export, validated_clip,
};

/// Length of the windows the estimates encode, in output seconds.
pub(crate) const SAMPLE_SECONDS: f64 = 1.0;

// Rule-out margins. A step is ruled out when its estimate is above `budget * margin`, so a margin must be at least the
// largest estimate-to-export ratio of a step that fits. The ratios below are measured by the sampled-estimate tests in
// `tests/export.rs` on `h264_320x240.mp4` and `portrait_rotated.mp4`. Each margin is at least 4 % above the worst ratio.

/// GIF Quality engine. Ratios: 1.03 to 1.05 over one-second windows (the search's window), 1.04 to 1.11 over half-second
/// windows.
pub(crate) const QUALITY_MARGIN: f64 = 1.15;

/// Fast engine. Ratios: 1.03 to 1.04 over one-second windows, 1.18 over half-second windows.
pub(crate) const FAST_MARGIN: f64 = 1.25;

/// GPU-dither engine, whose palette is built from the window frames only. Ratios: 1.08 to 1.12 over one-second windows,
/// 1.18 over half-second windows.
pub(crate) const DITHER_MARGIN: f64 = 1.25;

/// Video. Ratios: 1.21 over one-second windows, 1.33 over half-second windows.
pub(crate) const VIDEO_MARGIN: f64 = 1.4;

// Each margin covers the worst measured ratio, so a step that fits is not ruled out on the test clips.
const _: () = assert!(
    QUALITY_MARGIN > 1.105 && FAST_MARGIN > 1.18 && DITHER_MARGIN > 1.184 && VIDEO_MARGIN > 1.328
);

/// gifski quality steps, applied below the project's quality.
const GIF_QUALITY_STEPS: [u8; 3] = [80, 70, 60];
/// Palette sizes for the Fast and GPU-dither engines, applied below the project's colours.
const COLOR_STEPS: [u16; 3] = [128, 64, 32];
/// Frame rates the GIF search tries, as (numerator, denominator), highest first.
const FPS_STEPS: [(u32, u32); 5] = [(25, 1), (20, 1), (15, 1), (25, 2), (10, 1)];
/// Width steps as percentages of the base output width, highest first.
const WIDTH_PERCENTS: [u32; 7] = [90, 80, 70, 60, 50, 40, 30];
/// gifski lossy quality steps, the last resort.
const LOSSY_STEPS: [u8; 3] = [80, 50, 30];
/// Video quality steps, applied below the project's quality.
const VIDEO_QUALITY_STEPS: [u8; 4] = [80, 60, 40, 20];
/// The narrowest output a width step may reach.
const MIN_WIDTH: u32 = 16;

/// One set of settings on the ladder, with a label for [`ExportEvent::TargetPass`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    /// What the settings are, for the UI.
    pub(crate) description: String,
    /// The export settings of this step.
    pub(crate) export: ExportSettings,
    /// The output settings of this step (frame rate and size).
    pub(crate) output: Output,
}

impl Candidate {
    /// The request this candidate encodes: the same clip, media and paths, with this step's settings and no budget, so
    /// the candidate runs as a plain export.
    pub(crate) fn apply(&self, req: &ExportRequest) -> ExportRequest {
        let mut next = req.clone();
        next.project.export = self.export.clone();
        next.project.export.target_bytes = None;
        next.project.output = self.output;
        next
    }
}

/// The ladder of settings the search tries, starting with the project's own settings. `base_width` is the output width
/// of those settings in pixels.
pub(crate) fn ladder(export: &ExportSettings, output: &Output, base_width: u32) -> Vec<Candidate> {
    let mut settings = export.clone();
    settings.target_bytes = None;
    let mut current = *output;
    let mut width = base_width;
    let mut steps = vec![candidate(&settings, &current, width)];
    let is_gif = settings.format == ExportFormat::Gif;
    let quality_engine = is_gif && settings.gif_engine == GifEngine::Quality;

    if quality_engine {
        for quality in GIF_QUALITY_STEPS {
            if quality < settings.quality {
                settings.quality = quality;
                steps.push(candidate(&settings, &current, width));
            }
        }
    } else if is_gif {
        for colors in COLOR_STEPS {
            if colors < settings.colors {
                settings.colors = colors;
                steps.push(candidate(&settings, &current, width));
            }
        }
    } else {
        for quality in VIDEO_QUALITY_STEPS {
            if quality < settings.quality {
                settings.quality = quality;
                steps.push(candidate(&settings, &current, width));
            }
        }
    }

    if is_gif {
        for (numerator, denominator) in FPS_STEPS {
            let fps = Rational::new(numerator, denominator);
            if fps.as_f64() < current.fps.as_f64() {
                current.fps = fps;
                steps.push(candidate(&settings, &current, width));
            }
        }
    }

    for percent in WIDTH_PERCENTS {
        let scaled = (f64::from(base_width) * f64::from(percent) / 100.0).round() as u32;
        let target = scaled.max(MIN_WIDTH);
        if target < width {
            width = target;
            current.size = OutputSize::Width { width };
            steps.push(candidate(&settings, &current, width));
        }
    }

    if quality_engine {
        for lossy in LOSSY_STEPS {
            if lossy < settings.lossy_quality {
                settings.lossy_quality = lossy;
                steps.push(candidate(&settings, &current, width));
            }
        }
    }
    steps
}

fn candidate(settings: &ExportSettings, output: &Output, width: u32) -> Candidate {
    Candidate {
        description: describe(settings, output, width),
        export: settings.clone(),
        output: *output,
    }
}

/// A short label for one set of settings, such as `GIF quality 80, lossy 100, 20 fps, 480 px wide`.
fn describe(settings: &ExportSettings, output: &Output, width: u32) -> String {
    let fps = output.fps.as_f64();
    match settings.format {
        ExportFormat::Gif => match settings.gif_engine {
            GifEngine::Quality => format!(
                "GIF quality {}, lossy {}, {fps} fps, {width} px wide",
                settings.quality, settings.lossy_quality
            ),
            GifEngine::Fast => format!(
                "GIF (FFmpeg), {} colours, {fps} fps, {width} px wide",
                settings.colors
            ),
            GifEngine::GpuDither => format!(
                "GIF (GPU dither), {} colours, {fps} fps, {width} px wide",
                settings.colors
            ),
        },
        ExportFormat::Mp4H264 => format!(
            "MP4 H.264, quality {}, {fps} fps, {width} px wide",
            settings.quality
        ),
        ExportFormat::Mp4Hevc => format!(
            "MP4 HEVC, quality {}, {fps} fps, {width} px wide",
            settings.quality
        ),
        ExportFormat::WebmVp9 => format!(
            "WebM VP9, quality {}, {fps} fps, {width} px wide",
            settings.quality
        ),
        ExportFormat::WebmAv1 => format!(
            "WebM AV1, quality {}, {fps} fps, {width} px wide",
            settings.quality
        ),
    }
}

/// The margin a step's estimate may exceed the budget by before the step is ruled out (see the module docs).
pub(crate) fn rule_out_margin(settings: &ExportSettings) -> f64 {
    if settings.format != ExportFormat::Gif {
        return VIDEO_MARGIN;
    }
    match settings.gif_engine {
        GifEngine::Quality => QUALITY_MARGIN,
        GifEngine::Fast => FAST_MARGIN,
        GifEngine::GpuDither => DITHER_MARGIN,
    }
}

/// Runs the search for a request whose project has a size budget, and returns the first output that fits.
///
/// A step whose plan is short enough to be estimated whole is encoded once, for real, without an estimate. A longer
/// step is estimated first, and encoded for real unless its estimate is above the budget times the margin of
/// [`rule_out_margin`]. Each step's pass event carries the real size when it was encoded, and the estimate when it was
/// ruled out. Each encode starts its progress count from zero.
///
/// # Errors
///
/// [`ExportError::TargetUnreachable`] when no step fits, with the smallest real encode (estimates do not count).
/// Otherwise the errors of [`encode_to_partial`] and of the sampled estimates, and [`ExportError::Cancelled`] when
/// `cancel` fires between steps or inside an estimate or an encode.
pub(crate) fn run_target(
    ctx: &ExportContext,
    req: &ExportRequest,
    started: Instant,
    on_event: &mut dyn FnMut(ExportEvent),
    cancel: &CancelToken,
) -> Result<ExportResult, ExportError> {
    let Some(budget) = req.project.export.target_bytes else {
        return Err(ExportError::InvalidRequest(
            "target-size mode needs a size budget".to_owned(),
        ));
    };
    let clip = validated_clip(ctx, req)?;
    let base_width = plan_export(req, clip)?.dims.width;
    let steps = ladder(&req.project.export, &req.project.output, base_width);
    // One GPU state for the whole search: every estimate and every encode of the steps reuses it.
    let mut kit = RenderKit::new(ctx, &req.project.grade)?;
    let margin = rule_out_margin(&req.project.export);
    let mut best: Option<u64> = None;
    for (index, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let pass = u32::try_from(index).unwrap_or(u32::MAX);
        let attempt = step.apply(req);
        let planned = plan_export(&attempt, clip)?;
        let window = sample_window(SAMPLE_SECONDS, planned.plan.fps);
        if !is_full_plan(planned.plan.len(), window) {
            let estimate = estimate_in(&mut kit, ctx, &attempt, SAMPLE_SECONDS, cancel)?;
            if estimate.bytes as f64 > budget as f64 * margin {
                // An estimate runs high, so it is reported for the pass but never counts as the smallest size reached.
                on_event(ExportEvent::TargetPass {
                    pass,
                    description: step.description.clone(),
                    bytes: estimate.bytes,
                });
                continue;
            }
        }
        let (frames, bytes) = encode_to_partial(&mut kit, ctx, &attempt, on_event, cancel)?;
        best = Some(smallest(best, bytes));
        on_event(ExportEvent::TargetPass {
            pass,
            description: step.description.clone(),
            bytes,
        });
        if bytes <= budget {
            return finish_output(req, frames, bytes, started, on_event);
        }
        let _ = fs::remove_file(partial_path(&req.output));
    }
    Err(ExportError::TargetUnreachable {
        target_bytes: budget,
        best_bytes: best.unwrap_or(0),
    })
}

fn smallest(best: Option<u64>, bytes: u64) -> u64 {
    best.map_or(bytes, |current| current.min(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcut_core::Rational;

    fn gif_quality() -> ExportSettings {
        ExportSettings::default()
    }

    fn base_output() -> Output {
        Output {
            fps: Rational::integer(20),
            size: OutputSize::Width { width: 480 },
            ..Output::default()
        }
    }

    fn width_of(candidate: &Candidate) -> u32 {
        match candidate.output.size {
            OutputSize::Width { width } => width,
            other => panic!("unexpected size {other:?}"),
        }
    }

    #[test]
    fn gif_quality_ladder_degrades_in_the_product_order() {
        let steps = ladder(&gif_quality(), &base_output(), 480);
        // Base, three quality steps, three fps steps, seven width steps, three lossy steps.
        assert_eq!(steps.len(), 1 + 3 + 3 + 7 + 3);
        let qualities: Vec<u8> = steps[..4].iter().map(|s| s.export.quality).collect();
        assert_eq!(qualities, vec![90, 80, 70, 60]);
        let fps: Vec<f64> = steps[4..7].iter().map(|s| s.output.fps.as_f64()).collect();
        assert_eq!(fps, vec![15.0, 12.5, 10.0]);
        let widths: Vec<u32> = steps[7..14].iter().map(width_of).collect();
        assert_eq!(widths, vec![432, 384, 336, 288, 240, 192, 144]);
        let lossy: Vec<u8> = steps[14..].iter().map(|s| s.export.lossy_quality).collect();
        assert_eq!(lossy, vec![80, 50, 30]);
    }

    #[test]
    fn each_step_keeps_the_steps_before_it() {
        let steps = ladder(&gif_quality(), &base_output(), 480);
        let last = steps.last().expect("steps");
        assert_eq!(last.export.quality, 60);
        assert_eq!(last.output.fps.as_f64(), 10.0);
        assert_eq!(width_of(last), 144);
        assert_eq!(last.export.lossy_quality, 30);
    }

    #[test]
    fn steps_only_go_down() {
        let mut settings = gif_quality();
        settings.quality = 65;
        settings.lossy_quality = 60;
        let output = Output {
            fps: Rational::integer(15),
            ..base_output()
        };
        let steps = ladder(&settings, &output, 300);
        // Quality 65 keeps only 60. Fps 15 keeps 12.5 and 10. Lossy 60 keeps 50 and 30.
        let qualities: Vec<u8> = steps.iter().map(|s| s.export.quality).collect();
        assert_eq!(qualities[0], 65);
        assert_eq!(qualities[1], 60);
        assert!(steps.iter().all(|s| s.output.fps.as_f64() <= 15.0));
        let fps: Vec<f64> = steps.iter().map(|s| s.output.fps.as_f64()).collect();
        assert!(fps.contains(&12.5) && fps.contains(&10.0) && !fps.contains(&20.0));
        let lossy: Vec<u8> = steps.iter().map(|s| s.export.lossy_quality).collect();
        assert!(lossy.contains(&50) && lossy.contains(&30) && !lossy.contains(&80));
    }

    #[test]
    fn widths_never_grow_and_never_go_below_the_minimum() {
        let steps = ladder(&gif_quality(), &base_output(), 20);
        let widths: Vec<u32> = steps
            .iter()
            .map(|s| match s.output.size {
                OutputSize::Width { width } => width,
                _ => 0,
            })
            .collect();
        assert!(widths.iter().all(|&w| w >= MIN_WIDTH || w == 0));
        assert!(widths.windows(2).all(|pair| pair[1] <= pair[0]));
    }

    #[test]
    fn fast_and_gpu_engines_step_their_palette_first() {
        for engine in [GifEngine::Fast, GifEngine::GpuDither] {
            let settings = ExportSettings {
                gif_engine: engine,
                colors: 256,
                ..ExportSettings::default()
            };
            let steps = ladder(&settings, &base_output(), 480);
            let colors: Vec<u16> = steps[..4].iter().map(|s| s.export.colors).collect();
            assert_eq!(colors, vec![256, 128, 64, 32], "{engine:?}");
            // The quality and lossy steps do not apply to these engines.
            assert!(steps.iter().all(|s| s.export.lossy_quality == 100));
        }
    }

    #[test]
    fn video_ladder_adjusts_quality_then_width() {
        let settings = ExportSettings {
            format: ExportFormat::Mp4H264,
            quality: 90,
            ..ExportSettings::default()
        };
        let steps = ladder(&settings, &base_output(), 480);
        let qualities: Vec<u8> = steps[..5].iter().map(|s| s.export.quality).collect();
        assert_eq!(qualities, vec![90, 80, 60, 40, 20]);
        assert!(steps.iter().all(|s| s.output.fps.as_f64() == 20.0));
        assert_eq!(steps.len(), 5 + 7);
    }

    #[test]
    fn applying_a_candidate_clears_the_budget_and_sets_its_output() {
        let req = ExportRequest {
            project: loopcut_core::Project::new(
                "p",
                loopcut_core::Clip {
                    id: "c".to_owned(),
                    path: "clip.mp4".to_owned(),
                    stream_index: 0,
                    segments: Vec::new(),
                    holds: Vec::new(),
                },
            ),
            media: loopcut_core::MediaInfo {
                container: "mp4".to_owned(),
                duration: 1.0,
                size_bytes: 0,
                video: loopcut_core::media::VideoStreamInfo {
                    index: 0,
                    codec: "h264".to_owned(),
                    profile: None,
                    width: 2,
                    height: 2,
                    sample_aspect: Rational::integer(1),
                    pix_fmt: "yuv420p".to_owned(),
                    bit_depth: 8,
                    has_alpha: false,
                    rotation: 0,
                    frame_rate: Rational::integer(30),
                    avg_frame_rate: None,
                    variable_frame_rate: false,
                    color: loopcut_core::media::ColorInfo::SDR_BT709,
                    start_time: 0.0,
                    duration: 1.0,
                    frame_count: None,
                    interlaced: false,
                    web_playable: true,
                },
                alternate_video_streams: Vec::new(),
                has_audio: false,
            },
            output: std::path::PathBuf::from("out.gif"),
            decode: loopcut_core::DecodeChain::CPU,
            encoder: loopcut_core::EncoderBackend::Cpu,
        };
        let mut req = req;
        req.project.export.target_bytes = Some(1000);
        let step = ladder(&req.project.export, &req.project.output, 480)
            .into_iter()
            .nth(4)
            .expect("a step with a new frame rate");
        let applied = step.apply(&req);
        assert_eq!(applied.project.export.target_bytes, None);
        assert_eq!(applied.project.output.fps, step.output.fps);
        assert_eq!(applied.output, req.output);
    }

    #[test]
    fn descriptions_name_the_settings() {
        let steps = ladder(&gif_quality(), &base_output(), 480);
        assert_eq!(
            steps[0].description,
            "GIF quality 90, lossy 100, 20 fps, 480 px wide"
        );
        assert!(steps[4].description.contains("15 fps"));
        assert!(steps[7].description.contains("432 px wide"));
    }

    #[test]
    fn each_engine_has_its_own_margin() {
        let with = |format, gif_engine| ExportSettings {
            format,
            gif_engine,
            ..ExportSettings::default()
        };
        assert_eq!(
            rule_out_margin(&with(ExportFormat::Gif, GifEngine::Quality)),
            QUALITY_MARGIN
        );
        assert_eq!(
            rule_out_margin(&with(ExportFormat::Gif, GifEngine::Fast)),
            FAST_MARGIN
        );
        assert_eq!(
            rule_out_margin(&with(ExportFormat::Gif, GifEngine::GpuDither)),
            DITHER_MARGIN
        );
        assert_eq!(
            rule_out_margin(&with(ExportFormat::Mp4H264, GifEngine::Quality)),
            VIDEO_MARGIN
        );
    }

    #[test]
    fn smallest_keeps_the_lowest_size() {
        assert_eq!(smallest(None, 9), 9);
        assert_eq!(smallest(Some(4), 9), 4);
        assert_eq!(smallest(Some(12), 9), 9);
    }
}

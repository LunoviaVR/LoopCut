//! Preview engine tests on the software (WARP) adapter, and on the hardware adapter when one is present.
//!
//! Run with `cargo test -p loopcut-preview --test preview`. Every test needs the staged FFmpeg build and a WARP
//! adapter; without them it prints a notice and passes. The latency benchmark is `#[ignore]`d:
//! `cargo test -p loopcut-preview --test preview --release -- --ignored --nocapture`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use loopcut_core::geometry::{NormRect, OutputSize, output_dimensions};
use loopcut_core::sequencer::{self, Sample};
use loopcut_core::{Clip, DecodeChain, Direction, Project, TimeRange};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use loopcut_media::{FfmpegCommand, FfmpegPaths, InputOptions, MediaError, SpawnIo, probe};
use loopcut_preview::{
    PreviewEngine, PreviewError, PreviewQuality, PreviewSource, RenderedFrame, Viewport,
};
use loopcut_render::caption_layers::CaptionRenderer;
use loopcut_render::gpu::{AdapterChoice, GpuContext};

/// Every adapter the tests run on: WARP always, the default hardware adapter when it is hardware.
///
/// One context per adapter is shared by the whole test binary, as in the app. Creating devices from several threads at
/// once is what the compositor tests found to be unsafe.
fn contexts() -> Vec<(&'static str, Arc<GpuContext>)> {
    static CONTEXTS: OnceLock<Vec<(&'static str, Arc<GpuContext>)>> = OnceLock::new();
    CONTEXTS
        .get_or_init(|| {
            let mut list = Vec::new();
            match GpuContext::new(&AdapterChoice::Software) {
                Ok(ctx) => list.push(("software", Arc::new(ctx))),
                Err(err) => eprintln!("SKIP: no WARP adapter: {err}"),
            }
            match GpuContext::new(&AdapterChoice::Auto) {
                Ok(ctx) if !ctx.is_software() => list.push(("hardware", Arc::new(ctx))),
                _ => eprintln!("SKIP: no hardware adapter; the hardware pass is not run"),
            }
            list
        })
        .clone()
}

/// The staged FFmpeg build, or `None` (after a notice) when it is missing. With `LOOPCUT_REQUIRE_FFMPEG=1` a missing
/// build fails the test instead.
fn paths() -> Option<FfmpegPaths> {
    let found = FfmpegPaths::discover_dev();
    if found.is_none() {
        if std::env::var("LOOPCUT_REQUIRE_FFMPEG").is_ok_and(|value| value == "1") {
            panic!("LOOPCUT_REQUIRE_FFMPEG=1 but no staged FFmpeg build was found");
        }
        eprintln!(
            "SKIP: no staged FFmpeg build (set LOOPCUT_FFMPEG_DIR or stage src-tauri/resources/ffmpeg)"
        );
    }
    found
}

/// A file from the shared media corpus (`crates/loopcut-media/tests/media/generated`), or `None` when it is absent.
fn corpus(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("loopcut-media")
        .join("tests")
        .join("media")
        .join("generated")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("SKIP: corpus file {name} is missing");
        None
    }
}

/// An engine with its own frame store. The temporary folder lives as long as the returned guard.
fn engine_on(gpu: &Arc<GpuContext>, paths: &FfmpegPaths) -> (PreviewEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FrameStore::new(FrameStoreConfig {
        ram_budget_bytes: 512 * 1024 * 1024,
        disk_budget_bytes: 2 * 1024 * 1024 * 1024,
        spill_dir: dir.path().join("spill"),
    })
    .expect("frame store");
    let captions = Arc::new(Mutex::new(CaptionRenderer::new()));
    let engine = PreviewEngine::new(Arc::clone(gpu), paths.clone(), Arc::new(store), captions)
        .expect("engine");
    (engine, dir)
}

fn load(paths: &FfmpegPaths, file: &Path, decode: DecodeChain) -> PreviewSource {
    PreviewSource {
        path: file.to_path_buf(),
        media: probe(paths, file).expect("probe the test clip"),
        decode,
    }
}

/// A project that keeps the whole clip, with default output settings (480 px GIF at 20 fps).
fn project_for(source: &PreviewSource) -> Project {
    let video = &source.media.video;
    let duration = if video.duration > 0.0 {
        video.duration
    } else {
        source.media.duration
    };
    let clip = Clip {
        id: "clip".to_owned(),
        path: source.path.to_string_lossy().into_owned(),
        stream_index: video.index,
        segments: vec![TimeRange::new(0.0, duration)],
        holds: Vec::new(),
    };
    Project::new("test", clip)
}

fn viewport(width: u32, height: u32) -> Viewport {
    Viewport {
        width,
        height,
        dpr: 1.0,
    }
}

/// Waits until the background worker has decoded every planned range.
fn wait_decoded(engine: &PreviewEngine) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while engine.decode_progress() < 1.0 {
        assert!(Instant::now() < deadline, "the decode did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn digest(frame: &RenderedFrame) -> u64 {
    let mut hasher = DefaultHasher::new();
    frame.width.hash(&mut hasher);
    frame.height.hash(&mut hasher);
    frame.rgba.hash(&mut hasher);
    hasher.finish()
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "frames differ in size");
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / a.len().max(1) as f64
}

fn assert_opaque(frame: &RenderedFrame) {
    assert_eq!(
        frame.rgba.len(),
        frame.width as usize * frame.height as usize * 4
    );
    assert!(
        frame.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
        "output must be opaque"
    );
}

/// Generates a clip with lavfi into `out`, encoded with `output_args`.
fn generate(
    paths: &FfmpegPaths,
    graph: &str,
    output_args: &[&str],
    out: &Path,
) -> Result<(), MediaError> {
    let cmd = FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input(
            graph,
            &InputOptions {
                lavfi: true,
                ..InputOptions::default()
            },
        )?
        .output_args(output_args.iter().copied())
        .expect("valid FFmpeg argument")
        .output(out)?;
    cmd.spawn(SpawnIo::default())?.wait().map(|_| ())
}

#[test]
fn set_project_matches_the_plan_and_the_geometry() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let project = project_for(&source);
        let info = engine
            .set_project(source, &project, viewport(640, 360))
            .expect("set_project");
        let plan = sequencer::plan(
            project.clip().expect("clip"),
            &project.timing,
            project.output.fps,
        )
        .expect("plan");

        assert_eq!(info.frames as usize, plan.len(), "{label}: frame count");
        assert_eq!(info.fps, plan.fps, "{label}: fps");
        let delays: Vec<u16> = plan.frames.iter().map(|frame| frame.delay_cs).collect();
        assert_eq!(info.delays_cs, delays, "{label}: delays");
        let output = output_dimensions(320.0, 240.0, project.output.size);
        assert_eq!(
            (info.output_width, info.output_height),
            output,
            "{label}: output size"
        );
        // The 480x360 output fits the 640x360 panel without enlarging.
        assert_eq!(
            (info.render_width, info.render_height),
            output,
            "{label}: render size"
        );
        assert_eq!(info.quality, PreviewQuality::Full);
    }
}

#[test]
fn output_frames_render_at_the_plan_size_and_change_over_time() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let project = project_for(&source);
        let info = engine
            .set_project(source, &project, viewport(640, 360))
            .expect("set_project");
        let last = info.frames - 1;
        let first = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("frame 0");
        let middle = engine
            .render_output_frame(info.frames / 2, PreviewQuality::Full)
            .expect("middle frame");
        let end = engine
            .render_output_frame(last, PreviewQuality::Full)
            .expect("last frame");
        for frame in [&first, &middle, &end] {
            assert_eq!(
                (frame.width, frame.height),
                (480, 360),
                "{label}: dimensions"
            );
            assert_opaque(frame);
        }
        assert_eq!(
            first.to_wire().len(),
            8 + first.rgba.len(),
            "{label}: wire header"
        );
        assert_ne!(
            first.rgba, end.rgba,
            "{label}: the clip moves, so first and last differ"
        );
        assert!(matches!(
            engine.render_output_frame(info.frames, PreviewQuality::Full),
            Err(PreviewError::FrameOutOfRange { .. })
        ));
    }
}

#[test]
fn crop_grade_and_timing_edits_do_not_decode_again() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        engine
            .set_project(source.clone(), &project, viewport(640, 360))
            .expect("first set_project");
        wait_decoded(&engine);
        let decodes = engine.decode_sessions_started();
        let full = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("full frame");

        project.transform.crop = NormRect {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        project.grade.exposure = 0.5;
        project.timing.speed = 2.0;
        let info = engine
            .set_project(source, &project, viewport(640, 360))
            .expect("second set_project");
        let cropped = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("cropped frame");

        assert_eq!(
            engine.decode_sessions_started(),
            decodes,
            "{label}: edits must not re-decode"
        );
        assert_eq!(
            (cropped.width, cropped.height),
            (480, 360),
            "{label}: the output keeps its width"
        );
        assert_eq!(
            info.output_width, 480,
            "{label}: output width is fixed by the project"
        );
        assert_ne!(
            cropped.rgba, full.rgba,
            "{label}: the crop changes the picture"
        );
    }
}

#[test]
fn cut_edits_decode_only_the_range_that_changed() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        project.clips[0].segments = vec![TimeRange::new(0.0, 0.3), TimeRange::new(0.5, 0.95)];
        engine
            .set_project(source.clone(), &project, viewport(640, 360))
            .expect("first set_project");
        wait_decoded(&engine);
        let decodes = engine.decode_sessions_started();

        project.clips[0].segments[1].end = 0.8;
        engine
            .set_project(source, &project, viewport(640, 360))
            .expect("trimmed set_project");
        wait_decoded(&engine);
        assert_eq!(
            engine.decode_sessions_started(),
            decodes + 1,
            "{label}: only the trimmed range is decoded again"
        );
    }
}

#[test]
fn source_frames_follow_the_display_rotation() {
    let (Some(paths), Some(file)) = (paths(), corpus("portrait_rotated.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let (display_w, display_h) = source.media.video.display_size();
        let project = project_for(&source);
        engine
            .set_project(source, &project, viewport(1000, 1000))
            .expect("set_project");
        let frame = engine
            .render_source_frame(0.0, 1000, 1000)
            .expect("source frame");
        assert_opaque(&frame);
        let wanted = f64::from(display_w) / f64::from(display_h);
        let got = f64::from(frame.width) / f64::from(frame.height);
        assert!(
            (got - wanted).abs() / wanted < 0.02,
            "{label}: aspect {got} vs display {wanted}"
        );
        assert_eq!(
            frame.height > frame.width,
            display_h > display_w,
            "{label}: orientation"
        );
        assert!(
            frame.width <= 1000 && frame.height <= 1000,
            "{label}: the box is a maximum"
        );
    }
}

#[test]
fn source_frames_outside_the_plan_decode_a_window_and_match_the_plan() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let source = load(&paths, &file, DecodeChain::CPU);
        let duration = source.media.video.duration;

        // Reference: a plan that covers the whole clip.
        let (whole, _whole_dir) = engine_on(&gpu, &paths);
        let full_project = project_for(&source);
        whole
            .set_project(source.clone(), &full_project, viewport(640, 360))
            .expect("whole-clip set_project");
        let reference = whole
            .render_source_frame(duration * 0.75, 320, 240)
            .expect("reference frame from the plan");

        // Under test: a plan that covers only the first quarter, so the same time needs a window.
        let (partial, _partial_dir) = engine_on(&gpu, &paths);
        let mut short = project_for(&source);
        short.clips[0].segments = vec![TimeRange::new(0.0, duration * 0.25)];
        partial
            .set_project(source.clone(), &short, viewport(640, 360))
            .expect("partial set_project");
        let window = partial
            .render_source_frame(duration * 0.75, 320, 240)
            .expect("frame from a window outside the plan");
        assert_eq!(
            (window.width, window.height),
            (reference.width, reference.height),
            "{label}: dimensions"
        );
        let diff = mean_abs_diff(&window.rgba, &reference.rgba);
        assert!(
            diff <= 1.0,
            "{label}: the window frame differs from the plan's frame by {diff}"
        );

        // A time past the end of the source is clamped to it instead of failing.
        let past_end = partial
            .render_source_frame(duration + 10.0, 320, 240)
            .expect("a time past the end is clamped");
        assert_opaque(&past_end);
    }
}

#[test]
fn hardware_decode_matches_the_cpu_decode() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some((_, gpu)) = contexts().into_iter().next() else {
        return;
    };
    // The D3D11VA chain without a GPU scaler, verified on the dev adapter (docs/architecture.md, spike (g)). The
    // engine scales on the CPU after the download, as it does for the CPU chain, so the two decodes should agree.
    let d3d11va = DecodeChain {
        backend: loopcut_core::DecodeBackend::D3d11va { dxgi_index: 0 },
        scaler: loopcut_core::GpuScaler::None,
    };
    let render = |decode: DecodeChain| {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, decode);
        let project = project_for(&source);
        engine
            .set_project(source, &project, viewport(640, 360))
            .expect("set_project");
        wait_decoded(&engine);
        let frame = engine
            .render_output_frame(5, PreviewQuality::Full)
            .expect("frame 5");
        (frame, engine.take_fallbacks())
    };
    let (cpu, _) = render(DecodeChain::CPU);
    let (hardware, fallbacks) = render(d3d11va);
    if !fallbacks.is_empty() {
        // The chain is not usable on this machine; the fallback is covered by its own test.
        eprintln!(
            "SKIP: the D3D11VA chain fell back here: {}",
            fallbacks[0].reason
        );
        return;
    }
    assert_eq!((hardware.width, hardware.height), (cpu.width, cpu.height));
    let diff = mean_abs_diff(&hardware.rgba, &cpu.rgba);
    assert!(
        diff <= 2.0,
        "the hardware decode differs from the CPU decode by {diff}"
    );
}

#[test]
fn playback_delivers_the_plan_in_real_time_and_stops() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let project = project_for(&source);
        let info = engine
            .set_project(source, &project, viewport(640, 360))
            .expect("set_project");
        // The first frames wait for the decode, which is not part of the pacing measured below.
        wait_decoded(&engine);
        let expected: Duration = info
            .delays_cs
            .iter()
            .map(|cs| Duration::from_millis(u64::from(*cs) * 10))
            .sum();

        let shown: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_times = Arc::clone(&shown);
        let started = Instant::now();
        let handle = engine
            .play(
                0,
                false,
                Box::new(move |_frame| {
                    sink_times.lock().expect("times").push(Instant::now());
                    true
                }),
            )
            .expect("play");
        handle.wait();
        let elapsed = started.elapsed();

        let times = shown.lock().expect("times").clone();
        // Late frames are dropped rather than shown late, so under load a few may be missing. Unloaded, all arrive.
        let planned = info.frames as usize;
        assert!(
            times.len() <= planned && times.len() * 10 >= planned * 9,
            "{label}: delivered {} of {planned}",
            times.len()
        );
        let low = expected.mul_f64(0.75);
        let high = expected.mul_f64(1.25);
        assert!(
            elapsed >= low && elapsed <= high,
            "{label}: played in {elapsed:?}, expected about {expected:?}"
        );
    }
}

#[test]
fn looping_repeats_the_plan_from_its_first_frame() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        // A one-frame plan: under load a playback thread may drop late frames, so with several frames the position
        // of the wrap is not fixed. With one frame, every delivered frame must be the first frame of the plan.
        project.clips[0].segments = vec![TimeRange::new(0.0, 0.05)];
        let info = engine
            .set_project(source, &project, viewport(640, 360))
            .expect("set_project");
        assert_eq!(info.frames, 1, "{label}: one frame planned");
        let expected = digest(
            &engine
                .render_output_frame(0, PreviewQuality::Reduced)
                .expect("frame 0"),
        );
        let target = 4;

        let digests: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_digests = Arc::clone(&digests);
        let handle = engine
            .play(
                0,
                true,
                Box::new(move |frame| {
                    let mut seen = sink_digests.lock().expect("digests");
                    seen.push(digest(frame));
                    seen.len() < target
                }),
            )
            .expect("play");
        handle.wait();

        let seen = digests.lock().expect("digests").clone();
        assert_eq!(
            seen.len(),
            target,
            "{label}: a looping plan runs until the sink stops it"
        );
        assert!(
            seen.iter().all(|&d| d == expected),
            "{label}: every loop pass shows the plan's first frame"
        );
    }
}

#[test]
fn crossfade_blends_the_two_source_frames() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        project.timing.crossfade = 0.3;
        project.output.size = OutputSize::Width { width: 320 };
        engine
            .set_project(source, &project, viewport(320, 240))
            .expect("set_project");
        let plan = sequencer::plan(
            project.clip().expect("clip"),
            &project.timing,
            project.output.fps,
        )
        .expect("plan");
        let (a, b, weight) = match plan.frames[0].sample {
            Sample::Blend { a, b, weight_b } => (a, b, weight_b),
            other => panic!("{label}: the first frame of a crossfade is a blend, got {other:?}"),
        };

        let blended = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("blended frame");
        let from_a = engine
            .render_source_frame(a, 320, 240)
            .expect("source at a");
        let from_b = engine
            .render_source_frame(b, 320, 240)
            .expect("source at b");
        assert_eq!(
            (blended.width, blended.height),
            (from_a.width, from_a.height),
            "{label}: dimensions"
        );
        // The compositor mixes in the encoded domain, the same way on both sides, so the result is the weighted mean.
        let expected: Vec<u8> = from_a
            .rgba
            .iter()
            .zip(&from_b.rgba)
            .enumerate()
            .map(|(i, (&x, &y))| {
                if i % 4 == 3 {
                    255
                } else {
                    (f64::from(x) * (1.0 - f64::from(weight)) + f64::from(y) * f64::from(weight))
                        .round() as u8
                }
            })
            .collect();
        let diff = mean_abs_diff(&blended.rgba, &expected);
        assert!(
            diff <= 1.0,
            "{label}: mean difference {diff} from the weighted mix"
        );
    }
}

#[test]
fn reverse_direction_shows_the_last_source_frame_first() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        project.timing.direction = Direction::Reverse;
        // Output the same size as the source, so the output frame and the source frame compare pixel for pixel.
        project.output.size = OutputSize::Width { width: 320 };
        let info = engine
            .set_project(source, &project, viewport(320, 240))
            .expect("set_project");
        let plan = sequencer::plan(
            project.clip().expect("clip"),
            &project.timing,
            project.output.fps,
        )
        .expect("plan");
        let t_first = match plan.frames[0].sample {
            Sample::At { t } => t,
            Sample::Blend { a, .. } => a,
        };
        let duration = project.clip().expect("clip").segments[0].duration();
        assert!(
            t_first > duration - 0.2,
            "{label}: reverse starts near the end, got {t_first}"
        );

        let output = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("reversed frame 0");
        let source_frame = engine
            .render_source_frame(t_first, 320, 240)
            .expect("source frame at the same time");
        assert_eq!(
            (output.width, output.height),
            (source_frame.width, source_frame.height)
        );
        let diff = mean_abs_diff(&output.rgba, &source_frame.rgba);
        assert!(
            diff <= 1.0,
            "{label}: mean difference {diff} between the reversed frame and the source"
        );
        assert_eq!(info.frames as usize, plan.len(), "{label}: frame count");
    }
}

#[test]
fn invalid_requests_are_refused_before_any_work() {
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some((_, gpu)) = contexts().into_iter().next() else {
        return;
    };
    let (engine, _dir) = engine_on(&gpu, &paths);
    assert!(matches!(
        engine.render_output_frame(0, PreviewQuality::Full),
        Err(PreviewError::NoProject)
    ));
    assert!(matches!(
        engine.render_source_frame(f64::NAN, 100, 100),
        Err(PreviewError::InvalidRequest(_))
    ));

    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    assert!(matches!(
        engine.set_project(source.clone(), &project, viewport(0, 360)),
        Err(PreviewError::InvalidViewport(_))
    ));
    let mut wrong_stream = project_for(&source);
    wrong_stream.clips[0].stream_index += 1;
    assert!(matches!(
        engine.set_project(source, &wrong_stream, viewport(640, 360)),
        Err(PreviewError::InvalidProject(_))
    ));
}

/// Render latency at 720p on each adapter. Not a pass/fail test: it prints the per-frame cost.
#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn render_latency_at_720p() {
    let Some(paths) = paths() else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("hd.mp4");
    generate(
        &paths,
        "testsrc2=size=1280x720:rate=30:duration=2",
        &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        &file,
    )
    .expect("generate the 720p clip");

    for (label, gpu) in contexts() {
        let (engine, _store_dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        project.output.size = OutputSize::Width { width: 1280 };
        let info = engine
            .set_project(source, &project, viewport(1280, 720))
            .expect("set_project");
        wait_decoded(&engine);
        let frames = info.frames;
        // Warm the pipeline and the compositor's buffers before timing.
        for frame in 0..3 {
            engine
                .render_output_frame(frame, PreviewQuality::Full)
                .expect("warm-up");
        }
        for quality in [PreviewQuality::Full, PreviewQuality::Reduced] {
            let started = Instant::now();
            let count = 30u32;
            let mut size = (0, 0);
            for i in 0..count {
                let frame = engine
                    .render_output_frame(i % frames, quality)
                    .expect("render");
                size = (frame.width, frame.height);
            }
            let per_frame = started.elapsed().as_secs_f64() * 1000.0 / f64::from(count);
            println!(
                "{label} ({}): {quality:?} {}x{} frame: {per_frame:.2} ms per frame",
                gpu.desc.name, size.0, size.1
            );
        }
    }
}

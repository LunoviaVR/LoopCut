//! Integration tests for the export orchestrator: real decode, GPU composite and encode through the pinned FFmpeg.
//!
//! Every test returns early (with a note) when no staged FFmpeg build exists, so `cargo test` stays green without the
//! bundle. `LOOPCUT_REQUIRE_FFMPEG=1` and `LOOPCUT_REQUIRE_GPU_TESTS=1` turn a missing FFmpeg build or software adapter into
//! a failure, for runs where a skip must not pass. Renders use the software (WARP) adapter, which makes results
//! deterministic. Source clips come from the shared corpus in `loopcut-media`, or from small generated clips whose frames
//! can be told apart by their luma.
//!
//! Tests that need real hardware are `#[ignore]` smoke tests. Run them on the target machine with
//! `cargo test -p loopcut-export --test export -- --ignored --nocapture`.
//!
//! Run the timing benchmark with `cargo test -p loopcut-export --test export --release -- --ignored --nocapture`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use loopcut_core::geometry::{NormRect, OutputSize};
use loopcut_core::gif_timing::delays_cs;
use loopcut_core::hw::FORCE_HW_FAIL_ENV;
use loopcut_core::project::{
    Caption, CaptionAnimation, CaptionStyle, Direction, Dither, Grade, Hold,
};
use loopcut_core::{
    Clip, DecodeBackend, DecodeChain, EncoderBackend, ExportFormat, GifEngine, GpuScaler,
    MediaInfo, PixelLayout, Project, Rational, TimeRange,
};
use loopcut_export::frames::{Access, DecodeGeometry, DecodeOptions, SourceFrames};
use loopcut_export::job::partial_path;
use loopcut_export::{
    EstimateBasis, ExportContext, ExportError, ExportEvent, ExportRequest, ExportResult,
    SizeEstimate, estimate_size, estimate_size_with_cancel, run_export,
};
use loopcut_media::decode::{DecodeRequest, DecodeSession};
use loopcut_media::framestore::{FrameKey, FrameStore, FrameStoreConfig, StoredFrame};
use loopcut_media::{
    CancelToken, FfmpegCommand, FfmpegPaths, InputOptions, SpawnIo, probe, run_capture,
};
use loopcut_render::gpu::{AdapterChoice, GpuContext};

/// Upper bound for one command, so a hang fails the test instead of stalling the run.
const TIMEOUT: Duration = Duration::from_secs(120);

const MB: u64 = 1024 * 1024;

/// Frames in the level clip (see [`level_clip`]).
const LEVEL_FRAMES: usize = 20;

/// Serializes changes to the process environment. Every export holds the shared side while it runs, because the decode
/// and encode paths read `LOOPCUT_FORCE_HW_FAIL`. A test that sets the variable holds the exclusive side, so no export
/// reads the variable while it changes.
static ENV_LOCK: RwLock<()> = RwLock::new(());

/// Path of a corpus file in `loopcut-media`.
fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("loopcut-media")
        .join("tests")
        .join("media")
        .join("generated")
        .join(name)
}

/// Set to `1` to make a missing FFmpeg build a failure, so the tests cannot pass by skipping (as in `loopcut-preview`).
const REQUIRE_FFMPEG_ENV: &str = "LOOPCUT_REQUIRE_FFMPEG";

/// Set to `1` to make a missing software GPU adapter a failure. The grade, caption and GPU tests need it.
const REQUIRE_GPU_ENV: &str = "LOOPCUT_REQUIRE_GPU_TESTS";

/// True when the environment variable `name` is set to `1`.
fn required(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value.trim() == "1")
}

/// The staged FFmpeg build, or `None` (after a note) when it is missing. With `LOOPCUT_REQUIRE_FFMPEG=1` a missing build
/// fails the test.
fn paths() -> Option<FfmpegPaths> {
    let found = FfmpegPaths::discover_dev();
    if found.is_none() {
        assert!(
            !required(REQUIRE_FFMPEG_ENV),
            "{REQUIRE_FFMPEG_ENV}=1 but no staged FFmpeg build was found"
        );
        eprintln!(
            "skipping: no staged FFmpeg build (set LOOPCUT_FFMPEG_DIR or stage src-tauri/resources/ffmpeg)"
        );
    }
    found
}

/// One software (WARP) device shared by every test in this binary. With `LOOPCUT_REQUIRE_GPU_TESTS=1` a missing adapter
/// fails the test.
fn software_gpu() -> Option<Arc<GpuContext>> {
    static GPU: OnceLock<Option<Arc<GpuContext>>> = OnceLock::new();
    GPU.get_or_init(|| match GpuContext::new(&AdapterChoice::Software) {
        Ok(context) => Some(Arc::new(context)),
        Err(err) => {
            assert!(
                !required(REQUIRE_GPU_ENV),
                "{REQUIRE_GPU_ENV}=1 but there is no software GPU adapter: {err}"
            );
            eprintln!("skipping: no software GPU adapter: {err}");
            None
        }
    })
    .clone()
}

/// An export context with its own frame store, which the temporary folder keeps alive.
struct Env {
    ctx: ExportContext,
    _dir: tempfile::TempDir,
    /// The shared side of [`ENV_LOCK`], held for as long as the context lives. `None` when created by [`store_env`].
    _shared: Option<RwLockReadGuard<'static, ()>>,
}

/// An export context with a frame store of the given budgets, without taking the environment lock. Use it while a
/// [`ForcedFailure`] is alive, because [`env_with_budget`] waits for that lock.
fn store_env(paths: FfmpegPaths, gpu: Arc<GpuContext>, ram_bytes: u64, disk_bytes: u64) -> Env {
    let dir = tempfile::tempdir().expect("temporary folder");
    let framestore = FrameStore::new(FrameStoreConfig {
        ram_budget_bytes: ram_bytes,
        disk_budget_bytes: disk_bytes,
        spill_dir: dir.path().join("spill"),
    })
    .expect("frame store");
    Env {
        ctx: ExportContext {
            paths,
            gpu,
            framestore: Arc::new(framestore),
            captions: None,
        },
        _dir: dir,
        _shared: None,
    }
}

/// An export context with the given frame-store budgets. Holds the shared environment lock until it is dropped.
fn env_with_budget(
    paths: FfmpegPaths,
    gpu: Arc<GpuContext>,
    ram_bytes: u64,
    disk_bytes: u64,
) -> Env {
    let shared = ENV_LOCK.read().unwrap_or_else(PoisonError::into_inner);
    Env {
        _shared: Some(shared),
        ..store_env(paths, gpu, ram_bytes, disk_bytes)
    }
}

/// An export context with the default budgets (256 MB in RAM, 2 GB on disk).
fn env(paths: FfmpegPaths, gpu: Arc<GpuContext>) -> Env {
    env_with_budget(paths, gpu, 256 * MB, 2048 * MB)
}

/// Sets `LOOPCUT_FORCE_HW_FAIL` for as long as it lives. It holds the exclusive side of [`ENV_LOCK`], so no export can
/// run meanwhile. Build the export context with [`store_env`] while it lives.
struct ForcedFailure {
    _exclusive: RwLockWriteGuard<'static, ()>,
}

impl ForcedFailure {
    fn set(spec: &str) -> Self {
        let exclusive = ENV_LOCK.write().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: the exclusive lock excludes every export in this test binary, and each export holds the shared lock
        // while it reads the variable. No other test thread writes the environment, so this write does not race a read
        // of the variable.
        unsafe { std::env::set_var(FORCE_HW_FAIL_ENV, spec) };
        Self {
            _exclusive: exclusive,
        }
    }
}

impl Drop for ForcedFailure {
    fn drop(&mut self) {
        // SAFETY: as in `set`. The exclusive lock is still held, because the guard field is dropped after this body.
        unsafe { std::env::remove_var(FORCE_HW_FAIL_ENV) };
    }
}

fn media(paths: &FfmpegPaths, file: &Path) -> MediaInfo {
    probe(paths, file).expect("probe test media")
}

/// A project that keeps `segments` of `file`, with the default output settings.
fn project(media: &MediaInfo, file: &Path, segments: &[(f64, f64)]) -> Project {
    let clip = Clip {
        id: "clip-1".to_owned(),
        path: file.to_string_lossy().into_owned(),
        stream_index: media.video.index,
        segments: segments
            .iter()
            .map(|&(a, b)| TimeRange::new(a, b))
            .collect(),
        holds: Vec::new(),
    };
    Project::new("test", clip)
}

fn request(
    project: Project,
    media: MediaInfo,
    output: PathBuf,
    decode: DecodeChain,
    encoder: EncoderBackend,
) -> ExportRequest {
    ExportRequest {
        project,
        media,
        output,
        decode,
        encoder,
    }
}

/// Runs an export and returns its result with every event it sent.
fn run(
    env: &Env,
    req: &ExportRequest,
    cancel: &CancelToken,
) -> (Result<ExportResult, ExportError>, Vec<ExportEvent>) {
    let mut events = Vec::new();
    let result = run_export(&env.ctx, req, &mut |event| events.push(event), cancel);
    (result, events)
}

/// Runs an export that must succeed, and returns its result and events.
fn run_ok(env: &Env, req: &ExportRequest) -> (ExportResult, Vec<ExportEvent>) {
    let (result, events) = run(env, req, &CancelToken::new());
    (result.expect("export succeeds"), events)
}

fn fallbacks(events: &[ExportEvent]) -> Vec<(&str, &str, &str)> {
    events
        .iter()
        .filter_map(|event| match event {
            ExportEvent::Fallback {
                stage, from, to, ..
            } => Some((stage.as_str(), from.as_str(), to.as_str())),
            _ => None,
        })
        .collect()
}

/// A decoded GIF: canvas size, play count, per-frame delays and the full-canvas RGBA pixels of each frame.
///
/// gifski stores each frame as the rectangle that changed, so every frame is composited onto the canvas here, as a
/// viewer does. Transparent pixels in a frame keep what was on the canvas.
struct Gif {
    width: u16,
    height: u16,
    repeat: gif::Repeat,
    delays: Vec<u16>,
    frames: Vec<Vec<u8>>,
}

fn read_gif(path: &Path) -> Gif {
    let file = File::open(path).expect("open GIF");
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options.read_info(file).expect("read GIF header");
    let (width, height, repeat) = (decoder.width(), decoder.height(), decoder.repeat());
    let mut canvas = vec![0u8; usize::from(width) * usize::from(height) * 4];
    let mut delays = Vec::new();
    let mut frames = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("decode GIF frame") {
        assert!(
            u32::from(frame.left) + u32::from(frame.width) <= u32::from(width)
                && u32::from(frame.top) + u32::from(frame.height) <= u32::from(height),
            "frame box lies inside the canvas"
        );
        delays.push(frame.delay);
        composite(&mut canvas, width, frame);
        frames.push(canvas.clone());
        if frame.dispose == gif::DisposalMethod::Background {
            clear_box(&mut canvas, width, frame);
        }
    }
    Gif {
        width,
        height,
        repeat,
        delays,
        frames,
    }
}

/// Copies the opaque pixels of `frame` onto `canvas` at the frame's position.
fn composite(canvas: &mut [u8], canvas_width: u16, frame: &gif::Frame<'_>) {
    let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
    let stride = usize::from(canvas_width) * 4;
    for y in 0..fh {
        for x in 0..fw {
            let src = (y * fw + x) * 4;
            if frame.buffer[src + 3] == 0 {
                continue;
            }
            let dst = (usize::from(frame.top) + y) * stride + (usize::from(frame.left) + x) * 4;
            canvas[dst..dst + 4].copy_from_slice(&frame.buffer[src..src + 4]);
        }
    }
}

/// Clears the frame's rectangle to transparent, for the "restore to background" disposal method.
fn clear_box(canvas: &mut [u8], canvas_width: u16, frame: &gif::Frame<'_>) {
    let stride = usize::from(canvas_width) * 4;
    for y in 0..usize::from(frame.height) {
        let row = (usize::from(frame.top) + y) * stride + usize::from(frame.left) * 4;
        canvas[row..row + usize::from(frame.width) * 4].fill(0);
    }
}

/// Mean BT.601 luma of a tightly packed RGBA frame, ignoring alpha.
fn mean_luma(rgba: &[u8]) -> f64 {
    let total: f64 = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| 0.299 * f64::from(px[0]) + 0.587 * f64::from(px[1]) + 0.114 * f64::from(px[2]))
        .sum();
    total / (rgba.len() / 4) as f64
}

/// Mean luma of every frame of a video file, from the Y plane of each frame decoded on the CPU.
fn decoded_luma(paths: &FfmpegPaths, file: &Path) -> Vec<f64> {
    let info = media(paths, file);
    let video = &info.video;
    let request = DecodeRequest {
        input: file.to_path_buf(),
        stream_index: video.index,
        coded_width: video.width,
        coded_height: video.height,
        bit_depth: video.bit_depth,
        has_alpha: video.has_alpha,
        codec: video.codec.clone(),
        color: video.color,
        start_time: video.start_time,
        range: None,
        crop: None,
        size: None,
        fps: Some(video.frame_rate),
        chain: DecodeChain::CPU,
        layout: PixelLayout::Nv12,
    };
    let session =
        DecodeSession::start(paths, &request, CancelToken::new()).expect("decode the export");
    session
        .map(|frame| {
            let frame = frame.expect("frame decodes");
            let luma = &frame.data[..(frame.width * frame.height) as usize];
            luma.iter().map(|&value| f64::from(value)).sum::<f64>() / luma.len() as f64
        })
        .collect()
}

/// Runs ffprobe on `file` with `args` and returns its stdout.
fn ffprobe(paths: &FfmpegPaths, file: &Path, args: &[&str]) -> String {
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(file, &InputOptions::default())
        .expect("ffprobe input")
        .output_args(args.iter().copied())
        .expect("ffprobe arguments");
    String::from_utf8(run_capture(command, TIMEOUT).expect("ffprobe runs")).expect("UTF-8 output")
}

/// The value of `key=` in ffprobe's `default` output format.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

/// Summary of the video stream of an encoded file: codec, pixel format, size and decoded frame count.
fn video_summary(paths: &FfmpegPaths, file: &Path) -> String {
    ffprobe(
        paths,
        file,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,pix_fmt,width,height,nb_read_frames",
            "-of",
            "default=nw=1",
        ],
    )
}

/// Top-level MP4 box types in file order.
fn top_level_boxes(bytes: &[u8]) -> Vec<[u8; 4]> {
    let mut boxes = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let size32 =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        let kind = [
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ];
        boxes.push(kind);
        let size = match size32 {
            0 => bytes.len() - pos,
            1 if pos + 16 <= bytes.len() => {
                let mut large = [0u8; 8];
                large.copy_from_slice(&bytes[pos + 8..pos + 16]);
                usize::try_from(u64::from_be_bytes(large)).unwrap_or(usize::MAX)
            }
            other => other as usize,
        };
        if size < 8 {
            break;
        }
        pos = pos.saturating_add(size);
    }
    boxes
}

/// Generates a clip from a lavfi graph with the given output options.
fn generate(paths: &FfmpegPaths, graph: &str, output_args: &[&str], out: &Path) {
    let command = FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")
        .expect("global argument")
        .input(
            graph,
            &InputOptions {
                lavfi: true,
                ..InputOptions::default()
            },
        )
        .expect("lavfi input")
        .output_args(output_args.iter().copied())
        .expect("output arguments")
        .output(out)
        .expect("output path");
    command
        .spawn(SpawnIo::default())
        .expect("start ffmpeg")
        .wait()
        .expect("ffmpeg generates the clip");
}

/// Writes the level clip into `folder`: 20 frames at 20 fps, 64x48, lossless. Frame `n` is flat gray at limited-range
/// luma `16 + 11 n`, so every frame has its own level and an exported frame can be matched to the source frame it shows
/// by its mean luma.
fn level_clip(paths: &FfmpegPaths, folder: &Path) -> PathBuf {
    let out = folder.join("levels.mkv");
    generate(
        paths,
        // Chroma is pinned to neutral: without it geq leaves U and V at 16, which tints the frames.
        "color=c=black:s=64x48:r=20:d=1,geq=lum=16+11*N:cb=128:cr=128",
        &["-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p"],
        &out,
    );
    out
}

/// The container an export of the level clip is written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Gif,
    Mp4,
}

/// Exports the level clip over `segments` at `fps` (64 px wide) and returns the level of each output frame: the mean
/// luma of each GIF frame's composite, or of each decoded MP4 frame's luma plane. `configure` edits the project first.
fn export_levels(
    paths: &FfmpegPaths,
    gpu: &Arc<GpuContext>,
    source: &Path,
    kind: Kind,
    segments: &[(f64, f64)],
    fps: u32,
    configure: impl FnOnce(&mut Project),
) -> Vec<f64> {
    let info = media(paths, source);
    let env = env(paths.clone(), Arc::clone(gpu));
    let mut proj = project(&info, source, segments);
    proj.output.fps = Rational::integer(fps);
    proj.output.size = OutputSize::Width { width: 64 };
    if kind == Kind::Mp4 {
        proj.export.format = ExportFormat::Mp4H264;
    }
    configure(&mut proj);
    let folder = tempfile::tempdir().expect("output folder");
    let output = folder.path().join(match kind {
        Kind::Gif => "levels.gif",
        Kind::Mp4 => "levels.mp4",
    });
    run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    match kind {
        Kind::Gif => read_gif(&output)
            .frames
            .iter()
            .map(|frame| mean_luma(frame))
            .collect(),
        Kind::Mp4 => decoded_luma(paths, &output),
    }
}

#[test]
fn gif_from_a_clip_range_matches_the_plan() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("loop.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );

    let (result, events) = run_ok(&env, &req);
    assert_eq!(result.path, output);
    assert_eq!(result.frames, 10, "one GIF frame per planned frame");
    assert!(fallbacks(&events).is_empty(), "no fallback on the CPU path");

    let gif = read_gif(&output);
    assert_eq!((gif.width, gif.height), (160, 120));
    assert_eq!(gif.repeat, gif::Repeat::Infinite);
    assert_eq!(
        gif.frames.len(),
        10,
        "testsrc2 frames differ, so none merge"
    );
    assert!(
        gif.delays.iter().all(|&d| d == 10),
        "delays {:?}",
        gif.delays
    );
    assert_eq!(
        result.bytes,
        fs::metadata(&output).expect("output exists").len()
    );
    assert!(
        !partial_path(&output).exists(),
        "the partial file is renamed away"
    );

    // Progress is monotonic and ends at the plan's frame count, followed by Done.
    let progress: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            ExportEvent::Progress {
                frames_done,
                frames_total,
                ..
            } => {
                assert_eq!(*frames_total, 10);
                Some(*frames_done)
            }
            _ => None,
        })
        .collect();
    assert!(
        progress.windows(2).all(|pair| pair[0] <= pair[1]),
        "{progress:?}"
    );
    assert_eq!(progress.last().copied(), Some(10));
    assert!(matches!(events.last(), Some(ExportEvent::Done { .. })));
}

#[test]
fn cuts_join_their_segments_in_order() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // Two kept ranges from different parts of the clip: each is decoded on its own, and the plan joins them.
    let mut proj = project(&info, &source, &[(0.0, 0.5), (1.0, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("cuts.gif");
    let (result, _) = run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert_eq!(result.frames, 10);
    let gif = read_gif(&output);
    assert_eq!(gif.frames.len(), 10);
    assert!(gif.delays.iter().all(|&d| d == 10));
}

#[test]
fn a_hold_freezes_its_frame_for_the_hold_duration() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.clips[0].holds.push(Hold {
        at: 0.5,
        duration: 0.5,
    });
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("hold.gif");
    let (result, _) = run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    // Ten frames of motion plus five held frames make 1.5 s, and the total duration is kept exactly. gifski may
    // merge the identical held frames, but its temporal denoiser can change them first, so the record count can
    // stay at 15. Only the duration and the count are asserted.
    let gif = read_gif(&output);
    assert_eq!(gif.delays.iter().map(|&d| u32::from(d)).sum::<u32>(), 150);
    assert!(gif.frames.len() <= 15, "records: {}", gif.frames.len());
    assert_eq!(result.frames as usize, gif.frames.len());
}

#[test]
fn exported_frames_show_the_source_frame_for_their_output_time() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let folder = tempfile::tempdir().expect("temporary folder");
    let source = level_clip(&paths, folder.path());
    for (kind, tolerance) in [(Kind::Gif, 1.5), (Kind::Mp4, 3.0)] {
        // The reference is a 1:1 export of the whole clip at the source rate: the level of every source frame.
        let reference = export_levels(&paths, &gpu, &source, kind, &[(0.0, 1.0)], 20, |_| {});
        assert_eq!(
            reference.len(),
            LEVEL_FRAMES,
            "{kind:?}: one frame per source frame"
        );
        assert!(
            reference.windows(2).all(|pair| pair[1] > pair[0] + 5.0),
            "{kind:?}: the levels must be distinct: {reference:.1?}"
        );

        // Output at 15 fps from source time 0.25 s (source frame 5) for 0.6 s: nine frames. Output frame k is at source
        // time t = 0.25 + k / 15, which shows the source frame floor((t - 0.25) * 20 + 1e-6) counted from the range start.
        let shown = export_levels(&paths, &gpu, &source, kind, &[(0.25, 0.85)], 15, |_| {});
        assert_eq!(shown.len(), 9, "{kind:?}: frames of the plan");
        for (k, &level) in shown.iter().enumerate() {
            let source_frame = 5 + ((k as f64) * 20.0 / 15.0 + 1e-6).floor() as usize;
            let expected = reference[source_frame];
            assert!(
                (level - expected).abs() <= tolerance,
                "{kind:?}: output frame {k} shows level {level:.2}, but source frame {source_frame} is {expected:.2}"
            );
        }
    }
}

#[test]
fn crossfade_seam_frames_blend_the_tail_into_the_head() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let folder = tempfile::tempdir().expect("temporary folder");
    let source = level_clip(&paths, folder.path());
    let reference = export_levels(&paths, &gpu, &source, Kind::Gif, &[(0.0, 1.0)], 20, |_| {});
    // 10 fps over the whole clip with a 0.3 s crossfade: 10 - 3 = 7 frames. Seam frame i blends the tail sample i,
    // source frame 14 + 2i, into the head sample i, source frame 2i, with weight (i + 1) / 4 on the head.
    let shown = export_levels(
        &paths,
        &gpu,
        &source,
        Kind::Gif,
        &[(0.0, 1.0)],
        10,
        |proj| {
            proj.timing.crossfade = 0.3;
        },
    );
    assert_eq!(shown.len(), 7);
    for i in 0..3 {
        let tail = reference[14 + 2 * i];
        let head = reference[2 * i];
        let weight = (i + 1) as f64 / 4.0;
        let expected = (1.0 - weight) * tail + weight * head;
        assert!(
            (shown[i] - expected).abs() <= 1.5,
            "seam frame {i} shows {:.2}, expected {expected:.2} from tail {tail:.2} and head {head:.2} at weight {weight}",
            shown[i]
        );
    }
    // The frames after the seam are plain samples at 0.3, 0.4, 0.5 and 0.6 s: source frames 6, 8, 10 and 12.
    for (offset, source_frame) in [6usize, 8, 10, 12].into_iter().enumerate() {
        let expected = reference[source_frame];
        assert!(
            (shown[3 + offset] - expected).abs() <= 1.5,
            "frame {} shows {:.2}, expected source frame {source_frame} at {expected:.2}",
            3 + offset,
            shown[3 + offset]
        );
    }
}

#[test]
fn reverse_and_ping_pong_start_in_the_right_direction() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let folder = tempfile::tempdir().expect("temporary folder");
    let source = level_clip(&paths, folder.path());
    let reference = export_levels(&paths, &gpu, &source, Kind::Gif, &[(0.0, 1.0)], 20, |_| {});

    // Reverse at 10 fps over one second starts at the last source time: frame 0 is 0.9 s (source frame 18) and frame 9
    // is 0.0 s (source frame 0).
    let reverse = export_levels(
        &paths,
        &gpu,
        &source,
        Kind::Gif,
        &[(0.0, 1.0)],
        10,
        |proj| {
            proj.timing.direction = Direction::Reverse;
        },
    );
    assert_eq!(reverse.len(), 10);
    assert!(
        (reverse[0] - reference[18]).abs() <= 1.5,
        "reverse frame 0: {:.2}",
        reverse[0]
    );
    assert!(
        (reverse[9] - reference[0]).abs() <= 1.5,
        "reverse frame 9: {:.2}",
        reverse[9]
    );

    // Ping-pong: ten forward frames (0.0 to 0.9 s) and eight back (0.8 to 0.1 s). Frame 0 is source frame 0, frame 9
    // is 18, frame 10 is 16 and frame 17 is 2.
    let pingpong = export_levels(
        &paths,
        &gpu,
        &source,
        Kind::Gif,
        &[(0.0, 1.0)],
        10,
        |proj| {
            proj.timing.direction = Direction::PingPong;
        },
    );
    assert_eq!(pingpong.len(), 18);
    for (frame, source_frame) in [(0usize, 0usize), (9, 18), (10, 16), (17, 2)] {
        assert!(
            (pingpong[frame] - reference[source_frame]).abs() <= 1.5,
            "ping-pong frame {frame} shows {:.2}, expected source frame {source_frame} at {:.2}",
            pingpong[frame],
            reference[source_frame]
        );
    }
}

#[test]
fn portrait_source_exports_portrait_dimensions() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("portrait_rotated.mp4");
    let info = media(&paths, &source);
    assert_eq!(
        info.video.rotation % 180,
        90,
        "fixture is displayed rotated a quarter turn"
    );
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("portrait.gif");
    run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    let gif = read_gif(&output);
    // Stored 320x240 displayed as 240x320, so 160 wide is 214 tall.
    assert_eq!((gif.width, gif.height), (160, 214));
}

#[test]
fn crop_sets_the_output_aspect() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 0.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    // The middle half of the width: a 2:3 portrait crop of the 4:3 source.
    proj.transform.crop = NormRect {
        x: 0.25,
        y: 0.0,
        w: 0.5,
        h: 1.0,
    };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("crop.gif");
    run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    let gif = read_gif(&output);
    assert_eq!((gif.width, gif.height), (160, 240));
}

#[test]
fn hdr_pq_source_is_tone_mapped_not_clipped() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("hdr_pq.mp4");
    let info = media(&paths, &source);
    assert!(info.video.color.is_hdr(), "fixture is PQ");
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 320 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("hdr.gif");
    run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    let gif = read_gif(&output);
    let mean =
        gif.frames.iter().map(|frame| mean_luma(frame)).sum::<f64>() / gif.frames.len() as f64;
    eprintln!(
        "hdr_pq mean luma {mean:.1} over {} frames",
        gif.frames.len()
    );
    // A washed-out or clipped tone map would sit near the ends of the range.
    assert!((25.0..230.0).contains(&mean), "mean luma {mean}");
}

#[test]
fn alpha_source_composites_over_the_background() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("alpha_prores4444.mov");
    let info = media(&paths, &source);
    assert!(info.video.has_alpha, "fixture carries alpha");

    // The first source frame, decoded to RGBA on the CPU, tells which pixels are transparent.
    let video = &info.video;
    let decode = DecodeRequest {
        input: source.clone(),
        stream_index: video.index,
        coded_width: video.width,
        coded_height: video.height,
        bit_depth: video.bit_depth,
        has_alpha: video.has_alpha,
        codec: video.codec.clone(),
        color: video.color,
        start_time: video.start_time,
        range: Some(TimeRange::new(0.0, 0.1)),
        crop: None,
        size: None,
        fps: Some(Rational::integer(30)),
        chain: DecodeChain::CPU,
        layout: PixelLayout::Rgba8,
    };
    let mut session =
        DecodeSession::start(&paths, &decode, CancelToken::new()).expect("decode alpha");
    let first = session.next().expect("a frame").expect("decodes").data;
    drop(session);

    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 320 };
    proj.output.background = [255, 255, 255, 255];
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("alpha.gif");
    // An RGBA layout is CPU-only, so a hardware decode request is replaced silently, not reported as a fallback.
    let hardware = DecodeChain {
        backend: DecodeBackend::D3d11va { dxgi_index: 0 },
        scaler: GpuScaler::None,
    };
    let (result, events) = run_ok(
        &env,
        &request(proj, info, output.clone(), hardware, EncoderBackend::Cpu),
    );
    assert!(
        fallbacks(&events).is_empty(),
        "RGBA sources are not a fallback"
    );

    let gif = read_gif(&output);
    // The clip is static, so gifski merges its frames into fewer records; the file still plays the full second.
    assert_eq!(gif.frames.len() as u64, result.frames);
    assert_eq!(gif.delays.iter().map(|&d| u32::from(d)).sum::<u32>(), 100);
    let shown = &gif.frames[0];
    // The comparison pairs pixels by index, so both buffers must cover the same pixels.
    assert_eq!(
        first.len(),
        usize::from(gif.width) * usize::from(gif.height) * 4,
        "the source frame and the GIF frame have the same size"
    );
    assert_eq!(shown.len(), first.len());
    let mut transparent = 0usize;
    let mut near_white = 0usize;
    for (source_px, out_px) in first
        .as_chunks::<4>()
        .0
        .iter()
        .zip(shown.as_chunks::<4>().0.iter())
    {
        if source_px[3] == 0 {
            transparent += 1;
            if out_px[..3].iter().all(|&channel| channel >= 240) {
                near_white += 1;
            }
        }
    }
    assert!(transparent > 0, "the fixture has transparent pixels");
    assert!(
        near_white * 100 >= transparent * 95,
        "{near_white} of {transparent} transparent pixels are background"
    );
}

#[test]
fn mp4_h264_on_the_cpu_is_a_faststart_yuv420p_stream() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 320 };
    proj.export.format = ExportFormat::Mp4H264;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("loop.mp4");
    let (result, events) = run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert_eq!(result.frames, 10);
    assert!(fallbacks(&events).is_empty());

    let summary = video_summary(&paths, &output);
    assert_eq!(field(&summary, "codec_name"), Some("h264"), "{summary}");
    assert_eq!(field(&summary, "pix_fmt"), Some("yuv420p"), "{summary}");
    assert_eq!(field(&summary, "width"), Some("320"), "{summary}");
    assert_eq!(field(&summary, "height"), Some("240"), "{summary}");
    assert_eq!(field(&summary, "nb_read_frames"), Some("10"), "{summary}");

    let bytes = fs::read(&output).expect("read MP4");
    let boxes = top_level_boxes(&bytes);
    let moov = boxes.iter().position(|kind| kind == b"moov");
    let mdat = boxes.iter().position(|kind| kind == b"mdat");
    assert!(moov.is_some() && mdat.is_some(), "boxes {boxes:?}");
    assert!(moov < mdat, "+faststart puts moov before mdat: {boxes:?}");
}

#[test]
fn webm_vp9_and_av1_exports_use_the_cpu_encoders() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let out_dir = tempfile::tempdir().expect("output folder");
    for (format, codec) in [
        (ExportFormat::WebmVp9, "vp9"),
        (ExportFormat::WebmAv1, "av1"),
    ] {
        let mut proj = project(&info, &source, &[(0.0, 0.5)]);
        proj.output.fps = Rational::integer(10);
        proj.output.size = OutputSize::Width { width: 160 };
        proj.export.format = format;
        let output = out_dir.path().join(format!("{codec}.webm"));
        // A hardware encoder request is ignored for VP9, which has no GPU encoder, and used for AV1 only if it is
        // there; the CPU is requested here so the test is the same on every machine.
        let (result, _) = run_ok(
            &env,
            &request(
                proj,
                info.clone(),
                output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
        );
        assert_eq!(result.frames, 5, "{codec}");
        let summary = video_summary(&paths, &output);
        assert_eq!(field(&summary, "codec_name"), Some(codec), "{summary}");
        assert_eq!(field(&summary, "width"), Some("160"), "{summary}");
    }
}

#[test]
fn forced_decode_failure_falls_back_for_each_range_and_keeps_every_frame() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let _forced = ForcedFailure::set("decode");
    let env = store_env(paths.clone(), gpu, 256 * MB, 2048 * MB);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 0.5), (1.0, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");

    let cpu_output = out_dir.path().join("cpu.gif");
    let (cpu, cpu_events) = run_ok(
        &env,
        &request(
            proj.clone(),
            info.clone(),
            cpu_output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert!(
        fallbacks(&cpu_events).is_empty(),
        "the CPU run has no fallback"
    );

    // Forced failures happen before FFmpeg starts, so the forced chain needs no hardware. Each range starts on the
    // hardware chain again, so both ranges fall back, one event each.
    let forced_output = out_dir.path().join("forced.gif");
    let hardware = DecodeChain {
        backend: DecodeBackend::D3d11va { dxgi_index: 0 },
        scaler: GpuScaler::None,
    };
    let (forced, events) = run_ok(
        &env,
        &request(
            proj,
            info,
            forced_output.clone(),
            hardware,
            EncoderBackend::Cpu,
        ),
    );
    assert_eq!(
        fallbacks(&events),
        vec![("decode", "d3d11va", "cpu"), ("decode", "d3d11va", "cpu")],
        "one fallback per range"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            ExportEvent::Fallback { reason, .. } if reason.contains(FORCE_HW_FAIL_ENV)
        )),
        "the reason names the variable that forced it"
    );
    assert_eq!(forced.frames, cpu.frames, "frame count is unchanged");
    let forced_gif = read_gif(&forced_output);
    assert_eq!(forced_gif.frames.len(), 10);
    assert_eq!(
        forced_gif.frames,
        read_gif(&cpu_output).frames,
        "the CPU fallback decodes the same frames"
    );
}

#[test]
fn forced_encode_failure_restarts_the_encode_on_the_cpu() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let _forced = ForcedFailure::set("encode");
    let env = store_env(paths.clone(), gpu, 256 * MB, 2048 * MB);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 320 };
    proj.export.format = ExportFormat::Mp4H264;
    let out_dir = tempfile::tempdir().expect("output folder");

    let (cpu, cpu_events) = run_ok(
        &env,
        &request(
            proj.clone(),
            info.clone(),
            out_dir.path().join("cpu.mp4"),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert!(fallbacks(&cpu_events).is_empty());

    let forced_output = out_dir.path().join("forced.mp4");
    let (forced, events) = run_ok(
        &env,
        &request(
            proj,
            info,
            forced_output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Amf { dxgi_index: 0 },
        ),
    );
    assert_eq!(
        fallbacks(&events),
        vec![("encode", "h264_amf", "libx264")],
        "the forced AMF encoder restarts on libx264"
    );
    assert_eq!(forced.frames, cpu.frames, "frame count is unchanged");
    assert_eq!(forced.frames, 10);
    let summary = video_summary(&paths, &forced_output);
    assert_eq!(field(&summary, "codec_name"), Some("h264"), "{summary}");
    assert_eq!(field(&summary, "nb_read_frames"), Some("10"), "{summary}");
}

#[test]
fn a_range_the_store_drops_streams_and_exports_the_same_gif() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let stored_output = out_dir.path().join("stored.gif");
    let streamed_output = out_dir.path().join("streamed.gif");

    let (stored, stored_events) = {
        let env = env(paths.clone(), Arc::clone(&gpu));
        run_ok(
            &env,
            &request(
                proj.clone(),
                info.clone(),
                stored_output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
        )
    };
    assert!(fallbacks(&stored_events).is_empty());

    // One kilobyte each for RAM and disk: no frame fits, so the store drops each frame as it arrives and the range
    // is streamed.
    let tiny = env_with_budget(paths.clone(), gpu, 1024, 1024);
    let (streamed, events) = run_ok(
        &tiny,
        &request(
            proj,
            info,
            streamed_output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert!(
        tiny.ctx.framestore.stats().dropped_frames > 0,
        "the store dropped frames"
    );
    assert!(fallbacks(&events).is_empty());
    assert_eq!(streamed.frames, stored.frames);
    assert_eq!(
        read_gif(&streamed_output).frames,
        read_gif(&stored_output).frames,
        "the streamed export shows the same frames"
    );
}

#[test]
fn a_range_that_outgrows_the_store_is_streamed_and_matches() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let stored_output = out_dir.path().join("stored.gif");
    let streamed_output = out_dir.path().join("streamed.gif");
    let (stored, _) = {
        let env = env(paths.clone(), Arc::clone(&gpu));
        run_ok(
            &env,
            &request(
                proj.clone(),
                info.clone(),
                stored_output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
        )
    };
    // A 160x120 NV12 frame is 28,800 bytes. With no RAM tier and room for three frames on disk, the fourth frame of
    // the range evicts its first frame, so the range does not fit and is streamed.
    let frame_bytes = 160 * 120 * 3 / 2;
    let tight = env_with_budget(paths, gpu, 0, 3 * frame_bytes);
    let (streamed, events) = run_ok(
        &tight,
        &request(
            proj,
            info,
            streamed_output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert!(fallbacks(&events).is_empty());
    assert_eq!(streamed.frames, stored.frames);
    assert!(
        tight.ctx.framestore.stats().disk_frames <= 3,
        "the cache stays within its budget"
    );
    assert_eq!(
        read_gif(&streamed_output).frames,
        read_gif(&stored_output).frames
    );
}

#[test]
fn fast_and_gpu_dither_engines_stream_a_range_that_outgrows_the_store() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // Half speed makes a 4 s plan at 20 fps: 80 frames. Both engines read their palette ahead, then encode from the first
    // frame, so a streamed range is traversed twice. 80 frames are more than three 1 s windows (60 frames), so the target
    // variant runs sampled estimates over the streamed range too.
    let mut base = project(&info, &source, &[(0.0, 2.0)]);
    base.output.fps = Rational::integer(20);
    base.output.size = OutputSize::Width { width: 160 };
    base.timing.speed = 0.5;
    let out_dir = tempfile::tempdir().expect("output folder");
    // A 160x120 NV12 frame is 28,800 bytes: room for three on disk and none in RAM, so the range is streamed.
    let frame_bytes = 160 * 120 * 3 / 2;
    for engine in [GifEngine::Fast, GifEngine::GpuDither] {
        base.export.gif_engine = engine;
        base.export.colors = 64;
        let stored_output = out_dir.path().join(format!("{engine:?}-stored.gif"));
        let stored_req = request(
            base.clone(),
            info.clone(),
            stored_output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        );
        let (stored_result, stored_estimate) = {
            let env = env(paths.clone(), Arc::clone(&gpu));
            let (stored, _) = run_ok(&env, &stored_req);
            let estimate = estimate_size(&env.ctx, &stored_req, 1.0).expect("stored estimate");
            (stored, estimate)
        };
        let stored_gif = read_gif(&stored_output);
        assert_eq!(stored_gif.frames.len() as u64, stored_result.frames);

        let tight = env_with_budget(paths.clone(), Arc::clone(&gpu), 0, 3 * frame_bytes);
        // The sampled estimate reads the same streamed range, so it gives the stored range's size.
        let streamed_estimate =
            estimate_size(&tight.ctx, &stored_req, 1.0).expect("streamed estimate");
        assert_eq!(stored_estimate.basis, EstimateBasis::Sample, "{engine:?}");
        assert_eq!(
            streamed_estimate, stored_estimate,
            "{engine:?}: a streamed range estimates the same size"
        );
        let streamed_output = out_dir.path().join(format!("{engine:?}-streamed.gif"));
        let (streamed, events) = run_ok(
            &tight,
            &request(
                base.clone(),
                info.clone(),
                streamed_output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
        );
        assert!(fallbacks(&events).is_empty(), "{engine:?}");
        assert_eq!(streamed.frames, stored_result.frames, "{engine:?}");
        assert!(
            tight.ctx.framestore.stats().disk_frames <= 3,
            "{engine:?}: the cache stays within its budget"
        );
        assert_eq!(
            read_gif(&streamed_output).frames,
            stored_gif.frames,
            "{engine:?}: the streamed export shows the same frames"
        );

        // The same range with a size target that the first step fits: the search estimates it over streamed windows,
        // then encodes it for real.
        let mut targeted = base.clone();
        targeted.export.target_bytes = Some(stored_result.bytes * 2);
        let target_output = out_dir.path().join(format!("{engine:?}-target.gif"));
        let (targeted_result, targeted_events) = run_ok(
            &tight,
            &request(
                targeted,
                info.clone(),
                target_output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
        );
        assert!(fallbacks(&targeted_events).is_empty(), "{engine:?}");
        assert_eq!(targeted_result.frames, stored_result.frames, "{engine:?}");
        // The first step fits its budget, so it is encoded for real and kept: one pass, at the real size.
        let passes: Vec<u64> = targeted_events
            .iter()
            .filter_map(|event| match event {
                ExportEvent::TargetPass { bytes, .. } => Some(*bytes),
                _ => None,
            })
            .collect();
        assert_eq!(passes, vec![targeted_result.bytes], "{engine:?}");
        assert_eq!(
            read_gif(&target_output).frames,
            stored_gif.frames,
            "{engine:?}: the targeted export shows the same frames"
        );
    }
}

#[test]
fn a_streamed_range_decodes_once_more_and_serves_the_stored_frames() {
    let Some(paths) = paths() else {
        return;
    };
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let clip = project(&info, &source, &[(0.5, 1.5)]).clips[0].clone();
    let options = DecodeOptions {
        chain: DecodeChain::CPU,
        geometry: DecodeGeometry {
            crop: None,
            size: (160, 120),
        },
        access: Access::Forward,
    };
    let folder = tempfile::tempdir().expect("temporary folder");
    let store = |ram: u64, disk: u64, name: &str| {
        Arc::new(
            FrameStore::new(FrameStoreConfig {
                ram_budget_bytes: ram,
                disk_budget_bytes: disk,
                spill_dir: folder.path().join(name),
            })
            .expect("frame store"),
        )
    };
    let mut stored = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        options,
        store(256 * MB, 2048 * MB, "big"),
        &CancelToken::new(),
    )
    .expect("decode into a large store");
    let mut streamed = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        options,
        store(1024, 1024, "tiny"),
        &CancelToken::new(),
    )
    .expect("decode into a tiny store");
    assert_eq!((stored.decode_passes(), stored.streaming_ranges()), (1, 0));
    assert_eq!(
        (streamed.decode_passes(), streamed.streaming_ranges()),
        (1, 1),
        "the store pass dropped the range"
    );

    for k in 0..10 {
        let t = 0.5 + k as f64 / 10.0;
        let a = stored.frame_at(t).expect("stored frame");
        let b = streamed.frame_at(t).expect("streamed frame");
        assert_eq!((a.width, a.height), (b.width, b.height));
        assert!(
            a.data == b.data,
            "source time {t} differs between stored and streamed frames"
        );
    }
    assert_eq!(
        streamed.decode_passes(),
        2,
        "one store pass and one stream, no more"
    );
    // Going back in time decodes the range again from its start: one more pass, and the stored frame.
    let back = streamed.frame_at(0.6).expect("frame after going back");
    assert!(
        back.data == stored.frame_at(0.6).expect("stored frame").data,
        "the restarted stream serves the same frame"
    );
    assert_eq!(
        streamed.decode_passes(),
        3,
        "going back is one more stream pass"
    );

    // Random access cannot stream, so the same range fails when the store cannot keep it.
    let random = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        DecodeOptions {
            access: Access::Random,
            ..options
        },
        store(1024, 1024, "random"),
        &CancelToken::new(),
    );
    assert!(
        matches!(random, Err(ExportError::TooLarge(_))),
        "random access needs the whole range in the store"
    );
}

#[test]
fn a_range_that_fits_is_stored_when_the_store_drops_other_frames() {
    let Some(paths) = paths() else {
        return;
    };
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let clip = project(&info, &source, &[(0.5, 1.5)]).clips[0].clone();
    let options = DecodeOptions {
        chain: DecodeChain::CPU,
        geometry: DecodeGeometry {
            crop: None,
            size: (160, 120),
        },
        access: Access::Forward,
    };
    let folder = tempfile::tempdir().expect("temporary folder");
    let new_store = |ram: u64, disk: u64, name: &str| {
        Arc::new(
            FrameStore::new(FrameStoreConfig {
                ram_budget_bytes: ram,
                disk_budget_bytes: disk,
                spill_dir: folder.path().join(name),
            })
            .expect("frame store"),
        )
    };
    // The range's frame count and frame size, from a store large enough for everything.
    let mut reference = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        options,
        new_store(256 * MB, 2048 * MB, "reference"),
        &CancelToken::new(),
    )
    .expect("decode into a large store");
    let frames = reference.decoded_frames();
    let frame_bytes = reference.frame_at(0.5).expect("frame").data.len() as u64;

    // A RAM tier of exactly the range's size and no disk tier. The range fits, but the frames of another source that
    // were already in the store are pushed out, and each push-out is a dropped frame. The range must still be stored.
    let store = new_store(frames * frame_bytes, 0, "exact");
    for index in 0..5 {
        store.insert(
            FrameKey {
                source: u64::MAX,
                index,
            },
            StoredFrame {
                width: 160,
                height: 120,
                layout: PixelLayout::Nv12,
                time: 0.0,
                data: Arc::from(vec![0u8; frame_bytes as usize]),
            },
        );
    }
    let fitted = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        options,
        Arc::clone(&store),
        &CancelToken::new(),
    )
    .expect("decode into the exact store");
    assert!(
        store.stats().dropped_frames > 0,
        "the range pushed the other frames out"
    );
    assert_eq!(
        (fitted.streaming_ranges(), fitted.decode_passes()),
        (0, 1),
        "the range fits, so it is stored in its one pass"
    );
    assert_eq!(fitted.decoded_frames(), frames);
}

#[test]
fn frames_the_store_loses_are_recovered_within_two_passes() {
    let Some(paths) = paths() else {
        return;
    };
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let clip = project(&info, &source, &[(0.5, 1.5)]).clips[0].clone();
    let folder = tempfile::tempdir().expect("temporary folder");
    let store = |name: &str| {
        Arc::new(
            FrameStore::new(FrameStoreConfig {
                ram_budget_bytes: 256 * MB,
                disk_budget_bytes: 2048 * MB,
                spill_dir: folder.path().join(name),
            })
            .expect("frame store"),
        )
    };
    let options = DecodeOptions {
        chain: DecodeChain::CPU,
        geometry: DecodeGeometry {
            crop: None,
            size: (160, 120),
        },
        access: Access::Forward,
    };

    // Forward: the store loses every frame after the store pass. The range is streamed from the lost position on, and
    // the frame is the same one.
    let forward_store = store("forward");
    let mut forward = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        options,
        Arc::clone(&forward_store),
        &CancelToken::new(),
    )
    .expect("decode");
    let expected = forward.frame_at(0.6).expect("stored frame").data;
    forward_store.clear();
    let recovered = forward.frame_at(0.6).expect("streamed frame").data;
    assert!(
        recovered == expected,
        "the streamed frame matches the stored one"
    );
    assert_eq!(forward.decode_passes(), 2, "the store pass and one stream");
    assert_eq!(forward.streaming_ranges(), 1);
    assert!(forward.frame_at(0.6).expect("same position").data == expected);

    // Random: one refill recovers a lost range; a second loss is too large, because the range has used its two passes.
    let random_store = store("random");
    let mut random = SourceFrames::decode(
        &paths,
        &info,
        &clip,
        DecodeOptions {
            access: Access::Random,
            ..options
        },
        Arc::clone(&random_store),
        &CancelToken::new(),
    )
    .expect("decode");
    let expected = random.frame_at(1.0).expect("stored frame").data;
    random_store.clear();
    let refilled = random.frame_at(1.0).expect("refilled frame").data;
    assert!(
        refilled == expected,
        "the refilled frame matches the stored one"
    );
    assert_eq!(random.decode_passes(), 2, "the store pass and one refill");
    random_store.clear();
    assert!(
        matches!(random.frame_at(1.0), Err(ExportError::TooLarge(_))),
        "a second loss exceeds the pass budget"
    );
    assert_eq!(random.decode_passes(), 2, "no third pass");
}

#[test]
fn a_reverse_plan_that_outgrows_the_store_is_too_large() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env_with_budget(paths.clone(), gpu, 1024, 1024);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.timing.direction = Direction::Reverse;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("reverse.gif");
    let (result, _) = run(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
        &CancelToken::new(),
    );
    match result {
        Err(ExportError::TooLarge(message)) => {
            assert!(message.contains("Shorten the selection"), "{message}");
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert!(!output.exists(), "no output after a refused plan");
    assert!(!partial_path(&output).exists());
}

#[test]
fn an_output_that_is_the_source_clip_is_refused() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let folder = tempfile::tempdir().expect("temporary folder");
    let clip_file = folder.path().join("clip.mp4");
    fs::copy(corpus("h264_320x240.mp4"), &clip_file).expect("copy the clip");
    let before = fs::read(&clip_file).expect("read the clip");
    let info = media(&paths, &clip_file);
    let proj = project(&info, &clip_file, &[(0.0, 1.0)]);
    let env = env(paths.clone(), gpu);
    // Both spellings name the source: the path itself, and the same folder reached through `.`.
    for output in [clip_file.clone(), folder.path().join(".").join("clip.mp4")] {
        let (result, _) = run(
            &env,
            &request(
                proj.clone(),
                info.clone(),
                output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
            &CancelToken::new(),
        );
        assert!(
            matches!(&result, Err(ExportError::InvalidRequest(message)) if message.contains("overwrite")),
            "{}: {result:?}",
            output.display()
        );
    }
    assert_eq!(
        fs::read(&clip_file).expect("read the clip"),
        before,
        "the source clip is untouched"
    );
}

#[test]
fn an_output_named_like_a_project_file_is_refused() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let proj = project(&info, &source, &[(0.0, 1.0)]);
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("loop.loopcut");
    let (result, _) = run(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
        &CancelToken::new(),
    );
    assert!(
        matches!(&result, Err(ExportError::InvalidRequest(message)) if message.contains("project file")),
        "{result:?}"
    );
    assert!(!output.exists());
}

#[test]
#[ignore = "hardware smoke test: needs an AMD AMF HEVC encoder; run with --ignored on the target machine"]
fn hardware_hevc_amf_at_320x240_restarts_on_libx265() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 320 };
    proj.export.format = ExportFormat::Mp4Hevc;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("hevc.mp4");
    // Hardware smoke test. The AMF HEVC encoder refuses frames of 320x240 and smaller on the RX 9060 XT, so this encode
    // must restart on libx265. Other machines may lack the encoder and fall back the same way.
    let (result, events) = run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Amf { dxgi_index: 0 },
        ),
    );
    assert!(
        fallbacks(&events).contains(&("encode", "hevc_amf", "libx265")),
        "expected an encode fallback, got {:?}",
        fallbacks(&events)
    );
    assert_eq!(result.frames, 10);
    let summary = video_summary(&paths, &output);
    assert_eq!(field(&summary, "codec_name"), Some("hevc"), "{summary}");
    assert_eq!(field(&summary, "pix_fmt"), Some("yuv420p"), "{summary}");
    assert_eq!(field(&summary, "nb_read_frames"), Some("10"), "{summary}");
}

#[test]
#[ignore = "hardware smoke test: needs an AMD AMF H.264 encoder; run with --ignored on the target machine"]
fn hardware_h264_amf_at_640x360_encodes_without_fallback() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("hevc10_640x360.mkv");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 0.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 640 };
    proj.export.format = ExportFormat::Mp4H264;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("amf.mp4");
    let (result, events) = run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Amf { dxgi_index: 0 },
        ),
    );
    assert!(fallbacks(&events).is_empty(), "h264_amf accepts 640x360");
    assert_eq!(result.frames, 5);
    let summary = video_summary(&paths, &output);
    assert_eq!(field(&summary, "codec_name"), Some("h264"), "{summary}");
    assert_eq!(field(&summary, "width"), Some("640"), "{summary}");
}

#[test]
#[ignore = "hardware smoke test: needs an NVIDIA GPU with NVDEC; run with --ignored on the target machine"]
fn hardware_cuda_decode_exports_with_or_without_fallback() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("cuda.gif");
    let cuda = DecodeChain {
        backend: DecodeBackend::Cuda { cuda_index: 0 },
        scaler: GpuScaler::Cuda,
    };
    let (result, events) = run_ok(
        &env,
        &request(proj, info, output.clone(), cuda, EncoderBackend::Cpu),
    );
    // Either NVDEC works, or the range falls back once. Both must produce the full export.
    assert!(
        fallbacks(&events)
            .iter()
            .all(|event| *event == ("decode", "cuda", "cpu")),
        "{:?}",
        fallbacks(&events)
    );
    assert_eq!(result.frames, 10);
    assert_eq!(read_gif(&output).frames.len(), 10);
}

#[test]
fn cancelling_mid_export_deletes_the_partial_file() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 2.0)]);
    proj.output.fps = Rational::integer(50);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("cancelled.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );

    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let mut events = Vec::new();
    let result = run_export(
        &env.ctx,
        &req,
        &mut |event| {
            if let ExportEvent::Progress { frames_done, .. } = &event
                && *frames_done >= 5
            {
                trigger.cancel();
            }
            events.push(event);
        },
        &cancel,
    );
    assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
    assert!(!output.exists(), "no output after a cancel");
    assert!(
        !partial_path(&output).exists(),
        "the partial file is deleted"
    );
}

#[test]
fn a_clip_stream_that_is_not_probed_is_refused() {
    let Some(paths) = paths() else {
        return;
    };
    let Some(gpu) = software_gpu() else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.0, 1.0)]);
    proj.clips[0].stream_index = info.video.index + 7;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("wrong.gif");
    let (result, _) = run(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
        &CancelToken::new(),
    );
    assert!(
        matches!(result, Err(ExportError::InvalidRequest(_))),
        "{result:?}"
    );
    assert!(!output.exists());
}

/// Distinct RGB colours over every composited frame of a GIF.
fn distinct_colors(gif: &Gif) -> usize {
    let mut colors = std::collections::HashSet::new();
    for frame in &gif.frames {
        for px in frame.as_chunks::<4>().0 {
            colors.insert([px[0], px[1], px[2]]);
        }
    }
    colors.len()
}

/// A 320x240 clip range exported at 10 fps and 160 px wide with the given engine and palette.
fn gif_project(info: &MediaInfo, source: &Path, engine: GifEngine, colors: u16) -> Project {
    let mut proj = project(info, source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.export.gif_engine = engine;
    proj.export.colors = colors;
    proj
}

#[test]
fn fast_engine_writes_a_gif_of_the_plan_frames() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let proj = gif_project(&info, &source, GifEngine::Fast, 64);
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("fast.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );

    let (result, events) = run_ok(&env, &req);
    assert_eq!(result.frames, 10, "one GIF frame per planned frame");
    assert!(fallbacks(&events).is_empty(), "no fallback on the CPU path");
    let gif = read_gif(&output);
    assert_eq!((gif.width, gif.height), (160, 120));
    assert_eq!(gif.repeat, gif::Repeat::Infinite);
    assert_eq!(gif.frames.len(), 10);
    // FFmpeg takes the delay from the frame rate: 10 fps is 10 cs for every frame.
    assert!(gif.delays.iter().all(|&d| d == 10), "{:?}", gif.delays);
    let colors = distinct_colors(&gif);
    assert!(colors <= 64, "{colors} colours in a 64-colour palette");
    assert!(!partial_path(&output).exists());
}

#[test]
fn fast_engine_loop_count_follows_the_project_model() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let out_dir = tempfile::tempdir().expect("output folder");
    for (loop_count, expected) in [(1u16, gif::Repeat::Finite(0)), (3, gif::Repeat::Finite(2))] {
        let mut proj = gif_project(&info, &source, GifEngine::Fast, 32);
        proj.output.loop_count = loop_count;
        let output = out_dir.path().join(format!("fast-{loop_count}.gif"));
        let req = request(
            proj,
            info.clone(),
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        );
        run_ok(&env, &req);
        assert_eq!(
            read_gif(&output).repeat,
            expected,
            "loop count {loop_count}"
        );
    }
}

#[test]
fn gpu_dither_engine_keeps_the_plan_delays_and_loop_count() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // 15 fps has no exact centisecond rate, so the plan's delays alternate 7 and 6 cs: they must come out unchanged.
    let mut proj = gif_project(&info, &source, GifEngine::GpuDither, 32);
    proj.output.fps = Rational::integer(15);
    proj.output.loop_count = 3;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("dither.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );

    let (result, events) = run_ok(&env, &req);
    assert!(fallbacks(&events).is_empty(), "no fallback on WARP");
    let gif = read_gif(&output);
    assert_eq!(gif.repeat, gif::Repeat::Finite(2), "three plays");
    let expected = delays_cs(result.frames as usize, Rational::integer(15));
    assert!(expected.contains(&6) && expected.contains(&7));
    assert_eq!(gif.delays, expected, "delays are the plan's centiseconds");
    assert_eq!(gif.frames.len() as u64, result.frames);
    let colors = distinct_colors(&gif);
    assert!(colors <= 32, "{colors} colours in a 32-colour palette");
}

#[test]
fn gpu_dither_export_is_deterministic_and_its_estimate_is_exact() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let proj = gif_project(&info, &source, GifEngine::GpuDither, 64);
    let out_dir = tempfile::tempdir().expect("output folder");
    let first = out_dir.path().join("first.gif");
    let second = out_dir.path().join("second.gif");
    let req = request(
        proj.clone(),
        info.clone(),
        first.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, _) = run_ok(&env, &req);
    let again = request(
        proj,
        info,
        second.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    run_ok(&env, &again);
    assert_eq!(
        fs::read(&first).expect("first"),
        fs::read(&second).expect("second"),
        "the same project gives the same bytes"
    );

    // Ten frames fit in three one-second windows, so the estimate encodes the whole plan with the same palette.
    let estimate = estimate_size(&env.ctx, &req, 1.0).expect("estimate");
    assert_eq!(estimate.basis, EstimateBasis::Full);
    assert_eq!(estimate.bytes, result.bytes);
}

#[test]
fn gpu_dither_uses_the_whole_palette_budget_on_real_footage() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // 255 colours is the most the engine uses (one index is kept for transparency).
    let proj = gif_project(&info, &source, GifEngine::GpuDither, 256);
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("full-palette.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    run_ok(&env, &req);
    let colors = distinct_colors(&read_gif(&output));
    assert!(colors <= 255, "{colors} colours");
    assert!(
        colors > 64,
        "a natural clip should use more than 64 colours, got {colors}"
    );
}

#[test]
fn target_size_fits_after_degradation_and_reports_each_pass() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let out_dir = tempfile::tempdir().expect("output folder");

    let base = gif_project(&info, &source, GifEngine::Quality, 256);
    let base_out = out_dir.path().join("base.gif");
    let base_req = request(
        base.clone(),
        info.clone(),
        base_out,
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (base_result, _) = run_ok(&env, &base_req);

    // A budget 30 % under the base size cannot be met by the base settings, so the search has to degrade.
    let budget = base_result.bytes * 7 / 10;
    let mut targeted = base;
    targeted.export.target_bytes = Some(budget);
    let output = out_dir.path().join("target.gif");
    let req = request(
        targeted,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run(&env, &req, &CancelToken::new());
    let result = result.expect("a setting fits the budget");
    assert!(result.bytes <= budget, "{} > {budget}", result.bytes);
    assert_eq!(result.bytes, fs::metadata(&output).expect("output").len());
    assert!(!partial_path(&output).exists());

    let passes: Vec<(u32, u64)> = events
        .iter()
        .filter_map(|event| match event {
            ExportEvent::TargetPass { pass, bytes, .. } => Some((*pass, *bytes)),
            _ => None,
        })
        .collect();
    assert!(
        passes.len() >= 2,
        "the base settings did not fit: {passes:?}"
    );
    let numbers: Vec<u32> = passes.iter().map(|(pass, _)| *pass).collect();
    assert_eq!(numbers, (0..numbers.len() as u32).collect::<Vec<_>>());
    assert!(
        passes[0].1 > budget,
        "pass 0 is the base settings and is too big"
    );
    let last = passes.last().expect("a pass");
    assert!(last.1 <= budget);
    assert!(matches!(events.last(), Some(ExportEvent::Done { .. })));
    // Pass events come before Done, in order.
    let done = events
        .iter()
        .position(|event| matches!(event, ExportEvent::Done { .. }))
        .expect("done");
    let last_pass = events
        .iter()
        .rposition(|event| matches!(event, ExportEvent::TargetPass { .. }))
        .expect("pass");
    assert!(last_pass < done);
}

#[test]
fn an_impossible_budget_is_unreachable_and_writes_nothing() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = gif_project(&info, &source, GifEngine::Quality, 256);
    proj.export.target_bytes = Some(1);
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("impossible.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run(&env, &req, &CancelToken::new());
    match result {
        Err(ExportError::TargetUnreachable {
            target_bytes,
            best_bytes,
        }) => {
            assert_eq!(target_bytes, 1);
            assert!(
                best_bytes > 1,
                "the smallest size is reported: {best_bytes}"
            );
        }
        other => panic!("expected TargetUnreachable, got {other:?}"),
    }
    assert!(!output.exists());
    assert!(!partial_path(&output).exists());
    let passes = events
        .iter()
        .filter(|event| matches!(event, ExportEvent::TargetPass { .. }))
        .count();
    assert!(passes >= 5, "every step was tried: {passes} passes");
}

#[test]
fn an_unreachable_budget_reports_no_size_when_estimates_rule_out_every_step() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // Half speed makes a 40-frame plan, longer than three 1 s windows, so every step is estimated before any encode. A
    // 1-byte budget rules every step out on its estimate, so no step is encoded for real.
    let mut proj = project(&info, &source, &[(0.0, 2.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.timing.speed = 0.5;
    proj.export.target_bytes = Some(1);
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("estimates-only.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run(&env, &req, &CancelToken::new());
    match result {
        Err(ExportError::TargetUnreachable {
            target_bytes: 1,
            best_bytes,
        }) => assert_eq!(best_bytes, 0, "no real encode ran, so no size is reported"),
        other => panic!("expected TargetUnreachable, got {other:?}"),
    }
    let passes: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            ExportEvent::TargetPass { bytes, .. } => Some(*bytes),
            _ => None,
        })
        .collect();
    assert!(
        !passes.is_empty() && passes.iter().all(|&bytes| bytes > 1),
        "each pass reports its estimate: {passes:?}"
    );
    assert!(!output.exists());
    assert!(!partial_path(&output).exists());
}

#[test]
fn a_missing_lut_fails_the_export_before_any_frame_is_encoded() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = gif_project(&info, &source, GifEngine::Quality, 256);
    proj.grade.lut_path = Some(
        out_dir_path("missing-look.cube")
            .to_string_lossy()
            .into_owned(),
    );
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("lut.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run(&env, &req, &CancelToken::new());
    match result {
        Err(ExportError::Lut { path, .. }) => assert!(path.contains("missing-look.cube")),
        other => panic!("expected a LUT error, got {other:?}"),
    }
    assert!(!output.exists());
    assert!(
        !events.iter().any(
            |event| matches!(event, ExportEvent::Progress { frames_done, .. } if *frames_done > 0)
        ),
        "no frame was encoded"
    );
}

/// A path under the system temporary folder that does not exist, for error tests.
fn out_dir_path(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("loopcut-export-tests-does-not-exist")
        .join(name)
}

#[test]
fn a_grade_changes_the_exported_frames() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    // The probe grade must be active: an identity grade would make the comparison below pass without testing anything.
    let probe = Grade {
        exposure: 1.0,
        ..Grade::default()
    };
    assert!(
        !loopcut_render::grade::prepare_grade(&probe, None).is_identity(),
        "an exposure grade must not prepare as the identity"
    );
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let out_dir = tempfile::tempdir().expect("output folder");
    let plain = out_dir.path().join("plain.gif");
    let graded_out = out_dir.path().join("graded.gif");
    let plain_req = request(
        gif_project(&info, &source, GifEngine::Quality, 256),
        info.clone(),
        plain.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let mut graded_proj = gif_project(&info, &source, GifEngine::Quality, 256);
    graded_proj.grade.exposure = 1.0;
    let graded_req = request(
        graded_proj,
        info,
        graded_out.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    run_ok(&env, &plain_req);
    run_ok(&env, &graded_req);
    let plain_gif = read_gif(&plain);
    let graded_gif = read_gif(&graded_out);
    assert_ne!(
        plain_gif.frames, graded_gif.frames,
        "the grade reached the export"
    );
    assert!(mean_luma(&graded_gif.frames[0]) > mean_luma(&plain_gif.frames[0]));
}

#[test]
fn captions_are_drawn_into_the_exported_frames() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    // A caption must produce layers, or the comparison below would pass without drawing anything.
    let caption = Caption {
        id: "c1".to_owned(),
        text: "HELLO".to_owned(),
        range: TimeRange::new(0.0, 1.0),
        x: 0.5,
        y: 0.8,
        style: CaptionStyle::default(),
        animation: CaptionAnimation::None,
    };
    let captions = vec![caption];
    let layers = loopcut_render::caption_layers::CaptionRenderer::new()
        .layers_at(&captions, 0.5, 1.0, 160, 120);
    assert!(
        !layers.is_empty(),
        "the test caption must draw at 0.5 s, or the export comparison checks nothing"
    );
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let out_dir = tempfile::tempdir().expect("output folder");
    let plain = out_dir.path().join("plain.gif");
    let captioned = out_dir.path().join("captioned.gif");
    run_ok(
        &env,
        &request(
            gif_project(&info, &source, GifEngine::Quality, 256),
            info.clone(),
            plain.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    let mut with_caption = gif_project(&info, &source, GifEngine::Quality, 256);
    with_caption.captions = captions;
    run_ok(
        &env,
        &request(
            with_caption,
            info,
            captioned.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    assert_ne!(read_gif(&plain).frames, read_gif(&captioned).frames);
}

#[test]
fn a_full_plan_estimate_is_the_exact_export_size() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let req = request(
        proj,
        info,
        out_dir.path().join("full.gif"),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );

    // Ten frames fit in three one-second windows, so the whole plan is encoded and the size is exact.
    let estimate = estimate_size(&env.ctx, &req, 1.0).expect("estimate");
    assert_eq!(estimate.basis, EstimateBasis::Full);
    assert_eq!(
        (estimate.low, estimate.high),
        (estimate.bytes, estimate.bytes)
    );
    assert_eq!(estimate.frames, 10);
    let (result, _) = run_ok(&env, &req);
    assert_eq!(
        estimate.bytes, result.bytes,
        "the full estimate is the export's size"
    );

    let invalid = estimate_size(&env.ctx, &req, 0.0);
    assert!(
        matches!(invalid, Err(ExportError::InvalidRequest(_))),
        "{invalid:?}"
    );
}

/// One export whose size is estimated from sampled windows.
struct SampledCase {
    file: &'static str,
    range: (f64, f64),
    /// Output speed: 0.5 doubles the plan's length.
    speed: f64,
    fps: u32,
    width: u32,
    format: ExportFormat,
    engine: GifEngine,
    /// Window length of the estimate, in output seconds.
    window_seconds: f64,
    /// Frames the plan has.
    planned_frames: u64,
}

/// Estimates `case` from sampled windows, exports it for real, and returns the estimate and the export's result.
fn estimate_and_export(
    paths: &FfmpegPaths,
    gpu: &Arc<GpuContext>,
    case: &SampledCase,
) -> (SizeEstimate, ExportResult) {
    let source = corpus(case.file);
    let info = media(paths, &source);
    let mut proj = project(&info, &source, &[case.range]);
    proj.output.fps = Rational::integer(case.fps);
    proj.output.size = OutputSize::Width { width: case.width };
    proj.timing.speed = case.speed;
    proj.export.format = case.format;
    proj.export.gif_engine = case.engine;
    let out_dir = tempfile::tempdir().expect("output folder");
    let req = request(
        proj,
        info,
        out_dir.path().join("sampled.out"),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let env = env(paths.clone(), Arc::clone(gpu));
    let estimate = estimate_size(&env.ctx, &req, case.window_seconds).expect("estimate");
    assert_eq!(
        estimate.basis,
        EstimateBasis::Sample,
        "{} {:?} {:?}",
        case.file,
        case.format,
        case.engine
    );
    assert_eq!(estimate.frames, case.planned_frames, "{}", case.file);
    let (result, _) = run_ok(&env, &req);
    (estimate, result)
}

#[test]
fn sampled_estimates_land_within_35_percent_of_the_real_export() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    // Half-second windows: each plan is longer than three windows, so the estimate is sampled.
    let base = |format, engine| SampledCase {
        file: "h264_320x240.mp4",
        range: (0.0, 2.0),
        speed: 1.0,
        fps: 10,
        width: 160,
        format,
        engine,
        window_seconds: 0.5,
        planned_frames: 20,
    };
    let cases = [
        base(ExportFormat::Gif, GifEngine::Quality),
        SampledCase {
            file: "portrait_rotated.mp4",
            fps: 15,
            width: 120,
            planned_frames: 30,
            ..base(ExportFormat::Gif, GifEngine::Quality)
        },
        base(ExportFormat::Mp4H264, GifEngine::Quality),
        base(ExportFormat::Gif, GifEngine::Fast),
        base(ExportFormat::Gif, GifEngine::GpuDither),
    ];
    for case in &cases {
        let (estimate, result) = estimate_and_export(&paths, &gpu, case);
        let ratio = estimate.bytes as f64 / result.bytes as f64;
        eprintln!(
            "{} {:?} {:?} (0.5 s windows): estimated {} bytes (band {}..{}), exported {} bytes, ratio {ratio:.3}",
            case.file,
            case.format,
            case.engine,
            estimate.bytes,
            estimate.low,
            estimate.high,
            result.bytes
        );
        assert!(
            (0.65..=1.35).contains(&ratio),
            "{} {:?} {:?}: estimate {} vs export {} (ratio {ratio:.3})",
            case.file,
            case.format,
            case.engine,
            estimate.bytes,
            result.bytes
        );
        assert!(estimate.low <= estimate.bytes && estimate.bytes <= estimate.high);
    }
}

/// The search estimates each step over one-second windows, and the rule-out margins apply to those estimates. The clip
/// plans are 40 frames (60 for the portrait clip), longer than three one-second windows, so each estimate is sampled at
/// the window the search uses.
#[test]
fn sampled_estimates_at_the_search_window_are_measured_for_each_engine() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let base = |format, engine| SampledCase {
        file: "h264_320x240.mp4",
        range: (0.0, 2.0),
        speed: 0.5,
        fps: 10,
        width: 160,
        format,
        engine,
        window_seconds: 1.0,
        planned_frames: 40,
    };
    let portrait = |format, engine| SampledCase {
        file: "portrait_rotated.mp4",
        fps: 15,
        width: 120,
        planned_frames: 60,
        ..base(format, engine)
    };
    let cases = [
        base(ExportFormat::Gif, GifEngine::Quality),
        base(ExportFormat::Gif, GifEngine::Fast),
        base(ExportFormat::Gif, GifEngine::GpuDither),
        base(ExportFormat::Mp4H264, GifEngine::Quality),
        portrait(ExportFormat::Gif, GifEngine::Quality),
        portrait(ExportFormat::Gif, GifEngine::Fast),
        portrait(ExportFormat::Gif, GifEngine::GpuDither),
    ];
    for case in &cases {
        let (estimate, result) = estimate_and_export(&paths, &gpu, case);
        let ratio = estimate.bytes as f64 / result.bytes as f64;
        eprintln!(
            "MARGIN {:?} {:?}: estimated {} bytes, exported {} bytes, estimate/export {ratio:.3}",
            case.format, case.engine, estimate.bytes, result.bytes
        );
        assert!(
            (0.65..=1.35).contains(&ratio),
            "{:?} {:?}: estimate {} vs export {} (ratio {ratio:.3})",
            case.format,
            case.engine,
            estimate.bytes,
            result.bytes
        );
    }
}

/// A GIF Quality step whose estimate is above its budget but inside the rule-out margin is encoded for real, and kept
/// when the real size fits. The budget is the real size, so the estimate is above it by less than the margin.
#[test]
fn a_quality_step_inside_the_margin_is_encoded_and_kept_when_it_fits() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // Half speed makes a 40-frame plan, longer than three 1 s windows, so the step has a sampled estimate.
    let mut proj = project(&info, &source, &[(0.0, 2.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.timing.speed = 0.5;
    let out_dir = tempfile::tempdir().expect("output folder");
    let plain_req = request(
        proj.clone(),
        info.clone(),
        out_dir.path().join("plain.gif"),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (plain, _) = run_ok(&env, &plain_req);
    let estimate = estimate_size(&env.ctx, &plain_req, 1.0).expect("estimate");
    assert_eq!(estimate.basis, EstimateBasis::Sample);
    let real = plain.bytes as f64;
    let sampled = estimate.bytes as f64;
    // The GIF Quality margin is 1.15: the estimate must be above the real size and within 15 % of it.
    assert!(
        sampled > real && sampled <= 1.15 * real,
        "the estimate ({sampled}) must sit above the real size ({real}) and inside the 1.15 margin"
    );

    let mut targeted = proj;
    targeted.export.target_bytes = Some(plain.bytes);
    let output = out_dir.path().join("target.gif");
    let req = request(
        targeted,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run(&env, &req, &CancelToken::new());
    let result = result.expect("the first step fits");
    let passes: Vec<(u32, u64)> = events
        .iter()
        .filter_map(|event| match event {
            ExportEvent::TargetPass { pass, bytes, .. } => Some((*pass, *bytes)),
            _ => None,
        })
        .collect();
    // The step was encoded for real, not ruled out on its estimate, so its pass reports the real size and it is kept.
    assert_eq!(passes, vec![(0, plain.bytes)]);
    assert_eq!(result.bytes, plain.bytes);
    assert!(!partial_path(&output).exists());
}

#[test]
fn a_video_step_inside_the_margin_is_encoded_and_kept_when_it_fits() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // Half speed doubles the 2 s clip to a 4 s plan at 10 fps: 40 frames, longer than three 1 s windows, so the step
    // has a sampled estimate.
    let mut proj = project(&info, &source, &[(0.0, 2.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.export.format = ExportFormat::Mp4H264;
    proj.timing.speed = 0.5;
    let out_dir = tempfile::tempdir().expect("output folder");
    let plain_req = request(
        proj.clone(),
        info.clone(),
        out_dir.path().join("plain.mp4"),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (plain, _) = run_ok(&env, &plain_req);
    let estimate = estimate_size(&env.ctx, &plain_req, 1.0).expect("estimate");
    assert_eq!(estimate.basis, EstimateBasis::Sample);
    let real = plain.bytes as f64;
    let sampled = estimate.bytes as f64;
    // The budget is between the real size and 0.85 of the estimate, the band the old low-end rule ruled out.
    assert!(
        real < 0.85 * sampled,
        "this clip's estimate ({sampled}) is too close to its real size ({real}) for the test"
    );
    let budget = (real + (0.85 * sampled - real) / 2.0).round() as u64;
    assert!(
        sampled <= 1.4 * budget as f64,
        "the estimate is outside the video margin at this budget"
    );

    let mut targeted = proj;
    targeted.export.target_bytes = Some(budget);
    let output = out_dir.path().join("target.mp4");
    let req = request(
        targeted,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run(&env, &req, &CancelToken::new());
    let result = result.expect("the first step fits");
    let passes: Vec<(u32, u64)> = events
        .iter()
        .filter_map(|event| match event {
            ExportEvent::TargetPass { pass, bytes, .. } => Some((*pass, *bytes)),
            _ => None,
        })
        .collect();
    // The step was encoded for real, not ruled out on its estimate: its pass reports the real size and it is kept.
    assert_eq!(passes, vec![(0, plain.bytes)], "budget {budget}");
    assert_eq!(result.bytes, plain.bytes);
    assert!(result.bytes <= budget);
}

/// Exports `range` of the test clip with the Fast engine (128 colours, Bayer dither) at `fps` and 160 px wide. Encodes the
/// same range with the one-pass FFmpeg graph the engine replaced: palettegen over every frame, then paletteuse. Returns
/// the two sizes in bytes.
fn fast_engine_and_one_pass_bytes(
    paths: &FfmpegPaths,
    gpu: Arc<GpuContext>,
    range: (f64, f64),
    fps: u32,
) -> (f64, f64) {
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(paths, &source);
    let mut proj = project(&info, &source, &[range]);
    proj.output.fps = Rational::integer(fps);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.export.gif_engine = GifEngine::Fast;
    proj.export.colors = 128;
    proj.export.dither = Dither::Bayer;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("fast.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    run_ok(&env, &req);
    let exported = fs::metadata(&output).expect("export").len() as f64;

    let reference = out_dir.path().join("reference.gif");
    let graph = format!(
        "fps={fps},scale=160:120:flags=bicubic,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff:reserve_transparent=0[p];[b][p]paletteuse=dither={}:diff_mode=rectangle",
        loopcut_export::gif::fast::ffmpeg_dither(Dither::Bayer)
    );
    let status = std::process::Command::new(&paths.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss"])
        .arg(range.0.to_string())
        .arg("-t")
        .arg((range.1 - range.0).to_string())
        .arg("-i")
        .arg(&source)
        .args(["-vf", &graph, "-loop", "0", "-f", "gif"])
        .arg(&reference)
        .status()
        .expect("run the reference FFmpeg");
    assert!(status.success(), "the reference encode failed");
    let reference_len = fs::metadata(&reference).expect("reference").len() as f64;
    (exported, reference_len)
}

#[test]
fn fast_engine_size_is_close_to_a_one_pass_palette_graph() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    // 1 s at 10 fps: every frame of the plan is a palette sample.
    let (exported, reference) = fast_engine_and_one_pass_bytes(&paths, gpu, (0.5, 1.5), 10);
    let ratio = exported / reference;
    eprintln!(
        "fast engine {exported} bytes, one-pass reference {reference} bytes, ratio {ratio:.3}"
    );
    assert!(
        (0.85..=1.15).contains(&ratio),
        "the Fast engine is {ratio:.3} times the one-pass reference"
    );
}

#[test]
fn fast_engine_size_is_close_to_a_one_pass_palette_graph_on_a_plan_longer_than_the_sample() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    // 2 s at 20 fps is 40 frames: the palette samples 30 of them evenly, and the one-pass graph reads all 40.
    let (exported, reference) = fast_engine_and_one_pass_bytes(&paths, gpu, (0.0, 2.0), 20);
    let ratio = exported / reference;
    eprintln!(
        "fast engine (40 frames) {exported} bytes, one-pass reference {reference} bytes, ratio {ratio:.3}"
    );
    assert!(
        (0.85..=1.15).contains(&ratio),
        "the Fast engine is {ratio:.3} times the one-pass reference on a 40-frame plan"
    );
}

#[test]
fn fast_engine_encodes_every_frame_of_a_plan_longer_than_the_palette_sample() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // 2 s at 20 fps is 40 frames: the palette is built from 30 of them, and the encode pass still writes all 40.
    let mut proj = project(&info, &source, &[(0.0, 2.0)]);
    proj.output.fps = Rational::integer(20);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.export.gif_engine = GifEngine::Fast;
    proj.export.colors = 64;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("long.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run_ok(&env, &req);
    assert_eq!(result.frames, 40);
    assert!(fallbacks(&events).is_empty());
    let gif = read_gif(&output);
    assert_eq!(gif.frames.len(), 40);
    assert!(gif.delays.iter().all(|&d| d == 5), "{:?}", gif.delays);
    assert!(distinct_colors(&gif) <= 64);
    assert!(!partial_path(&output).exists());
}

#[test]
fn a_cancelled_sampled_estimate_returns_cancelled_and_writes_nothing() {
    let (Some(paths), Some(gpu)) = (paths(), software_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    // Half speed makes the plan longer than three 1 s windows, so the estimate is sampled and checks the token.
    let mut proj = project(&info, &source, &[(0.0, 2.0)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    proj.timing.speed = 0.5;
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("never.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    let result = estimate_size_with_cancel(&env.ctx, &req, 1.0, &cancel);
    assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
    assert!(!output.exists());
    assert!(!partial_path(&output).exists());
}

#[test]
#[ignore = "timing benchmark; run with --ignored --nocapture in release"]
fn bench_gif_480x270_100_frames() {
    let Some(paths) = paths() else {
        return;
    };
    let dir = tempfile::tempdir().expect("temporary folder");
    let source = dir.path().join("testsrc16x9.mp4");
    generate(
        &paths,
        "testsrc2=size=640x360:rate=30:duration=4",
        &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        &source,
    );
    let info = media(&paths, &source);

    let mut adapters: Vec<(&str, Arc<GpuContext>)> = Vec::new();
    if let Some(software) = software_gpu() {
        adapters.push(("software", software));
    }
    if let Ok(auto) = GpuContext::new(&AdapterChoice::Auto)
        && !auto.is_software()
    {
        adapters.push(("hardware", Arc::new(auto)));
    }
    for (label, gpu) in adapters {
        let env = env(paths.clone(), gpu);
        let mut proj = project(&info, &source, &[(0.0, 2.0)]);
        proj.output.fps = Rational::integer(50);
        proj.output.size = OutputSize::Width { width: 480 };
        let out_dir = tempfile::tempdir().expect("output folder");
        let output = out_dir.path().join("bench.gif");
        let started = Instant::now();
        let (result, _) = run_ok(
            &env,
            &request(
                proj,
                info.clone(),
                output.clone(),
                DecodeChain::CPU,
                EncoderBackend::Cpu,
            ),
        );
        let seconds = started.elapsed().as_secs_f64();
        let gif = read_gif(&output);
        assert_eq!((gif.width, gif.height), (480, 270));
        println!(
            "{label} ({}): {} frames in {seconds:.2} s = {:.1} frames/s, {} bytes",
            env.ctx.gpu.desc.name,
            result.frames,
            result.frames as f64 / seconds,
            result.bytes
        );
    }
}

#[test]
#[ignore = "hardware smoke test: runs the Auto adapter; run with --ignored on a machine with a GPU"]
fn the_auto_adapter_exports_when_hardware_exists() {
    let Some(paths) = paths() else {
        return;
    };
    let Ok(auto) = GpuContext::new(&AdapterChoice::Auto) else {
        eprintln!("skipping: no GPU adapter at all");
        return;
    };
    if auto.is_software() {
        eprintln!("skipping: no hardware adapter; only the software path ran");
        return;
    }
    let env = env(paths.clone(), Arc::new(auto));
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let mut proj = project(&info, &source, &[(0.5, 1.5)]);
    proj.output.fps = Rational::integer(10);
    proj.output.size = OutputSize::Width { width: 160 };
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("auto.gif");
    run_ok(
        &env,
        &request(
            proj,
            info,
            output.clone(),
            DecodeChain::CPU,
            EncoderBackend::Cpu,
        ),
    );
    let gif = read_gif(&output);
    assert_eq!((gif.width, gif.height), (160, 120));
    assert_eq!(gif.frames.len(), 10);
}

/// The machine's own adapter, or `None` (after a note) when only the software adapter exists.
fn hardware_gpu() -> Option<Arc<GpuContext>> {
    match GpuContext::new(&AdapterChoice::Auto) {
        Ok(ctx) if !ctx.is_software() => Some(Arc::new(ctx)),
        _ => {
            eprintln!("skipping: no hardware GPU adapter");
            None
        }
    }
}

/// Runs on the dev machine's GPU, where the GPU-dither pass is not WARP. Run with `-- --ignored`.
#[test]
#[ignore = "needs a hardware GPU; run on the target machine"]
fn gpu_dither_export_runs_on_the_hardware_adapter() {
    let (Some(paths), Some(gpu)) = (paths(), hardware_gpu()) else {
        return;
    };
    let env = env(paths.clone(), gpu);
    let source = corpus("h264_320x240.mp4");
    let info = media(&paths, &source);
    let proj = gif_project(&info, &source, GifEngine::GpuDither, 64);
    let out_dir = tempfile::tempdir().expect("output folder");
    let output = out_dir.path().join("hardware-dither.gif");
    let req = request(
        proj,
        info,
        output.clone(),
        DecodeChain::CPU,
        EncoderBackend::Cpu,
    );
    let (result, events) = run_ok(&env, &req);
    assert!(
        fallbacks(&events).is_empty(),
        "the hardware pass runs without a fallback: {:?}",
        fallbacks(&events)
    );
    let gif = read_gif(&output);
    assert_eq!(gif.frames.len() as u64, result.frames);
    assert!(distinct_colors(&gif) <= 64);
}

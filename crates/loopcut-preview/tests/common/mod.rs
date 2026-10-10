//! Helpers shared by the preview test binaries. Every test needs the staged FFmpeg build and the media corpus; without
//! them a test prints a notice and passes. A WARP adapter is required; a hardware adapter adds a second pass.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use loopcut_core::{Clip, Project, TimeRange};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use loopcut_media::{FfmpegCommand, FfmpegPaths, InputOptions, MediaError, SpawnIo, probe};
use loopcut_preview::{PreviewEngine, PreviewSource, RenderedFrame, Viewport};
use loopcut_render::caption_layers::CaptionRenderer;
use loopcut_render::gpu::{AdapterChoice, GpuContext};

/// Every adapter the tests run on: WARP always, the default hardware adapter when it is hardware. One context per
/// adapter is shared by the whole binary, as in the app.
pub fn contexts() -> Vec<(&'static str, Arc<GpuContext>)> {
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

/// The first adapter of [`contexts`], or `None` (after a notice).
pub fn first_context() -> Option<Arc<GpuContext>> {
    contexts().into_iter().next().map(|(_, gpu)| gpu)
}

/// The staged FFmpeg build, or `None` (after a notice) when it is missing. With `LOOPCUT_REQUIRE_FFMPEG=1` a missing
/// build fails the test instead, so that a CI run without FFmpeg cannot pass by skipping everything.
pub fn paths() -> Option<FfmpegPaths> {
    let found = FfmpegPaths::discover_dev();
    if found.is_none() {
        if std::env::var("LOOPCUT_REQUIRE_FFMPEG").is_ok_and(|value| value == "1") {
            panic!(
                "LOOPCUT_REQUIRE_FFMPEG=1 but no staged FFmpeg build was found (set LOOPCUT_FFMPEG_DIR or stage src-tauri/resources/ffmpeg)"
            );
        }
        eprintln!(
            "SKIP: no staged FFmpeg build (set LOOPCUT_FFMPEG_DIR or stage src-tauri/resources/ffmpeg)"
        );
    }
    found
}

/// A file from the shared media corpus, or `None` when it is absent.
pub fn corpus(name: &str) -> Option<PathBuf> {
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

/// A frame store with its own temporary spill folder. The guard keeps the folder alive.
pub fn store(ram_budget: u64) -> (Arc<FrameStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FrameStore::new(FrameStoreConfig {
        ram_budget_bytes: ram_budget,
        disk_budget_bytes: 2 * 1024 * 1024 * 1024,
        spill_dir: dir.path().join("spill"),
    })
    .expect("frame store");
    (Arc::new(store), dir)
}

/// An engine whose store has `ram_budget` bytes of RAM. The engine takes its window budget from the store.
pub fn engine_with(
    gpu: &Arc<GpuContext>,
    paths: &FfmpegPaths,
    ram_budget: u64,
) -> (PreviewEngine, Arc<FrameStore>, tempfile::TempDir) {
    let (store, dir) = store(ram_budget);
    let captions = Arc::new(Mutex::new(CaptionRenderer::new()));
    let engine = PreviewEngine::new(Arc::clone(gpu), paths.clone(), Arc::clone(&store), captions)
        .expect("engine");
    (engine, store, dir)
}

/// An engine with a 512 MiB store, as most tests use.
pub fn engine_on(
    gpu: &Arc<GpuContext>,
    paths: &FfmpegPaths,
) -> (PreviewEngine, Arc<FrameStore>, tempfile::TempDir) {
    engine_with(gpu, paths, 512 * 1024 * 1024)
}

/// The probe of `file` as a preview source that decodes on `decode`.
pub fn load(paths: &FfmpegPaths, file: &Path, decode: loopcut_core::DecodeChain) -> PreviewSource {
    PreviewSource {
        path: file.to_path_buf(),
        media: probe(paths, file).expect("probe the test clip"),
        decode,
    }
}

/// A project that keeps the whole clip, with default output settings (480 px GIF at 20 fps).
pub fn project_for(source: &PreviewSource) -> Project {
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

pub fn viewport(width: u32, height: u32) -> Viewport {
    Viewport {
        width,
        height,
        dpr: 1.0,
    }
}

/// Waits until the worker has decoded every planned range of the current plan.
pub fn wait_decoded(engine: &PreviewEngine) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while engine.decode_progress() < 1.0 {
        assert!(Instant::now() < deadline, "the decode did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until the decode progress stops changing for `quiet`: the window has settled.
pub fn wait_settled(engine: &PreviewEngine, quiet: Duration) -> f32 {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last = engine.decode_progress();
    let mut since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(10));
        let now = engine.decode_progress();
        if (now - last).abs() > f32::EPSILON {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= quiet {
            return now;
        }
        assert!(Instant::now() < deadline, "the decode window never settled");
    }
}

pub fn digest(frame: &RenderedFrame) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    frame.width.hash(&mut hasher);
    frame.height.hash(&mut hasher);
    frame.rgba.hash(&mut hasher);
    hasher.finish()
}

pub fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "frames differ in size");
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / a.len().max(1) as f64
}

/// Mean of all colour bytes (alpha excluded).
pub fn mean_colour(frame: &RenderedFrame) -> f64 {
    let (sum, count) =
        frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .fold((0u64, 0u64), |(sum, count), px| {
                (
                    sum + u64::from(px[0]) + u64::from(px[1]) + u64::from(px[2]),
                    count + 3,
                )
            });
    sum as f64 / count.max(1) as f64
}

/// Number of pixels whose colour differs between two frames of the same size.
pub fn changed_pixels(a: &RenderedFrame, b: &RenderedFrame) -> usize {
    assert_eq!(
        (a.width, a.height),
        (b.width, b.height),
        "frames differ in size"
    );
    a.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0)
        .filter(|(x, y)| x[..3] != y[..3])
        .count()
}

/// Generates a clip with lavfi into `out`, encoded with `output_args`.
pub fn generate(
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

/// Serialises the timing-sensitive tests of one binary, so that they do not compete for the CPU with each other.
pub fn timing_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

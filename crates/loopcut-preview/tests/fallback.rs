//! CPU fallback: a hardware decode chain that fails is replaced by the CPU chain for the rest of the source.
//!
//! This is its own test binary because it sets `LOOPCUT_FORCE_HW_FAIL`, which is process-wide. Run with
//! `cargo test -p loopcut-preview --test fallback`. It needs the staged FFmpeg build and a WARP adapter.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use loopcut_core::hw::FORCE_HW_FAIL_ENV;
use loopcut_core::{Clip, DecodeBackend, DecodeChain, GpuScaler, Project, TimeRange};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use loopcut_media::{FfmpegPaths, probe};
use loopcut_preview::{PreviewEngine, PreviewQuality, PreviewSource, Viewport};
use loopcut_render::caption_layers::CaptionRenderer;
use loopcut_render::gpu::{AdapterChoice, GpuContext};

fn corpus(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("loopcut-media")
        .join("tests")
        .join("media")
        .join("generated")
        .join(name);
    path.is_file().then_some(path)
}

fn engine(gpu: &Arc<GpuContext>, paths: &FfmpegPaths, dir: &Path) -> PreviewEngine {
    let store = FrameStore::new(FrameStoreConfig {
        ram_budget_bytes: 256 * 1024 * 1024,
        disk_budget_bytes: 1024 * 1024 * 1024,
        spill_dir: dir.join("spill"),
    })
    .expect("frame store");
    let captions = Arc::new(std::sync::Mutex::new(CaptionRenderer::new()));
    PreviewEngine::new(Arc::clone(gpu), paths.clone(), Arc::new(store), captions).expect("engine")
}

fn wait_decoded(engine: &PreviewEngine) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while engine.decode_progress() < 1.0 {
        assert!(Instant::now() < deadline, "the decode did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The staged FFmpeg build. Missing it is a skip, unless `LOOPCUT_REQUIRE_FFMPEG=1` makes it a failure.
fn ffmpeg_or_skip() -> Option<FfmpegPaths> {
    let found = FfmpegPaths::discover_dev();
    if found.is_none() {
        if std::env::var("LOOPCUT_REQUIRE_FFMPEG").is_ok_and(|value| value == "1") {
            panic!("LOOPCUT_REQUIRE_FFMPEG=1 but no staged FFmpeg build was found");
        }
        eprintln!("SKIP: needs the staged FFmpeg build and the test corpus");
    }
    found
}

#[test]
fn a_failing_hardware_chain_falls_back_to_the_cpu() {
    let (Some(paths), Some(file)) = (ffmpeg_or_skip(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let gpu = match GpuContext::new(&AdapterChoice::Software) {
        Ok(ctx) => Arc::new(ctx),
        Err(err) => {
            eprintln!("SKIP: no WARP adapter: {err}");
            return;
        }
    };
    let media = probe(&paths, &file).expect("probe the test clip");
    let clip = Clip {
        id: "clip".to_owned(),
        path: file.to_string_lossy().into_owned(),
        stream_index: media.video.index,
        segments: vec![TimeRange::new(0.0, media.video.duration)],
        holds: Vec::new(),
    };
    let project = Project::new("fallback", clip);
    let viewport = Viewport {
        width: 640,
        height: 360,
        dpr: 1.0,
    };
    let hardware = DecodeChain {
        backend: DecodeBackend::Amf { dxgi_index: 0 },
        scaler: GpuScaler::Amf,
    };
    let source = |decode: DecodeChain| PreviewSource {
        path: file.clone(),
        media: media.clone(),
        decode,
    };

    // Reference: a preview that decodes on the CPU from the start.
    let cpu_dir = tempfile::tempdir().expect("tempdir");
    let cpu = engine(&gpu, &paths, cpu_dir.path());
    cpu.set_project(source(DecodeChain::CPU), &project, viewport)
        .expect("cpu set_project");
    wait_decoded(&cpu);
    let expected = cpu
        .render_output_frame(0, PreviewQuality::Full)
        .expect("cpu frame 0");

    // The hardware chain fails by fault injection, the same switch CI uses. The variable is set before any engine
    // starts decoding and is removed before the test ends.
    //
    // SAFETY: this binary runs one test, and no other thread reads or writes the environment while it is set.
    unsafe { std::env::set_var(FORCE_HW_FAIL_ENV, "amf") };
    let dir = tempfile::tempdir().expect("tempdir");
    let fallback = engine(&gpu, &paths, dir.path());
    let outcome = fallback
        .set_project(source(hardware), &project, viewport)
        .and_then(|_| {
            wait_decoded(&fallback);
            fallback.render_output_frame(0, PreviewQuality::Full)
        });
    // SAFETY: as above; the variable is removed before any assertion can panic.
    unsafe { std::env::remove_var(FORCE_HW_FAIL_ENV) };
    let frame = outcome.expect("the fallback renders");

    let fallbacks = fallback.take_fallbacks();
    assert!(!fallbacks.is_empty(), "the failed chain is recorded");
    assert_eq!(fallbacks[0].from, "amf");
    assert_eq!(fallbacks[0].to, "cpu");
    assert!(
        fallbacks[0].reason.contains(FORCE_HW_FAIL_ENV),
        "{}",
        fallbacks[0].reason
    );
    assert_eq!(
        (frame.width, frame.height),
        (expected.width, expected.height)
    );
    assert_eq!(
        frame.rgba, expected.rgba,
        "the fallback decodes the same pixels as the CPU chain"
    );
}

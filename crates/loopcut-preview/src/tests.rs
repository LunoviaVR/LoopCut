//! Engine tests that need the fault hooks in `faults`. They run the real decoder and compositor, as the integration tests
//! do, and they follow the same skip rules: a missing FFmpeg build or WARP adapter is a skip, unless
//! `LOOPCUT_REQUIRE_FFMPEG=1` makes a missing FFmpeg build a failure.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use loopcut_core::{Clip, DecodeBackend, DecodeChain, GpuScaler, Project, TimeRange};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use loopcut_media::{FfmpegPaths, probe};
use loopcut_render::caption_layers::CaptionRenderer;
use loopcut_render::gpu::{AdapterChoice, GpuContext};

use crate::{PreviewEngine, PreviewError, PreviewQuality, PreviewSource, Viewport};

/// A caption rasterizer for one engine, as the app shares one with its exports.
fn shared_captions() -> Arc<std::sync::Mutex<CaptionRenderer>> {
    Arc::new(std::sync::Mutex::new(CaptionRenderer::new()))
}

const MIB: u64 = 1024 * 1024;

/// Serialises the tests of this binary. Each engine runs a worker, and the tests wait on timing.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The WARP adapter, shared by the binary's tests. Creating devices from several threads at once is not safe, so the
/// context is made once.
fn warp() -> Option<Arc<GpuContext>> {
    static CONTEXT: OnceLock<Option<Arc<GpuContext>>> = OnceLock::new();
    CONTEXT
        .get_or_init(|| match GpuContext::new(&AdapterChoice::Software) {
            Ok(ctx) => Some(Arc::new(ctx)),
            Err(err) => {
                eprintln!("SKIP: no WARP adapter: {err}");
                None
            }
        })
        .clone()
}

/// The staged FFmpeg build. Missing it is a skip, unless `LOOPCUT_REQUIRE_FFMPEG=1` makes it a failure.
fn paths() -> Option<FfmpegPaths> {
    let found = FfmpegPaths::discover_dev();
    if found.is_none() {
        if std::env::var("LOOPCUT_REQUIRE_FFMPEG").is_ok_and(|value| value == "1") {
            panic!("LOOPCUT_REQUIRE_FFMPEG=1 but no staged FFmpeg build was found");
        }
        eprintln!("SKIP: no staged FFmpeg build");
    }
    found
}

/// A file from the shared media corpus, or `None` after a notice.
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

/// What every test here needs: the FFmpeg build, the 320x240 corpus clip and the WARP adapter.
struct Fixture {
    paths: FfmpegPaths,
    file: PathBuf,
    gpu: Arc<GpuContext>,
}

fn fixture() -> Option<Fixture> {
    Some(Fixture {
        paths: paths()?,
        file: corpus("h264_320x240.mp4")?,
        gpu: warp()?,
    })
}

impl Fixture {
    /// An engine whose store has `budget` bytes of RAM. The temporary folder lives as long as the returned guard.
    fn engine(&self, budget: u64) -> (PreviewEngine, Arc<FrameStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            FrameStore::new(FrameStoreConfig {
                ram_budget_bytes: budget,
                disk_budget_bytes: 1024 * MIB,
                spill_dir: dir.path().join("spill"),
            })
            .expect("frame store"),
        );
        let engine = PreviewEngine::new(
            Arc::clone(&self.gpu),
            self.paths.clone(),
            Arc::clone(&store),
            shared_captions(),
        )
        .expect("engine");
        (engine, store, dir)
    }

    /// The clip as a preview source that decodes on `decode`.
    fn source(&self, decode: DecodeChain) -> PreviewSource {
        PreviewSource {
            path: self.file.clone(),
            media: probe(&self.paths, &self.file).expect("probe the test clip"),
            decode,
        }
    }
}

/// A project that keeps the whole clip, with the default 480 px GIF output.
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

fn viewport() -> Viewport {
    Viewport {
        width: 320,
        height: 240,
        dpr: 1.0,
    }
}

/// Polls `condition` until it holds or `limit` passes. Returns whether it held.
fn wait_until(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// Waits until the worker has decoded every planned range of the current plan.
fn wait_decoded(engine: &PreviewEngine) {
    assert!(
        wait_until(Duration::from_secs(120), || engine.decode_progress() >= 1.0),
        "the decode did not finish"
    );
}

/// Writes a file that is not a video, and returns its path.
fn garbage_file(dir: &Path) -> PathBuf {
    let path = dir.join("garbage.mp4");
    std::fs::write(&path, b"this is not a video stream").expect("write the garbage file");
    path
}

#[test]
fn the_engine_takes_its_window_budget_from_the_store() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, store, _dir) = fx.engine(8 * MIB);
    assert_eq!(engine.shared.ram_budget, store.ram_budget());
    assert_eq!(engine.shared.ram_budget, 8 * MIB);
}

#[test]
fn a_stalled_decode_times_out_after_the_configured_wait() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    engine.set_progress_timeout(Duration::from_millis(300));
    let faults = &engine.shared.faults;
    // The worker claims the first chunk and then sleeps inside its decode, so the render awaits a decode that makes no
    // progress.
    faults.stall_decode.store(true, Ordering::SeqCst);
    let source = fx.source(DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport())
        .expect("set_project");
    assert!(
        wait_until(Duration::from_secs(20), || faults
            .stalled
            .load(Ordering::SeqCst)),
        "the worker reached its stalled decode"
    );

    let started = Instant::now();
    let result = engine.render_output_frame(0, PreviewQuality::Full);
    let waited = started.elapsed();
    assert!(
        matches!(result, Err(PreviewError::Timeout { .. })),
        "a decode with no progress times out: {result:?}"
    );
    assert!(
        waited >= Duration::from_millis(300),
        "the wait lasted the configured time: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(10),
        "and not the default: {waited:?}"
    );
    faults.stall_decode.store(false, Ordering::SeqCst);
}

#[test]
fn a_render_whose_claim_is_cancelled_runs_the_frame_again() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let faults = &engine.shared.faults;
    // The worker stays idle, so the render runs its own claim and the injected cancellation lands on it.
    faults.pause_worker.store(true, Ordering::SeqCst);
    let source = fx.source(DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport())
        .expect("set_project");
    faults.cancel_next_claim.store(1, Ordering::SeqCst);

    let frame = engine
        .render_output_frame(0, PreviewQuality::Full)
        .expect("the cancelled claim is run again");
    assert_eq!(
        faults.cancel_next_claim.load(Ordering::SeqCst),
        0,
        "the cancellation was taken by the render's claim"
    );
    assert_eq!(
        (frame.width, frame.height),
        (320, 240),
        "the 480 px output is fitted into the 320x240 viewport"
    );
    let status = engine.status();
    assert!(
        status.last_error.is_none(),
        "a cancelled claim that runs again is not an error: {:?}",
        status.last_error
    );
    assert_eq!(status.failed_ranges, 0);
    faults.pause_worker.store(false, Ordering::SeqCst);
}

#[test]
fn a_project_update_ends_a_request_that_is_waiting_for_a_decode() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let faults = &engine.shared.faults;
    // The worker stalls inside its decode, so a render of frame 0 waits for it.
    faults.stall_decode.store(true, Ordering::SeqCst);
    let source = fx.source(DecodeChain::CPU);
    let mut project = project_for(&source);
    engine
        .set_project(source.clone(), &project, viewport())
        .expect("set_project");
    assert!(
        wait_until(Duration::from_secs(20), || faults
            .stalled
            .load(Ordering::SeqCst)),
        "the worker reached its stalled decode"
    );

    // A trim changes the plan, so the request that began under the old plan ends.
    project.clips[0].segments = vec![TimeRange::new(0.0, 0.5)];
    let result = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| engine.render_output_frame(0, PreviewQuality::Full));
        std::thread::sleep(Duration::from_millis(300));
        engine
            .set_project(source.clone(), &project, viewport())
            .expect("the update lands");
        waiter.join().expect("the waiting render does not panic")
    });
    assert!(
        matches!(result, Err(PreviewError::Cancelled)),
        "a plan change ends the waiting request: {result:?}"
    );
    faults.stall_decode.store(false, Ordering::SeqCst);
}

#[test]
fn a_grade_edit_leaves_a_request_that_is_waiting_for_a_decode_in_flight() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let faults = &engine.shared.faults;
    faults.stall_decode.store(true, Ordering::SeqCst);
    let source = fx.source(DecodeChain::CPU);
    let mut project = project_for(&source);
    engine
        .set_project(source.clone(), &project, viewport())
        .expect("set_project");
    assert!(
        wait_until(Duration::from_secs(20), || faults
            .stalled
            .load(Ordering::SeqCst)),
        "the worker reached its stalled decode"
    );

    // A grade edit keeps the plan, so the waiting request is not ended: it finishes once the decode does.
    project.grade.exposure = 0.5;
    let result = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| engine.render_output_frame(0, PreviewQuality::Full));
        std::thread::sleep(Duration::from_millis(300));
        engine
            .set_project(source.clone(), &project, viewport())
            .expect("the grade edit lands");
        faults.stall_decode.store(false, Ordering::SeqCst);
        waiter.join().expect("the waiting render does not panic")
    });
    let frame = result.expect("the request completes after the grade edit");
    assert_eq!((frame.width, frame.height), (320, 240));
}

#[test]
fn a_cancelled_playback_frame_does_not_end_playback() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let faults = &engine.shared.faults;
    let source = fx.source(DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport())
        .expect("set_project");
    wait_decoded(&engine);

    faults.cancel_next_play_frame.store(1, Ordering::SeqCst);
    let shown = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&shown);
    let handle = engine
        .play(
            0,
            true,
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                true
            }),
        )
        .expect("play");
    assert!(
        wait_until(Duration::from_secs(20), || shown.load(Ordering::SeqCst)
            >= 4),
        "playback keeps showing frames"
    );
    assert!(
        handle.is_running(),
        "a cancelled frame does not end the playback"
    );
    assert!(
        handle.take_error().is_none(),
        "a cancellation is not an error"
    );
    assert_eq!(
        faults.cancel_next_play_frame.load(Ordering::SeqCst),
        0,
        "the cancelled frame was taken"
    );
    handle.stop();
    handle.wait();
}

#[test]
fn a_panicking_playback_thread_still_clears_the_playing_flag() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let source = fx.source(DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport())
        .expect("set_project");
    engine
        .shared
        .faults
        .panic_playback
        .store(true, Ordering::SeqCst);
    let handle = engine.play(0, false, Box::new(|_| true)).expect("play");
    assert!(
        wait_until(Duration::from_secs(20), || !handle.is_running()),
        "the playback thread ends"
    );
    assert!(
        !engine.status().playing,
        "the playing flag is cleared even though the thread panicked"
    );
}

#[test]
fn a_data_error_mid_decode_is_reported_and_keeps_the_hardware_chain() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let faults = &engine.shared.faults;
    // One frame arrives, then the decode fails with a data error. The hardware chain is requested: a data error must
    // not switch the source to the CPU, because only a session that fails to start says the chain does not work.
    faults.data_error_after.store(1, Ordering::SeqCst);
    let hardware = DecodeChain {
        backend: DecodeBackend::D3d11va { dxgi_index: 0 },
        scaler: GpuScaler::None,
    };
    let source = fx.source(hardware);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport())
        .expect("set_project");
    let settled = wait_until(Duration::from_secs(60), || {
        let status = engine.status();
        status.failed_ranges >= 1 || !status.fallbacks.is_empty()
    });
    assert!(settled, "the decode fails");
    let status = engine.status();
    if let Some(fallback) = status.fallbacks.first() {
        // The injected data error must never be the reason for a fallback.
        assert!(
            !fallback.reason.contains("injected data error"),
            "a data error switched the source to the CPU: {}",
            fallback.reason
        );
        // Otherwise the hardware chain did not start on this machine. That is a setup failure, not the case under test.
        eprintln!(
            "SKIP: the D3D11VA chain is not usable here: {}",
            fallback.reason
        );
        return;
    }
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("injected data error")),
        "the data error is reported: {:?}",
        status.last_error
    );
    assert!(
        !engine.shared.core.lock().cpu_fallback,
        "a data error does not switch the source to the CPU"
    );
}

#[test]
fn last_error_clears_when_a_new_generation_starts_and_when_a_claim_finishes() {
    let _serial = serial();
    let Some(fx) = fixture() else { return };

    // A new generation (another source) starts with no error.
    let (engine, _store, dir) = fx.engine(512 * MIB);
    let valid = fx.source(DecodeChain::CPU);
    let project = project_for(&valid);
    let broken = PreviewSource {
        path: garbage_file(dir.path()),
        media: valid.media.clone(),
        decode: DecodeChain::CPU,
    };
    engine
        .set_project(broken, &project, viewport())
        .expect("broken set_project");
    assert!(
        wait_until(Duration::from_secs(60), || engine
            .status()
            .last_error
            .is_some()),
        "the broken source reports an error"
    );
    engine
        .set_project(valid.clone(), &project, viewport())
        .expect("valid set_project");
    wait_decoded(&engine);
    assert!(
        engine.status().last_error.is_none(),
        "a new generation starts with no error: {:?}",
        engine.status().last_error
    );

    // The same generation: a claim that finishes clears the error its predecessor left.
    let (engine, _store, _dir) = fx.engine(512 * MIB);
    let faults = &engine.shared.faults;
    let segment = |project: &mut Project, start: f64, end: f64| {
        project.clips[0].segments = vec![TimeRange::new(start, end)];
    };
    let mut first = project_for(&valid);
    segment(&mut first, 0.0, 0.3);
    faults.data_error_after.store(1, Ordering::SeqCst);
    engine
        .set_project(valid.clone(), &first, viewport())
        .expect("first set_project");
    assert!(
        wait_until(Duration::from_secs(60), || engine
            .status()
            .last_error
            .is_some()),
        "the injected failure is reported"
    );
    faults.data_error_after.store(0, Ordering::SeqCst);
    let mut second = project_for(&valid);
    segment(&mut second, 0.5, 0.8);
    engine
        .set_project(valid.clone(), &second, viewport())
        .expect("second set_project");
    assert!(
        wait_until(Duration::from_secs(60), || engine
            .status()
            .last_error
            .is_none()),
        "a claim that finishes clears the error"
    );
    assert_eq!(engine.status().failed_ranges, 0);
}

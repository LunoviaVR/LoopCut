//! Playback: stopping, looping across a plan change, late frames, pacing and status.
//!
//! Run with `cargo test -p loopcut-preview --test playback`. The timing tests take a lock so they do not compete for
//! the CPU with each other. Needs the staged FFmpeg build and the test corpus.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::*;
use loopcut_core::project::ExportFormat;
use loopcut_core::{DecodeChain, OutputSize, Rational, TimeRange};
use loopcut_preview::{PreviewError, PreviewSource};

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

#[test]
fn dropping_the_engine_ends_playback_within_two_seconds() {
    let _timing = timing_lock();
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport(320, 240))
        .expect("set_project");
    // Looping with a sink that never stops: the only way out is the engine going away while decodes are pending.
    let handle = engine.play(0, true, Box::new(|_| true)).expect("play");
    std::thread::sleep(Duration::from_millis(5));
    drop(engine);

    let started = Instant::now();
    let ended = wait_until(Duration::from_secs(2), || !handle.is_running());
    assert!(
        ended,
        "the playback thread is still running {:?} after the engine was dropped",
        started.elapsed()
    );
    assert!(
        handle.take_error().is_none(),
        "a dropped engine is a cancellation, not an error"
    );
}

#[test]
fn looping_wraps_within_the_plan_when_it_shrinks_under_playback() {
    let _timing = timing_lock();
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let mut project = project_for(&source);
    engine
        .set_project(source.clone(), &project, viewport(320, 240))
        .expect("set_project");
    let shown = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&shown);
    let handle = engine
        .play(
            30,
            true,
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                true
            }),
        )
        .expect("play");
    assert!(
        wait_until(Duration::from_secs(10), || shown.load(Ordering::SeqCst)
            >= 5),
        "playback shows frames"
    );

    // The plan shrinks to three frames while the playhead is past the new end.
    project.clips[0].segments = vec![TimeRange::new(0.0, 0.15)];
    engine
        .set_project(source, &project, viewport(320, 240))
        .expect("shrunk set_project");
    let before = shown.load(Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(10), || shown.load(Ordering::SeqCst)
            >= before + 10),
        "the looping playback keeps going after the plan shrinks"
    );
    assert!(
        handle.take_error().is_none(),
        "wrapping is not an error: {:?}",
        handle.take_error()
    );
    handle.stop();
    handle.wait();
}

#[test]
fn a_slow_sink_drops_late_frames_and_playback_still_ends() {
    let _timing = timing_lock();
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    let info = engine
        .set_project(source, &project, viewport(320, 240))
        .expect("set_project");
    let planned = info.frames as usize;
    let delivered = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&delivered);
    // Each frame takes 120 ms in the sink, three times the 50 ms slot of a 20 fps GIF.
    let handle = engine
        .play(
            0,
            false,
            Box::new(move |_| {
                std::thread::sleep(Duration::from_millis(120));
                counter.fetch_add(1, Ordering::SeqCst);
                true
            }),
        )
        .expect("play");
    let ended = wait_until(Duration::from_secs(60), || !handle.is_running());
    assert!(ended, "a one-shot playback ends at the end of the plan");
    let shown = delivered.load(Ordering::SeqCst);
    let dropped = handle.dropped_frames();
    assert!(dropped > 0, "late frames are dropped, not shown late");
    assert!(
        shown < planned,
        "the sink received {shown} of {planned} frames"
    );
    assert!(shown >= 1, "some frames still arrive");
    assert!(
        shown + dropped as usize <= planned,
        "every frame is either shown or dropped (shown {shown}, dropped {dropped}, planned {planned})"
    );
    assert!(handle.take_error().is_none());
}

#[test]
fn non_gif_plans_pace_by_the_frame_rate() {
    let _timing = timing_lock();
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let mut project = project_for(&source);
    // 60 fps is above the GIF limit of 50. A GIF would play it at 50 fps (2 cs per frame); an MP4 plays at 60.
    project.export.format = ExportFormat::Mp4H264;
    project.output.fps = Rational::integer(60);
    project.output.size = OutputSize::Width { width: 160 };
    let info = engine
        .set_project(source, &project, viewport(160, 120))
        .expect("set_project");
    assert!(!engine.is_gif(), "an MP4 export is not a GIF");
    assert_eq!(info.fps, Rational::integer(60), "the plan keeps 60 fps");
    let planned = info.frames as usize;
    let shown = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&shown);
    let started = Instant::now();
    let handle = engine
        .play(
            0,
            false,
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                true
            }),
        )
        .expect("play");
    handle.wait();
    let elapsed = started.elapsed().as_secs_f64();
    let expected = (planned as f64 - 1.0) / 60.0;
    assert!(
        elapsed >= expected * 0.9 && elapsed <= expected + 0.2,
        "{planned} frames at 60 fps took {elapsed:.3} s, expected about {expected:.3} s"
    );
    assert!(
        shown.load(Ordering::SeqCst) * 10 >= planned * 9,
        "most frames arrive"
    );
}

#[test]
fn status_tracks_a_playback_and_a_normal_run_has_no_error() {
    let _timing = timing_lock();
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport(320, 240))
        .expect("set_project");
    let handle = engine.play(0, false, Box::new(|_| true)).expect("play");
    assert!(engine.status().playing, "the status reports the playback");
    handle.wait();
    let status = engine.status();
    assert!(!status.playing, "the playback has ended");
    assert!(status.last_error.is_none());
}

#[test]
fn a_terminal_decode_error_ends_playback_and_is_taken_once() {
    let _timing = timing_lock();
    let (Some(paths), Some(file)) = (paths(), corpus("h264_320x240.mp4")) else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let garbage = dir.path().join("garbage.mp4");
    std::fs::write(&garbage, b"this is not a video stream").expect("write the garbage file");
    let (engine, _store, _store_dir) = engine_on(&gpu, &paths);
    // The media describes the valid clip, but the bytes come from a file that is not a video.
    let valid = load(&paths, &file, DecodeChain::CPU);
    let broken = PreviewSource {
        path: garbage,
        media: valid.media.clone(),
        decode: DecodeChain::CPU,
    };
    let project = project_for(&valid);
    engine
        .set_project(broken, &project, viewport(320, 240))
        .expect("set_project");
    let handle = engine.play(0, false, Box::new(|_| true)).expect("play");
    assert!(
        wait_until(Duration::from_secs(60), || !handle.is_running()),
        "the playback ends on the decode error"
    );
    assert!(
        matches!(handle.take_error(), Some(PreviewError::Decode(_))),
        "the decode error is the playback's terminal error"
    );
    assert!(
        handle.take_error().is_none(),
        "the terminal error is taken once"
    );
    let status = engine.status();
    assert!(!status.playing, "the playing flag is cleared");
    assert!(
        status.last_error.is_some(),
        "the engine reports the same failure"
    );
}

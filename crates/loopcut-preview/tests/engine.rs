//! Engine behaviour: decode windows, hysteresis, the source box, status, failures, grade and captions.
//!
//! Run with `cargo test -p loopcut-preview --test engine`. Needs the staged FFmpeg build and the test corpus; without
//! them each test prints a notice and passes.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use loopcut_core::project::{Caption, CaptionAnimation, CaptionStyle};
use loopcut_core::{DecodeChain, OutputSize, TimeRange};
use loopcut_preview::PreviewSource;
use loopcut_preview::{PreviewError, PreviewQuality};

/// A 1280x720, one-second test clip (generated once per test that needs it).
fn clip_720p(paths: &loopcut_media::FfmpegPaths, dir: &Path) -> Option<PathBuf> {
    let file = dir.join("testsrc_720p.mp4");
    generate(
        paths,
        "testsrc2=size=1280x720:rate=30:duration=1",
        &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        &file,
    )
    .expect("generate the 720p clip");
    Some(file)
}

#[test]
fn the_decode_is_kept_within_twice_the_need_and_renewed_outside_it() {
    let (Some(paths), Some(dir)) = (paths(), tempfile::tempdir().ok()) else {
        return;
    };
    let Some(file) = clip_720p(&paths, dir.path()) else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _store, _store_dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let project = project_for(&source);
        engine
            .set_project(source.clone(), &project, viewport(640, 360))
            .expect("first set_project");
        wait_decoded(&engine);
        let decodes = engine.decode_sessions_started();

        // The 480 px output fits 400 px wide: the need (400) is inside the kept window of the 600 px decode.
        engine
            .set_project(source.clone(), &project, viewport(400, 225))
            .expect("inside the window");
        assert_eq!(
            engine.decode_sessions_started(),
            decodes,
            "{label}: a need inside the window keeps the decode"
        );

        // At 199 px the need is below half of the decode, so the decode is renewed.
        engine
            .set_project(source, &project, viewport(200, 112))
            .expect("outside the window");
        wait_decoded(&engine);
        assert!(
            engine.decode_sessions_started() > decodes,
            "{label}: a need below half the decode renews it"
        );
    }
}

#[test]
fn flips_and_half_turns_do_not_redecode() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _store, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        engine
            .set_project(source.clone(), &project, viewport(640, 360))
            .expect("first set_project");
        wait_decoded(&engine);
        let decodes = engine.decode_sessions_started();
        project.transform.rotation = 180;
        project.transform.flip_h = true;
        project.transform.flip_v = true;
        engine
            .set_project(source, &project, viewport(640, 360))
            .expect("second set_project");
        assert_eq!(
            engine.decode_sessions_started(),
            decodes,
            "{label}: flips and a half turn keep the decode"
        );
    }
}

#[test]
fn source_frames_at_a_large_box_decode_at_the_box_size() {
    let Some(paths) = paths() else { return };
    let Some(dir) = tempfile::tempdir().ok() else {
        return;
    };
    let Some(file) = clip_720p(&paths, dir.path()) else {
        return;
    };
    for (label, gpu) in contexts() {
        // Small plan decode: a 160 px output decodes about 200 px wide.
        let (small, _s1, _d1) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut small_project = project_for(&source);
        small_project.output.size = OutputSize::Width { width: 160 };
        small
            .set_project(source.clone(), &small_project, viewport(160, 90))
            .expect("small set_project");

        // Reference: a 640 px output, so the plan decode is about 800 px wide and covers a 640 px box.
        let (reference, _s2, _d2) = engine_on(&gpu, &paths);
        let mut big_project = project_for(&source);
        big_project.output.size = OutputSize::Width { width: 640 };
        reference
            .set_project(source.clone(), &big_project, viewport(640, 360))
            .expect("reference set_project");

        let boxed = small
            .render_source_frame(0.5, 640, 360)
            .expect("source frame from the small plan");
        let expected = reference
            .render_source_frame(0.5, 640, 360)
            .expect("source frame from the plan decode");
        assert_eq!(
            (boxed.width, boxed.height),
            (expected.width, expected.height),
            "{label}: dimensions"
        );
        let diff = mean_abs_diff(&boxed.rgba, &expected.rgba);
        assert!(
            diff <= 1.0,
            "{label}: the box was filled from a 200 px decode instead of its own size (difference {diff})"
        );
    }
}

#[test]
fn grade_exposure_brightens_output_frames_and_leaves_source_frames_alone() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _store, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        engine
            .set_project(source.clone(), &project, viewport(320, 240))
            .expect("identity set_project");
        wait_decoded(&engine);
        let identity = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("identity frame");
        let identity_source = engine
            .render_source_frame(0.0, 320, 240)
            .expect("identity source frame");

        project.grade.exposure = 1.0;
        engine
            .set_project(source, &project, viewport(320, 240))
            .expect("graded set_project");
        let graded = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("graded frame");
        let graded_source = engine
            .render_source_frame(0.0, 320, 240)
            .expect("graded source frame");

        assert!(
            mean_colour(&graded) > mean_colour(&identity) + 2.0,
            "{label}: +1 exposure brightens the output: {} vs {}",
            mean_colour(&graded),
            mean_colour(&identity)
        );
        assert!(
            mean_abs_diff(&graded_source.rgba, &identity_source.rgba) <= 0.5,
            "{label}: source frames are not graded"
        );
    }
}

#[test]
fn a_caption_changes_the_pixels_where_its_text_is() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    for (label, gpu) in contexts() {
        let (engine, _store, _dir) = engine_on(&gpu, &paths);
        let source = load(&paths, &file, DecodeChain::CPU);
        let mut project = project_for(&source);
        engine
            .set_project(source.clone(), &project, viewport(320, 240))
            .expect("plain set_project");
        wait_decoded(&engine);
        let plain = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("plain frame 0");

        project.captions = vec![Caption {
            id: "c1".to_owned(),
            text: "HELLO".to_owned(),
            range: TimeRange::new(0.0, 10.0),
            x: 0.5,
            y: 0.5,
            style: CaptionStyle {
                font_size: 0.3,
                ..CaptionStyle::default()
            },
            animation: CaptionAnimation::default(),
        }];
        engine
            .set_project(source, &project, viewport(320, 240))
            .expect("captioned set_project");
        let captioned = engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("captioned frame 0");

        let changed = changed_pixels(&plain, &captioned);
        assert!(
            changed > 200,
            "{label}: the caption changed {changed} pixels over frame 0"
        );
        // The caption is centred, so nothing in the outer ten percent of the frame may change.
        let (w, h) = (plain.width as usize, plain.height as usize);
        let (margin_x, margin_y) = (w / 10, h / 10);
        let stray = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                x < margin_x || x >= w - margin_x || y < margin_y || y >= h - margin_y
            })
            .filter(|&(x, y)| {
                let i = (y * w + x) * 4;
                plain.rgba[i..i + 3] != captioned.rgba[i..i + 3]
            })
            .count();
        assert_eq!(
            stray, 0,
            "{label}: the caption changed {stray} pixels outside its place in the centre"
        );
    }
}

#[test]
fn the_decode_window_is_bounded_and_follows_the_playhead() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    // An 8 MiB store: the plan's window may hold half of it (4 MiB) and the clip's 60 frames (6.9 MB) do not fit.
    let budget = 8 * 1024 * 1024;
    let window = budget / 2;
    let (engine, store, _dir) = engine_with(&gpu, &paths, budget);
    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    let info = engine
        .set_project(source, &project, viewport(640, 360))
        .expect("set_project");
    let last = info.frames - 1;
    let progress = wait_settled(&engine, Duration::from_millis(400));
    assert!(
        progress < 0.99,
        "the window does not hold the whole plan, progress {progress}"
    );
    let stats = store.stats();
    assert!(
        stats.ram_bytes <= window,
        "the window holds {} bytes, the limit is {window}",
        stats.ram_bytes
    );
    assert_eq!(stats.disk_bytes, 0, "the window stays in RAM");

    // Moving the playhead to the end decodes the frames there, and the window stays bounded.
    engine.set_playhead(last);
    engine
        .render_output_frame(last, PreviewQuality::Reduced)
        .expect("the last frame decodes after a seek");
    let after = store.stats();
    assert!(
        after.ram_bytes <= window,
        "after a seek the window holds {} bytes",
        after.ram_bytes
    );
}

#[test]
fn a_failed_decode_stays_failed_until_the_inputs_change() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(dir) = tempfile::tempdir().ok() else {
        return;
    };
    let garbage = dir.path().join("garbage.mp4");
    std::fs::write(&garbage, b"this is not a video stream").expect("write the garbage file");
    for (label, gpu) in contexts() {
        let (engine, _store, _store_dir) = engine_on(&gpu, &paths);
        let valid = load(&paths, &file, DecodeChain::CPU);
        // The media describes the valid clip, but the bytes come from a file that is not a video.
        let broken = PreviewSource {
            path: garbage.clone(),
            media: valid.media.clone(),
            decode: DecodeChain::CPU,
        };
        let project = project_for(&valid);
        engine
            .set_project(broken.clone(), &project, viewport(320, 240))
            .expect("set_project");
        wait_settled(&engine, Duration::from_millis(300));
        assert!(
            matches!(
                engine.render_output_frame(0, PreviewQuality::Full),
                Err(PreviewError::Decode(_))
            ),
            "{label}: a broken source fails the frame"
        );
        let status = engine.status();
        assert!(
            status.failed_ranges >= 1,
            "{label}: the failed range is counted"
        );
        assert!(
            status.last_error.is_some(),
            "{label}: the error is reported"
        );

        let decodes = engine.decode_sessions_started();
        engine
            .set_project(broken, &project, viewport(320, 240))
            .expect("same inputs");
        assert!(matches!(
            engine.render_output_frame(0, PreviewQuality::Full),
            Err(PreviewError::Decode(_))
        ));
        assert_eq!(
            engine.decode_sessions_started(),
            decodes,
            "{label}: a failed range is not decoded again for the same inputs"
        );

        // A working source changes the inputs: the plan decodes again and renders.
        engine
            .set_project(valid, &project, viewport(320, 240))
            .expect("valid set_project");
        wait_decoded(&engine);
        engine
            .render_output_frame(0, PreviewQuality::Full)
            .expect("the valid source renders");
        assert_eq!(
            engine.status().failed_ranges,
            0,
            "{label}: nothing failed now"
        );
    }
}

#[test]
fn a_range_past_the_end_of_the_source_is_an_empty_decode_not_a_fallback() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let duration = source.media.video.duration;
    let mut project = project_for(&source);
    project.clips[0].segments = vec![TimeRange::new(duration + 1.0, duration + 2.0)];
    engine
        .set_project(source, &project, viewport(320, 240))
        .expect("set_project");
    // The empty range counts as decoded once the worker has tried it.
    wait_decoded(&engine);
    // Status and render agree: the range is decoded (empty, not failed), and it has no frame to show, so the render
    // reports the frame as unavailable rather than as a decode failure.
    let status = engine.status();
    assert_eq!(
        status.failed_ranges, 0,
        "an empty range is not a failed range"
    );
    assert!((status.decode_progress - 1.0).abs() < f32::EPSILON);
    let result = engine.render_output_frame(0, PreviewQuality::Full);
    assert!(
        matches!(result, Err(PreviewError::FrameUnavailable { .. })),
        "a frame past the end of the source has nothing to show: {result:?}"
    );
    assert!(
        engine.take_fallbacks().is_empty(),
        "an empty range is not a hardware failure"
    );
}

#[test]
fn a_frame_in_an_empty_range_shows_the_nearest_valid_frame() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let duration = source.media.video.duration;
    let mut project = project_for(&source);
    // A short valid range, then a range past the end of the source that decodes to no frames.
    project.clips[0].segments = vec![
        TimeRange::new(0.0, 0.3),
        TimeRange::new(duration + 1.0, duration + 2.0),
    ];
    let info = engine
        .set_project(source, &project, viewport(320, 240))
        .expect("set_project");
    wait_decoded(&engine);
    let status = engine.status();
    assert_eq!(status.failed_ranges, 0, "the empty range is not failed");
    assert!(status.last_error.is_none());

    // The last plan frame lies in the empty range. It shows the nearest valid frame, the end of the first range.
    let last = info.frames - 1;
    let frame = engine
        .render_output_frame(last, PreviewQuality::Full)
        .expect("a frame in an empty range shows the nearest valid frame");
    assert_eq!(
        (frame.width, frame.height),
        (320, 240),
        "the 480 px output is fitted into the 320x240 viewport"
    );
    assert!(
        mean_colour(&frame) > 0.0,
        "the frame carries the clip's picture"
    );
    assert!(
        engine.status().last_error.is_none(),
        "a frame that falls back is not an error"
    );
}

#[test]
fn rendering_far_frames_without_a_seek_keeps_the_window_bounded() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    // The same budget as the window test: the window may hold 4 MiB of the clip's 6.9 MB.
    let budget = 8 * 1024 * 1024;
    let window = budget / 2;
    let (engine, store, _dir) = engine_with(&gpu, &paths, budget);
    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    let info = engine
        .set_project(source, &project, viewport(640, 360))
        .expect("set_project");
    let last = info.frames - 1;
    wait_settled(&engine, Duration::from_millis(300));
    // No set_playhead anywhere: each render moves the decode window to its own frame, so frames far from the window
    // decode into it and the old frames leave.
    for frame in [last / 2, 0, last, last / 4, 3 * last / 4, last / 2] {
        engine
            .render_output_frame(frame, PreviewQuality::Reduced)
            .expect("render");
        let stats = store.stats();
        assert!(
            stats.ram_bytes <= window,
            "after frame {frame} the store holds {} bytes, the window limit is {window}",
            stats.ram_bytes
        );
    }
    assert_eq!(store.stats().disk_bytes, 0, "the window stays in RAM");
}

#[test]
fn the_wait_timeout_defaults_to_fifteen_seconds_and_can_be_set() {
    let Some(paths) = paths() else { return };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    assert_eq!(engine.progress_timeout(), Duration::from_secs(15));
    engine.set_progress_timeout(Duration::from_secs(2));
    assert_eq!(engine.progress_timeout(), Duration::from_secs(2));
}

#[test]
fn trimmed_ranges_release_their_frames() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let mut project = project_for(&source);
    project.clips[0].segments = vec![TimeRange::new(0.0, 0.3), TimeRange::new(0.5, 0.95)];
    engine
        .set_project(source.clone(), &project, viewport(320, 240))
        .expect("first set_project");
    wait_decoded(&engine);
    let before = store.stats().ram_frames;

    project.clips[0].segments = vec![TimeRange::new(0.0, 0.3)];
    engine
        .set_project(source, &project, viewport(320, 240))
        .expect("trimmed set_project");
    let after = store.stats().ram_frames;
    assert!(
        after < before,
        "the removed range's frames leave the store ({before} -> {after})"
    );
    let status = engine.status();
    assert!((status.decode_progress - 1.0).abs() < f32::EPSILON);
    assert_eq!(status.failed_ranges, 0);
    assert!(!status.playing);
    assert!(status.last_error.is_none());
}

#[test]
fn status_reports_a_healthy_decode() {
    let Some(paths) = paths() else { return };
    let Some(file) = corpus("h264_320x240.mp4") else {
        return;
    };
    let Some(gpu) = first_context() else { return };
    let (engine, _store, _dir) = engine_on(&gpu, &paths);
    let source = load(&paths, &file, DecodeChain::CPU);
    let project = project_for(&source);
    engine
        .set_project(source, &project, viewport(320, 240))
        .expect("set_project");
    wait_decoded(&engine);
    let status = engine.status();
    assert!((status.decode_progress - 1.0).abs() < f32::EPSILON);
    assert_eq!(status.failed_ranges, 0);
    assert!(status.fallbacks.is_empty());
    assert!(!status.playing);
    assert!(status.last_error.is_none());
    assert!(engine.is_gif(), "the default export is a GIF");
}

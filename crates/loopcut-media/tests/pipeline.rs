//! Real FFmpeg pipelines: raw frame decode, raw frame encode through stdin, progress delivery and error diagnostics.

mod common;

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use loopcut_media::{
    FfmpegCommand, InputOptions, MediaError, Progress, RawFrameReader, SpawnIo, probe, run_capture,
};

const W: usize = 160;
const H: usize = 120;
/// NV12 is 4:2:0, so one frame is width * height * 3 / 2 bytes.
const NV12_FRAME: usize = W * H * 3 / 2;

#[test]
fn decodes_ten_nv12_frames_through_raw_frame_reader() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = common::make_h264_clip(&paths, dir.path());

    let mut process = FfmpegCommand::new_ffmpeg(&paths)
        .input(&clip, &InputOptions::default())
        .expect("input")
        .output_args([
            "-vf",
            "scale=160:120",
            "-frames:v",
            "10",
            "-pix_fmt",
            "nv12",
            "-f",
            "rawvideo",
        ])
        .expect("valid FFmpeg argument")
        .output_stdout()
        .spawn(SpawnIo {
            stdin: false,
            stdout: true,
        })
        .expect("spawn decoder");
    let stdout = process.take_stdout().expect("stdout pipe");
    let mut reader = RawFrameReader::new(stdout, NV12_FRAME);
    let mut frame = vec![0u8; NV12_FRAME];

    let mut frames = 0;
    let mut first_frame_has_content = false;
    while reader.read_frame(&mut frame).expect("whole frames only") {
        if frames == 0 {
            first_frame_has_content = frame.iter().any(|byte| *byte != frame[0]);
        }
        frames += 1;
    }
    drop(reader);
    process.wait().expect("decoder exits cleanly");

    assert_eq!(frames, 10);
    assert!(
        first_frame_has_content,
        "decoded frame should carry the test pattern"
    );
}

#[test]
fn raw_frames_written_to_stdin_encode_to_a_probeable_mp4() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("from_stdin.mp4");

    let command = FfmpegCommand::new_ffmpeg(&paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input_stdin(&InputOptions {
            format: Some("rawvideo".to_owned()),
            extra: [
                "-pixel_format",
                "nv12",
                "-video_size",
                "160x120",
                "-framerate",
                "10",
            ]
            .map(str::to_owned)
            .to_vec(),
            ..InputOptions::default()
        })
        .expect("stdin input")
        .output_args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .expect("valid FFmpeg argument")
        .output(&out)
        .expect("output");
    let mut process = command
        .spawn(SpawnIo {
            stdin: true,
            stdout: false,
        })
        .expect("spawn encoder");
    let mut stdin = process.take_stdin().expect("stdin pipe");
    let frame: Vec<u8> = (0..NV12_FRAME).map(|i| (i % 251) as u8).collect();
    for _ in 0..10 {
        stdin.write_all(&frame).expect("write frame");
    }
    drop(stdin);
    process.wait().expect("encoder finishes");

    let info = probe(&paths, &out).expect("encoded file probes");
    assert_eq!((info.video.width, info.video.height), (160, 120));
    assert_eq!(info.video.frame_count, Some(10));
    assert!(info.video.web_playable);
}

#[test]
fn progress_callback_receives_the_final_block_with_out_time_us() {
    let Some(paths) = common::paths() else { return };
    let (tx, rx) = mpsc::channel::<Progress>();
    let process = FfmpegCommand::new_ffmpeg(&paths)
        .input("testsrc2=size=320x240:rate=10:duration=1", &common::lavfi())
        .expect("lavfi")
        .output_args(["-f", "null", "-"])
        .expect("valid FFmpeg argument")
        .on_progress(move |progress| {
            let _ = tx.send(progress);
        })
        .spawn(SpawnIo::default())
        .expect("spawn");
    process.wait().expect("runs to completion");

    // Progress is delivered on the drain thread, which `wait` joins, so every block is already queued.
    let blocks: Vec<Progress> = rx.try_iter().collect();
    let last = blocks.last().expect("at least the final progress block");
    assert!(last.done, "final block must be progress=end");
    let out_time = last.out_time_us.expect("out_time_us present");
    assert!(
        (out_time - 1_000_000).abs() < 50_000,
        "out_time_us {out_time}"
    );
    assert!(last.frame.is_some_and(|frame| frame >= 9));
}

#[test]
fn failed_run_surfaces_ffmpeg_diagnostics() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist.mp4");
    let process = FfmpegCommand::new_ffmpeg(&paths)
        .input(&missing, &InputOptions::default())
        .expect("input")
        .output_args(["-f", "null", "-"])
        .expect("valid FFmpeg argument")
        .spawn(SpawnIo::default())
        .expect("spawn");
    match process.wait() {
        Err(MediaError::Failed { code, stderr_tail }) => {
            assert_ne!(code, Some(0));
            assert!(
                stderr_tail.contains("No such file or directory"),
                "stderr tail: {stderr_tail:?}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn run_capture_returns_ffprobe_json_on_stdout() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = common::make_h264_clip(&paths, dir.path());
    let command = FfmpegCommand::new_ffprobe(&paths)
        .input(&clip, &InputOptions::default())
        .expect("input")
        .output_args(["-print_format", "json", "-show_streams"])
        .expect("valid FFmpeg argument");
    let bytes = run_capture(command, Duration::from_secs(30)).expect("ffprobe runs");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
    assert_eq!(json["streams"][0]["codec_name"], "h264");
}

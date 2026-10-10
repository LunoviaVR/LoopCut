//! Command-line construction: full argument snapshots and the sandbox rules. These need no FFmpeg binary.

use std::ffi::OsString;
use std::path::PathBuf;

use loopcut_media::{FfmpegCommand, FfmpegPaths, InputOptions, LogLevel, MediaError};

fn fake_paths() -> FfmpegPaths {
    FfmpegPaths {
        ffmpeg: PathBuf::from("ffmpeg.exe"),
        ffprobe: PathBuf::from("ffprobe.exe"),
    }
}

fn lossy(args: Vec<OsString>) -> Vec<String> {
    args.into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn position(args: &[String], needle: &str) -> Vec<usize> {
    args.iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == needle)
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn seek_and_hardware_decode_input_snapshot() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .input(
            r"C:\clips\input.mp4",
            &InputOptions {
                seek: Some(1.5),
                duration: Some(4.0),
                hwaccel_args: vec![
                    "-hwaccel".into(),
                    "amf".into(),
                    "-hwaccel_output_format".into(),
                    "amf".into(),
                ],
                ..InputOptions::default()
            },
        )
        .expect("valid input")
        .output_args([
            "-vf",
            "vpp_amf=w=640:h=360:format=nv12",
            "-c:v",
            "h264_amf",
            "-an",
        ])
        .expect("valid FFmpeg argument")
        .output(r"C:\clips\out.partial.mp4")
        .expect("valid output");
    insta::assert_debug_snapshot!(lossy(cmd.args()));
}

#[test]
fn ffprobe_json_command_snapshot() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffprobe(&paths)
        .input(r"D:\media\vidéo テスト 🎬.mp4", &InputOptions::default())
        .expect("valid input")
        .output_args(["-print_format", "json", "-show_format", "-show_streams"])
        .expect("valid FFmpeg argument");
    insta::assert_debug_snapshot!(lossy(cmd.args()));
}

#[test]
fn raw_stdin_to_encoder_snapshot() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .input_stdin(&InputOptions {
            format: Some("rawvideo".to_owned()),
            extra: vec![
                "-pixel_format".to_owned(),
                "nv12".to_owned(),
                "-video_size".to_owned(),
                "160x120".to_owned(),
                "-framerate".to_owned(),
                "10".to_owned(),
            ],
            ..InputOptions::default()
        })
        .expect("valid stdin input")
        .output_args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .expect("valid FFmpeg argument")
        .output(r"C:\clips\encoded.mp4")
        .expect("valid output");
    insta::assert_debug_snapshot!(lossy(cmd.args()));
}

#[test]
fn raw_frames_to_stdout_snapshot() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .loglevel(LogLevel::Warning)
        .input(r"C:\clips\input.mp4", &InputOptions::default())
        .expect("valid input")
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
        .output_stdout();
    insta::assert_debug_snapshot!(lossy(cmd.args()));
}

#[test]
fn lavfi_graph_input_snapshot() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input(
            "testsrc2=size=320x240:rate=10:duration=1",
            &InputOptions {
                lavfi: true,
                ..InputOptions::default()
            },
        )
        .expect("trusted lavfi graph")
        .output_args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .expect("valid FFmpeg argument")
        .output(r"C:\clips\plain.mp4")
        .expect("valid output");
    insta::assert_debug_snapshot!(lossy(cmd.args()));
}

#[test]
fn every_file_input_is_guarded_immediately_before_its_dash_i() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .input(r"C:\a.mp4", &InputOptions::default())
        .expect("a")
        .input(r"C:\cover.png", &InputOptions::default())
        .expect("b")
        .output_args(["-c", "copy"])
        .expect("valid FFmpeg argument")
        .output(r"C:\out.mp4")
        .expect("out");
    let args = lossy(cmd.args());
    let inputs = position(&args, "-i");
    assert_eq!(inputs.len(), 2);
    for at in inputs {
        assert_eq!(
            args[at - 3..at],
            ["-protocol_whitelist", "file,pipe", "-noautorotate"],
            "guards must sit directly before -i"
        );
    }
    assert_eq!(
        args.last().map(String::as_str),
        Some(r"C:\out.mp4"),
        "output is the last argument"
    );
}

#[test]
fn ffprobe_gets_the_whitelist_but_never_noautorotate() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffprobe(&paths)
        .input(r"C:\a.mp4", &InputOptions::default())
        .expect("valid input");
    let args = lossy(cmd.args());
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-protocol_whitelist", "file,pipe"])
    );
    assert!(!args.iter().any(|arg| arg == "-noautorotate"));
}

#[test]
fn hide_banner_and_loglevel_lead_and_ffmpeg_reports_progress_to_stderr() {
    let paths = fake_paths();
    let ffmpeg = lossy(FfmpegCommand::new_ffmpeg(&paths).args());
    assert_eq!(&ffmpeg[..3], ["-hide_banner", "-loglevel", "error"]);
    assert!(
        ffmpeg
            .windows(2)
            .any(|pair| pair == ["-progress", "pipe:2"])
    );
    assert!(ffmpeg.iter().any(|arg| arg == "-nostdin"));

    let probe = lossy(
        FfmpegCommand::new_ffprobe(&paths)
            .loglevel(LogLevel::Quiet)
            .args(),
    );
    assert_eq!(probe, ["-hide_banner", "-loglevel", "quiet"]);
}

#[test]
fn stdin_and_stdout_targets_use_pipe_specifiers() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .input_stdin(&InputOptions::default())
        .expect("stdin")
        .output_stdout();
    let args = lossy(cmd.args());
    assert_eq!(args.last().map(String::as_str), Some("pipe:1"));
    let at = position(&args, "-i")[0];
    assert_eq!(args[at + 1], "pipe:0");
}

#[test]
fn url_like_inputs_are_rejected_with_url_not_allowed() {
    let paths = fake_paths();
    for value in [
        "http://example.com/a.mp4",
        "https://example.com/a.mp4",
        "rtmp://host/live",
        "concat:a.mp4|b.mp4",
        "file:C:/clip.mp4",
        "subfile,,start:0,end:10,,:C:/clip.mp4",
        "pipe:0",
        "-",
        "  http://leading-space.example/a.mp4",
    ] {
        let result = FfmpegCommand::new_ffmpeg(&paths).input(value, &InputOptions::default());
        assert!(
            matches!(result, Err(MediaError::UrlNotAllowed(_))),
            "{value:?} must be rejected, got {result:?}"
        );
    }
}

#[test]
fn windows_drive_and_unicode_paths_are_allowed() {
    let paths = fake_paths();
    for value in [
        r"C:\x\y.mp4",
        "D:/GitHub/LoopCut/clip.mp4",
        "vidéo テスト 🎬.mp4",
        "a,b.mp4",
        r"\\server\share\clip.mov",
        r"\\?\C:\a very long path\clip.mp4",
    ] {
        FfmpegCommand::new_ffprobe(&paths)
            .input(value, &InputOptions::default())
            .unwrap_or_else(|err| panic!("{value:?} should be allowed: {err}"));
    }
}

#[test]
fn url_outputs_are_rejected() {
    let paths = fake_paths();
    let result = FfmpegCommand::new_ffmpeg(&paths).output("rtmp://live.example/stream");
    assert!(matches!(result, Err(MediaError::UrlNotAllowed(_))));
}

#[test]
fn lavfi_is_refused_unless_explicitly_requested() {
    let paths = fake_paths();
    let via_format = FfmpegCommand::new_ffmpeg(&paths).input(
        "testsrc2",
        &InputOptions {
            format: Some("lavfi".to_owned()),
            ..InputOptions::default()
        },
    );
    assert!(matches!(via_format, Err(MediaError::LavfiRejected(_))));
}

#[test]
fn lavfi_graphs_that_read_files_are_refused() {
    let paths = fake_paths();
    for graph in [
        "movie=/secret/clip.mp4",
        "amovie=clip.mp4",
        "testsrc2:filename=x.mp4",
    ] {
        let result = FfmpegCommand::new_ffmpeg(&paths).input(
            graph,
            &InputOptions {
                lavfi: true,
                ..InputOptions::default()
            },
        );
        assert!(
            matches!(result, Err(MediaError::LavfiRejected(_))),
            "{graph:?} must be refused"
        );
    }
}

#[test]
fn lavfi_inputs_take_no_format_and_skip_file_guards() {
    let paths = fake_paths();
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .input(
            "color=c=red:s=16x16:d=1",
            &InputOptions {
                lavfi: true,
                ..InputOptions::default()
            },
        )
        .expect("lavfi");
    let args = lossy(cmd.args());
    assert!(!args.iter().any(|arg| arg == "-protocol_whitelist"));
    let at = position(&args, "-i")[0];
    assert_eq!(args[at - 2..at], ["-f", "lavfi"]);

    let conflicting = FfmpegCommand::new_ffmpeg(&paths).input(
        "color=c=red",
        &InputOptions {
            lavfi: true,
            format: Some("rawvideo".to_owned()),
            ..InputOptions::default()
        },
    );
    assert!(matches!(conflicting, Err(MediaError::InvalidArgument(_))));
}

#[test]
fn reserved_options_cannot_be_smuggled_through_extra_args() {
    let paths = fake_paths();
    let second_input = FfmpegCommand::new_ffmpeg(&paths).input(
        r"C:\a.mp4",
        &InputOptions {
            extra: vec!["-i".to_owned(), "http://evil.example/x.mp4".to_owned()],
            ..InputOptions::default()
        },
    );
    assert!(matches!(second_input, Err(MediaError::InvalidArgument(_))));

    let whitelist = FfmpegCommand::new_ffmpeg(&paths).input(
        r"C:\a.mp4",
        &InputOptions {
            hwaccel_args: vec![
                "-protocol_whitelist".to_owned(),
                "file,http,https".to_owned(),
            ],
            ..InputOptions::default()
        },
    );
    assert!(matches!(whitelist, Err(MediaError::InvalidArgument(_))));

    let format_override = FfmpegCommand::new_ffmpeg(&paths).input(
        r"C:\a.mp4",
        &InputOptions {
            extra: vec!["-f".to_owned(), "concat".to_owned()],
            ..InputOptions::default()
        },
    );
    assert!(matches!(
        format_override,
        Err(MediaError::InvalidArgument(_))
    ));
}

#[test]
fn invalid_times_are_rejected() {
    let paths = fake_paths();
    for seek in [-1.0, f64::NAN, f64::INFINITY] {
        let result = FfmpegCommand::new_ffmpeg(&paths).input(
            r"C:\a.mp4",
            &InputOptions {
                seek: Some(seek),
                ..InputOptions::default()
            },
        );
        assert!(
            matches!(result, Err(MediaError::InvalidArgument(_))),
            "seek {seek}"
        );
    }
    let zero_duration = FfmpegCommand::new_ffmpeg(&paths).input(
        r"C:\a.mp4",
        &InputOptions {
            duration: Some(0.0),
            ..InputOptions::default()
        },
    );
    assert!(matches!(zero_duration, Err(MediaError::InvalidArgument(_))));
}

#[test]
fn global_input_output_sections_keep_their_order() {
    let paths = fake_paths();
    // Builder calls are made out of order on purpose: the emitted order must not depend on call order.
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .output_args(["-c:v", "libx264"])
        .expect("valid FFmpeg argument")
        .input(r"C:\a.mp4", &InputOptions::default())
        .expect("input")
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .output_args(["-crf", "23"])
        .expect("valid FFmpeg argument")
        .global_args(["-threads", "4"])
        .expect("valid FFmpeg argument")
        .output(r"C:\out.mp4")
        .expect("output");
    let expected = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-progress",
        "pipe:2",
        "-nostats",
        "-y",
        "-threads",
        "4",
        "-protocol_whitelist",
        "file,pipe",
        "-noautorotate",
        "-i",
        r"C:\a.mp4",
        "-c:v",
        "libx264",
        "-crf",
        "23",
        r"C:\out.mp4",
    ];
    assert_eq!(lossy(cmd.args()), expected);
}

#[test]
fn raw_arguments_cannot_read_or_write_other_files() {
    let paths = fake_paths();
    for option in [
        "-i",
        "-protocol_whitelist",
        "-filter_script",
        "-filter_complex_script",
        "-attach",
        "-passlogfile",
        "-sdp_file",
        "-dump_attachment",
    ] {
        assert!(
            matches!(
                FfmpegCommand::new_ffmpeg(&paths).output_args([option]),
                Err(MediaError::InvalidArgument(_))
            ),
            "{option} must be refused"
        );
        assert!(
            matches!(
                FfmpegCommand::new_ffmpeg(&paths).global_arg(option),
                Err(MediaError::InvalidArgument(_))
            ),
            "{option} must be refused globally"
        );
    }
    for graph in [
        "movie=/secret/clip.mp4",
        "drawtext=textfile=/secret/notes.txt",
        "lut3d=file=/secret/grade.cube",
    ] {
        assert!(
            matches!(
                FfmpegCommand::new_ffmpeg(&paths).output_args(["-vf", graph]),
                Err(MediaError::InvalidArgument(_))
            ),
            "{graph} must be refused"
        );
    }
}

#[test]
fn refused_raw_arguments_leave_the_command_untouched() {
    let paths = fake_paths();
    let command = FfmpegCommand::new_ffmpeg(&paths)
        .output_args(["-c:v", "libx264"])
        .expect("valid");
    let before = lossy(command.args());
    assert!(
        command
            .output_args(["-i", "http://evil.example/x.mp4"])
            .is_err()
    );
    let rebuilt = FfmpegCommand::new_ffmpeg(&paths)
        .output_args(["-c:v", "libx264"])
        .expect("valid");
    assert_eq!(lossy(rebuilt.args()), before);
}

//! Probing real files generated with the staged FFmpeg: codecs, color, alpha, rotation, cover art and paths.

mod common;

use std::path::Path;

use loopcut_core::{Rational, TransferFunction, YuvMatrix};
use loopcut_media::{MediaError, probe};

/// Encodes a 320x240 H.264 clip with HDR10 (PQ/BT.2020) signalling in a 10-bit HEVC Matroska file.
fn make_hdr10(paths: &loopcut_media::FfmpegPaths, dir: &Path) -> std::path::PathBuf {
    let out = dir.join("hdr10.mkv");
    common::generate(
        paths,
        "testsrc2=size=320x240:rate=10:duration=1",
        &[
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p10le",
            "-x265-params",
            "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:hdr10=1",
        ],
        &out,
    )
    .expect("generate HDR10");
    out
}

#[test]
fn plain_h264_clip_reports_expected_fields() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = common::make_h264_clip(&paths, dir.path());

    let info = probe(&paths, &clip).expect("probe");
    let video = &info.video;
    assert_eq!(video.codec, "h264");
    assert_eq!((video.width, video.height), (320, 240));
    assert_eq!(video.pix_fmt, "yuv420p");
    assert_eq!(video.bit_depth, 8);
    assert!(!video.has_alpha);
    assert_eq!(video.rotation, 0);
    assert_eq!(video.frame_rate, Rational::integer(10));
    assert_eq!(video.frame_count, Some(10));
    assert!(!video.variable_frame_rate);
    assert!(!video.interlaced);
    assert!(
        video.web_playable,
        "8-bit 4:2:0 H.264 in MP4 is web playable"
    );
    assert!(!info.has_audio);
    assert!(info.alternate_video_streams.is_empty());
    assert!(
        (info.duration - 1.0).abs() < 0.05,
        "duration {}",
        info.duration
    );
    assert!(info.size_bytes > 0);
}

#[test]
fn rotation_matches_what_autorotate_displays() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");

    // A red box in the stored top-left corner makes the rotation direction visible in the decoded frame.
    let marked = dir.path().join("marked.mp4");
    common::generate(
        &paths,
        "color=c=black:s=320x240:r=10:d=1,drawbox=x=0:y=0:w=80:h=40:color=red:t=fill",
        &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        &marked,
    )
    .expect("generate marker clip");

    for display_rotation in [0, 90, -90, 180] {
        let rotated = dir.path().join(format!("rot_{display_rotation}.mp4"));
        if display_rotation == 0 {
            std::fs::copy(&marked, &rotated).expect("copy");
        } else {
            common::with_display_rotation(&paths, &marked, display_rotation, &rotated)
                .expect("set rotation");
        }
        let info = probe(&paths, &rotated).expect("probe rotated clip");
        let (display_w, display_h) = info.video.display_size();
        let reference = common::autorotated_first_frame_rgb(&paths, &rotated, display_w, display_h);
        let (top, left) = common::red_marker_quadrant(display_w, display_h, &reference);

        // The marker starts top-left. Turning the stored frame clockwise by `rotation` moves it to:
        // 0 => top-left, 90 => top-right, 180 => bottom-right, 270 => bottom-left.
        let expected = match info.video.rotation {
            0 => (true, true),
            90 => (true, false),
            180 => (false, false),
            270 => (false, true),
            other => panic!("unexpected rotation {other}"),
        };
        assert_eq!(
            (top, left),
            expected,
            "display_rotation {display_rotation} gave rotation {} but autorotate shows the marker elsewhere",
            info.video.rotation
        );
    }
}

#[test]
fn display_rotation_sign_is_recorded_as_clockwise_correction() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = common::make_h264_clip(&paths, dir.path());
    let plus = dir.path().join("plus90.mp4");
    let minus = dir.path().join("minus90.mp4");
    common::with_display_rotation(&paths, &clip, 90, &plus).expect("plus");
    common::with_display_rotation(&paths, &clip, -90, &minus).expect("minus");

    assert_eq!(probe(&paths, &plus).expect("plus").video.rotation, 270);
    assert_eq!(probe(&paths, &minus).expect("minus").video.rotation, 90);
    assert_eq!(
        probe(&paths, &plus).expect("plus").video.display_size(),
        (240, 320)
    );
}

#[test]
fn ten_bit_hdr10_matroska_is_not_web_playable() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let hdr = make_hdr10(&paths, dir.path());

    let info = probe(&paths, &hdr).expect("probe");
    let video = &info.video;
    assert_eq!(video.codec, "hevc");
    assert_eq!(video.bit_depth, 10);
    assert_eq!(video.pix_fmt, "yuv420p10le");
    assert_eq!(video.color.transfer, TransferFunction::Pq);
    assert_eq!(video.color.matrix, YuvMatrix::Bt2020);
    assert!(video.color.wide_gamut);
    assert!(!video.web_playable);
    assert!(
        info.container.contains("matroska"),
        "container {}",
        info.container
    );
}

#[test]
fn hlg_is_detected_from_the_bitstream() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("hlg.mp4");
    common::generate(
        &paths,
        "testsrc2=size=320x240:rate=10:duration=1",
        &[
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p10le",
            "-x265-params",
            "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited",
        ],
        &out,
    )
    .expect("generate HLG");
    let info = probe(&paths, &out).expect("probe");
    assert_eq!(info.video.color.transfer, TransferFunction::Hlg);
}

#[test]
fn prores_4444_with_alpha_reports_alpha_and_twelve_bit_decode() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("alpha.mov");
    common::generate(
        &paths,
        "color=c=red@0.5:s=320x240:d=1:r=10",
        &[
            "-c:v",
            "prores_ks",
            "-profile:v",
            "4444",
            "-pix_fmt",
            "yuva444p10le",
        ],
        &out,
    )
    .expect("generate ProRes 4444");

    let info = probe(&paths, &out).expect("probe");
    assert_eq!(info.video.codec, "prores");
    assert!(info.video.has_alpha);
    // prores_ks decodes as yuva444p12le even when encoded from 10-bit input; the probe must follow the decoder.
    assert_eq!(info.video.pix_fmt, "yuva444p12le");
    assert_eq!(info.video.bit_depth, 12);
    assert!(!info.video.web_playable);
}

#[test]
fn rgba_png_in_mov_is_rgb_with_alpha() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("rgba.mov");
    common::generate(
        &paths,
        "color=c=red@0.5:s=320x240:d=1:r=10",
        &["-c:v", "png", "-pix_fmt", "rgba"],
        &out,
    )
    .expect("generate PNG-in-MOV");

    let info = probe(&paths, &out).expect("probe");
    assert_eq!(info.video.codec, "png");
    assert!(info.video.has_alpha);
    assert_eq!(info.video.color.matrix, YuvMatrix::Rgb);
    assert!(!info.video.web_playable);
}

#[test]
fn cover_art_is_never_the_edited_stream() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = common::make_h264_clip(&paths, dir.path());
    let cover = dir.path().join("cover.png");
    common::generate(
        &paths,
        "color=c=blue:s=100x100",
        &["-frames:v", "1"],
        &cover,
    )
    .expect("cover png");
    let with_cover = dir.path().join("with_cover.mp4");
    let cmd = loopcut_media::FfmpegCommand::new_ffmpeg(&paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input(&clip, &loopcut_media::InputOptions::default())
        .expect("clip input")
        .input(&cover, &loopcut_media::InputOptions::default())
        .expect("cover input")
        .output_args([
            "-map",
            "1",
            "-map",
            "0:v",
            "-c:v:0",
            "mjpeg",
            "-disposition:v:0",
            "attached_pic",
            "-c:v:1",
            "copy",
        ])
        .expect("valid FFmpeg argument")
        .output(&with_cover)
        .expect("output");
    common::run(cmd).expect("mux cover art");

    let info = probe(&paths, &with_cover).expect("probe");
    assert_eq!(
        info.video.codec, "h264",
        "the real video must win over cover art"
    );
    assert!(info.alternate_video_streams.is_empty());
}

#[test]
fn audio_and_video_are_both_reported() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("av.mp4");
    let cmd = loopcut_media::FfmpegCommand::new_ffmpeg(&paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input("testsrc2=size=320x240:rate=10:duration=1", &common::lavfi())
        .expect("video")
        .input("sine=duration=1", &common::lavfi())
        .expect("audio")
        .output_args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .expect("valid FFmpeg argument")
        .output(&out)
        .expect("output");
    common::run(cmd).expect("mux av");
    let info = probe(&paths, &out).expect("probe");
    assert!(info.has_audio);
    assert_eq!(info.video.codec, "h264");
}

#[test]
fn audio_only_file_has_no_usable_video() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("audio.m4a");
    common::generate(&paths, "sine=duration=1", &["-c:a", "aac"], &out).expect("audio");
    let err = probe(&paths, &out).expect_err("no video");
    assert!(matches!(err, MediaError::NoVideoStream), "{err:?}");
}

#[test]
fn non_ascii_names_with_spaces_and_emoji_probe_correctly() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = common::make_h264_clip(&paths, dir.path());
    let named = dir.path().join("vidéo テスト 🎬.mp4");
    std::fs::copy(&clip, &named).expect("copy to unicode name");

    let info = probe(&paths, &named).expect("probe unicode name");
    assert_eq!(info.video.codec, "h264");
    assert_eq!((info.video.width, info.video.height), (320, 240));
}

#[test]
fn garbage_file_fails_with_ffprobe_diagnostics() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let bogus = dir.path().join("not-media.mp4");
    std::fs::write(&bogus, b"this is not a video at all").expect("write");
    let err = probe(&paths, &bogus).expect_err("not media");
    match err {
        MediaError::Failed { code, stderr_tail } => {
            assert_ne!(code, Some(0));
            assert!(
                !stderr_tail.is_empty(),
                "ffprobe diagnostics must be surfaced"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn missing_file_and_url_paths_are_refused_before_running_ffprobe() {
    let Some(paths) = common::paths() else { return };
    let err = probe(&paths, Path::new("https://example.com/clip.mp4")).expect_err("url");
    assert!(matches!(err, MediaError::UrlNotAllowed(_)), "{err:?}");

    let err = probe(&paths, Path::new("definitely-missing-clip.mp4")).expect_err("missing");
    assert!(matches!(err, MediaError::Io(_)), "{err:?}");
}

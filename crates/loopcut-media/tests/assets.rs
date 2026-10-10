//! Corpus-backed tests for keyframes, thumbnails and timeline proxies, run against the generated media in
//! `tests/media/generated` (regenerate with `node scripts/gen-test-media.mjs`). Every test skips with a notice when the
//! staged FFmpeg build or a corpus file is missing, so `cargo test` stays green on a bare checkout.
//!
//! Orientation and color are checked against FFmpeg's own decode (autorotate on, or BT.709 metadata read back by
//! ffprobe), not against hard-coded expectations.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use std::time::Instant;

use loopcut_core::{ColorRange, EncoderBackend, Rational, TransferFunction, YuvMatrix};
use loopcut_media::keyframes::{keyframe_index, max_gop_seconds};
use loopcut_media::proxy::{ProxyRequest, make_proxy};
use loopcut_media::thumbnails::{ThumbnailRequest, generate_thumbnails};
use loopcut_media::{CancelToken, FfmpegCommand, FfmpegPaths, MediaError, probe};

/// Path of a corpus file, or `None` (after a notice) when it has not been generated.
fn asset(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/media/generated")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("skipping: {name} missing; run `node scripts/gen-test-media.mjs`");
        None
    }
}

/// Runs FFmpeg with `args` and returns its stdout, failing the test on a non-zero exit.
fn ffmpeg_stdout(paths: &FfmpegPaths, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new(&paths.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(args)
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// The Y (luma) plane of the first frame of `file`, decoded as 8-bit 4:2:0 with the given size.
///
/// `autorotate` controls whether FFmpeg applies the display matrix, so the same helper reads both a raw source (for a
/// reference picture) and a proxy (which must carry no display matrix at all).
fn first_luma_plane(
    paths: &FfmpegPaths,
    file: &Path,
    autorotate: bool,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut args: Vec<&str> = Vec::new();
    if !autorotate {
        args.push("-noautorotate");
    }
    let file = file.to_str().expect("UTF-8 path");
    args.extend([
        "-i",
        file,
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]);
    let frame = ffmpeg_stdout(paths, &args);
    let luma = (width * height) as usize;
    assert!(frame.len() >= luma, "decoded frame too small");
    frame[..luma].to_vec()
}

fn mean(values: &[u8]) -> f64 {
    values.iter().map(|v| f64::from(*v)).sum::<f64>() / values.len() as f64
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "compared buffers differ in size");
    a.iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs())
        .sum::<f64>()
        / a.len() as f64
}

/// Generates a clip from a lavfi source into `dir`, returning its path.
fn generated(
    paths: &FfmpegPaths,
    dir: &Path,
    name: &str,
    graph: &str,
    output_args: &[&str],
) -> PathBuf {
    let out = dir.join(name);
    common::generate(paths, graph, output_args, &out).expect("generate clip");
    out
}

#[test]
fn keyframes_of_corpus_clips_are_at_the_start() {
    let Some(paths) = common::paths() else { return };
    for name in ["h264_320x240.mp4", "vp9.webm"] {
        let Some(clip) = asset(name) else { continue };
        let info = probe(&paths, &clip).expect("probe");
        let keys =
            keyframe_index(&paths, &clip, info.video.index, info.video.start_time).expect("index");
        assert_eq!(keys, vec![0.0], "{name}: one keyframe at the start");
        let gop = max_gop_seconds(&keys, info.duration);
        assert!(
            (gop - info.duration).abs() < 0.05,
            "{name}: one GOP spans the clip ({gop} vs {})",
            info.duration
        );
    }
}

#[test]
fn keyframes_of_a_generated_gop_are_every_ten_frames() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = generated(
        &paths,
        dir.path(),
        "gop10.mp4",
        "testsrc2=size=160x120:rate=25:duration=3",
        &[
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "10",
            "-keyint_min",
            "10",
            "-sc_threshold",
            "0",
        ],
    );
    let info = probe(&paths, &clip).expect("probe");
    let keys =
        keyframe_index(&paths, &clip, info.video.index, info.video.start_time).expect("index");
    assert_eq!(
        keys.len(),
        8,
        "75 frames at GOP 10 give 8 keyframes: {keys:?}"
    );
    for (i, key) in keys.iter().enumerate() {
        assert!((key - 0.4 * i as f64).abs() < 1e-3, "keyframe {i} at {key}");
    }
    assert!((max_gop_seconds(&keys, info.duration) - 0.4).abs() < 1e-3);
}

#[test]
fn keyframes_select_the_absolute_stream_index() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    // Two video streams of one container with different GOP sizes: stream 0 has a keyframe every 10 frames (1 s),
    // stream 1 every 5 frames (0.5 s). Each index must report its own stream's keyframes.
    let clip = dir.path().join("two_gops.mkv");
    let graph = "testsrc2=size=160x120:rate=10:duration=3";
    let cmd = FfmpegCommand::new_ffmpeg(&paths)
        .global_arg("-y")
        .expect("global")
        .input(graph, &common::lavfi())
        .expect("first input")
        .input(graph, &common::lavfi())
        .expect("second input")
        .output_args([
            "-map",
            "0:v",
            "-map",
            "1:v",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-sc_threshold",
            "0",
            "-keyint_min:v:0",
            "10",
            "-g:v:0",
            "10",
            "-keyint_min:v:1",
            "5",
            "-g:v:1",
            "5",
        ])
        .expect("output args")
        .output(&clip)
        .expect("output");
    common::run(cmd).expect("generate two-stream clip");

    let first = keyframe_index(&paths, &clip, 0, 0.0).expect("stream 0");
    let second = keyframe_index(&paths, &clip, 1, 0.0).expect("stream 1");
    assert!(
        (max_gop_seconds(&first, 3.0) - 1.0).abs() < 1e-3,
        "stream 0 keys {first:?}"
    );
    assert!(
        (max_gop_seconds(&second, 3.0) - 0.5).abs() < 1e-3,
        "stream 1 keys {second:?}"
    );
    let missing = keyframe_index(&paths, &clip, 9, 0.0).expect("no such stream");
    assert!(
        missing.is_empty(),
        "a stream that does not exist has no keyframes"
    );
}

#[test]
fn keyframes_work_on_unicode_and_non_ascii_paths() {
    let Some(paths) = common::paths() else { return };
    let Some(clip) = asset("unicode vidéo テスト 🎬.mp4") else {
        return;
    };
    let keys = keyframe_index(&paths, &clip, 0, 0.0).expect("index");
    assert_eq!(keys, vec![0.0]);
}

#[test]
fn ten_thumbnails_of_the_h264_clip_are_evenly_spaced() {
    let Some(paths) = common::paths() else { return };
    let Some(clip) = asset("h264_320x240.mp4") else {
        return;
    };
    let info = probe(&paths, &clip).expect("probe");
    let (w, h) = info.video.display_size();
    let aspect = f64::from(w) / f64::from(h);
    let request = ThumbnailRequest {
        input: clip.clone(),
        stream_index: info.video.index,
        start_time: info.video.start_time,
        duration: info.duration,
        count: 10,
        height: 90,
        rotation_cw: info.video.rotation,
        display_aspect: aspect,
        color: info.video.color,
    };
    let thumbs = generate_thumbnails(&paths, &request, CancelToken::new()).expect("thumbnails");

    assert_eq!(thumbs.len(), 10);
    for (k, thumb) in thumbs.iter().enumerate() {
        assert_eq!((thumb.width, thumb.height), (120, 90));
        assert_eq!(thumb.rgba.len(), 120 * 90 * 4);
        assert!(
            (thumb.time - k as f64 * 0.2).abs() < 1e-9,
            "thumbnail {k} at {}",
            thumb.time
        );
    }
    // testsrc2 changes over time, so the first and last samples must differ.
    assert!(
        mean_abs_diff(&thumbs[0].rgba, &thumbs[9].rgba) > 5.0,
        "samples are not identical"
    );
}

#[test]
fn thumbnails_are_the_frames_at_their_sample_times() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    // 10 fps with a keyframe every 10 frames. A 0.7 s step samples frames 0, 7, 14 and 21, which fall between the
    // sampler's grid points, so this checks the cell anchor as well as the frame choice.
    let clip = generated(
        &paths,
        dir.path(),
        "plain10.mp4",
        "testsrc2=size=160x120:rate=10:duration=3",
        &[
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "10",
            "-keyint_min",
            "10",
            "-sc_threshold",
            "0",
        ],
    );
    let info = probe(&paths, &clip).expect("probe");
    let request = ThumbnailRequest {
        input: clip.clone(),
        stream_index: info.video.index,
        start_time: info.video.start_time,
        duration: 2.8,
        count: 4,
        height: 60,
        rotation_cw: 0,
        display_aspect: 4.0 / 3.0,
        color: info.video.color,
    };
    let thumbs = generate_thumbnails(&paths, &request, CancelToken::new()).expect("thumbnails");
    assert_eq!(thumbs.len(), 4);

    // Reference: every frame decoded with the same scale, then picked by index.
    let file = clip.to_str().expect("UTF-8 path");
    let every = ffmpeg_stdout(
        &paths,
        &[
            "-i",
            file,
            "-map",
            "0:0",
            "-vf",
            "scale=80:60:flags=lanczos",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ],
    );
    let frame_bytes = 80 * 60 * 4;
    assert_eq!(every.len() % frame_bytes, 0);
    for (k, thumb) in thumbs.iter().enumerate() {
        let index = k * 7; // 0.7 s at 10 fps
        assert!(
            (thumb.time - k as f64 * 0.7).abs() < 1e-9,
            "thumbnail {k} labelled {}",
            thumb.time
        );
        let reference = &every[index * frame_bytes..(index + 1) * frame_bytes];
        assert!(
            thumb.rgba == reference,
            "thumbnail {k} is not decoded frame {index}"
        );
    }
}

#[test]
fn portrait_thumbnails_are_upright_and_match_ffmpeg_autorotate() {
    let Some(paths) = common::paths() else { return };
    let Some(clip) = asset("portrait_rotated.mp4") else {
        return;
    };
    let info = probe(&paths, &clip).expect("probe");
    assert_ne!(info.video.rotation, 0, "fixture carries a display rotation");
    let (w, h) = info.video.display_size();
    assert!(h > w, "fixture is portrait once displayed");

    let thumb_height = 64;
    let aspect = f64::from(w) / f64::from(h);
    let request = ThumbnailRequest {
        input: clip.clone(),
        stream_index: info.video.index,
        start_time: info.video.start_time,
        duration: info.duration,
        count: 1,
        height: thumb_height,
        rotation_cw: info.video.rotation,
        display_aspect: aspect,
        color: info.video.color,
    };
    let thumb = generate_thumbnails(&paths, &request, CancelToken::new())
        .expect("thumbnail")
        .remove(0);
    assert!(thumb.width < thumb.height, "thumbnail is portrait");

    // Reference: FFmpeg's own autorotate, scaled to the same size with lanczos.
    let file = clip.to_str().expect("UTF-8 path");
    let scale = format!("scale={}:{}:flags=lanczos", thumb.width, thumb.height);
    let reference = ffmpeg_stdout(
        &paths,
        &[
            "-i",
            file,
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ],
    );
    assert_eq!(reference.len(), thumb.rgba.len());
    let diff = mean_abs_diff(&thumb.rgba, &reference);
    assert!(
        diff < 12.0,
        "thumbnail differs from autorotated reference by {diff:.2} (wrong rotation?)"
    );
}

#[test]
fn cpu_proxy_of_10_bit_hevc_is_8_bit_bt709_and_reports_progress() {
    let Some(paths) = common::paths() else { return };
    let Some(input) = asset("hevc10_640x360.mkv") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("proxy.mp4");
    let info = probe(&paths, &input).expect("probe source");
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);

    let proxy = make_proxy(
        &paths,
        &ProxyRequest {
            input: input.clone(),
            stream_index: info.video.index,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: info.video.color,
            rotation_cw: info.video.rotation,
        },
        move |fraction| sink.lock().expect("progress lock").push(fraction),
        CancelToken::new(),
    )
    .expect("CPU proxy");

    assert_eq!(
        (proxy.width, proxy.height),
        (640, 360),
        "360p source is not upscaled or cropped"
    );
    assert_eq!(proxy.bytes, fs::metadata(&output).expect("output").len());
    assert!(
        !dir.path().join("proxy.mp4.partial").exists(),
        "no partial file left behind"
    );

    let described = probe(&paths, &output).expect("probe proxy");
    assert_eq!(described.video.codec, "h264");
    assert_eq!(described.video.bit_depth, 8);
    assert_eq!(described.video.pix_fmt, "yuv420p");
    assert_eq!(described.video.color.matrix, YuvMatrix::Bt709);
    assert_eq!(described.video.color.transfer, TransferFunction::Sdr);
    assert_eq!(described.video.color.range, ColorRange::Limited);
    assert_eq!(described.video.rotation, 0);

    let fractions = reports.lock().expect("progress lock").clone();
    assert!(!fractions.is_empty(), "progress was reported");
    assert_eq!(fractions.last().copied(), Some(1.0));
    assert!(fractions.iter().all(|f| (0.0..=1.0).contains(f)));
}

/// True when the AMF H.264 encoder can encode a frame on this machine. Skips AMF tests elsewhere.
fn amf_available(paths: &FfmpegPaths) -> bool {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("probe.mp4");
    let ok = common::generate(
        paths,
        "testsrc2=size=320x240:rate=10:duration=1",
        &["-c:v", "h264_amf", "-pix_fmt", "yuv420p"],
        &out,
    )
    .is_ok();
    if !ok {
        eprintln!("skipping: h264_amf is not usable on this machine");
    }
    ok
}

#[test]
fn amf_proxy_of_10_bit_hevc_matches_the_cpu_geometry() {
    let Some(paths) = common::paths() else { return };
    let Some(input) = asset("hevc10_640x360.mkv") else {
        return;
    };
    if !amf_available(&paths) {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("amf.mp4");
    let info = probe(&paths, &input).expect("probe source");
    let proxy = make_proxy(
        &paths,
        &ProxyRequest {
            input,
            stream_index: info.video.index,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Amf { dxgi_index: 0 },
            color: info.video.color,
            rotation_cw: info.video.rotation,
        },
        |_| {},
        CancelToken::new(),
    )
    .expect("AMF proxy");
    assert_eq!((proxy.width, proxy.height), (640, 360));
    let described = probe(&paths, &output).expect("probe proxy");
    assert_eq!(described.video.codec, "h264");
    assert_eq!(described.video.bit_depth, 8);
    assert_eq!(described.video.color.matrix, YuvMatrix::Bt709);
}

/// Encodes `name` (an HDR source) with the CPU encoder and returns the proxy's Y plane mean and dimensions.
fn hdr_proxy_luma_mean(paths: &FfmpegPaths, name: &str) -> Option<f64> {
    let input = asset(name)?;
    let info = probe(paths, &input).expect("probe source");
    assert!(info.video.color.is_hdr(), "{name} is HDR");
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("hdr.mp4");
    let proxy = make_proxy(
        paths,
        &ProxyRequest {
            input,
            stream_index: info.video.index,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: info.video.color,
            rotation_cw: info.video.rotation,
        },
        |_| {},
        CancelToken::new(),
    )
    .expect("HDR proxy");
    let described = probe(paths, &output).expect("probe proxy");
    assert_eq!(described.video.bit_depth, 8, "tonemapped to 8-bit");
    assert_eq!(
        described.video.color.transfer,
        TransferFunction::Sdr,
        "tonemapped to SDR"
    );
    let luma = first_luma_plane(paths, &output, true, proxy.width, proxy.height);
    Some(mean(&luma))
}

#[test]
fn hdr_pq_proxy_is_tonemapped_not_washed_out() {
    let Some(paths) = common::paths() else { return };
    // The source's 10-bit PQ code values average about 121 and the Hable tonemap measures about 130 on the first frame.
    // A missing tonemap (PQ code values read as SDR) or a clipped one would fall far outside this band.
    if let Some(mean_luma) = hdr_proxy_luma_mean(&paths, "hdr_pq.mp4") {
        assert!(
            (60.0..=200.0).contains(&mean_luma),
            "PQ proxy mean luma {mean_luma:.1}"
        );
    }
}

#[test]
fn hdr_hlg_proxy_is_tonemapped_to_sdr() {
    let Some(paths) = common::paths() else { return };
    if let Some(mean_luma) = hdr_proxy_luma_mean(&paths, "hdr_hlg.mp4") {
        assert!(
            (40.0..=210.0).contains(&mean_luma),
            "HLG proxy mean luma {mean_luma:.1}"
        );
    }
}

#[test]
fn portrait_proxy_is_upright_without_a_display_matrix() {
    let Some(paths) = common::paths() else { return };
    let Some(input) = asset("portrait_rotated.mp4") else {
        return;
    };
    let info = probe(&paths, &input).expect("probe source");
    let (display_w, display_h) = info.video.display_size();
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("portrait.mp4");
    let proxy = make_proxy(
        &paths,
        &ProxyRequest {
            input: input.clone(),
            stream_index: info.video.index,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: info.video.color,
            rotation_cw: info.video.rotation,
        },
        |_| {},
        CancelToken::new(),
    )
    .expect("portrait proxy");

    assert_eq!((proxy.width, proxy.height), (display_w, display_h));
    assert!(proxy.height > proxy.width, "proxy is portrait");
    let described = probe(&paths, &output).expect("probe proxy");
    assert_eq!(
        described.video.rotation, 0,
        "proxy carries no rotation of its own"
    );
    assert_eq!(
        (described.video.width, described.video.height),
        (proxy.width, proxy.height)
    );

    // Reference: FFmpeg's autorotated picture with the proxy's own scale and BT.709 conversion. Untagged SD-height
    // sources are read as BT.601, so the conversion changes luma levels by a few units. That is expected, so the
    // reference applies it too and only orientation and encoder noise remain. A transposed-the-wrong-way proxy
    // differs by about 110 on this clip, far above the threshold.
    let file = input.to_str().expect("UTF-8 path");
    let convert = "scale=-2:'2*trunc(min(720,ih)/2)':flags=lanczos:out_color_matrix=bt709:out_range=tv,format=yuv420p";
    let reference = ffmpeg_stdout(
        &paths,
        &[
            "-i",
            file,
            "-frames:v",
            "1",
            "-vf",
            convert,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ],
    );
    let luma_len = (proxy.width * proxy.height) as usize;
    let proxy_luma = first_luma_plane(&paths, &output, true, proxy.width, proxy.height);
    let diff = mean_abs_diff(&proxy_luma, &reference[..luma_len]);
    assert!(
        diff < 12.0,
        "proxy luma differs from the upright reference by {diff:.2}"
    );
}

#[test]
fn multi_stream_proxy_uses_the_requested_stream() {
    let Some(paths) = common::paths() else { return };
    let Some(input) = asset("multi_video.mkv") else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("second.mp4");
    let proxy = make_proxy(
        &paths,
        &ProxyRequest {
            input,
            stream_index: 1,
            output,
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: loopcut_core::ColorInfo::SDR_BT709,
            rotation_cw: 0,
        },
        |_| {},
        CancelToken::new(),
    )
    .expect("proxy of stream 1");
    assert_eq!(
        (proxy.width, proxy.height),
        (160, 120),
        "stream 1 is the 160x120 video"
    );
}

#[test]
fn proxy_honours_a_frame_rate_cap() {
    let Some(paths) = common::paths() else { return };
    let Some(input) = asset("h264_320x240.mp4") else {
        return;
    };
    let info = probe(&paths, &input).expect("probe source");
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("capped.mp4");
    make_proxy(
        &paths,
        &ProxyRequest {
            input,
            stream_index: info.video.index,
            output: output.clone(),
            max_height: 720,
            fps_cap: Some(Rational::integer(5)),
            encoder: EncoderBackend::Cpu,
            color: info.video.color,
            rotation_cw: 0,
        },
        |_| {},
        CancelToken::new(),
    )
    .expect("capped proxy");
    let described = probe(&paths, &output).expect("probe proxy");
    assert_eq!(described.video.frame_rate, Rational::integer(5));
}

#[test]
fn failed_proxy_leaves_neither_output_nor_partial_file() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let garbage = dir.path().join("not-media.mp4");
    fs::write(&garbage, b"this is not a video file").expect("write");
    let output = dir.path().join("proxy.mp4");
    let result = make_proxy(
        &paths,
        &ProxyRequest {
            input: garbage,
            stream_index: 0,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: loopcut_core::ColorInfo::SDR_BT709,
            rotation_cw: 0,
        },
        |_| {},
        CancelToken::new(),
    );
    assert!(result.is_err());
    assert!(!output.exists());
    assert!(!dir.path().join("proxy.mp4.partial").exists());
}

#[test]
fn cancelled_proxy_is_killed_and_cleaned_up() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = generated(
        &paths,
        dir.path(),
        "long1080.mp4",
        "testsrc2=size=1920x1080:rate=60:duration=8",
        &[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let output = dir.path().join("cancelled.mp4");
    // Cancel as soon as the encoder reports progress: the encode is then certainly still running, so the outcome does
    // not depend on how fast the machine is.
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let result = make_proxy(
        &paths,
        &ProxyRequest {
            input: clip,
            stream_index: 0,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: loopcut_core::ColorInfo::SDR_BT709,
            rotation_cw: 0,
        },
        move |_fraction| trigger.cancel(),
        cancel,
    );
    assert!(matches!(result, Err(MediaError::Cancelled)), "{result:?}");
    assert!(!output.exists());
    assert!(!dir.path().join("cancelled.mp4.partial").exists());
}

#[test]
fn wide_gamut_sdr_proxy_is_converted_to_bt709() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let input = generated(
        &paths,
        dir.path(),
        "bt2020_sdr.mp4",
        "testsrc2=size=320x240:rate=10:duration=1",
        &[
            "-vf",
            "format=yuv420p,setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt709:range=tv",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let info = probe(&paths, &input).expect("probe source");
    assert!(
        info.video.color.wide_gamut,
        "source is tagged with BT.2020 primaries"
    );
    let output = dir.path().join("wide.mp4");
    make_proxy(
        &paths,
        &ProxyRequest {
            input,
            stream_index: info.video.index,
            output: output.clone(),
            max_height: 720,
            fps_cap: None,
            encoder: EncoderBackend::Cpu,
            color: info.video.color,
            rotation_cw: 0,
        },
        |_| {},
        CancelToken::new(),
    )
    .expect("wide-gamut proxy");
    let described = probe(&paths, &output).expect("probe proxy");
    assert!(
        !described.video.color.wide_gamut,
        "proxy primaries are BT.709"
    );
    assert_eq!(described.video.color.matrix, YuvMatrix::Bt709);
    assert_eq!(described.video.color.transfer, TransferFunction::Sdr);
}

/// Benchmark, not a correctness test: `cargo test -p loopcut-media --test assets -- --ignored --nocapture`.
/// Prints the realtime multiple of CPU and AMF proxy encodes of a 20 s 1080p60 clip.
#[test]
#[ignore = "benchmark: run explicitly with --ignored --nocapture"]
fn proxy_speed_cpu_versus_amf_on_1080p60() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = generated(
        &paths,
        dir.path(),
        "bench1080p60.mp4",
        "testsrc2=size=1920x1080:rate=60:duration=20",
        &[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let info = probe(&paths, &clip).expect("probe");
    let mut encoders = vec![("cpu", EncoderBackend::Cpu)];
    if amf_available(&paths) {
        encoders.push(("amf", EncoderBackend::Amf { dxgi_index: 0 }));
    }
    for (label, encoder) in encoders {
        let output = dir.path().join(format!("bench-{label}.mp4"));
        let started = Instant::now();
        let proxy = make_proxy(
            &paths,
            &ProxyRequest {
                input: clip.clone(),
                stream_index: info.video.index,
                output,
                max_height: 720,
                fps_cap: None,
                encoder,
                color: info.video.color,
                rotation_cw: 0,
            },
            |_| {},
            CancelToken::new(),
        )
        .expect("proxy");
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!(
            "proxy {label}: {}x{} from 1920x1080p60 x 20 s in {elapsed:.2} s = {:.2}x realtime, {} bytes",
            proxy.width,
            proxy.height,
            info.duration / elapsed,
            proxy.bytes
        );
    }
}

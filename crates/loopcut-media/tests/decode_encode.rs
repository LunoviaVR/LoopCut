//! Decode and encode command builders (argument snapshots and validation) and real FFmpeg round trips.
//!
//! Snapshot and validation tests need no FFmpeg binary. Round-trip tests skip with a message when no staged build
//! exists, and hardware tests skip with the failure reason when the chain does not start on the machine.

mod common;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use loopcut_core::{
    ColorInfo, DecodeBackend, DecodeChain, EncoderBackend, GpuScaler, PixelLayout, PixelRect,
    Rational, TimeRange, VideoCodec,
};
use loopcut_media::decode::{DecodeRequest, DecodeSession, DecodedFrame, build_decode_command};
use loopcut_media::encode::{Container, EncodeRequest, EncodeSession, build_encode_command};
use loopcut_media::runner::{
    CancelToken, FfmpegCommand, InputOptions, LogLevel, SpawnIo, run_capture,
};
use loopcut_media::{FfmpegPaths, MediaError, probe};

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

/// A 1920x1080 H.264 source, cropped, scaled and decoded for 0.5-1.5 s at 30 fps. Snapshot tests vary one field.
fn base_decode() -> DecodeRequest {
    DecodeRequest {
        input: PathBuf::from(r"C:\clips\input.mp4"),
        stream_index: 0,
        coded_width: 1920,
        coded_height: 1080,
        bit_depth: 8,
        has_alpha: false,
        codec: "h264".to_owned(),
        color: ColorInfo::SDR_BT709,
        start_time: 0.0,
        range: Some(TimeRange::new(0.5, 1.5)),
        crop: Some(PixelRect {
            x: 320,
            y: 180,
            w: 1280,
            h: 720,
        }),
        size: Some((640, 360)),
        fps: Some(Rational::integer(30)),
        chain: DecodeChain::CPU,
        layout: PixelLayout::Nv12,
    }
}

fn decode_args(req: &DecodeRequest) -> Vec<String> {
    lossy(
        build_decode_command(&fake_paths(), req)
            .expect("valid decode request")
            .args(),
    )
}

#[test]
fn decode_cpu_argv() {
    insta::assert_debug_snapshot!("decode_cpu", decode_args(&base_decode()));
}

#[test]
fn decode_cpu_stream_with_an_offset_start_seeks_to_absolute_time() {
    let mut req = base_decode();
    req.start_time = 10.0;
    insta::assert_debug_snapshot!("decode_cpu_offset_stream", decode_args(&req));
}

#[test]
fn decode_d3d11va_argv() {
    let mut req = base_decode();
    req.chain = DecodeChain {
        backend: DecodeBackend::D3d11va { dxgi_index: 0 },
        scaler: GpuScaler::None,
    };
    insta::assert_debug_snapshot!("decode_d3d11va", decode_args(&req));
}

#[test]
fn decode_d3d11va_with_scale_d3d11_argv() {
    let mut req = base_decode();
    req.chain = DecodeChain {
        backend: DecodeBackend::D3d11va { dxgi_index: 0 },
        scaler: GpuScaler::D3d11,
    };
    insta::assert_debug_snapshot!("decode_d3d11va_scaler_d3d11", decode_args(&req));
}

#[test]
fn decode_amf_with_amf_scaler_argv() {
    let mut req = base_decode();
    req.chain = DecodeChain {
        backend: DecodeBackend::Amf { dxgi_index: 0 },
        scaler: GpuScaler::Amf,
    };
    insta::assert_debug_snapshot!("decode_amf_scaler_amf", decode_args(&req));
}

#[test]
fn decode_cuda_with_cuda_scaler_argv() {
    let mut req = base_decode();
    req.chain = DecodeChain {
        backend: DecodeBackend::Cuda { cuda_index: 0 },
        scaler: GpuScaler::Cuda,
    };
    insta::assert_debug_snapshot!("decode_cuda_scaler_cuda", decode_args(&req));
}

#[test]
fn decode_qsv_with_qsv_scaler_argv() {
    let mut req = base_decode();
    req.chain = DecodeChain {
        backend: DecodeBackend::Qsv { dxgi_index: 0 },
        scaler: GpuScaler::Qsv,
    };
    insta::assert_debug_snapshot!("decode_qsv_scaler_qsv", decode_args(&req));
}

#[test]
fn decode_rgba_from_vp9_alpha_forces_libvpx() {
    let mut req = base_decode();
    req.codec = "vp9".to_owned();
    req.has_alpha = true;
    req.layout = PixelLayout::Rgba8;
    req.crop = None;
    req.size = None;
    insta::assert_debug_snapshot!("decode_rgba_vp9_alpha", decode_args(&req));
}

fn encode_request(
    codec: VideoCodec,
    backend: EncoderBackend,
    container: Container,
    width: u32,
    height: u32,
) -> EncodeRequest {
    EncodeRequest {
        output: PathBuf::from(r"C:\clips\out.partial"),
        width,
        height,
        fps: Rational::new(30000, 1001),
        codec,
        backend,
        quality: 80,
        container,
        keyint: None,
    }
}

fn encode_args(req: &EncodeRequest) -> Vec<String> {
    lossy(
        build_encode_command(&fake_paths(), req)
            .expect("valid encode request")
            .args(),
    )
}

#[test]
fn encode_cpu_argv() {
    let mp4 = Container::Mp4;
    insta::assert_debug_snapshot!(
        "encode_cpu_h264",
        encode_args(&encode_request(
            VideoCodec::H264,
            EncoderBackend::Cpu,
            mp4,
            640,
            360
        ))
    );
    insta::assert_debug_snapshot!(
        "encode_cpu_hevc",
        encode_args(&encode_request(
            VideoCodec::Hevc,
            EncoderBackend::Cpu,
            mp4,
            640,
            360
        ))
    );
    insta::assert_debug_snapshot!(
        "encode_cpu_av1",
        encode_args(&encode_request(
            VideoCodec::Av1,
            EncoderBackend::Cpu,
            mp4,
            640,
            360
        ))
    );
    insta::assert_debug_snapshot!(
        "encode_cpu_vp9",
        encode_args(&encode_request(
            VideoCodec::Vp9,
            EncoderBackend::Cpu,
            Container::Webm,
            640,
            360
        ))
    );
}

#[test]
fn encode_nvenc_argv() {
    let backend = EncoderBackend::Nvenc { cuda_index: 1 };
    for (name, codec) in [
        ("encode_nvenc_h264", VideoCodec::H264),
        ("encode_nvenc_hevc", VideoCodec::Hevc),
        ("encode_nvenc_av1", VideoCodec::Av1),
    ] {
        let req = encode_request(codec, backend, Container::Mp4, 640, 360);
        insta::assert_debug_snapshot!(name, encode_args(&req));
    }
}

#[test]
fn encode_amf_argv() {
    let backend = EncoderBackend::Amf { dxgi_index: 0 };
    for (name, codec) in [
        ("encode_amf_h264", VideoCodec::H264),
        ("encode_amf_hevc", VideoCodec::Hevc),
        ("encode_amf_av1", VideoCodec::Av1),
    ] {
        let req = encode_request(codec, backend, Container::Mp4, 640, 360);
        insta::assert_debug_snapshot!(name, encode_args(&req));
    }
}

#[test]
fn encode_qsv_argv() {
    let backend = EncoderBackend::Qsv { dxgi_index: 0 };
    for (name, codec) in [
        ("encode_qsv_h264", VideoCodec::H264),
        ("encode_qsv_hevc", VideoCodec::Hevc),
        ("encode_qsv_av1", VideoCodec::Av1),
    ] {
        let req = encode_request(codec, backend, Container::Mp4, 640, 360);
        insta::assert_debug_snapshot!(name, encode_args(&req));
    }
}

#[test]
fn encode_keyint_and_container_are_explicit() {
    let mut req = encode_request(
        VideoCodec::Av1,
        EncoderBackend::Cpu,
        Container::Webm,
        320,
        240,
    );
    req.keyint = Some(15);
    insta::assert_debug_snapshot!("encode_cpu_av1_webm_keyint15", encode_args(&req));
}

#[test]
fn invalid_decode_requests_have_no_command() {
    let paths = fake_paths();
    let mut req = base_decode();
    req.crop = Some(PixelRect {
        x: 1900,
        y: 0,
        w: 640,
        h: 360,
    });
    assert!(matches!(
        build_decode_command(&paths, &req),
        Err(MediaError::InvalidArgument(_))
    ));

    let mut req = base_decode();
    req.chain = DecodeChain {
        backend: DecodeBackend::Cpu,
        scaler: GpuScaler::Qsv,
    };
    assert!(matches!(
        build_decode_command(&paths, &req),
        Err(MediaError::InvalidArgument(_))
    ));

    let mut req = base_decode();
    req.input = PathBuf::from("http://example.com/clip.mp4");
    assert!(matches!(
        build_decode_command(&paths, &req),
        Err(MediaError::UrlNotAllowed(_))
    ));
}

#[test]
fn invalid_encode_requests_are_refused() {
    let paths = fake_paths();
    let mut req = encode_request(
        VideoCodec::Hevc,
        EncoderBackend::Cpu,
        Container::Webm,
        640,
        360,
    );
    assert!(matches!(
        build_encode_command(&paths, &req),
        Err(MediaError::Unsupported(_))
    ));

    req = encode_request(
        VideoCodec::Vp9,
        EncoderBackend::Nvenc { cuda_index: 0 },
        Container::Webm,
        640,
        360,
    );
    assert!(matches!(
        build_encode_command(&paths, &req),
        Err(MediaError::Unsupported(_))
    ));

    req = encode_request(
        VideoCodec::H264,
        EncoderBackend::Cpu,
        Container::Mp4,
        641,
        360,
    );
    assert!(matches!(
        build_encode_command(&paths, &req),
        Err(MediaError::InvalidArgument(_))
    ));

    req = encode_request(
        VideoCodec::H264,
        EncoderBackend::Cpu,
        Container::Mp4,
        640,
        360,
    );
    req.output = PathBuf::from("http://example.com/out.partial.mp4");
    assert!(matches!(
        build_encode_command(&paths, &req),
        Err(MediaError::UrlNotAllowed(_))
    ));
}

// ---------------------------------------------------------------------------------------------------------------
// Round trips through the pinned FFmpeg.
// ---------------------------------------------------------------------------------------------------------------

/// The 320x240, 30 fps, 2 s H.264 corpus clip with audio, or a freshly generated 10 fps clip when the corpus is absent.
fn source_clip(paths: &FfmpegPaths, dir: &Path) -> PathBuf {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("media")
        .join("generated")
        .join("h264_320x240.mp4");
    if corpus.is_file() {
        corpus
    } else {
        common::make_h264_clip(paths, dir)
    }
}

/// A decode request built from what the probe reports, so the test exercises the same numbers the app will pass.
fn request_from_probe(paths: &FfmpegPaths, file: &Path) -> DecodeRequest {
    let info = probe(paths, file).expect("probe test media");
    let video = &info.video;
    DecodeRequest {
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
        layout: if video.bit_depth > 8 {
            PixelLayout::P010
        } else {
            PixelLayout::Nv12
        },
    }
}

/// Decodes the first `count` frames of `req`.
fn first_frames(
    paths: &FfmpegPaths,
    req: &DecodeRequest,
    count: usize,
) -> Result<Vec<DecodedFrame>, MediaError> {
    DecodeSession::start(paths, req, CancelToken::new())?
        .take(count)
        .collect()
}

/// Mean absolute difference of two equal-length byte planes.
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "planes differ in length");
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / a.len() as f64
}

/// Luma plane of an NV12 frame (the first `w*h` bytes).
fn luma(frame: &DecodedFrame) -> &[u8] {
    &frame.data[..frame.width as usize * frame.height as usize]
}

#[test]
fn cpu_decode_returns_whole_frames_with_source_times() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let req = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let frames = first_frames(&paths, &req, 10).expect("decode 10 frames");
    assert_eq!(frames.len(), 10);
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.index, i as u64);
        assert!(
            (frame.time - i as f64 / 30.0).abs() < 1e-9,
            "frame {i} at {}",
            frame.time
        );
        assert_eq!((frame.width, frame.height), (320, 240));
        assert_eq!(frame.data.len(), 320 * 240 * 3 / 2);
    }
    assert!(
        frames[0].data.iter().any(|byte| *byte != frames[0].data[0]),
        "the test pattern has content"
    );
}

#[test]
fn hardware_decode_matches_cpu_decode() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let base = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let cpu = first_frames(&paths, &base, 10).expect("CPU decode");

    for (name, backend) in [
        ("D3d11va{0}", DecodeBackend::D3d11va { dxgi_index: 0 }),
        ("Amf{0}", DecodeBackend::Amf { dxgi_index: 0 }),
    ] {
        let req = DecodeRequest {
            chain: DecodeChain {
                backend,
                scaler: GpuScaler::None,
            },
            ..base.clone()
        };
        let hardware = match first_frames(&paths, &req, 10) {
            Ok(frames) => frames,
            Err(err) => {
                eprintln!("skipping {name}: the chain does not run here: {err}");
                continue;
            }
        };
        assert_eq!(hardware.len(), 10, "{name}");
        let mut worst = 0.0_f64;
        for (i, (hw, sw)) in hardware.iter().zip(&cpu).enumerate() {
            assert_eq!(hw.data.len(), sw.data.len(), "{name} frame {i}");
            let diff = mean_abs_diff(luma(hw), luma(sw));
            worst = worst.max(diff);
            assert!(diff < 3.0, "{name} frame {i}: luma differs by {diff:.2}");
        }
        eprintln!("{name}: worst luma mean abs diff against CPU over 10 frames: {worst:.3}");
    }
}

#[test]
fn range_decodes_at_a_constant_rate_from_the_requested_start() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let whole = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let source = first_frames(&paths, &whole, 30).expect("source frames");

    let ranged = DecodeRequest {
        range: Some(TimeRange::new(0.5, 1.0)),
        fps: Some(Rational::integer(10)),
        ..whole.clone()
    };
    let frames = first_frames(&paths, &ranged, 100).expect("ranged decode");
    assert_eq!(frames.len(), 5, "0.5 s at 10 fps");
    for (i, frame) in frames.iter().enumerate() {
        let expected = 0.5 + 0.1 * i as f64;
        assert!(
            (frame.time - expected).abs() < 1e-9,
            "frame {i} at {}",
            frame.time
        );
        // Source frame 15 is at 0.5 s and every third frame after it is 0.1 s later at 30 fps.
        let matching = &source[15 + 3 * i];
        let diff = mean_abs_diff(luma(frame), luma(matching));
        assert!(
            diff < 1.0,
            "frame {i} is not the source frame at {expected}: diff {diff:.2}"
        );
    }
}

#[test]
fn a_seek_between_frames_labels_each_output_frame_at_its_grid_instant() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    // 30 fps for two seconds starting at PTS 5 s: source frame n is at 5 + n / 30 s.
    let clip = dir.path().join("offset.mp4");
    common::generate(
        &paths,
        "testsrc2=size=160x120:rate=30:duration=2",
        &[
            "-vf",
            "setpts=PTS+5/TB",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ],
        &clip,
    )
    .expect("generate offset clip");
    let whole = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let source = first_frames(&paths, &whole, 60).expect("source frames");

    // Seek to 0.55 s, which lies between source frames 16 and 17, and decode at 10 fps.
    let ranged = DecodeRequest {
        range: Some(TimeRange::new(0.55, 1.55)),
        fps: Some(Rational::integer(10)),
        ..whole.clone()
    };
    let frames = first_frames(&paths, &ranged, 4).expect("ranged decode");
    // Output frames sit at 0.55, 0.65 and 0.75 s. Each is the last source frame at or before its instant, except the
    // first, which has none before the seek point and so takes the first decoded frame (17).
    for (frame, expected) in frames.iter().zip([17usize, 19, 22]) {
        let diff = mean_abs_diff(luma(frame), luma(&source[expected]));
        assert!(
            diff < 1.0,
            "frame at {} s is not source frame {expected}: diff {diff:.2}",
            frame.time
        );
    }
    assert!(
        (frames[1].time - 0.65).abs() < 1e-9,
        "labels follow the grid"
    );
}

#[test]
fn source_frame_rate_is_resolved_when_unset() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let req = DecodeRequest {
        fps: None,
        ..request_from_probe(&paths, &clip)
    };
    let session = DecodeSession::start(&paths, &req, CancelToken::new()).expect("start");
    assert_eq!(
        session.fps().as_f64(),
        request_from_probe(&paths, &clip).fps.unwrap().as_f64()
    );
}

#[test]
fn ten_bit_source_decodes_to_p010_with_ten_significant_bits() {
    let Some(paths) = common::paths() else { return };
    let clip =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/generated/hevc10_640x360.mkv");
    if !clip.is_file() {
        eprintln!("skipping: {} is not generated", clip.display());
        return;
    }
    let req = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    assert_eq!(req.bit_depth, 10);
    assert_eq!(req.layout, PixelLayout::P010);
    let frames = first_frames(&paths, &req, 2).expect("decode P010");
    assert_eq!(frames.len(), 2);
    for frame in &frames {
        assert_eq!(frame.data.len(), 640 * 360 * 3);
        // P010 keeps the 10 significant bits in the top bits of each little-endian 16-bit sample.
        let samples: Vec<u16> = frame
            .data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        assert!(
            samples.iter().all(|sample| sample & 0x3F == 0),
            "low six bits must be zero"
        );
        let (min, max) = samples.iter().fold((u16::MAX, 0u16), |(lo, hi), sample| {
            (lo.min(*sample), hi.max(*sample))
        });
        assert!(max - min > 256, "the pattern spans the 10-bit range");
    }
}

/// Returns the first frame's alpha values at the left and right edges, which the alpha clips ramp from 0 to 255.
fn alpha_edges(frame: &DecodedFrame) -> (u8, u8) {
    let width = frame.width as usize;
    let row = 0;
    let left = frame.data[(row * width) * 4 + 3];
    let right = frame.data[(row * width + width - 1) * 4 + 3];
    (left, right)
}

#[test]
fn prores_alpha_decodes_to_rgba_with_alpha() {
    let Some(paths) = common::paths() else { return };
    let clip =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/generated/alpha_prores4444.mov");
    if !clip.is_file() {
        eprintln!("skipping: {} is not generated", clip.display());
        return;
    }
    let req = DecodeRequest {
        layout: PixelLayout::Rgba8,
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    assert!(req.has_alpha);
    let frames = first_frames(&paths, &req, 1).expect("decode RGBA");
    let frame = &frames[0];
    assert_eq!(frame.data.len(), 320 * 240 * 4);
    let (left, right) = alpha_edges(frame);
    assert!(left < 32, "left edge alpha {left}");
    assert!(right > 223, "right edge alpha {right}");
}

#[test]
fn vp9_alpha_decodes_with_libvpx_and_keeps_alpha() {
    let Some(paths) = common::paths() else { return };
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media/generated/alpha_vp9.webm");
    if !clip.is_file() {
        eprintln!("skipping: {} is not generated", clip.display());
        return;
    }
    let req = DecodeRequest {
        layout: PixelLayout::Rgba8,
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    assert!(req.has_alpha, "the alpha_mode tag marks VP9 alpha");
    let frames = first_frames(&paths, &req, 1).expect("decode VP9 alpha");
    let (left, right) = alpha_edges(&frames[0]);
    // The native VP9 decoder would return opaque pixels everywhere. A ramp from near 0 to near 255 proves libvpx.
    assert!(left < 32 && right > 223, "alpha ramp {left}..{right}");
}

#[test]
fn crop_and_scale_on_cpu_match_amf_scale_then_crop() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let base = DecodeRequest {
        crop: Some(PixelRect {
            x: 64,
            y: 48,
            w: 160,
            h: 120,
        }),
        size: Some((80, 60)),
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let cpu = first_frames(&paths, &base, 3).expect("CPU crop and scale");
    let amf = DecodeRequest {
        chain: DecodeChain {
            backend: DecodeBackend::Amf { dxgi_index: 0 },
            scaler: GpuScaler::Amf,
        },
        ..base.clone()
    };
    let hardware = match first_frames(&paths, &amf, 3) {
        Ok(frames) => frames,
        Err(err) => {
            eprintln!("skipping the AMF scale-then-crop comparison: {err}");
            return;
        }
    };
    for (i, (gpu, sw)) in hardware.iter().zip(&cpu).enumerate() {
        assert_eq!((gpu.width, gpu.height), (80, 60));
        let diff = mean_abs_diff(luma(gpu), luma(sw));
        eprintln!(
            "AMF scale-then-crop, frame {i}: mean luma diff against CPU crop-then-scale {diff:.3}"
        );
        assert!(
            diff < 2.0,
            "frame {i}: scale-then-crop differs by {diff:.2}"
        );
    }
}

#[test]
fn cancelling_a_decode_stops_it_at_the_next_frame() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let req = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let cancel = CancelToken::new();
    let mut session = DecodeSession::start(&paths, &req, cancel.clone()).expect("start");
    assert!(session.next().expect("a frame").is_ok());
    cancel.cancel();
    assert!(matches!(session.next(), Some(Err(MediaError::Cancelled))));
    assert!(
        session.next().is_none(),
        "the session ends after cancellation"
    );
}

#[test]
fn a_decode_that_fails_reports_the_stderr_tail() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    // Not a container at all: FFmpeg starts, then fails while opening the input.
    let not_video = dir.path().join("not_a_video.mp4");
    fs::write(&not_video, b"this is not a container").expect("write");
    let req = DecodeRequest {
        input: not_video,
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let items: Vec<_> = DecodeSession::start(&paths, &req, CancelToken::new())
        .expect("the process starts")
        .collect();
    assert_eq!(items.len(), 1, "no frames, then the failure: {items:?}");
    match &items[0] {
        Err(MediaError::Failed { code, stderr_tail }) => {
            assert_ne!(*code, Some(0));
            assert!(
                !stderr_tail.is_empty(),
                "the stderr tail explains the failure"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Encode round trips.
// ---------------------------------------------------------------------------------------------------------------

/// One RGBA frame of a moving gradient, so that consecutive frames differ.
fn gradient_frame(width: u32, height: u32, frame: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let r = x * 255 / width;
            let g = y * 255 / height;
            let b = (frame * 8 + x + y) % 256;
            out.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    out
}

/// Encodes `frames` gradient frames with `req` and checks the totals.
fn encode_clip(paths: &FfmpegPaths, req: &EncodeRequest, frames: u32) -> Result<(), MediaError> {
    let mut session = EncodeSession::start(paths, req, None)?;
    for frame in 0..frames {
        session.write_frame(&gradient_frame(req.width, req.height, frame))?;
    }
    let stats = session.finish()?;
    assert_eq!(stats.frames, u64::from(frames));
    assert!(stats.bytes > 0);
    Ok(())
}

/// The first video stream of `file` as ffprobe reports it, with the frame count read by decoding.
fn ffprobe_video(paths: &FfmpegPaths, file: &Path) -> serde_json::Value {
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(file, &InputOptions::default())
        .expect("input")
        .output_args(["-count_frames", "-print_format", "json", "-show_streams"])
        .expect("ffprobe args");
    let stdout = run_capture(command, std::time::Duration::from_secs(60)).expect("ffprobe");
    let json: serde_json::Value = serde_json::from_slice(&stdout).expect("ffprobe JSON");
    json["streams"][0].clone()
}

fn assert_encoded(paths: &FfmpegPaths, file: &Path, codec: &str, width: u64, height: u64) {
    let stream = ffprobe_video(paths, file);
    assert_eq!(stream["codec_name"], codec, "codec");
    assert_eq!(stream["pix_fmt"], "yuv420p", "pixel format");
    assert_eq!(stream["width"], width, "width");
    assert_eq!(stream["height"], height, "height");
    assert_eq!(stream["nb_read_frames"], "30", "frame count");
    assert_eq!(stream["color_transfer"], "bt709", "transfer tag");
    assert_eq!(stream["color_primaries"], "bt709", "primaries tag");
    assert_eq!(stream["color_space"], "bt709", "matrix tag");
    assert_eq!(stream["color_range"], "tv", "range tag");
}

fn encode_request_in(
    dir: &Path,
    name: &str,
    codec: VideoCodec,
    backend: EncoderBackend,
    container: Container,
    width: u32,
    height: u32,
) -> EncodeRequest {
    EncodeRequest {
        output: dir.join(format!("{name}.partial")),
        width,
        height,
        fps: Rational::integer(30),
        codec,
        backend,
        quality: 70,
        container,
        keyint: None,
    }
}

#[test]
fn cpu_encoders_write_probeable_files_with_bt709_tags() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");

    let x264 = encode_request_in(
        dir.path(),
        "x264.mp4",
        VideoCodec::H264,
        EncoderBackend::Cpu,
        Container::Mp4,
        640,
        360,
    );
    encode_clip(&paths, &x264, 30).expect("libx264");
    assert_encoded(&paths, &x264.output, "h264", 640, 360);

    let vp9 = encode_request_in(
        dir.path(),
        "vp9.webm",
        VideoCodec::Vp9,
        EncoderBackend::Cpu,
        Container::Webm,
        640,
        360,
    );
    encode_clip(&paths, &vp9, 30).expect("libvpx-vp9");
    assert_encoded(&paths, &vp9.output, "vp9", 640, 360);

    let av1 = encode_request_in(
        dir.path(),
        "av1.mp4",
        VideoCodec::Av1,
        EncoderBackend::Cpu,
        Container::Mp4,
        640,
        360,
    );
    encode_clip(&paths, &av1, 30).expect("libsvtav1");
    assert_encoded(&paths, &av1.output, "av1", 640, 360);
}

#[test]
fn cancelled_encode_removes_its_partial_output() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let req = encode_request_in(
        dir.path(),
        "cancelled.mp4",
        VideoCodec::H264,
        EncoderBackend::Cpu,
        Container::Mp4,
        320,
        240,
    );
    let mut session = EncodeSession::start(&paths, &req, None).expect("start");
    session
        .write_frame(&gradient_frame(320, 240, 0))
        .expect("first frame");
    assert_eq!(session.frames_written(), 1);
    session.cancel();
    assert!(!req.output.exists(), "the partial file is deleted");
}

#[test]
fn write_frame_rejects_a_frame_of_the_wrong_size() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let req = encode_request_in(
        dir.path(),
        "wrong_size.mp4",
        VideoCodec::H264,
        EncoderBackend::Cpu,
        Container::Mp4,
        320,
        240,
    );
    let mut session = EncodeSession::start(&paths, &req, None).expect("start");
    let err = session
        .write_frame(&[0u8; 16])
        .expect_err("16 bytes is not a frame");
    assert!(matches!(err, MediaError::InvalidArgument(_)), "{err:?}");
    session.cancel();
}

/// Runs one hardware encode. Returns `false` (after printing why) when the encoder refuses the size on this machine.
fn hardware_encode(paths: &FfmpegPaths, name: &str, req: &EncodeRequest) -> bool {
    match encode_clip(paths, req, 30) {
        Ok(()) => {
            assert_encoded(
                paths,
                &req.output,
                &codec_name(req.codec),
                u64::from(req.width),
                u64::from(req.height),
            );
            true
        }
        Err(err) => {
            eprintln!("skipping {name}: {err}");
            false
        }
    }
}

fn codec_name(codec: VideoCodec) -> String {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => "av1",
        VideoCodec::Vp9 => "vp9",
    }
    .to_owned()
}

#[test]
fn amf_encoders_write_probeable_files_at_640x360() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let amf = EncoderBackend::Amf { dxgi_index: 0 };
    for (name, codec) in [
        ("h264_amf", VideoCodec::H264),
        ("hevc_amf", VideoCodec::Hevc),
        ("av1_amf", VideoCodec::Av1),
    ] {
        let req = encode_request_in(
            dir.path(),
            &format!("{name}.mp4"),
            codec,
            amf,
            Container::Mp4,
            640,
            360,
        );
        hardware_encode(&paths, name, &req);
    }
}

#[test]
fn hevc_amf_refuses_frames_below_its_minimum_size_with_the_stderr_tail() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let req = encode_request_in(
        dir.path(),
        "hevc_amf_small.mp4",
        VideoCodec::Hevc,
        EncoderBackend::Amf { dxgi_index: 0 },
        Container::Mp4,
        320,
        240,
    );
    match encode_clip(&paths, &req, 30) {
        Err(MediaError::Failed { stderr_tail, .. }) => {
            assert!(!stderr_tail.is_empty(), "the encoder explains the failure");
            assert!(
                !req.output.exists(),
                "a failed encode leaves no partial file"
            );
        }
        other => panic!("hevc_amf at 320x240 must fail with Failed, got {other:?}"),
    }
}

#[test]
fn an_mkv_with_mjpeg_cover_art_first_probes_the_clip_stream() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("cover_first.mkv");
    let command = FfmpegCommand::new_ffmpeg(&paths)
        .global_arg("-y")
        .expect("global")
        .input("testsrc2=size=320x240:rate=10:duration=1", &common::lavfi())
        .expect("clip")
        .input(
            "color=c=0x335577:size=320x240:duration=0.1",
            &common::lavfi(),
        )
        .expect("cover")
        .output_args([
            "-map",
            "1:v",
            "-map",
            "0:v",
            "-c:v:0",
            "mjpeg",
            "-frames:v:0",
            "1",
            "-c:v:1",
            "libx264",
            "-pix_fmt:v:1",
            "yuv420p",
            "-disposition:v:0",
            "0",
            "-an",
        ])
        .expect("outputs")
        .output(&out)
        .expect("output");
    common::run(command).expect("generate MKV with cover art");

    let info = probe(&paths, &out).expect("probe");
    assert_eq!(
        info.video.codec, "h264",
        "the clip, not the cover, is edited"
    );
    assert_eq!(info.video.index, 1);
    assert!(info.alternate_video_streams.is_empty());
}

#[test]
fn encode_reports_progress_to_the_callback() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let req = encode_request_in(
        dir.path(),
        "progress.mp4",
        VideoCodec::H264,
        EncoderBackend::Cpu,
        Container::Mp4,
        320,
        240,
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&seen);
    let callback: Box<dyn FnMut(loopcut_media::Progress) + Send> =
        Box::new(move |progress| sink.lock().expect("progress lock").push(progress));
    let mut session = EncodeSession::start(&paths, &req, Some(callback)).expect("start");
    for frame in 0..30 {
        session
            .write_frame(&gradient_frame(320, 240, frame))
            .expect("frame");
    }
    session.finish().expect("finish");
    let progress = seen.lock().expect("progress lock");
    assert!(
        progress.iter().any(|block| block.done),
        "the end block is reported"
    );
    assert!(
        progress.iter().filter_map(|block| block.frame).max() == Some(30),
        "the last frame count is 30: {progress:?}"
    );
}

/// Decodes the whole request with info logging and returns the stderr tail. FFmpeg falls back to software decoding
/// without an error when a hardware chain fails, so only the log shows which decoder actually ran.
fn decode_log(paths: &FfmpegPaths, req: &DecodeRequest) -> Result<String, MediaError> {
    let mut process = build_decode_command(paths, req)?
        .loglevel(LogLevel::Info)
        .spawn(SpawnIo {
            stdin: false,
            stdout: true,
        })?;
    let mut stdout = process
        .take_stdout()
        .ok_or_else(|| MediaError::Io(io::Error::other("decoder stdout pipe is missing")))?;
    io::copy(&mut stdout, &mut io::sink())?;
    drop(stdout);
    Ok(process.wait()?.stderr_tail)
}

#[test]
fn hardware_decoders_are_really_engaged() {
    let Some(paths) = common::paths() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = source_clip(&paths, dir.path());
    let base = DecodeRequest {
        fps: Some(Rational::integer(30)),
        ..request_from_probe(&paths, &clip)
    };
    let cases = [
        ("CPU", DecodeBackend::Cpu, "h264 (native)"),
        (
            "D3d11va{0}",
            DecodeBackend::D3d11va { dxgi_index: 0 },
            "Using device",
        ),
        ("Amf{0}", DecodeBackend::Amf { dxgi_index: 0 }, "(h264_amf)"),
    ];
    for (name, backend, marker) in cases {
        let req = DecodeRequest {
            chain: DecodeChain {
                backend,
                scaler: GpuScaler::None,
            },
            ..base.clone()
        };
        match decode_log(&paths, &req) {
            Ok(log) => assert!(
                log.contains(marker),
                "{name}: the log does not show {marker:?}, so FFmpeg fell back to software:\n{log}"
            ),
            Err(err) => eprintln!("skipping {name}: {err}"),
        }
    }
}

//! Runs the real capability probe against the staged FFmpeg build and the local adapters.
//!
//! These tests are `#[ignore]`d, so a plain `cargo test` stays fast and passes on machines without an AMD GPU. Run them
//! with `cargo test -p loopcut-hw -- --ignored --nocapture`. Set `LOOPCUT_REQUIRE_HW=1` to turn a missing FFmpeg build or
//! AMD adapter into a failure. Use it on the hardware machine so the tests cannot pass by skipping.
//!
//! The AMD assertions follow the RX 9060 XT measurements in docs/architecture.md: h264_amf and hevc_amf encode, and
//! hevc_amf refuses 320x240 and smaller (the probe finds 426x240 as its minimum on this machine).

use std::time::Duration;

use loopcut_core::VideoCodec;
use loopcut_core::hw::GpuVendor;
use loopcut_hw::caps::CapStatus;
use loopcut_hw::devices::GpuDevice;
use loopcut_hw::{ProbeEvent, ProbeOptions, enumerate_gpus, probe_capabilities, select};
use loopcut_media::FfmpegPaths;
use loopcut_media::runner::CancelToken;

/// Environment variable that turns a missing FFmpeg build or AMD adapter into a test failure.
const REQUIRE_ENV: &str = "LOOPCUT_REQUIRE_HW";

/// The staged FFmpeg, every adapter, and the first AMD adapter.
type Hardware = (FfmpegPaths, Vec<GpuDevice>, GpuDevice);

/// Finds the hardware this test needs. Without it, the test reports why on stderr and returns `None`, or panics when
/// `LOOPCUT_REQUIRE_HW=1`.
fn hardware_under_test() -> Option<Hardware> {
    let require = std::env::var(REQUIRE_ENV).is_ok_and(|value| value.trim() == "1");
    let Some(paths) = FfmpegPaths::discover_dev() else {
        return missing("no staged FFmpeg build", require);
    };
    let gpus = enumerate_gpus();
    let Some(amd) = gpus
        .iter()
        .find(|gpu| gpu.vendor == GpuVendor::Amd && !gpu.is_software)
        .cloned()
    else {
        return missing(
            &format!(
                "no AMD adapter on this machine ({} adapters found)",
                gpus.len()
            ),
            require,
        );
    };
    Some((paths, gpus, amd))
}

/// Reports a missing requirement: a panic when hardware is required, otherwise a skip.
fn missing(reason: &str, require: bool) -> Option<Hardware> {
    assert!(!require, "{REQUIRE_ENV}=1 but {reason}");
    eprintln!("skipping: {reason}");
    None
}

#[test]
#[ignore = "runs the real probe; needs the staged FFmpeg and an AMD adapter (run with --ignored)"]
fn this_machine_probe_finds_the_amd_encoders_and_selects_them() {
    let Some((paths, gpus, amd)) = hardware_under_test() else {
        return;
    };

    let opts = ProbeOptions {
        timeout: Duration::from_secs(20),
        benchmark: false,
    };
    let cancel = CancelToken::new();
    let mut steps = 0_u32;
    let mut failed_steps = Vec::new();
    let caps = probe_capabilities(
        &paths,
        &gpus,
        &opts,
        &mut |event| match event {
            ProbeEvent::Step { .. } => steps += 1,
            ProbeEvent::Result { label, ok } => {
                if !ok {
                    failed_steps.push(label);
                }
            }
        },
        &cancel,
    )
    .expect("probe completes");

    let report = select::describe(&caps);
    println!("{report}");
    println!("steps run: {steps}; steps not ok: {}", failed_steps.len());

    assert!(steps > 0, "the probe reported no steps");
    let amd_caps = caps
        .devices
        .iter()
        .find(|entry| entry.device.key == amd.key)
        .expect("the AMD adapter is in the probe result");

    // AMF runs only on the default AMD adapter. The probe records the other AMD adapters as unsupported.
    let default =
        loopcut_hw::devices::amf_default_adapter(&gpus).expect("an AMD adapter is the AMF default");
    for entry in caps.devices.iter().filter(|entry| {
        entry.device.vendor == GpuVendor::Amd && entry.device.dxgi_index != default.dxgi_index
    }) {
        assert!(
            entry
                .decode
                .iter()
                .filter(|cap| cap.backend == "amf")
                .all(|cap| cap.status == CapStatus::Unsupported),
            "AMF decode must be unsupported on {}",
            entry.device.key.name
        );
    }

    let h264 = amd_caps
        .encode
        .iter()
        .find(|cap| cap.codec == VideoCodec::H264 && cap.backend == "amf")
        .expect("h264 amf was probed");
    assert_eq!(
        h264.status,
        CapStatus::Ok,
        "h264_amf must encode: {:?}",
        h264.error
    );

    let hevc = amd_caps
        .encode
        .iter()
        .find(|cap| cap.codec == VideoCodec::Hevc && cap.backend == "amf")
        .expect("hevc amf was probed");
    assert_eq!(
        hevc.status,
        CapStatus::Ok,
        "hevc_amf must encode: {:?}",
        hevc.error
    );
    let (min_w, min_h) = (
        hevc.min_width.expect("hevc minimum width"),
        hevc.min_height.expect("hevc minimum height"),
    );
    assert!(
        u64::from(min_w) * u64::from(min_h) > 320 * 240,
        "hevc_amf must not accept 320x240 or smaller, found {min_w}x{min_h}"
    );
    println!("hevc_amf minimum from the probe: {min_w}x{min_h}");

    let h264_decode = amd_caps
        .decode
        .iter()
        .find(|cap| cap.codec == VideoCodec::H264 && cap.backend == "amf")
        .expect("h264 amf decode was probed");
    assert_eq!(h264_decode.status, CapStatus::Ok, "{:?}", h264_decode.error);
    assert!(
        amd_caps
            .scale
            .iter()
            .any(|cap| cap.codec == VideoCodec::H264
                && cap.backend == "amf"
                && cap.status == CapStatus::Ok),
        "the H.264 scaler was probed after the H.264 decode"
    );

    // Selection on the live probe data: Auto picks the AMD adapter, its native decoder and scaler, AMF for export
    // above the measured minimum, and CPU below it.
    let settings = loopcut_core::HwSettings::default();
    let chosen = select::choose_gpu(&caps, &settings).expect("Auto should pick the AMD adapter");
    assert_eq!(chosen.device.key, amd.key);

    let chain = select::choose_decode_chain(&caps, &settings, "hevc", 10, false);
    assert_eq!(
        chain,
        loopcut_core::DecodeChain {
            backend: loopcut_core::DecodeBackend::Amf {
                dxgi_index: amd.dxgi_index
            },
            scaler: loopcut_core::GpuScaler::Amf,
        }
    );
    assert_eq!(
        select::choose_encoder(&caps, &settings, VideoCodec::H264, 640, 360, false),
        loopcut_core::EncoderBackend::Amf {
            dxgi_index: amd.dxgi_index
        }
    );
    assert_eq!(
        select::choose_encoder(&caps, &settings, VideoCodec::Hevc, 320, 240, true),
        loopcut_core::EncoderBackend::Cpu,
        "320x240 is below the measured hevc_amf minimum"
    );
    assert_eq!(
        select::choose_effects(&settings, &gpus),
        select::EffectsChoice::Gpu(amd.key.clone())
    );
}

#[test]
#[ignore = "runs the real benchmark (300 frames per encoder); needs the staged FFmpeg and an AMD adapter (run with --ignored)"]
fn this_machine_benchmark_reports_hardware_and_cpu_reference_speeds() {
    let Some((paths, gpus, amd)) = hardware_under_test() else {
        return;
    };
    let opts = ProbeOptions {
        timeout: Duration::from_secs(30),
        benchmark: true,
    };
    let cancel = CancelToken::new();
    let caps = probe_capabilities(&paths, &gpus, &opts, &mut |_| {}, &cancel).expect("probe");
    let amd_caps = caps
        .devices
        .iter()
        .find(|entry| entry.device.key == amd.key)
        .expect("AMD entry");

    let h264 = amd_caps
        .encode
        .iter()
        .find(|cap| cap.codec == VideoCodec::H264 && cap.backend == "amf")
        .expect("h264 amf");
    let hw_speed = h264.speed_x.expect("benchmark speed for h264_amf");
    assert!(hw_speed > 0.0 && hw_speed.is_finite(), "speed {hw_speed}");

    // Every CPU reference the selector compares against must have been measured.
    for codec in [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1] {
        let cpu = caps
            .cpu_encode
            .iter()
            .find(|cap| cap.codec == codec)
            .unwrap_or_else(|| panic!("CPU entry for {codec:?}"));
        let speed = cpu
            .speed_x
            .unwrap_or_else(|| panic!("benchmark speed for {} ({codec:?})", cpu.backend));
        assert!(
            speed > 0.0 && speed.is_finite(),
            "{} speed {speed}",
            cpu.backend
        );
    }
    println!("{}", select::describe(&caps));
}

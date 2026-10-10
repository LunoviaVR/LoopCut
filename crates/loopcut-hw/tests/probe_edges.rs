//! Probe behavior that does not need a working FFmpeg: cancellation, and what a broken FFmpeg install looks like.

use std::path::PathBuf;
use std::time::Duration;

use loopcut_core::hw::{GpuKey, GpuVendor};
use loopcut_hw::caps::CapStatus;
use loopcut_hw::devices::GpuDevice;
use loopcut_hw::{HwError, ProbeEvent, ProbeOptions, probe_capabilities};
use loopcut_media::FfmpegPaths;
use loopcut_media::runner::CancelToken;

fn missing_paths() -> FfmpegPaths {
    FfmpegPaths {
        ffmpeg: PathBuf::from("loopcut-test-missing-ffmpeg.exe"),
        ffprobe: PathBuf::from("loopcut-test-missing-ffprobe.exe"),
    }
}

fn amd_like() -> GpuDevice {
    GpuDevice {
        key: GpuKey {
            vendor_id: 0x1002,
            device_id: 0x7550,
            ordinal: 0,
            name: "Test AMD".to_owned(),
        },
        vendor: GpuVendor::Amd,
        dxgi_index: 0,
        luid: 7,
        dedicated_vram_mb: 16_000,
        driver_version: None,
        is_software: false,
        cuda_index: None,
    }
}

#[test]
fn a_cancelled_token_stops_the_probe_before_it_starts() {
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut events = 0_u32;
    let result = probe_capabilities(
        &missing_paths(),
        &[amd_like()],
        &ProbeOptions::default(),
        &mut |_| events += 1,
        &cancel,
    );
    assert!(matches!(result, Err(HwError::Cancelled)), "{result:?}");
    assert_eq!(events, 0, "no events are sent after cancellation");
}

#[test]
fn a_missing_ffmpeg_records_failures_instead_of_erroring() {
    let cancel = CancelToken::new();
    let gpus = [amd_like()];
    let mut starts = 0_u32;
    let mut results = 0_u32;
    let caps = probe_capabilities(
        &missing_paths(),
        &gpus,
        &ProbeOptions {
            timeout: Duration::from_secs(5),
            benchmark: false,
        },
        &mut |event| match event {
            ProbeEvent::Step { .. } => starts += 1,
            ProbeEvent::Result { .. } => results += 1,
        },
        &cancel,
    )
    .expect("a missing binary is a recorded failure, not a probe error");

    assert_eq!(starts, results, "every step reports a result");
    assert_eq!(caps.ffmpeg_version, "unknown");
    assert_eq!(caps.cpu_encode.len(), 4);
    assert!(
        caps.cpu_encode
            .iter()
            .all(|cap| cap.status != CapStatus::Ok)
    );
    let amd = &caps.devices[0];
    assert_eq!(
        amd.decode.len(),
        6,
        "amf and d3d11va for three fixtures each"
    );
    assert!(amd.decode.iter().all(|cap| cap.status != CapStatus::Ok));
    assert_eq!(amd.encode.len(), 3);
    assert!(amd.encode.iter().all(|cap| cap.status == CapStatus::Failed));
    assert!(
        caps.warnings
            .iter()
            .any(|warning| warning.contains("fixture"))
    );
}

#[test]
fn software_adapters_are_listed_without_tests() {
    let cancel = CancelToken::new();
    let mut warp = amd_like();
    warp.key.vendor_id = 0x1414;
    warp.vendor = GpuVendor::Microsoft;
    warp.is_software = true;
    let caps = probe_capabilities(
        &missing_paths(),
        &[warp],
        &ProbeOptions {
            timeout: Duration::from_secs(5),
            benchmark: false,
        },
        &mut |_| {},
        &cancel,
    )
    .expect("probe");
    assert_eq!(caps.devices.len(), 1);
    let entry = &caps.devices[0];
    assert!(entry.decode.is_empty() && entry.scale.is_empty() && entry.encode.is_empty());
}

fn igpu_like() -> GpuDevice {
    GpuDevice {
        key: GpuKey {
            vendor_id: 0x1002,
            device_id: 0x1900,
            ordinal: 0,
            name: "Test AMD APU".to_owned(),
        },
        vendor: GpuVendor::Amd,
        dxgi_index: 0,
        luid: 3,
        dedicated_vram_mb: 512,
        driver_version: None,
        is_software: false,
        cuda_index: None,
    }
}

#[test]
fn amf_is_unsupported_on_every_amd_adapter_but_the_default_one() {
    // DXGI order decides the AMF default: the APU (index 0) is the first AMD adapter, so the discrete card (index 1)
    // cannot run AMF and must say why.
    let cancel = CancelToken::new();
    let apu = igpu_like();
    let mut dgpu = amd_like();
    dgpu.dxgi_index = 1;
    let caps = probe_capabilities(
        &missing_paths(),
        &[apu, dgpu],
        &ProbeOptions {
            timeout: Duration::from_secs(5),
            benchmark: false,
        },
        &mut |_| {},
        &cancel,
    )
    .expect("a missing binary is a recorded failure, not a probe error");

    let (default_adapter, other) = (&caps.devices[0], &caps.devices[1]);
    let amf_on_other: Vec<_> = other
        .decode
        .iter()
        .filter(|cap| cap.backend == "amf")
        .collect();
    assert_eq!(amf_on_other.len(), 3, "one unsupported entry per fixture");
    for cap in &amf_on_other {
        assert_eq!(cap.status, CapStatus::Unsupported);
        let reason = cap.error.as_deref().unwrap_or_default();
        assert!(reason.contains("default AMD adapter"), "{reason}");
        assert!(
            reason.contains("Test AMD APU"),
            "the owner is named: {reason}"
        );
    }
    assert!(
        other
            .encode
            .iter()
            .filter(|cap| cap.backend == "amf")
            .all(|cap| cap.status == CapStatus::Unsupported),
        "AMF encode is unsupported on the discrete card too"
    );
    assert!(
        other.decode.iter().any(|cap| cap.backend == "d3d11va"),
        "D3D11VA is still tested on the discrete card"
    );

    // The default adapter was actually tried: its AMF entries come from failed jobs, not from the planning rule.
    assert!(
        default_adapter
            .decode
            .iter()
            .filter(|cap| cap.backend == "amf")
            .all(|cap| cap.status == CapStatus::Failed),
        "AMF decode runs (and fails here, with no FFmpeg) on the default adapter"
    );
}

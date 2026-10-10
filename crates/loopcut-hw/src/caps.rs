//! Capability records produced by the probe and read by the selector, plus the name mapping between FFmpeg,
//! the settings UI and the cache.
//!
//! Backend names are the strings used on the wire and in reports: `"d3d11va"`, `"cuda"`, `"qsv"`, `"amf"` for
//! decoders and scalers, and `"nvenc"`, `"amf"`, `"qsv"` for hardware encoders. CPU encoders use the FFmpeg encoder
//! name (`"libx264"`, ...).

use loopcut_core::hw::{DecodeBackend, EncoderBackend, GpuScaler, GpuVendor, VideoCodec};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::devices::GpuDevice;

/// Version of the capability record layout. Stored in every cache file and checked on load.
pub const SCHEMA_VERSION: u32 = 2;

/// Outcome of one probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum CapStatus {
    /// The probe ran and produced the expected output.
    Ok,
    /// The FFmpeg build or the adapter cannot provide it (missing filter, hwaccel or encoder, or no device index).
    Unsupported,
    /// The probe ran and failed (driver error, minimum size, timeout, or wrong output).
    Failed,
}

/// One hardware decode chain tested on one adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DecodeCap {
    /// Codec of the fixture that was decoded.
    pub codec: VideoCodec,
    /// True for the 10-bit fixture (decoded as P010).
    pub ten_bit: bool,
    /// `"d3d11va"`, `"cuda"`, `"qsv"` or `"amf"`.
    pub backend: String,
    pub status: CapStatus,
    /// Decode speed relative to real time. Not measured by the current probe (fixtures are one second long).
    pub speed_x: Option<f32>,
    /// Why the probe did not pass.
    pub error: Option<String>,
}

/// One GPU scaler tested after a decode of one codec on one backend. A scaler is probed only for a codec whose decode
/// passed, and selection uses it only for that codec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScalerCap {
    /// The codec whose fixture was decoded and scaled: the scaler was probed with this codec and no other.
    pub codec: VideoCodec,
    pub scaler: GpuScaler,
    /// The decode backend the scaler ran after: `"d3d11va"`, `"cuda"`, `"qsv"` or `"amf"`.
    pub backend: String,
    pub status: CapStatus,
    pub error: Option<String>,
}

/// One hardware encoder tested on one adapter, or a CPU encoder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EncodeCap {
    pub codec: VideoCodec,
    /// `"nvenc"`, `"amf"` or `"qsv"` for hardware; the FFmpeg encoder name (`"libx264"`, ...) for CPU encoders.
    pub backend: String,
    pub status: CapStatus,
    /// Smallest frame width that encoded, from the size search. `None` for CPU encoders and failed probes.
    pub min_width: Option<u32>,
    /// Smallest frame height that encoded, from the size search.
    pub min_height: Option<u32>,
    /// Encode speed relative to real time at 1280x720, present only when the benchmark ran.
    pub speed_x: Option<f32>,
    /// Why the probe did not pass.
    pub error: Option<String>,
}

/// Everything probed for one adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeviceCaps {
    pub device: GpuDevice,
    pub decode: Vec<DecodeCap>,
    pub scale: Vec<ScalerCap>,
    pub encode: Vec<EncodeCap>,
}

/// A complete probe result: the cache file body and the input to backend selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Capabilities {
    /// [`SCHEMA_VERSION`] at the time of the probe.
    pub schema: u32,
    /// The FFmpeg version string, for example `n9.0.2-25-g67b60c310b-20261010`.
    pub ffmpeg_version: String,
    /// Unix time (seconds) when the probe finished.
    pub probed_at_unix: u64,
    /// One entry per adapter in enumeration order, including the software adapter (which has no probes).
    pub devices: Vec<DeviceCaps>,
    /// CPU encoders: libx264, libx265, libsvtav1 and libvpx-vp9.
    pub cpu_encode: Vec<EncodeCap>,
    /// Surprising results worth showing in diagnostics.
    pub warnings: Vec<String>,
}

/// Canonical name of a decode backend, as used on the wire and in reports.
pub fn decode_backend_name(backend: DecodeBackend) -> &'static str {
    match backend {
        DecodeBackend::Cpu => "cpu",
        DecodeBackend::D3d11va { .. } => "d3d11va",
        DecodeBackend::Cuda { .. } => "cuda",
        DecodeBackend::Qsv { .. } => "qsv",
        DecodeBackend::Amf { .. } => "amf",
    }
}

/// Canonical name of a hardware encoder. CPU encoders are named by FFmpeg encoder (see [`cpu_encoder_name`]).
pub fn encoder_name(backend: EncoderBackend) -> &'static str {
    match backend {
        EncoderBackend::Cpu => "cpu",
        EncoderBackend::Nvenc { .. } => "nvenc",
        EncoderBackend::Amf { .. } => "amf",
        EncoderBackend::Qsv { .. } => "qsv",
    }
}

/// The FFmpeg filter a GPU scaler uses, for build checks and labels.
pub fn scaler_filter(scaler: GpuScaler) -> Option<&'static str> {
    match scaler {
        GpuScaler::None => None,
        GpuScaler::Cuda => Some("scale_cuda"),
        GpuScaler::Qsv => Some("vpp_qsv"),
        GpuScaler::Amf => Some("vpp_amf"),
        GpuScaler::D3d11 => Some("scale_d3d11"),
    }
}

/// The decode backend a vendor should try first. Other vendors only get the vendor-neutral `d3d11va`.
pub fn native_decode_name(vendor: GpuVendor) -> Option<&'static str> {
    match vendor {
        GpuVendor::Nvidia => Some("cuda"),
        GpuVendor::Amd => Some("amf"),
        GpuVendor::Intel => Some("qsv"),
        GpuVendor::Microsoft | GpuVendor::Other => None,
    }
}

/// The hardware encoder a vendor provides, if any.
pub fn native_encode_name(vendor: GpuVendor) -> Option<&'static str> {
    match vendor {
        GpuVendor::Nvidia => Some("nvenc"),
        GpuVendor::Amd => Some("amf"),
        GpuVendor::Intel => Some("qsv"),
        GpuVendor::Microsoft | GpuVendor::Other => None,
    }
}

/// The GPU scaler that runs after a decode backend, when one exists (`d3d11va` pairs with `scale_d3d11`).
pub fn native_scaler_for(backend_name: &str) -> Option<GpuScaler> {
    match backend_name {
        "amf" => Some(GpuScaler::Amf),
        "cuda" => Some(GpuScaler::Cuda),
        "qsv" => Some(GpuScaler::Qsv),
        "d3d11va" => Some(GpuScaler::D3d11),
        _ => None,
    }
}

/// Builds the decode backend for a backend name on `device`, using its DXGI or CUDA index. `None` when the adapter
/// lacks that index (for example NVDEC without a CUDA device for the adapter's LUID).
pub fn decode_backend_for(backend_name: &str, device: &GpuDevice) -> Option<DecodeBackend> {
    match backend_name {
        "d3d11va" => Some(DecodeBackend::D3d11va {
            dxgi_index: device.dxgi_index,
        }),
        "amf" => Some(DecodeBackend::Amf {
            dxgi_index: device.dxgi_index,
        }),
        "qsv" => Some(DecodeBackend::Qsv {
            dxgi_index: device.dxgi_index,
        }),
        "cuda" => device
            .cuda_index
            .map(|cuda_index| DecodeBackend::Cuda { cuda_index }),
        _ => None,
    }
}

/// Builds the hardware encoder for a backend name on `device`. `None` when the adapter lacks the index it needs.
pub fn encode_backend_for(backend_name: &str, device: &GpuDevice) -> Option<EncoderBackend> {
    match backend_name {
        "amf" => Some(EncoderBackend::Amf {
            dxgi_index: device.dxgi_index,
        }),
        "qsv" => Some(EncoderBackend::Qsv {
            dxgi_index: device.dxgi_index,
        }),
        "nvenc" => device
            .cuda_index
            .map(|cuda_index| EncoderBackend::Nvenc { cuda_index }),
        _ => None,
    }
}

/// The FFmpeg encoder name of a CPU encoder for a codec.
pub fn cpu_encoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "libx264",
        VideoCodec::Hevc => "libx265",
        VideoCodec::Av1 => "libsvtav1",
        VideoCodec::Vp9 => "libvpx-vp9",
    }
}

/// Lower-case codec name as FFmpeg reports it (`h264`, `hevc`, `av1`, `vp9`).
pub fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => "av1",
        VideoCodec::Vp9 => "vp9",
    }
}

/// Maps a codec name from ffprobe (`h265` is accepted as an alias) to a [`VideoCodec`].
pub fn parse_codec(name: &str) -> Option<VideoCodec> {
    match name.to_ascii_lowercase().as_str() {
        "h264" | "avc" => Some(VideoCodec::H264),
        "hevc" | "h265" => Some(VideoCodec::Hevc),
        "av1" => Some(VideoCodec::Av1),
        "vp9" => Some(VideoCodec::Vp9),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_names_round_trip() {
        for codec in [
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::Av1,
            VideoCodec::Vp9,
        ] {
            assert_eq!(parse_codec(codec_name(codec)), Some(codec));
        }
        assert_eq!(parse_codec("H265"), Some(VideoCodec::Hevc));
        assert_eq!(parse_codec("prores"), None);
    }

    #[test]
    fn vendor_backends_are_consistent() {
        assert_eq!(native_decode_name(GpuVendor::Amd), Some("amf"));
        assert_eq!(native_encode_name(GpuVendor::Nvidia), Some("nvenc"));
        assert_eq!(native_decode_name(GpuVendor::Other), None);
        assert_eq!(native_scaler_for("d3d11va"), Some(GpuScaler::D3d11));
        assert_eq!(native_scaler_for("nvenc"), None);
        assert_eq!(cpu_encoder_name(VideoCodec::Vp9), "libvpx-vp9");
    }
}

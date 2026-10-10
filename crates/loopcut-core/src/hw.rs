//! Hardware-acceleration vocabulary shared by probing (`loopcut-hw`), command building
//! (`loopcut-media`), the app settings UI and the export orchestrator.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable identity of a GPU across launches.
///
/// DXGI LUIDs change every boot, so adapters are identified by PCI vendor/device IDs plus an ordinal
/// among adapters with the same IDs (to tell identical cards apart). `name` is for display only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GpuKey {
    pub vendor_id: u32,
    pub device_id: u32,
    pub ordinal: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Microsoft,
    Other,
}

impl GpuVendor {
    pub fn from_pci_id(vendor_id: u32) -> Self {
        match vendor_id {
            0x10DE => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            0x1414 => Self::Microsoft,
            _ => Self::Other,
        }
    }
}

/// How FFmpeg decodes a source. Indices are in the namespace each API uses: DXGI enumeration order for
/// D3D11VA / QSV / AMF and CUDA ordinals for NVDEC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum DecodeBackend {
    Cpu,
    /// Vendor-neutral DXVA decode (frames downloaded, then scaled on the CPU unless a GPU scaler is set).
    D3d11va {
        dxgi_index: u32,
    },
    /// NVIDIA NVDEC through CUDA.
    Cuda {
        cuda_index: u32,
    },
    /// Intel Quick Sync (oneVPL) with a D3D11 child device.
    Qsv {
        dxgi_index: u32,
    },
    /// AMD AMF decode (`-hwaccel amf`).
    Amf {
        dxgi_index: u32,
    },
}

impl DecodeBackend {
    pub fn is_hardware(&self) -> bool {
        !matches!(self, Self::Cpu)
    }
}

/// GPU scaler used before frames are downloaded to system memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum GpuScaler {
    /// Download full-size frames and scale with swscale (lanczos).
    None,
    /// `scale_cuda` (NVIDIA).
    Cuda,
    /// `vpp_qsv` (Intel; can also crop).
    Qsv,
    /// `vpp_amf` (AMD).
    Amf,
    /// `scale_d3d11` (vendor-neutral, but fails on some drivers — probe before use).
    D3d11,
}

/// A complete decode configuration selected for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DecodeChain {
    pub backend: DecodeBackend,
    pub scaler: GpuScaler,
}

impl DecodeChain {
    pub const CPU: Self = Self {
        backend: DecodeBackend::Cpu,
        scaler: GpuScaler::None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
    Vp9,
}

/// Which encoder implementation produces a video file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum EncoderBackend {
    /// libx264 / libx265 / libsvtav1 / libvpx-vp9.
    Cpu,
    Nvenc {
        cuda_index: u32,
    },
    Amf {
        dxgi_index: u32,
    },
    Qsv {
        dxgi_index: u32,
    },
}

impl EncoderBackend {
    pub fn is_hardware(&self) -> bool {
        !matches!(self, Self::Cpu)
    }

    /// FFmpeg encoder name for a codec, or `None` when the backend cannot encode it.
    pub fn ffmpeg_encoder(&self, codec: VideoCodec) -> Option<&'static str> {
        use VideoCodec::*;
        Some(match (self, codec) {
            (Self::Cpu, H264) => "libx264",
            (Self::Cpu, Hevc) => "libx265",
            (Self::Cpu, Av1) => "libsvtav1",
            (Self::Cpu, Vp9) => "libvpx-vp9",
            (Self::Nvenc { .. }, H264) => "h264_nvenc",
            (Self::Nvenc { .. }, Hevc) => "hevc_nvenc",
            (Self::Nvenc { .. }, Av1) => "av1_nvenc",
            (Self::Amf { .. }, H264) => "h264_amf",
            (Self::Amf { .. }, Hevc) => "hevc_amf",
            (Self::Amf { .. }, Av1) => "av1_amf",
            (Self::Qsv { .. }, H264) => "h264_qsv",
            (Self::Qsv { .. }, Hevc) => "hevc_qsv",
            (Self::Qsv { .. }, Av1) => "av1_qsv",
            (_, Vp9) => return None,
        })
    }
}

/// Which GPU LoopCut uses for decoding, effects and encoding.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum GpuPreference {
    /// The fastest hardware adapter that passed the probes.
    #[default]
    Auto,
    /// Software only (CPU decode/encode, WARP effects).
    Software,
    Specific {
        key: GpuKey,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DecodePreference {
    #[default]
    Auto,
    Cpu,
    D3d11va,
    Cuda,
    Qsv,
    Amf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum EncoderPreference {
    #[default]
    Auto,
    Cpu,
    Nvenc,
    Amf,
    Qsv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum EffectsRenderer {
    /// wgpu on the selected GPU.
    #[default]
    Gpu,
    /// wgpu on WARP (Microsoft's software rasterizer) — same shaders, CPU only.
    Cpu,
}

/// User-facing hardware acceleration settings (Settings → Performance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct HwSettings {
    /// Master switch. Off = CPU decode, CPU encoders, WARP effects.
    pub enabled: bool,
    pub gpu: GpuPreference,
    /// Decoding for the timeline, thumbnails, preview and export.
    pub decode: DecodePreference,
    /// Encoder for timeline proxies.
    pub proxy_encoder: EncoderPreference,
    /// Encoder for MP4/WebM exports.
    pub export_encoder: EncoderPreference,
    /// Renderer for previews and exports (color, captions, transforms).
    pub effects: EffectsRenderer,
    /// Use GPU scalers before downloading frames when the probe verified them.
    pub gpu_scaling: bool,
}

impl Default for HwSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            gpu: GpuPreference::Auto,
            decode: DecodePreference::Auto,
            proxy_encoder: EncoderPreference::Auto,
            export_encoder: EncoderPreference::Auto,
            effects: EffectsRenderer::Gpu,
            gpu_scaling: true,
        }
    }
}

/// Environment variable that forces hardware stages to fail, so every CPU fallback path can be tested.
///
/// Comma-separated tokens: a stage (`decode`, `scale`, `encode`, `all`) or a backend name (`amf`, `nvenc`,
/// `qsv`, `cuda`, `d3d11va`). Example: `LOOPCUT_FORCE_HW_FAIL=encode` or `LOOPCUT_FORCE_HW_FAIL=amf,decode`.
pub const FORCE_HW_FAIL_ENV: &str = "LOOPCUT_FORCE_HW_FAIL";

/// Pipeline stage a forced failure applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwStage {
    Decode,
    Scale,
    Encode,
}

/// True when `spec` (the value of [`FORCE_HW_FAIL_ENV`]) forces `backend` to fail at `stage`.
/// `backend` is the lower-case backend name (`amf`, `nvenc`, `qsv`, `cuda`, `d3d11va`); CPU never fails.
pub fn forced_failure(spec: &str, stage: HwStage, backend: &str) -> bool {
    if backend.is_empty() || backend.eq_ignore_ascii_case("cpu") {
        return false;
    }
    spec.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .any(|token| {
            let t = token.to_ascii_lowercase();
            t == "all"
                || t == backend.to_ascii_lowercase()
                || matches!(
                    (t.as_str(), stage),
                    ("decode", HwStage::Decode)
                        | ("scale", HwStage::Scale)
                        | ("encode", HwStage::Encode)
                )
        })
}

/// [`forced_failure`] evaluated against the current process environment.
pub fn forced_failure_from_env(stage: HwStage, backend: &str) -> bool {
    std::env::var(FORCE_HW_FAIL_ENV).is_ok_and(|spec| forced_failure(&spec, stage, backend))
}

impl DecodeBackend {
    /// Lower-case backend name used by fault injection and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::D3d11va { .. } => "d3d11va",
            Self::Cuda { .. } => "cuda",
            Self::Qsv { .. } => "qsv",
            Self::Amf { .. } => "amf",
        }
    }
}

impl EncoderBackend {
    /// Lower-case backend name used by fault injection and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Nvenc { .. } => "nvenc",
            Self::Amf { .. } => "amf",
            Self::Qsv { .. } => "qsv",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_names() {
        assert_eq!(
            EncoderBackend::Amf { dxgi_index: 0 }.ffmpeg_encoder(VideoCodec::Hevc),
            Some("hevc_amf")
        );
        assert_eq!(
            EncoderBackend::Nvenc { cuda_index: 1 }.ffmpeg_encoder(VideoCodec::Vp9),
            None
        );
        assert_eq!(
            EncoderBackend::Cpu.ffmpeg_encoder(VideoCodec::Av1),
            Some("libsvtav1")
        );
    }

    #[test]
    fn settings_json_shape() {
        let json = serde_json::to_value(HwSettings::default()).unwrap();
        assert_eq!(json["gpu"]["mode"], "auto");
        assert_eq!(json["decode"], "auto");
        let back: HwSettings = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!back.enabled && back.gpu_scaling);
    }

    #[test]
    fn forced_failures() {
        assert!(forced_failure("encode", HwStage::Encode, "amf"));
        assert!(!forced_failure("encode", HwStage::Decode, "amf"));
        assert!(forced_failure("amf", HwStage::Decode, "amf"));
        assert!(!forced_failure("amf", HwStage::Decode, "cuda"));
        assert!(forced_failure("all", HwStage::Scale, "qsv"));
        assert!(!forced_failure("all", HwStage::Encode, "cpu"));
        assert!(forced_failure(
            " nvenc , decode ",
            HwStage::Decode,
            "d3d11va"
        ));
        assert!(!forced_failure("", HwStage::Decode, "amf"));
    }

    #[test]
    fn vendor_ids() {
        assert_eq!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(GpuVendor::from_pci_id(0x10DE), GpuVendor::Nvidia);
    }
}

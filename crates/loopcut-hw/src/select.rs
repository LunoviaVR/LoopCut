//! Backend selection: which GPU, decoder, GPU scaler and encoder a job uses, with automatic CPU fallback.
//!
//! Every choice is a pure function of the probed [`Capabilities`] and the user's [`HwSettings`], except the effects
//! renderer, which depends only on the adapter list and the settings (see [`choose_effects`]). Anything that is not
//! proven to work (a missing probe, a failed probe, a size below the encoder's minimum) falls back to CPU, so no
//! decision here can make a job fail that the CPU path would have finished.
//!
//! Fault injection: the environment variable [`FORCE_HW_FAIL_ENV`] (`LOOPCUT_FORCE_HW_FAIL`) treats matching
//! capabilities as failed. Its value is a comma-separated list of `decode`, `scale`, `encode`, `all`, or a backend name
//! (`amf`, `nvenc`, `qsv`, `cuda`, `d3d11va`). It is read on every call, so tests and the CI jobs can flip it. It does
//! not apply to [`choose_effects`]: the effects renderer is wgpu, not an FFmpeg stage, and its fallback to WARP is a
//! renderer concern.
//!
//! AMF runs only on the default AMD adapter (see [`crate::devices::amf_default_adapter`]). On any other AMD adapter the
//! AMF entries are unsupported, decode falls back to D3D11VA, and encode falls back to CPU.

use std::cmp::Reverse;
use std::fmt::Write as _;

use loopcut_core::hw::{
    DecodePreference, EffectsRenderer, EncoderPreference, GpuPreference, GpuVendor, VideoCodec,
};
use loopcut_core::{DecodeChain, EncoderBackend, GpuKey, GpuScaler, HwSettings};

use crate::caps::{self, CapStatus, Capabilities, DecodeCap, DeviceCaps, EncodeCap, ScalerCap};
use crate::devices::GpuDevice;

/// Environment variable that forces capabilities to fail. See the module documentation.
pub const FORCE_HW_FAIL_ENV: &str = "LOOPCUT_FORCE_HW_FAIL";

/// Parsed value of [`FORCE_HW_FAIL_ENV`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForcedFailures {
    all: bool,
    decode: bool,
    scale: bool,
    encode: bool,
    backends: Vec<String>,
}

/// The kind of capability being checked, for fault matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Decode,
    Scale,
    Encode,
}

impl ForcedFailures {
    /// Parses a comma-separated value. Empty, missing and unknown tokens are handled as follows: `None` and empty
    /// values force nothing, and any other token is a backend name, compared case-insensitively.
    pub fn parse(value: Option<&str>) -> Self {
        let mut forced = Self::default();
        let tokens = value
            .unwrap_or_default()
            .split(',')
            .map(|token| token.trim().to_ascii_lowercase())
            .filter(|token| !token.is_empty());
        for token in tokens {
            match token.as_str() {
                "all" => forced.all = true,
                "decode" => forced.decode = true,
                "scale" => forced.scale = true,
                "encode" => forced.encode = true,
                _ => forced.backends.push(token),
            }
        }
        forced
    }

    /// Reads [`FORCE_HW_FAIL_ENV`]. An unset or non-UTF-8 variable forces nothing.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(FORCE_HW_FAIL_ENV).ok().as_deref())
    }

    /// True when a capability of `kind` on `backend` must be treated as failed.
    fn forces(&self, kind: Kind, backend: &str) -> bool {
        if self.all {
            return true;
        }
        let by_kind = match kind {
            Kind::Decode => self.decode,
            Kind::Scale => self.scale,
            Kind::Encode => self.encode,
        };
        by_kind || self.backends.iter().any(|name| name == backend)
    }
}

/// A capability counts as usable when its probe passed and fault injection does not force it to fail.
fn is_usable(status: CapStatus, forced: &ForcedFailures, kind: Kind, backend: &str) -> bool {
    status == CapStatus::Ok && !forced.forces(kind, backend)
}

/// True when two keys name the same adapter. The display name is not compared.
fn same_adapter(a: &GpuKey, b: &GpuKey) -> bool {
    a.vendor_id == b.vendor_id && a.device_id == b.device_id && a.ordinal == b.ordinal
}

/// Ranking key for Auto: the fastest measured encoder first, then the number of working capabilities, then VRAM, then
/// the lower DXGI index.
type Rank = (u64, usize, u64, Reverse<u32>);

fn rank(device: &DeviceCaps, forced: &ForcedFailures) -> Option<Rank> {
    let mut usable = 0usize;
    let mut speed = 0.0_f32;
    for cap in &device.decode {
        if is_usable(cap.status, forced, Kind::Decode, &cap.backend) {
            usable += 1;
        }
    }
    for cap in &device.scale {
        if is_usable(cap.status, forced, Kind::Scale, &cap.backend) {
            usable += 1;
        }
    }
    for cap in &device.encode {
        if is_usable(cap.status, forced, Kind::Encode, &cap.backend) {
            usable += 1;
            if let Some(value) = cap.speed_x {
                speed = speed.max(value);
            }
        }
    }
    (usable > 0).then_some((
        (speed * 1000.0) as u64,
        usable,
        device.device.dedicated_vram_mb,
        Reverse(device.device.dxgi_index),
    ))
}

/// Which GPU a job should use, or `None` for software (CPU decode and encode, WARP effects).
///
/// `Software` in settings, a disabled master switch, and a probe that found no usable hardware all return `None`.
/// [`GpuPreference::Specific`] returns the named adapter whenever it is present, because the user chose it. Each
/// capability is still checked separately by the functions below.
pub fn choose_gpu<'a>(caps: &'a Capabilities, settings: &HwSettings) -> Option<&'a DeviceCaps> {
    choose_gpu_with(caps, settings, &ForcedFailures::from_env())
}

/// [`choose_gpu`] with the fault injection passed in, for tests and for callers that manage their own overrides.
pub fn choose_gpu_with<'a>(
    caps: &'a Capabilities,
    settings: &HwSettings,
    forced: &ForcedFailures,
) -> Option<&'a DeviceCaps> {
    if !settings.enabled {
        return None;
    }
    match &settings.gpu {
        GpuPreference::Software => None,
        GpuPreference::Specific { key } => caps
            .devices
            .iter()
            .find(|device| !device.device.is_software && same_adapter(&device.device.key, key)),
        GpuPreference::Auto => {
            let mut best: Option<(&DeviceCaps, Rank)> = None;
            for device in caps.devices.iter().filter(|d| !d.device.is_software) {
                let Some(candidate) = rank(device, forced) else {
                    continue;
                };
                if best
                    .as_ref()
                    .is_none_or(|(_, current)| candidate > *current)
                {
                    best = Some((device, candidate));
                }
            }
            best.map(|(device, _)| device)
        }
    }
}

/// The decode entry for `codec` on `backend_name`, matching bit depth when possible.
///
/// A 10-bit entry also covers 8-bit sources, because a decoder that handles 10-bit handles 8-bit too. A 10-bit source
/// never uses an 8-bit-only entry.
fn find_decode<'a>(
    device: &'a DeviceCaps,
    codec: VideoCodec,
    bit_depth: u8,
    backend_name: &str,
    forced: &ForcedFailures,
) -> Option<&'a DecodeCap> {
    let usable = |cap: &&DecodeCap| {
        cap.codec == codec
            && cap.backend == backend_name
            && is_usable(cap.status, forced, Kind::Decode, backend_name)
    };
    let want_ten_bit = bit_depth > 8;
    device
        .decode
        .iter()
        .filter(usable)
        .find(|cap| cap.ten_bit == want_ten_bit)
        .or_else(|| {
            if want_ten_bit {
                None
            } else {
                device.decode.iter().filter(usable).find(|cap| cap.ten_bit)
            }
        })
}

/// True when `scaler` passed its probe after a `codec` decode on `backend_name`. A scaler probed with one codec says
/// nothing about another.
fn scaler_usable(
    device: &DeviceCaps,
    scaler: GpuScaler,
    codec: VideoCodec,
    backend_name: &str,
    forced: &ForcedFailures,
) -> bool {
    device.scale.iter().any(|cap: &ScalerCap| {
        cap.codec == codec
            && cap.scaler == scaler
            && cap.backend == backend_name
            && is_usable(cap.status, forced, Kind::Scale, backend_name)
    })
}

/// Chooses the decode chain for one source stream, with CPU as the fallback.
///
/// `codec` is the ffprobe codec name (`h264`, `hevc`, `av1`, `vp9`). Sources with alpha and codecs without a probed
/// decoder use CPU. With `settings.decode` on Auto, the vendor's own decoder (AMF, NVDEC or QSV) is tried first, then
/// `d3d11va`. The AMF preference on an AMD adapter tries AMF and then `d3d11va` too, so a non-default AMD adapter
/// (where AMF cannot run) still gets hardware decode. A backend uses its matching GPU scaler when `settings.gpu_scaling`
/// is on and that scaler passed its probe for this codec. Otherwise frames are downloaded and scaled on the CPU.
pub fn choose_decode_chain(
    caps: &Capabilities,
    settings: &HwSettings,
    codec: &str,
    bit_depth: u8,
    needs_alpha: bool,
) -> DecodeChain {
    choose_decode_chain_with(
        caps,
        settings,
        codec,
        bit_depth,
        needs_alpha,
        &ForcedFailures::from_env(),
    )
}

/// [`choose_decode_chain`] with the fault injection passed in.
pub fn choose_decode_chain_with(
    caps: &Capabilities,
    settings: &HwSettings,
    codec: &str,
    bit_depth: u8,
    needs_alpha: bool,
    forced: &ForcedFailures,
) -> DecodeChain {
    if !settings.enabled || needs_alpha || settings.decode == DecodePreference::Cpu {
        return DecodeChain::CPU;
    }
    let Some(codec) = caps::parse_codec(codec) else {
        return DecodeChain::CPU;
    };
    let Some(device) = choose_gpu_with(caps, settings, forced) else {
        return DecodeChain::CPU;
    };

    let names: Vec<&str> = match settings.decode {
        DecodePreference::Auto => caps::native_decode_name(device.device.vendor)
            .into_iter()
            .chain(["d3d11va"])
            .collect(),
        DecodePreference::D3d11va => vec!["d3d11va"],
        DecodePreference::Cuda => vec!["cuda"],
        DecodePreference::Qsv => vec!["qsv"],
        DecodePreference::Amf if device.device.vendor == GpuVendor::Amd => vec!["amf", "d3d11va"],
        DecodePreference::Amf => vec!["amf"],
        DecodePreference::Cpu => return DecodeChain::CPU,
    };

    for name in names {
        let Some(backend) = caps::decode_backend_for(name, &device.device) else {
            continue;
        };
        if find_decode(device, codec, bit_depth, name, forced).is_none() {
            continue;
        }
        if settings.gpu_scaling
            && let Some(scaler) = caps::native_scaler_for(name)
            && scaler_usable(device, scaler, codec, name, forced)
        {
            return DecodeChain { backend, scaler };
        }
        return DecodeChain {
            backend,
            scaler: GpuScaler::None,
        };
    }
    DecodeChain::CPU
}

/// The hardware encoder for an export or proxy, with CPU as the fallback.
///
/// VP9 has no hardware encoder here, so it is always CPU. The encoder must have passed its probe, and the frame size must
/// be at least the minimum that encoded during the probe. With `EncoderPreference::Auto` the vendor's encoder is used
/// unless the benchmark shows it slower than the CPU encoder of the same codec (libx264, libx265 or libsvtav1). A
/// non-default AMD adapter has no AMF encoder, so it uses CPU.
pub fn choose_encoder(
    caps: &Capabilities,
    settings: &HwSettings,
    codec: VideoCodec,
    width: u32,
    height: u32,
    for_proxy: bool,
) -> EncoderBackend {
    choose_encoder_with(
        caps,
        settings,
        codec,
        width,
        height,
        for_proxy,
        &ForcedFailures::from_env(),
    )
}

/// [`choose_encoder`] with the fault injection passed in.
pub fn choose_encoder_with(
    caps: &Capabilities,
    settings: &HwSettings,
    codec: VideoCodec,
    width: u32,
    height: u32,
    for_proxy: bool,
    forced: &ForcedFailures,
) -> EncoderBackend {
    if !settings.enabled || codec == VideoCodec::Vp9 {
        return EncoderBackend::Cpu;
    }
    let preference = if for_proxy {
        settings.proxy_encoder
    } else {
        settings.export_encoder
    };
    let explicit = match preference {
        EncoderPreference::Cpu => return EncoderBackend::Cpu,
        EncoderPreference::Auto => None,
        EncoderPreference::Nvenc => Some("nvenc"),
        EncoderPreference::Amf => Some("amf"),
        EncoderPreference::Qsv => Some("qsv"),
    };
    let Some(device) = choose_gpu_with(caps, settings, forced) else {
        return EncoderBackend::Cpu;
    };
    let Some(name) = explicit.or_else(|| caps::native_encode_name(device.device.vendor)) else {
        return EncoderBackend::Cpu;
    };
    let Some(cap) = device.encode.iter().find(|cap: &&EncodeCap| {
        cap.codec == codec
            && cap.backend == name
            && is_usable(cap.status, forced, Kind::Encode, name)
    }) else {
        return EncoderBackend::Cpu;
    };
    let (Some(min_width), Some(min_height)) = (cap.min_width, cap.min_height) else {
        return EncoderBackend::Cpu;
    };
    if width < min_width || height < min_height {
        return EncoderBackend::Cpu;
    }
    if explicit.is_none()
        && let (Some(hardware), Some(cpu)) = (cap.speed_x, cpu_reference_speed(caps, codec))
        && hardware < cpu
    {
        return EncoderBackend::Cpu;
    }
    caps::encode_backend_for(name, &device.device).unwrap_or(EncoderBackend::Cpu)
}

/// Measured speed of the CPU encoder for `codec`, the reference for Auto's "slower than CPU" rule. The reference is the
/// same codec's encoder (libx264 for H.264, libx265 for HEVC, libsvtav1 for AV1), so the comparison is like with like.
/// `None` when the benchmark did not run or the encoder failed.
fn cpu_reference_speed(caps: &Capabilities, codec: VideoCodec) -> Option<f32> {
    caps.cpu_encode
        .iter()
        .find(|cap| cap.codec == codec && cap.status == CapStatus::Ok)
        .and_then(|cap| cap.speed_x)
}

/// What draws the previews and exports: the GPU (wgpu on the selected adapter) or software (wgpu on WARP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectsChoice {
    /// Run effects on this adapter.
    Gpu(GpuKey),
    /// Run effects on the software adapter.
    Software,
}

/// Chooses the effects renderer from the adapter list alone, independently of the FFmpeg probes. `gpus` is the DXGI
/// list from [`crate::devices::enumerate_gpus`], in enumeration order.
///
/// Rules, in order:
/// 1. Software when the master switch is off or `effects` is [`EffectsRenderer::Cpu`].
/// 2. Software for [`GpuPreference::Software`].
/// 3. [`GpuPreference::Specific`]: that adapter when it is present among the hardware adapters, otherwise software.
/// 4. [`GpuPreference::Auto`]: the first non-software adapter in DXGI order.
/// 5. Software when no hardware adapter exists.
///
/// The FFmpeg probes do not affect this choice, because the renderer is wgpu and the probes only test FFmpeg's paths.
pub fn choose_effects(settings: &HwSettings, gpus: &[GpuDevice]) -> EffectsChoice {
    if !settings.enabled || settings.effects == EffectsRenderer::Cpu {
        return EffectsChoice::Software;
    }
    let mut hardware = gpus.iter().filter(|device| !device.is_software);
    let chosen = match &settings.gpu {
        GpuPreference::Software => None,
        GpuPreference::Specific { key } => hardware.find(|device| same_adapter(&device.key, key)),
        GpuPreference::Auto => hardware.next(),
    };
    chosen.map_or(EffectsChoice::Software, |device| {
        EffectsChoice::Gpu(device.key.clone())
    })
}

/// Status with its reason, for the report: `OK 12.3x`, `unsupported (...)`, or `FAILED (...)`.
fn status_text(status: CapStatus, speed: Option<f32>, error: Option<&str>) -> String {
    match status {
        CapStatus::Ok => match speed {
            Some(value) => format!("OK {value:.1}x"),
            None => "OK".to_owned(),
        },
        CapStatus::Unsupported => match error {
            Some(reason) => format!("unsupported ({})", short(reason)),
            None => "unsupported".to_owned(),
        },
        CapStatus::Failed => match error {
            Some(reason) => format!("FAILED ({})", short(reason)),
            None => "FAILED".to_owned(),
        },
    }
}

/// Keeps one report entry on one line of reasonable length.
fn short(reason: &str) -> String {
    let line = reason.lines().next().unwrap_or_default();
    if line.chars().count() > 110 {
        let cut: String = line.chars().take(110).collect();
        format!("{cut}...")
    } else {
        line.to_owned()
    }
}

/// Human-readable capability report, used by "Copy diagnostics". One line per device and kind.
pub fn describe(caps: &Capabilities) -> String {
    let mut lines = vec![format!(
        "FFmpeg {} (probed at unix {})",
        caps.ffmpeg_version, caps.probed_at_unix
    )];
    for (index, entry) in caps.devices.iter().enumerate() {
        let device = &entry.device;
        if device.is_software {
            lines.push(format!(
                "[{index}] {} (software; no FFmpeg probes)",
                device.key.name
            ));
            continue;
        }
        let mut header = format!(
            "[{index}] {} ({:?} {:04x}:{:04x} #{}) dxgi {} {} MiB driver {}",
            device.key.name,
            device.vendor,
            device.key.vendor_id,
            device.key.device_id,
            device.key.ordinal,
            device.dxgi_index,
            device.dedicated_vram_mb,
            device.driver_version.as_deref().unwrap_or("n/a"),
        );
        if let Some(cuda) = device.cuda_index {
            let _ = write!(header, " cuda {cuda}");
        }
        lines.push(header);

        let decode: Vec<String> = entry
            .decode
            .iter()
            .map(|cap| {
                format!(
                    "{} {}-bit {} {}",
                    caps::codec_name(cap.codec),
                    if cap.ten_bit { 10 } else { 8 },
                    cap.backend,
                    status_text(cap.status, cap.speed_x, cap.error.as_deref())
                )
            })
            .collect();
        lines.push(format!("    decode: {}", join_or_none(&decode)));

        let scale: Vec<String> = entry
            .scale
            .iter()
            .map(|cap| {
                format!(
                    "{} {} after {} {}",
                    caps::codec_name(cap.codec),
                    caps::scaler_filter(cap.scaler).unwrap_or("none"),
                    cap.backend,
                    status_text(cap.status, None, cap.error.as_deref())
                )
            })
            .collect();
        lines.push(format!("    scale: {}", join_or_none(&scale)));

        let encode: Vec<String> = entry
            .encode
            .iter()
            .map(|cap| {
                let size = match (cap.min_width, cap.min_height) {
                    (Some(w), Some(h)) => format!(" min {w}x{h}"),
                    _ => String::new(),
                };
                format!(
                    "{} {}{} {}",
                    caps::codec_name(cap.codec),
                    cap.backend,
                    size,
                    status_text(cap.status, cap.speed_x, cap.error.as_deref())
                )
            })
            .collect();
        lines.push(format!("    encode: {}", join_or_none(&encode)));
    }

    let cpu: Vec<String> = caps
        .cpu_encode
        .iter()
        .map(|cap| {
            format!(
                "{} {}",
                cap.backend,
                status_text(cap.status, cap.speed_x, cap.error.as_deref())
            )
        })
        .collect();
    lines.push(format!("CPU encoders: {}", join_or_none(&cpu)));

    if !caps.warnings.is_empty() {
        lines.push("Warnings:".to_owned());
        lines.extend(caps.warnings.iter().map(|warning| format!("- {warning}")));
    }
    lines.join("\n")
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use loopcut_core::hw::GpuVendor;

    use super::*;
    use crate::caps::SCHEMA_VERSION;
    use crate::devices::GpuDevice;

    fn key(vendor_id: u32, device_id: u32, ordinal: u32, name: &str) -> GpuKey {
        GpuKey {
            vendor_id,
            device_id,
            ordinal,
            name: name.to_owned(),
        }
    }

    fn device(
        vendor: GpuVendor,
        key: GpuKey,
        dxgi_index: u32,
        cuda_index: Option<u32>,
        software: bool,
    ) -> GpuDevice {
        GpuDevice {
            key,
            vendor,
            dxgi_index,
            luid: u64::from(dxgi_index) + 1,
            dedicated_vram_mb: 16_368,
            driver_version: Some("32.0.31041.1004".to_owned()),
            is_software: software,
            cuda_index,
        }
    }

    fn decode(codec: VideoCodec, ten_bit: bool, backend: &str, status: CapStatus) -> DecodeCap {
        DecodeCap {
            codec,
            ten_bit,
            backend: backend.to_owned(),
            status,
            speed_x: None,
            error: None,
        }
    }

    fn encode(
        codec: VideoCodec,
        backend: &str,
        min: Option<(u32, u32)>,
        speed: Option<f32>,
    ) -> EncodeCap {
        EncodeCap {
            codec,
            backend: backend.to_owned(),
            status: CapStatus::Ok,
            min_width: min.map(|m| m.0),
            min_height: min.map(|m| m.1),
            speed_x: speed,
            error: None,
        }
    }

    fn scaler(codec: VideoCodec, scaler: GpuScaler, backend: &str, status: CapStatus) -> ScalerCap {
        ScalerCap {
            codec,
            scaler,
            backend: backend.to_owned(),
            status,
            error: None,
        }
    }

    /// AMD default adapter: AMF decode and scale work for H.264 and HEVC, hevc_amf needs 640x360, av1_amf needs 320x240,
    /// and scale_d3d11 fails for HEVC.
    fn amd(dxgi_index: u32) -> DeviceCaps {
        DeviceCaps {
            device: device(
                GpuVendor::Amd,
                key(0x1002, 0x7550, 0, "AMD Radeon RX 9060 XT"),
                dxgi_index,
                None,
                false,
            ),
            decode: vec![
                decode(VideoCodec::H264, false, "amf", CapStatus::Ok),
                decode(VideoCodec::Hevc, true, "amf", CapStatus::Ok),
                decode(VideoCodec::Av1, false, "amf", CapStatus::Ok),
                decode(VideoCodec::H264, false, "d3d11va", CapStatus::Ok),
                decode(VideoCodec::Hevc, true, "d3d11va", CapStatus::Ok),
            ],
            scale: vec![
                scaler(VideoCodec::H264, GpuScaler::Amf, "amf", CapStatus::Ok),
                scaler(VideoCodec::Hevc, GpuScaler::Amf, "amf", CapStatus::Ok),
                scaler(
                    VideoCodec::Hevc,
                    GpuScaler::D3d11,
                    "d3d11va",
                    CapStatus::Failed,
                ),
            ],
            encode: vec![
                encode(VideoCodec::H264, "amf", Some((256, 144)), Some(18.0)),
                encode(VideoCodec::Hevc, "amf", Some((640, 360)), Some(11.0)),
                encode(VideoCodec::Av1, "amf", Some((320, 240)), None),
            ],
        }
    }

    /// A second AMD adapter that AMF cannot use: its AMF entries are unsupported with the reason, while D3D11VA works
    /// and its scaler passed for H.264.
    fn non_default_amd(dxgi_index: u32, name: &str, ordinal: u32) -> DeviceCaps {
        let reason = "AMF runs only on the default AMD adapter".to_owned();
        let unsupported = |codec, ten_bit| DecodeCap {
            codec,
            ten_bit,
            backend: "amf".to_owned(),
            status: CapStatus::Unsupported,
            speed_x: None,
            error: Some(reason.clone()),
        };
        DeviceCaps {
            device: device(
                GpuVendor::Amd,
                key(0x1002, 0x1900, ordinal, name),
                dxgi_index,
                None,
                false,
            ),
            decode: vec![
                unsupported(VideoCodec::H264, false),
                unsupported(VideoCodec::Hevc, true),
                unsupported(VideoCodec::Av1, false),
                decode(VideoCodec::H264, false, "d3d11va", CapStatus::Ok),
                decode(VideoCodec::Hevc, true, "d3d11va", CapStatus::Ok),
            ],
            scale: vec![scaler(
                VideoCodec::H264,
                GpuScaler::D3d11,
                "d3d11va",
                CapStatus::Ok,
            )],
            encode: vec![EncodeCap {
                codec: VideoCodec::H264,
                backend: "amf".to_owned(),
                status: CapStatus::Unsupported,
                min_width: None,
                min_height: None,
                speed_x: None,
                error: Some(reason),
            }],
        }
    }

    /// NVIDIA-like: NVDEC, scale_cuda and NVENC all work.
    fn nvidia(dxgi_index: u32, cuda_index: Option<u32>) -> DeviceCaps {
        DeviceCaps {
            device: device(
                GpuVendor::Nvidia,
                key(0x10DE, 0x2684, 0, "NVIDIA GeForce RTX 4090"),
                dxgi_index,
                cuda_index,
                false,
            ),
            decode: vec![
                decode(VideoCodec::H264, false, "cuda", CapStatus::Ok),
                decode(VideoCodec::Hevc, true, "cuda", CapStatus::Ok),
                decode(VideoCodec::H264, false, "d3d11va", CapStatus::Ok),
            ],
            scale: vec![scaler(
                VideoCodec::H264,
                GpuScaler::Cuda,
                "cuda",
                CapStatus::Ok,
            )],
            encode: vec![encode(VideoCodec::H264, "nvenc", Some((256, 256)), None)],
        }
    }

    /// Intel-like: QSV decode and vpp_qsv work, no encoder probed as working.
    fn intel(dxgi_index: u32) -> DeviceCaps {
        DeviceCaps {
            device: device(
                GpuVendor::Intel,
                key(0x8086, 0xA780, 0, "Intel UHD Graphics 770"),
                dxgi_index,
                None,
                false,
            ),
            decode: vec![decode(VideoCodec::H264, false, "qsv", CapStatus::Ok)],
            scale: vec![scaler(
                VideoCodec::H264,
                GpuScaler::Qsv,
                "qsv",
                CapStatus::Ok,
            )],
            encode: vec![EncodeCap {
                codec: VideoCodec::H264,
                backend: "qsv".to_owned(),
                status: CapStatus::Failed,
                min_width: None,
                min_height: None,
                speed_x: None,
                error: Some("probe failed".to_owned()),
            }],
        }
    }

    fn software() -> DeviceCaps {
        DeviceCaps {
            device: device(
                GpuVendor::Microsoft,
                key(0x1414, 0x008C, 0, "Microsoft Basic Render Driver"),
                1,
                None,
                true,
            ),
            decode: Vec::new(),
            scale: Vec::new(),
            encode: Vec::new(),
        }
    }

    fn cpu(codec: VideoCodec, speed: Option<f32>) -> EncodeCap {
        EncodeCap {
            codec,
            backend: caps::cpu_encoder_name(codec).to_owned(),
            status: CapStatus::Ok,
            min_width: None,
            min_height: None,
            speed_x: speed,
            error: None,
        }
    }

    fn caps_with(devices: Vec<DeviceCaps>) -> Capabilities {
        Capabilities {
            schema: SCHEMA_VERSION,
            ffmpeg_version: "n9.0.2".to_owned(),
            probed_at_unix: 0,
            devices,
            cpu_encode: vec![cpu(VideoCodec::H264, None)],
            warnings: Vec::new(),
        }
    }

    fn settings() -> HwSettings {
        HwSettings::default()
    }

    fn none() -> ForcedFailures {
        ForcedFailures::default()
    }

    fn gpu_device(caps_entry: &DeviceCaps) -> GpuDevice {
        caps_entry.device.clone()
    }

    #[test]
    fn auto_gpu_prefers_the_device_that_passed_probes() {
        let caps = caps_with(vec![software(), amd(0)]);
        let chosen = choose_gpu_with(&caps, &settings(), &none()).expect("amd");
        assert_eq!(chosen.device.vendor, GpuVendor::Amd);
    }

    #[test]
    fn software_or_disabled_settings_select_no_gpu() {
        let caps = caps_with(vec![software(), amd(0)]);
        let mut s = settings();
        s.gpu = GpuPreference::Software;
        assert!(choose_gpu_with(&caps, &s, &none()).is_none());

        let mut s = settings();
        s.enabled = false;
        assert!(choose_gpu_with(&caps, &s, &none()).is_none());
    }

    #[test]
    fn auto_gpu_skips_adapters_without_usable_probes() {
        let caps = caps_with(vec![software(), intel(1)]);
        // An adapter whose probes all failed or were never run is not chosen by Auto.
        let dead = DeviceCaps {
            decode: Vec::new(),
            scale: Vec::new(),
            encode: Vec::new(),
            ..intel(1)
        };
        let caps_dead = caps_with(vec![software(), dead]);
        assert!(choose_gpu_with(&caps_dead, &settings(), &none()).is_none());
        // Intel has a working decode and scaler, so it is usable.
        assert!(choose_gpu_with(&caps, &settings(), &none()).is_some());
    }

    #[test]
    fn auto_gpu_ranks_by_measured_speed_then_vram_then_index() {
        let mut slow = amd(0);
        slow.encode[1].speed_x = Some(2.0);
        let mut fast = amd(1);
        fast.encode[1].speed_x = Some(20.0);
        let caps = caps_with(vec![slow, fast]);
        let chosen = choose_gpu_with(&caps, &settings(), &none()).expect("gpu");
        assert_eq!(chosen.device.dxgi_index, 1);

        // Equal everything: the lower DXGI index wins.
        let caps_tie = caps_with(vec![amd(0), amd(1)]);
        let chosen_tie = choose_gpu_with(&caps_tie, &settings(), &none()).expect("gpu");
        assert_eq!(chosen_tie.device.dxgi_index, 0);
    }

    #[test]
    fn specific_gpu_is_honored_when_present() {
        let caps = caps_with(vec![software(), nvidia(0, Some(0)), amd(1)]);
        let mut s = settings();
        s.gpu = GpuPreference::Specific {
            key: key(0x1002, 0x7550, 0, "renamed in settings"),
        };
        let chosen = choose_gpu_with(&caps, &s, &none()).expect("amd by identity");
        assert_eq!(chosen.device.vendor, GpuVendor::Amd);

        s.gpu = GpuPreference::Specific {
            key: key(0x8086, 0x1234, 0, "absent"),
        };
        assert!(choose_gpu_with(&caps, &s, &none()).is_none());
    }

    #[test]
    fn amd_hevc_chain_uses_native_decode_and_its_verified_scaler() {
        let caps = caps_with(vec![amd(0)]);
        let chain = choose_decode_chain_with(&caps, &settings(), "hevc", 10, false, &none());
        assert_eq!(
            chain,
            DecodeChain {
                backend: loopcut_core::DecodeBackend::Amf { dxgi_index: 0 },
                scaler: GpuScaler::Amf,
            }
        );
    }

    #[test]
    fn a_scaler_is_used_only_for_the_codec_it_was_probed_with() {
        // AMF AV1 decode passed, but no scaler was probed for AV1: AV1 must not use vpp_amf.
        let caps = caps_with(vec![amd(0)]);
        let s = settings();
        let av1 = choose_decode_chain_with(&caps, &s, "av1", 8, false, &none());
        assert_eq!(
            av1.backend,
            loopcut_core::DecodeBackend::Amf { dxgi_index: 0 }
        );
        assert_eq!(av1.scaler, GpuScaler::None);

        let h264 = choose_decode_chain_with(&caps, &s, "h264", 8, false, &none());
        assert_eq!(
            h264.scaler,
            GpuScaler::Amf,
            "H.264 has its own passing scaler"
        );
    }

    #[test]
    fn decode_falls_back_to_d3d11va_when_native_is_missing() {
        let mut device = amd(0);
        device.decode.retain(|cap| cap.backend != "amf");
        device.scale.retain(|cap| cap.backend != "amf");
        let caps = caps_with(vec![device]);
        let chain = choose_decode_chain_with(&caps, &settings(), "h264", 8, false, &none());
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 0 }
        );
        assert_eq!(
            chain.scaler,
            GpuScaler::None,
            "no scale_d3d11 probe passed for H.264 in this fixture"
        );
    }

    #[test]
    fn decode_uses_cpu_for_alpha_unknown_codecs_and_missing_bit_depth() {
        let caps = caps_with(vec![amd(0)]);
        let s = settings();
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "h264", 8, true, &none()),
            DecodeChain::CPU
        );
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "vp9", 8, false, &none()),
            DecodeChain::CPU
        );
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "prores", 10, false, &none()),
            DecodeChain::CPU
        );
        // H.264 10-bit was never probed.
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "h264", 10, false, &none()),
            DecodeChain::CPU
        );
    }

    #[test]
    fn eight_bit_hevc_uses_the_ten_bit_probe_but_ten_bit_never_uses_eight_bit_only() {
        let mut device = amd(0);
        device
            .decode
            .retain(|cap| !(cap.codec == VideoCodec::Hevc && cap.ten_bit && cap.backend == "amf"));
        device
            .decode
            .push(decode(VideoCodec::Hevc, false, "amf", CapStatus::Ok));
        let caps = caps_with(vec![device]);
        let s = settings();
        let chain = choose_decode_chain_with(&caps, &s, "hevc", 10, false, &none());
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 0 }
        );
    }

    #[test]
    fn gpu_scaling_setting_disables_scalers() {
        let caps = caps_with(vec![amd(0)]);
        let mut s = settings();
        s.gpu_scaling = false;
        let chain = choose_decode_chain_with(&caps, &s, "h264", 8, false, &none());
        assert_eq!(chain.scaler, GpuScaler::None);
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::Amf { dxgi_index: 0 }
        );
    }

    #[test]
    fn nvidia_chain_needs_a_cuda_index() {
        let with_index = caps_with(vec![nvidia(0, Some(0))]);
        let chain = choose_decode_chain_with(&with_index, &settings(), "h264", 8, false, &none());
        assert_eq!(
            chain,
            DecodeChain {
                backend: loopcut_core::DecodeBackend::Cuda { cuda_index: 0 },
                scaler: GpuScaler::Cuda,
            }
        );

        let without = caps_with(vec![nvidia(0, None)]);
        let chain = choose_decode_chain_with(&without, &settings(), "h264", 8, false, &none());
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 0 },
            "cuda needs an index"
        );
    }

    #[test]
    fn decode_preference_overrides_auto_and_cpu_wins_over_all() {
        let caps = caps_with(vec![amd(0)]);
        let mut s = settings();
        s.decode = DecodePreference::D3d11va;
        let chain = choose_decode_chain_with(&caps, &s, "h264", 8, false, &none());
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 0 }
        );

        s.decode = DecodePreference::Cuda;
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "h264", 8, false, &none()),
            DecodeChain::CPU
        );

        s.decode = DecodePreference::Cpu;
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "h264", 8, false, &none()),
            DecodeChain::CPU
        );
    }

    #[test]
    fn non_default_amd_adapter_falls_back_to_d3d11va_for_decode() {
        // APU is DXGI 0 (the default AMD adapter for AMF). The discrete card is DXGI 1 and cannot run AMF.
        let caps = caps_with(vec![amd(0), non_default_amd(1, "AMD Radeon RX 9060 XT", 0)]);
        let s = settings();
        let dgpu = GpuKey {
            vendor_id: 0x1002,
            device_id: 0x1900,
            ordinal: 0,
            name: "AMD Radeon RX 9060 XT".to_owned(),
        };

        // The user picked the discrete card: H.264 decodes with D3D11VA and its scaler, not AMF.
        let mut pinned = s.clone();
        pinned.gpu = GpuPreference::Specific { key: dgpu.clone() };
        let h264 = choose_decode_chain_with(&caps, &pinned, "h264", 8, false, &none());
        assert_eq!(
            h264,
            DecodeChain {
                backend: loopcut_core::DecodeBackend::D3d11va { dxgi_index: 1 },
                scaler: GpuScaler::D3d11,
            }
        );

        // HEVC 10-bit has no passing scale_d3d11 probe here, so the decode runs without a GPU scaler.
        let hevc = choose_decode_chain_with(&caps, &pinned, "hevc", 10, false, &none());
        assert_eq!(
            hevc.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 1 }
        );
        assert_eq!(hevc.scaler, GpuScaler::None);

        // An explicit AMF preference also falls back to D3D11VA on that adapter.
        pinned.decode = DecodePreference::Amf;
        let amf_pref = choose_decode_chain_with(&caps, &pinned, "h264", 8, false, &none());
        assert_eq!(
            amf_pref.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 1 }
        );
    }

    #[test]
    fn non_default_amd_adapter_uses_cpu_for_encode() {
        let caps = caps_with(vec![amd(0), non_default_amd(1, "AMD Radeon RX 9060 XT", 0)]);
        let mut pinned = settings();
        pinned.gpu = GpuPreference::Specific {
            key: key(0x1002, 0x1900, 0, "AMD Radeon RX 9060 XT"),
        };
        assert_eq!(
            choose_encoder_with(&caps, &pinned, VideoCodec::H264, 640, 360, false, &none()),
            EncoderBackend::Cpu,
            "AMF cannot encode on the non-default adapter and D3D11VA has no encoder"
        );
        pinned.export_encoder = EncoderPreference::Amf;
        assert_eq!(
            choose_encoder_with(&caps, &pinned, VideoCodec::H264, 640, 360, false, &none()),
            EncoderBackend::Cpu,
            "an explicit AMF choice still cannot run there"
        );
    }

    #[test]
    fn apu_and_dgpu_auto_keeps_amf_on_the_default_adapter() {
        // Auto ranks by measured encoder speed. Only the default adapter has measured AMF encoders, so it is chosen.
        let caps = caps_with(vec![amd(0), non_default_amd(1, "AMD Radeon RX 9060 XT", 0)]);
        let chosen = choose_gpu_with(&caps, &settings(), &none()).expect("gpu");
        assert_eq!(chosen.device.dxgi_index, 0);
        assert_eq!(
            choose_encoder_with(
                &caps,
                &settings(),
                VideoCodec::H264,
                640,
                360,
                false,
                &none()
            ),
            EncoderBackend::Amf { dxgi_index: 0 }
        );
    }

    #[test]
    fn encoder_respects_minimum_sizes_and_codec_support() {
        let caps = caps_with(vec![amd(0)]);
        let s = settings();
        // hevc_amf needs 640x360: a 480x270 proxy falls back to CPU, a 640x360 export does not.
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::Hevc, 480, 270, false, &none()),
            EncoderBackend::Cpu
        );
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::Hevc, 640, 360, false, &none()),
            EncoderBackend::Amf { dxgi_index: 0 }
        );
        // h264_amf works from 256x144.
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 320, 180, true, &none()),
            EncoderBackend::Amf { dxgi_index: 0 }
        );
        // VP9 never uses hardware, and a codec with no probe falls back.
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::Vp9, 1280, 720, false, &none()),
            EncoderBackend::Cpu
        );
        let no_probe = choose_encoder_with(
            &caps_with(vec![intel(0)]),
            &s,
            VideoCodec::H264,
            640,
            360,
            false,
            &none(),
        );
        assert_eq!(
            no_probe,
            EncoderBackend::Cpu,
            "the failed QSV probe falls back"
        );
    }

    #[test]
    fn encoder_preference_selects_or_forbids_backends() {
        let caps = caps_with(vec![amd(0)]);
        let mut s = settings();
        s.export_encoder = EncoderPreference::Cpu;
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 640, 360, false, &none()),
            EncoderBackend::Cpu
        );

        s.export_encoder = EncoderPreference::Nvenc;
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 640, 360, false, &none()),
            EncoderBackend::Cpu,
            "AMD cannot give NVENC"
        );

        s.export_encoder = EncoderPreference::Amf;
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 640, 360, false, &none()),
            EncoderBackend::Amf { dxgi_index: 0 }
        );

        s.proxy_encoder = EncoderPreference::Cpu;
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 640, 360, true, &none()),
            EncoderBackend::Cpu,
            "proxy uses its own setting"
        );
    }

    #[test]
    fn auto_encoder_drops_to_cpu_when_slower_than_the_same_codecs_cpu_encoder() {
        let mut device = amd(0);
        device.encode[0].speed_x = Some(0.5);
        let caps = Capabilities {
            cpu_encode: vec![cpu(VideoCodec::H264, Some(4.0))],
            ..caps_with(vec![device])
        };
        assert_eq!(
            choose_encoder_with(
                &caps,
                &settings(),
                VideoCodec::H264,
                640,
                360,
                false,
                &none()
            ),
            EncoderBackend::Cpu
        );
    }

    #[test]
    fn hardware_hevc_is_compared_with_libx265_not_libx264() {
        // hevc_amf runs at 11x. libx264 is 4x and libx265 is 20x: against the same codec's encoder, HEVC is slower and
        // must use CPU. Comparing with libx264 would wrongly keep AMF.
        let caps = Capabilities {
            cpu_encode: vec![
                cpu(VideoCodec::H264, Some(4.0)),
                cpu(VideoCodec::Hevc, Some(20.0)),
            ],
            ..caps_with(vec![amd(0)])
        };
        assert_eq!(
            choose_encoder_with(
                &caps,
                &settings(),
                VideoCodec::Hevc,
                640,
                360,
                false,
                &none()
            ),
            EncoderBackend::Cpu
        );
        // h264_amf at 18x beats libx264 at 4x, so it stays on hardware.
        assert_eq!(
            choose_encoder_with(
                &caps,
                &settings(),
                VideoCodec::H264,
                640,
                360,
                false,
                &none()
            ),
            EncoderBackend::Amf { dxgi_index: 0 }
        );
    }

    #[test]
    fn nvenc_needs_a_cuda_index() {
        let mut device = nvidia(0, None);
        device.encode[0].min_width = Some(256);
        let caps = caps_with(vec![device]);
        assert_eq!(
            choose_encoder_with(
                &caps,
                &settings(),
                VideoCodec::H264,
                640,
                360,
                false,
                &none()
            ),
            EncoderBackend::Cpu
        );
        let with = caps_with(vec![nvidia(0, Some(2))]);
        assert_eq!(
            choose_encoder_with(
                &with,
                &settings(),
                VideoCodec::H264,
                640,
                360,
                false,
                &none()
            ),
            EncoderBackend::Nvenc { cuda_index: 2 }
        );
    }

    #[test]
    fn amf_preference_on_nvidia_gets_cpu_not_another_vendor_path() {
        let caps = caps_with(vec![nvidia(0, Some(0))]);
        let mut s = settings();
        s.decode = DecodePreference::Amf;
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "h264", 8, false, &none()),
            DecodeChain::CPU
        );
    }

    #[test]
    fn effects_use_the_dxgi_list_even_without_probes() {
        // No probe results at all: the first hardware adapter in DXGI order still draws the effects.
        let gpus = vec![
            software_gpu(),
            gpu_device(&amd(1)),
            gpu_device(&nvidia(2, Some(0))),
        ];
        assert_eq!(
            choose_effects(&settings(), &gpus),
            EffectsChoice::Gpu(key(0x1002, 0x7550, 0, "AMD Radeon RX 9060 XT"))
        );
    }

    #[test]
    fn effects_fall_back_to_software_only_when_the_settings_or_hardware_say_so() {
        let gpus = vec![software_gpu(), gpu_device(&amd(1))];
        let mut off = settings();
        off.enabled = false;
        assert_eq!(choose_effects(&off, &gpus), EffectsChoice::Software);

        let mut cpu_effects = settings();
        cpu_effects.effects = EffectsRenderer::Cpu;
        assert_eq!(choose_effects(&cpu_effects, &gpus), EffectsChoice::Software);

        let mut software_pref = settings();
        software_pref.gpu = GpuPreference::Software;
        assert_eq!(
            choose_effects(&software_pref, &gpus),
            EffectsChoice::Software
        );

        let only_software = vec![software_gpu()];
        assert_eq!(
            choose_effects(&settings(), &only_software),
            EffectsChoice::Software
        );
        assert_eq!(choose_effects(&settings(), &[]), EffectsChoice::Software);
    }

    #[test]
    fn effects_follow_a_specific_adapter_and_software_when_it_is_absent() {
        let gpus = vec![
            software_gpu(),
            gpu_device(&amd(1)),
            gpu_device(&nvidia(2, Some(0))),
        ];
        let mut pinned = settings();
        pinned.gpu = GpuPreference::Specific {
            key: key(0x10DE, 0x2684, 0, "NVIDIA GeForce RTX 4090"),
        };
        assert_eq!(
            choose_effects(&pinned, &gpus),
            EffectsChoice::Gpu(key(0x10DE, 0x2684, 0, "NVIDIA GeForce RTX 4090"))
        );

        pinned.gpu = GpuPreference::Specific {
            key: key(0x8086, 0x1234, 0, "unplugged"),
        };
        assert_eq!(choose_effects(&pinned, &gpus), EffectsChoice::Software);
    }

    fn software_gpu() -> GpuDevice {
        device(
            GpuVendor::Microsoft,
            key(0x1414, 0x008C, 0, "Microsoft Basic Render Driver"),
            0,
            None,
            true,
        )
    }

    #[test]
    fn fault_injection_parses_values() {
        assert_eq!(ForcedFailures::parse(None), ForcedFailures::default());
        assert_eq!(ForcedFailures::parse(Some("  ")), ForcedFailures::default());
        let parsed = ForcedFailures::parse(Some("Decode, AMF"));
        assert!(parsed.decode);
        assert!(parsed.forces(Kind::Decode, "anything"));
        assert!(
            parsed.forces(Kind::Encode, "amf"),
            "backend names match across kinds"
        );
        assert!(!parsed.forces(Kind::Encode, "nvenc"));
        assert!(!parsed.scale);
    }

    #[test]
    fn fault_all_fails_every_ffmpeg_path_but_not_the_renderer() {
        let caps = caps_with(vec![amd(0)]);
        let s = settings();
        let all = ForcedFailures::parse(Some("all"));
        assert!(choose_gpu_with(&caps, &s, &all).is_none());
        assert_eq!(
            choose_decode_chain_with(&caps, &s, "h264", 8, false, &all),
            DecodeChain::CPU
        );
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 640, 360, false, &all),
            EncoderBackend::Cpu
        );
        // The effects renderer ignores FFmpeg fault injection.
        assert!(matches!(
            choose_effects(&s, &[gpu_device(&amd(0))]),
            EffectsChoice::Gpu(_)
        ));
    }

    #[test]
    fn fault_backend_name_forces_only_that_backend() {
        let caps = caps_with(vec![amd(0)]);
        let s = settings();
        let amf_down = ForcedFailures::parse(Some("amf"));
        // AMF decode and AMF scale are failed, so the chain falls back to d3d11va without a GPU scaler.
        let chain = choose_decode_chain_with(&caps, &s, "hevc", 10, false, &amf_down);
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::D3d11va { dxgi_index: 0 }
        );
        assert_eq!(chain.scaler, GpuScaler::None);
        // AMF encode is failed too.
        assert_eq!(
            choose_encoder_with(&caps, &s, VideoCodec::H264, 640, 360, false, &amf_down),
            EncoderBackend::Cpu
        );

        let d3d_down = ForcedFailures::parse(Some("d3d11va"));
        let chain = choose_decode_chain_with(&caps, &s, "h264", 8, false, &d3d_down);
        assert_eq!(
            chain.backend,
            loopcut_core::DecodeBackend::Amf { dxgi_index: 0 },
            "native decode is still healthy"
        );
    }

    #[test]
    fn describe_lists_devices_and_warnings() {
        let mut caps = caps_with(vec![software(), amd(0), nvidia(1, None)]);
        caps.warnings.push("example warning".to_owned());
        let report = describe(&caps);
        assert!(report.starts_with("FFmpeg n9.0.2"), "{report}");
        assert!(report.contains("AMD Radeon RX 9060 XT"), "{report}");
        assert!(report.contains("hevc 10-bit amf OK"), "{report}");
        assert!(report.contains("hevc amf min 640x360 OK 11.0x"), "{report}");
        assert!(report.contains("h264 vpp_amf after amf OK"), "{report}");
        assert!(report.contains("FAILED"), "{report}");
        assert!(report.contains("(software; no FFmpeg probes)"), "{report}");
        assert!(report.contains("- example warning"), "{report}");
    }
}

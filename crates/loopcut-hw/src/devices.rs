//! GPU enumeration through DXGI, in the same order FFmpeg's `d3d11va=dx:N` selector uses.
//!
//! Each adapter gets a [`GpuKey`] (PCI vendor and device IDs plus an ordinal among identical cards) that survives
//! restarts. The DXGI LUID is kept separately because it changes every boot, and it is what matches an NVIDIA adapter
//! to its CUDA ordinal.
//!
//! AMF cannot select an adapter: its decoders and encoders run on the default AMD adapter. LoopCut assumes that default
//! is the first non-software AMD adapter in DXGI order. See [`amf_default_adapter`].

use loopcut_core::GpuKey;
use loopcut_core::hw::GpuVendor;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::cuda;

/// One graphics adapter as LoopCut sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GpuDevice {
    /// Stable identity across launches. Shown in settings and stored in project and settings files.
    pub key: GpuKey,
    pub vendor: GpuVendor,
    /// Index in DXGI `EnumAdapters1` order: the index of `-init_hw_device d3d11va=dx:N` and of QSV's child device.
    pub dxgi_index: u32,
    /// Adapter LUID as `(HighPart << 32) | LowPart`. It changes every boot, so it is never part of [`GpuKey`].
    pub luid: u64,
    /// Dedicated video memory in MiB.
    pub dedicated_vram_mb: u64,
    /// User-mode driver version as `a.b.c.d`, or `None` when DXGI cannot report it.
    pub driver_version: Option<String>,
    /// The Microsoft software rasterizer (WARP). It has no FFmpeg probes.
    pub is_software: bool,
    /// CUDA ordinal for an NVIDIA adapter whose LUID matches a CUDA device.
    pub cuda_index: Option<u32>,
}

/// Upper bound on the adapters enumerated. Real machines have a handful.
const MAX_ADAPTERS: u32 = 32;

/// One adapter as DXGI reports it, before LoopCut's identity rules are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawAdapter {
    index: u32,
    vendor_id: u32,
    device_id: u32,
    name: String,
    dedicated_vram_bytes: u64,
    luid: u64,
    is_software: bool,
    driver_version: Option<String>,
}

/// Lists the adapters in DXGI order with their identity, driver version and CUDA ordinal.
///
/// Returns an empty list on non-Windows targets or when DXGI is unavailable. A software adapter that is listed twice
/// is kept once. CUDA is queried only when an NVIDIA adapter is present, so other machines never load `nvcuda.dll`.
pub fn enumerate_gpus() -> Vec<GpuDevice> {
    let raw = platform::enumerate_adapters();
    let has_nvidia = raw.iter().any(|adapter| {
        !adapter.is_software && GpuVendor::from_pci_id(adapter.vendor_id) == GpuVendor::Nvidia
    });
    let cuda_devices = if has_nvidia {
        cuda::cuda_luids()
    } else {
        Vec::new()
    };
    build_devices(raw, &cuda_devices)
}

/// The adapter AMF uses: the first non-software AMD adapter in DXGI order, or `None` when there is no AMD hardware.
///
/// AMF has no adapter selection. Its decode, scale and encode run on the default AMD adapter, which this function
/// identifies by assumption, so the probe tests AMF on that adapter only. The assumption is documented here and in
/// the probe module; it must be revisited if AMF gains adapter selection.
pub fn amf_default_adapter(devices: &[GpuDevice]) -> Option<&GpuDevice> {
    devices
        .iter()
        .filter(|device| !device.is_software && device.vendor == GpuVendor::Amd)
        .min_by_key(|device| device.dxgi_index)
}

/// Builds the device list from DXGI adapters in enumeration order and the CUDA `(ordinal, LUID)` pairs.
///
/// Identity rules:
/// * The ordinal counts earlier adapters with the same PCI vendor and device IDs, so two identical cards get 0 and 1.
/// * A software adapter with the same name as an earlier software adapter is dropped.
/// * An NVIDIA adapter gets the CUDA ordinal whose LUID equals its own. Position in either list does not matter.
fn build_devices(raw: Vec<RawAdapter>, cuda: &[(u32, u64)]) -> Vec<GpuDevice> {
    let mut devices: Vec<GpuDevice> = Vec::new();
    for adapter in raw {
        if adapter.is_software
            && devices
                .iter()
                .any(|kept| kept.is_software && kept.key.name == adapter.name)
        {
            continue;
        }
        let ordinal = devices
            .iter()
            .filter(|kept| {
                kept.key.vendor_id == adapter.vendor_id && kept.key.device_id == adapter.device_id
            })
            .count();
        devices.push(GpuDevice {
            key: GpuKey {
                vendor_id: adapter.vendor_id,
                device_id: adapter.device_id,
                ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                name: adapter.name,
            },
            vendor: GpuVendor::from_pci_id(adapter.vendor_id),
            dxgi_index: adapter.index,
            luid: adapter.luid,
            dedicated_vram_mb: adapter.dedicated_vram_bytes / (1024 * 1024),
            driver_version: adapter.driver_version,
            is_software: adapter.is_software,
            cuda_index: None,
        });
    }

    for device in devices
        .iter_mut()
        .filter(|device| device.vendor == GpuVendor::Nvidia && !device.is_software)
    {
        device.cuda_index = cuda::match_cuda_to_dxgi(cuda, device.luid);
    }
    devices
}

/// Formats a DXGI user-mode driver version (`CheckInterfaceSupport`'s 64-bit `LARGE_INTEGER`) as `a.b.c.d`.
///
/// The high word holds `a` (upper half) and `b` (lower half); the low word holds `c` and `d`. Each part is 16 bits.
pub fn format_umd_version(value: i64) -> String {
    let bits = value as u64;
    format!(
        "{}.{}.{}.{}",
        (bits >> 48) & 0xffff,
        (bits >> 32) & 0xffff,
        (bits >> 16) & 0xffff,
        bits & 0xffff
    )
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIDevice, IDXGIFactory1,
    };
    use windows::core::Interface;

    use super::{MAX_ADAPTERS, RawAdapter, format_umd_version};

    /// Walks `EnumAdapters1` until it reports the end of the list.
    pub(super) fn enumerate_adapters() -> Vec<RawAdapter> {
        // SAFETY: CreateDXGIFactory1 takes no pointers. It returns an owned COM reference that is released on drop.
        let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
            Ok(factory) => factory,
            Err(err) => {
                tracing::warn!(%err, "DXGI factory unavailable; no GPUs enumerated");
                return Vec::new();
            }
        };

        let mut adapters = Vec::new();
        for index in 0..MAX_ADAPTERS {
            // SAFETY: `factory` is a live factory and `index` is a plain integer. The error for an index past the end
            // (DXGI_ERROR_NOT_FOUND) is the normal way to end the loop.
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            // SAFETY: `adapter` is live. GetDesc1 writes only into the returned struct.
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                tracing::warn!(index, "GetDesc1 failed; adapter skipped");
                continue;
            };
            let iid = <IDXGIDevice as Interface>::IID;
            // SAFETY: `iid` outlives the call. CheckInterfaceSupport only writes the 64-bit version it returns.
            let driver_version = unsafe { adapter.CheckInterfaceSupport(&iid) }
                .ok()
                .map(format_umd_version);

            let name_len = desc
                .Description
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(desc.Description.len());
            let flags = desc.Flags;
            let software_flag = DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32;
            adapters.push(RawAdapter {
                index,
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                name: String::from_utf16_lossy(&desc.Description[..name_len]),
                dedicated_vram_bytes: desc.DedicatedVideoMemory as u64,
                luid: (u64::from(desc.AdapterLuid.HighPart as u32) << 32)
                    | u64::from(desc.AdapterLuid.LowPart),
                is_software: (flags & software_flag) != 0,
                driver_version,
            });
        }
        adapters
    }
}

#[cfg(not(windows))]
mod platform {
    use super::RawAdapter;

    /// DXGI exists only on Windows, so there are no adapters elsewhere.
    pub(super) fn enumerate_adapters() -> Vec<RawAdapter> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(
        index: u32,
        vendor_id: u32,
        device_id: u32,
        name: &str,
        luid: u64,
        software: bool,
    ) -> RawAdapter {
        RawAdapter {
            index,
            vendor_id,
            device_id,
            name: name.to_owned(),
            dedicated_vram_bytes: 16 << 30,
            luid,
            is_software: software,
            driver_version: Some("32.0.31041.1004".to_owned()),
        }
    }

    const AMD_VENDOR: u32 = 0x1002;
    const AMD_NAVI48: u32 = 0x7550;
    const NVIDIA_VENDOR: u32 = 0x10DE;
    const NVIDIA_AD102: u32 = 0x2684;
    const WARP_VENDOR: u32 = 0x1414;
    const WARP_DEVICE: u32 = 0x008C;

    fn warp(index: u32) -> RawAdapter {
        raw(
            index,
            WARP_VENDOR,
            WARP_DEVICE,
            "Microsoft Basic Render Driver",
            0x9,
            true,
        )
    }

    #[test]
    fn umd_version_splits_into_four_16_bit_fields() {
        // 32.0.31041.1004 as the AMD driver reports it: high word 0x0020_0000, low word 0x7941_03EC.
        let value = (0x0020_0000_i64 << 32) | 0x7941_03EC_i64;
        assert_eq!(format_umd_version(value), "32.0.31041.1004");
        assert_eq!(format_umd_version(0), "0.0.0.0");
    }

    #[test]
    fn identical_cards_get_ordinals_in_dxgi_order() {
        let devices = build_devices(
            vec![
                raw(
                    0,
                    AMD_VENDOR,
                    AMD_NAVI48,
                    "AMD Radeon RX 9060 XT",
                    10,
                    false,
                ),
                raw(
                    1,
                    AMD_VENDOR,
                    AMD_NAVI48,
                    "AMD Radeon RX 9060 XT",
                    11,
                    false,
                ),
            ],
            &[],
        );
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].key.ordinal, 0);
        assert_eq!(devices[1].key.ordinal, 1);
        assert_ne!(
            devices[0].key, devices[1].key,
            "identical cards need distinct keys"
        );
        assert_eq!(devices[1].dxgi_index, 1);
    }

    #[test]
    fn duplicate_software_adapters_are_kept_once() {
        let devices = build_devices(
            vec![
                raw(
                    0,
                    AMD_VENDOR,
                    AMD_NAVI48,
                    "AMD Radeon RX 9060 XT",
                    10,
                    false,
                ),
                warp(1),
                warp(2),
            ],
            &[],
        );
        let software: Vec<&GpuDevice> = devices.iter().filter(|d| d.is_software).collect();
        assert_eq!(software.len(), 1, "WARP listed twice must be deduplicated");
        assert_eq!(software[0].dxgi_index, 1, "the first WARP entry is kept");
        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn cuda_index_follows_the_luid_not_the_position() {
        let devices = build_devices(
            vec![
                raw(
                    0,
                    NVIDIA_VENDOR,
                    NVIDIA_AD102,
                    "NVIDIA GeForce RTX 4090",
                    0x100,
                    false,
                ),
                raw(
                    1,
                    NVIDIA_VENDOR,
                    NVIDIA_AD102,
                    "NVIDIA GeForce RTX 4090",
                    0x200,
                    false,
                ),
            ],
            // CUDA enumerates the cards in the opposite order.
            &[(0, 0x200), (1, 0x100)],
        );
        assert_eq!(devices[0].cuda_index, Some(1));
        assert_eq!(devices[1].cuda_index, Some(0));
    }

    #[test]
    fn an_nvidia_adapter_without_a_matching_luid_has_no_cuda_index() {
        let devices = build_devices(
            vec![raw(
                0,
                NVIDIA_VENDOR,
                NVIDIA_AD102,
                "NVIDIA GeForce RTX 4090",
                0x100,
                false,
            )],
            &[(0, 0x999)],
        );
        assert_eq!(devices[0].cuda_index, None);
    }

    #[test]
    fn cuda_indices_only_apply_to_nvidia_adapters() {
        let devices = build_devices(
            vec![raw(
                0,
                AMD_VENDOR,
                AMD_NAVI48,
                "AMD Radeon RX 9060 XT",
                0x100,
                false,
            )],
            &[(0, 0x100)],
        );
        assert_eq!(
            devices[0].cuda_index, None,
            "a LUID match on an AMD card is ignored"
        );
    }

    #[test]
    fn amf_default_is_the_first_amd_adapter_in_dxgi_order() {
        // Laptop-style layout: software first, then an APU, then a discrete card. AMF uses the APU (the first AMD).
        let devices = build_devices(
            vec![
                warp(0),
                raw(1, AMD_VENDOR, 0x1900, "AMD Radeon 890M", 0x10, false),
                raw(
                    2,
                    AMD_VENDOR,
                    AMD_NAVI48,
                    "AMD Radeon RX 9060 XT",
                    0x20,
                    false,
                ),
            ],
            &[],
        );
        let default = amf_default_adapter(&devices).expect("an AMD adapter");
        assert_eq!(default.dxgi_index, 1);
        assert_eq!(default.key.name, "AMD Radeon 890M");
    }

    #[test]
    fn amf_has_no_default_without_amd_hardware() {
        let devices = build_devices(
            vec![
                warp(0),
                raw(
                    1,
                    NVIDIA_VENDOR,
                    NVIDIA_AD102,
                    "NVIDIA GeForce RTX 4090",
                    0x100,
                    false,
                ),
            ],
            &[],
        );
        assert!(amf_default_adapter(&devices).is_none());
    }

    #[test]
    fn enumerate_lists_software_once_and_keys_are_unique() {
        // Whatever this machine reports: a software adapter appears at most once and every key is unique.
        let devices = enumerate_gpus();
        let software = devices.iter().filter(|d| d.is_software).count();
        assert!(software <= 1, "software adapter listed {software} times");
        for (index, device) in devices.iter().enumerate() {
            let dup = devices[..index].iter().any(|other| other.key == device.key);
            assert!(!dup, "duplicate GPU key {:?}", device.key);
        }
    }
}

//! The on-disk capability cache.
//!
//! A cache file holds one [`Capabilities`] record and the key it was probed under. The key covers the FFmpeg version,
//! the probe logic version, whether the benchmark ran, every adapter's identity, driver version, DXGI index and CUDA
//! ordinal, and the record schema. A probe result is reused only when the key matches, so a driver or FFmpeg update, a
//! changed probe, or a reordered adapter list forces a new probe.
//!
//! Adapter LUIDs are left out of the key because they change on every boot. A cache hit is therefore re-stamped with
//! the live adapters: each cached device takes the live LUID and CUDA ordinal, and cached devices that are no longer
//! present are dropped.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::caps::{Capabilities, SCHEMA_VERSION};
use crate::devices::GpuDevice;
use crate::probe::PROBE_LOGIC_VERSION;

/// Stable 64-bit FNV-1a hash. `DefaultHasher` is not stable across Rust releases, so it cannot back an on-disk key.
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// Cache key for a probe: 16 hex digits over the FFmpeg version, the schema, the probe logic version, the benchmark
/// flag, and each adapter's key, driver and indices.
///
/// The canonical text is one line per fact, so a change in any of them changes the key.
pub fn cache_key(ffmpeg_version: &str, gpus: &[GpuDevice], benchmark: bool) -> String {
    let mut canonical = format!(
        "schema={SCHEMA_VERSION}\nprobe_logic={PROBE_LOGIC_VERSION}\nffmpeg={ffmpeg_version}\nbenchmark={benchmark}\n"
    );
    for gpu in gpus {
        // Writing to a String cannot fail, so the result is ignored.
        let _ = writeln!(
            canonical,
            "gpu vendor={:04x} device={:04x} ordinal={} name={} software={} driver={} dxgi={} cuda={}",
            gpu.key.vendor_id,
            gpu.key.device_id,
            gpu.key.ordinal,
            gpu.key.name,
            gpu.is_software,
            gpu.driver_version.as_deref().unwrap_or("none"),
            gpu.dxgi_index,
            gpu.cuda_index
                .map_or_else(|| "none".to_owned(), |index| index.to_string()),
        );
    }
    format!("{:016x}", fnv1a(canonical.as_bytes()))
}

/// Re-stamps a cached record with the live adapters.
///
/// Each cached device is matched by its [`GpuKey`](loopcut_core::GpuKey). A match takes the live device record, so the
/// LUID and CUDA ordinal are the current ones. A cached device with no live match is dropped. Live devices absent from
/// the cache are not added, because the cache key already requires the same adapter list.
pub fn restamp(mut caps: Capabilities, live: &[GpuDevice]) -> Capabilities {
    caps.devices.retain_mut(|entry| {
        let Some(current) = live.iter().find(|device| device.key == entry.device.key) else {
            return false;
        };
        entry.device = current.clone();
        true
    });
    caps
}

/// Borrowed form used when writing, so saving does not clone the record.
#[derive(Serialize)]
struct StoredRef<'a> {
    key: &'a str,
    capabilities: &'a Capabilities,
}

/// Owned form used when reading.
#[derive(Deserialize)]
struct StoredOwned {
    key: String,
    capabilities: Capabilities,
}

/// Loads the cached record at `path` when its key equals `key` and its schema is current, then re-stamps it with the
/// `live` adapters (see [`restamp`]).
///
/// Missing, unreadable, corrupt or stale files all return `None`, so the caller simply probes again.
pub fn load(path: &Path, key: &str, live: &[GpuDevice]) -> Option<Capabilities> {
    let text = fs::read_to_string(path).ok()?;
    let stored: StoredOwned = serde_json::from_str(&text).ok()?;
    (stored.key == key && stored.capabilities.schema == SCHEMA_VERSION)
        .then(|| restamp(stored.capabilities, live))
}

/// Writes `caps` to `path` under `key`, atomically: the data goes to a temporary file that is renamed over the target.
///
/// # Errors
///
/// Any I/O error while creating the folder, writing or renaming the file. A failed write leaves no temporary file.
pub fn save(path: &Path, key: &str, caps: &Capabilities) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(&StoredRef {
        key,
        capabilities: caps,
    })
    .map_err(io::Error::other)?;

    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let written = (|| -> io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use loopcut_core::GpuKey;
    use loopcut_core::hw::GpuVendor;

    use super::*;
    use crate::caps::{CapStatus, DecodeCap, DeviceCaps, EncodeCap};
    use loopcut_core::VideoCodec;

    fn gpu(driver: &str) -> GpuDevice {
        GpuDevice {
            key: GpuKey {
                vendor_id: 0x1002,
                device_id: 0x7550,
                ordinal: 0,
                name: "AMD Radeon RX 9060 XT".to_owned(),
            },
            vendor: GpuVendor::Amd,
            dxgi_index: 0,
            luid: 0x1234,
            dedicated_vram_mb: 16_368,
            driver_version: Some(driver.to_owned()),
            is_software: false,
            cuda_index: None,
        }
    }

    fn sample(gpus: &[GpuDevice], ffmpeg: &str) -> Capabilities {
        Capabilities {
            schema: SCHEMA_VERSION,
            ffmpeg_version: ffmpeg.to_owned(),
            probed_at_unix: 1_760_000_000,
            devices: gpus
                .iter()
                .map(|device| DeviceCaps {
                    device: device.clone(),
                    decode: vec![DecodeCap {
                        codec: VideoCodec::H264,
                        ten_bit: false,
                        backend: "amf".to_owned(),
                        status: CapStatus::Ok,
                        speed_x: None,
                        error: None,
                    }],
                    scale: Vec::new(),
                    encode: vec![EncodeCap {
                        codec: VideoCodec::Hevc,
                        backend: "amf".to_owned(),
                        status: CapStatus::Ok,
                        min_width: Some(640),
                        min_height: Some(360),
                        speed_x: Some(9.5),
                        error: None,
                    }],
                })
                .collect(),
            cpu_encode: Vec::new(),
            warnings: vec!["example".to_owned()],
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("hw-caps.json");
        let gpus = [gpu("32.0.31041.1004")];
        let key = cache_key("n9.0.2", &gpus, false);
        let caps = sample(&gpus, "n9.0.2");

        save(&path, &key, &caps).expect("save");
        assert_eq!(load(&path, &key, &gpus), Some(caps));
        assert!(
            !dir.path()
                .join("nested")
                .join(format!("hw-caps.tmp-{}", std::process::id()))
                .exists(),
            "temporary file must be renamed away"
        );
    }

    #[test]
    fn load_rejects_a_different_key_or_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hw-caps.json");
        let gpus = [gpu("32.0.1.1")];
        let key = cache_key("n9.0.2", &gpus, false);
        save(&path, &key, &sample(&gpus, "n9.0.2")).expect("save");

        assert!(load(&path, "0000000000000000", &gpus).is_none());
        assert!(load(&dir.path().join("absent.json"), &key, &gpus).is_none());
    }

    #[test]
    fn load_rejects_corrupt_or_stale_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hw-caps.json");
        fs::write(&path, b"{ not json").expect("write");
        assert!(load(&path, "anything", &[]).is_none());

        let gpus = [gpu("32.0.1.1")];
        let key = cache_key("n9.0.2", &gpus, false);
        let mut stale = sample(&gpus, "n9.0.2");
        stale.schema = SCHEMA_VERSION + 1;
        save(&path, &key, &stale).expect("save");
        assert!(
            load(&path, &key, &gpus).is_none(),
            "newer schema must not load"
        );
    }

    #[test]
    fn key_tracks_driver_ffmpeg_benchmark_and_indices_but_not_luid() {
        let base = [gpu("32.0.31041.1004")];
        let key = cache_key("n9.0.2", &base, false);
        assert_eq!(key.len(), 16);
        assert_eq!(key, cache_key("n9.0.2", &base, false), "key must be stable");

        let newer_driver = [gpu("32.0.31099.1")];
        assert_ne!(key, cache_key("n9.0.2", &newer_driver, false));
        assert_ne!(key, cache_key("n9.1.0", &base, false));
        assert_ne!(
            key,
            cache_key("n9.0.2", &base, true),
            "a benchmark run has its own cache entry"
        );

        // The LUID changes on every boot, so it must not invalidate the cache.
        let mut rebooted = base.clone();
        rebooted[0].luid = 0xFFFF_0000;
        assert_eq!(
            key,
            cache_key("n9.0.2", &rebooted, false),
            "LUID is not part of the key"
        );

        // A reordered adapter list changes the DXGI and CUDA indices the cached probe used, so it must re-probe.
        let mut reordered = base.clone();
        reordered[0].dxgi_index = 3;
        assert_ne!(key, cache_key("n9.0.2", &reordered, false));
        let mut cuda_moved = base.clone();
        cuda_moved[0].cuda_index = Some(1);
        assert_ne!(key, cache_key("n9.0.2", &cuda_moved, false));
    }

    #[test]
    fn a_cache_hit_takes_the_live_luid_and_cuda_index() {
        let cached_gpus = [gpu("32.0.31041.1004")];
        let caps = sample(&cached_gpus, "n9.0.2");

        let mut live = cached_gpus.to_vec();
        live[0].luid = 0xABCD_0001;
        live[0].cuda_index = Some(2);
        let restamped = restamp(caps, &live);
        assert_eq!(restamped.devices.len(), 1);
        assert_eq!(
            restamped.devices[0].device.luid, 0xABCD_0001,
            "LUID comes from the live adapter"
        );
        assert_eq!(
            restamped.devices[0].device.cuda_index,
            Some(2),
            "CUDA ordinal comes from the live adapter"
        );
        assert_eq!(
            restamped.devices[0].decode.len(),
            1,
            "probe results are kept"
        );
    }

    #[test]
    fn a_cache_hit_drops_devices_that_are_no_longer_present() {
        let mut first = gpu("32.0.31041.1004");
        first.key.ordinal = 0;
        let mut second = gpu("32.0.31041.1004");
        second.key.ordinal = 1;
        second.dxgi_index = 1;
        let caps = sample(&[first.clone(), second.clone()], "n9.0.2");

        // The second card has gone: only the first is live.
        let restamped = restamp(caps, std::slice::from_ref(&first));
        assert_eq!(restamped.devices.len(), 1);
        assert_eq!(restamped.devices[0].device.key, first.key);
    }

    #[test]
    fn restamp_does_not_add_live_devices_missing_from_the_cache() {
        let cached = [gpu("32.0.31041.1004")];
        let caps = sample(&cached, "n9.0.2");
        let mut extra = gpu("32.0.31041.1004");
        extra.key.ordinal = 1;
        let restamped = restamp(caps, &[cached[0].clone(), extra]);
        assert_eq!(restamped.devices.len(), 1);
    }
}

//! CUDA device discovery through the NVIDIA driver, used only to map an NVIDIA adapter to its CUDA ordinal.
//!
//! `nvcuda.dll` is loaded at run time, so LoopCut starts on machines without NVIDIA hardware. It is loaded by its full
//! path under `%SystemRoot%\System32` (never by a bare name, which would search the application folder and PATH), and
//! the library stays loaded for the rest of the process so the function pointers copied from it stay valid. The mapping
//! is by LUID: `cuDeviceGetLuid` returns the same 8 bytes DXGI reports as `AdapterLuid`. The matching is a pure
//! function so it can be tested without a GPU.

#[cfg(windows)]
use std::ffi::{c_char, c_int, c_uint};
#[cfg(windows)]
use std::sync::OnceLock;

#[cfg(windows)]
use libloading::Library;

/// `cuInit(unsigned int flags)`.
#[cfg(windows)]
type CuInit = unsafe extern "system" fn(c_uint) -> c_int;
/// `cuDeviceGetCount(int *count)`.
#[cfg(windows)]
type CuDeviceGetCount = unsafe extern "system" fn(*mut c_int) -> c_int;
/// `cuDeviceGet(CUdevice *device, int ordinal)`.
#[cfg(windows)]
type CuDeviceGet = unsafe extern "system" fn(*mut c_int, c_int) -> c_int;
/// `cuDeviceGetLuid(char luid[8], unsigned int *deviceNodeMask, CUdevice dev)`.
#[cfg(windows)]
type CuDeviceGetLuid = unsafe extern "system" fn(*mut c_char, *mut c_uint, c_int) -> c_int;

/// The driver library, loaded at most once per process and never unloaded.
#[cfg(windows)]
static NVCUDA: OnceLock<Option<Library>> = OnceLock::new();

/// Lists CUDA devices as `(ordinal, luid)` pairs.
///
/// Returns an empty list when `nvcuda.dll` is missing (no NVIDIA driver), when CUDA cannot initialize, or on non-Windows
/// targets.
pub fn cuda_luids() -> Vec<(u32, u64)> {
    #[cfg(windows)]
    {
        match driver_library() {
            Some(library) => match query_devices(library) {
                Ok(devices) => devices,
                Err(err) => {
                    tracing::debug!(%err, "CUDA device query failed");
                    Vec::new()
                }
            },
            None => Vec::new(),
        }
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Finds the CUDA ordinal whose LUID equals the DXGI adapter's LUID.
///
/// Returns `None` when no CUDA device has that LUID (for example an NVIDIA adapter that CUDA cannot use, or a machine
/// without the driver).
pub fn match_cuda_to_dxgi(cuda: &[(u32, u64)], dxgi_luid: u64) -> Option<u32> {
    cuda.iter()
        .find(|(_, luid)| *luid == dxgi_luid)
        .map(|(ordinal, _)| *ordinal)
}

/// Converts the 8-byte LUID that `cuDeviceGetLuid` writes into the 64-bit form DXGI uses.
///
/// Windows LUIDs are little-endian `{LowPart: u32, HighPart: i32}`, so the first four bytes are the low word.
pub fn luid_from_cuda_bytes(bytes: [u8; 8]) -> u64 {
    u64::from_le_bytes(bytes)
}

/// Full path of the NVIDIA driver library for a given `SystemRoot`, such as `C:\Windows`.
pub fn nvcuda_path(system_root: &str) -> String {
    format!(
        r"{}\System32\nvcuda.dll",
        system_root.trim_end_matches(['\\', '/'])
    )
}

/// The process-wide driver library, or `None` when `SystemRoot` is unset or the DLL cannot be loaded.
#[cfg(windows)]
fn driver_library() -> Option<&'static Library> {
    NVCUDA.get_or_init(load_driver).as_ref()
}

#[cfg(windows)]
fn load_driver() -> Option<Library> {
    let Ok(system_root) = std::env::var("SystemRoot") else {
        tracing::debug!("SystemRoot is not set; CUDA is unavailable");
        return None;
    };
    let path = nvcuda_path(&system_root);
    // SAFETY: nvcuda.dll is the NVIDIA driver's own user-mode library, loaded from its System32 path. Loading it runs
    // only its initializer. The library is stored in a static, so it is never unloaded while function pointers copied
    // from it are in use.
    match unsafe { Library::new(path.as_str()) } {
        Ok(library) => Some(library),
        Err(err) => {
            tracing::debug!(%err, path = %path, "nvcuda.dll not loadable; no CUDA devices");
            None
        }
    }
}

#[cfg(windows)]
fn query_devices(library: &Library) -> Result<Vec<(u32, u64)>, libloading::Error> {
    // SAFETY: each symbol is looked up by its documented name and the function types above match the CUDA driver
    // API (cuda.h). The library is 'static, so the copied function pointers stay valid.
    unsafe {
        let init = *library.get::<CuInit>(b"cuInit\0")?;
        let get_count = *library.get::<CuDeviceGetCount>(b"cuDeviceGetCount\0")?;
        let get_device = *library.get::<CuDeviceGet>(b"cuDeviceGet\0")?;
        let get_luid = *library.get::<CuDeviceGetLuid>(b"cuDeviceGetLuid\0")?;

        if init(0) != 0 {
            return Ok(Vec::new());
        }
        let mut count: c_int = 0;
        if get_count(&mut count) != 0 {
            return Ok(Vec::new());
        }

        let mut devices = Vec::new();
        for ordinal in 0..count {
            let mut device: c_int = 0;
            if get_device(&mut device, ordinal) != 0 {
                continue;
            }
            let mut luid = [0u8; 8];
            let mut node_mask: c_uint = 0;
            if get_luid(luid.as_mut_ptr().cast::<c_char>(), &mut node_mask, device) != 0 {
                continue;
            }
            if let Ok(ordinal) = u32::try_from(ordinal) {
                devices.push((ordinal, luid_from_cuda_bytes(luid)));
            }
        }
        Ok(devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_device_with_the_same_luid() {
        let cuda = [
            (0, 0x1111_2222_3333_4444_u64),
            (1, 0x0000_0000_0001_2345_u64),
        ];
        assert_eq!(match_cuda_to_dxgi(&cuda, 0x0000_0000_0001_2345), Some(1));
        assert_eq!(match_cuda_to_dxgi(&cuda, 0x1111_2222_3333_4444), Some(0));
        assert_eq!(match_cuda_to_dxgi(&cuda, 0xdead_beef), None);
        assert_eq!(match_cuda_to_dxgi(&[], 0x1), None);
    }

    #[test]
    fn luid_bytes_match_the_dxgi_layout() {
        // LowPart = 0x0001_2345, HighPart = 0x0000_0000 (the common case).
        let bytes = [0x45, 0x23, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(luid_from_cuda_bytes(bytes), 0x0001_2345);
        // HighPart = 0x0000_1000 lands in bits 32..64.
        let high = [0x01, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00];
        assert_eq!(luid_from_cuda_bytes(high), (0x1000_u64 << 32) | 1);
    }

    #[test]
    fn driver_is_loaded_from_system32_under_system_root() {
        assert_eq!(
            nvcuda_path(r"C:\Windows"),
            r"C:\Windows\System32\nvcuda.dll"
        );
        assert_eq!(
            nvcuda_path(r"C:\Windows\"),
            r"C:\Windows\System32\nvcuda.dll"
        );
        assert!(nvcuda_path(r"D:\Win").ends_with(r"\System32\nvcuda.dll"));
    }

    #[test]
    fn cuda_query_returns_without_panicking() {
        // The result depends on the machine: an NVIDIA box lists its GPUs, anything else gets an empty list.
        // The call must return either way, and a second call reuses the process-wide library handle.
        let first = cuda_luids();
        let second = cuda_luids();
        assert_eq!(first, second);
    }
}

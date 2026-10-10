//! `settings.json`: defaults, validation and tolerant loading.
//!
//! A missing file yields defaults. A file that does not parse is moved aside (`settings.json.corrupt`) so the user's
//! bytes survive, and defaults are used. Values outside their supported range are clamped instead of rejected, so a
//! hand-edited file never prevents the app from starting.

use std::path::Path;

use loopcut_media::framestore::default_ram_budget;

use crate::dto::AppSettings;
use crate::error::AppError;
use crate::storage;

const MIB: u64 = 1024 * 1024;
/// Total RAM assumed when the machine cannot report it.
const FALLBACK_TOTAL_RAM: u64 = 8 * 1024 * 1024 * 1024;

const MIN_RAM_BUDGET_MB: u64 = 64;
const MAX_RAM_BUDGET_MB: u64 = 64 * 1024;
const MIN_DISK_GB: f64 = 0.5;
const MAX_DISK_GB: f64 = 1024.0;
const MIN_RECENT: u32 = 1;
const MAX_RECENT: u32 = 100;

/// Defaults for this machine: the frame cache gets a quarter of RAM, capped at 2 GiB.
pub fn defaults_for_ram(total_ram_bytes: u64) -> AppSettings {
    let mut settings = AppSettings::default();
    settings.cache.ram_budget_mb =
        (default_ram_budget(total_ram_bytes) / MIB).max(MIN_RAM_BUDGET_MB);
    settings
}

/// Physical memory in bytes, or a conservative fallback when Windows cannot say.
pub fn total_ram_bytes() -> u64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX {
            dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: `status` is a valid, initialized MEMORYSTATUSEX whose `dwLength` names its size, as the API requires.
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() && status.ullTotalPhys > 0 {
            return status.ullTotalPhys;
        }
        FALLBACK_TOTAL_RAM
    }
    #[cfg(not(windows))]
    {
        FALLBACK_TOTAL_RAM
    }
}

/// Clamps every numeric value into its supported range and normalizes empty strings.
pub fn sanitize(mut settings: AppSettings) -> AppSettings {
    settings.cache.ram_budget_mb = settings
        .cache
        .ram_budget_mb
        .clamp(MIN_RAM_BUDGET_MB, MAX_RAM_BUDGET_MB);
    settings.cache.max_disk_gb = if settings.cache.max_disk_gb.is_finite() {
        settings.cache.max_disk_gb.clamp(MIN_DISK_GB, MAX_DISK_GB)
    } else {
        10.0
    };
    settings.general.recent_limit = settings.general.recent_limit.clamp(MIN_RECENT, MAX_RECENT);
    if settings
        .general
        .default_export_dir
        .as_deref()
        .is_some_and(|d| d.trim().is_empty())
    {
        settings.general.default_export_dir = None;
    }
    settings
}

/// Largest settings file that is read. Anything bigger is not a settings file LoopCut wrote.
pub const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// Loads settings from `path`, using `defaults` when the file is missing or unreadable. A corrupt file, and a file over
/// [`MAX_SETTINGS_BYTES`], is renamed to `<name>.corrupt-<ms>` before defaults are used. Returns the settings and a
/// warning when something was recovered from.
pub fn load(path: &Path, defaults: AppSettings) -> (AppSettings, Option<String>) {
    let bytes = match storage::read_capped(path, MAX_SETTINGS_BYTES) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (defaults, None),
        // `read_capped` reports an oversized file as invalid data, before reading any of it.
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            return set_aside(path, defaults, &err.to_string());
        }
        Err(err) => return (defaults, Some(format!("settings could not be read: {err}"))),
    };
    match serde_json::from_slice::<AppSettings>(&bytes) {
        Ok(settings) => (sanitize(settings), None),
        Err(err) => set_aside(path, defaults, &err.to_string()),
    }
}

/// Moves an unusable settings file aside and returns the defaults with the warning to show. The bytes are kept, so the
/// user can recover them.
fn set_aside(path: &Path, defaults: AppSettings, reason: &str) -> (AppSettings, Option<String>) {
    let aside = corrupt_copy_path(path);
    let moved = std::fs::rename(path, &aside).is_ok();
    let note = if moved {
        format!(
            "settings.json was unreadable ({reason}); it was kept as {}",
            aside.display()
        )
    } else {
        format!("settings.json was unreadable ({reason})")
    };
    (defaults, Some(note))
}

/// Where an unreadable settings file is kept: `settings.json.corrupt-<unix-ms>` beside it. The timestamp keeps every
/// earlier copy, so a second corruption never overwrites the first.
pub fn corrupt_copy_path(path: &Path) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_owned());
    path.with_file_name(format!("{name}.corrupt-{stamp}"))
}

/// Writes `settings` to `path` atomically, after sanitizing.
pub fn save(path: &Path, settings: &AppSettings) -> Result<(), AppError> {
    let json = serde_json::to_vec_pretty(&sanitize(settings.clone())).map_err(|err| {
        AppError::new(
            crate::error::code::IO,
            format!("settings could not be encoded: {err}"),
        )
    })?;
    storage::atomic_write(path, &json)
        .map_err(|err| crate::error::io_error(&err, "settings could not be saved"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults_without_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let defaults = defaults_for_ram(8 * 1024 * MIB);
        let (loaded, warning) = load(&dir.path().join("settings.json"), defaults.clone());
        assert_eq!(loaded, defaults);
        assert!(warning.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let mut settings = AppSettings::default();
        settings.general.recent_limit = 25;
        settings.cache.max_disk_gb = 4.5;
        settings.updates.skipped_version = Some("0.0.2".into());
        save(&path, &settings).expect("save");
        let (loaded, warning) = load(&path, AppSettings::default());
        assert!(warning.is_none());
        assert_eq!(loaded, settings);
    }

    #[test]
    fn corrupt_file_is_kept_aside_and_defaults_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"{ not json").expect("seed");
        let (loaded, warning) = load(&path, AppSettings::default());
        assert_eq!(loaded, AppSettings::default());
        assert!(warning.is_some());
        assert!(!path.exists());
        let kept: Vec<String> = std::fs::read_dir(dir.path())
            .expect("list")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("settings.json.corrupt-"))
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "the damaged bytes are kept under a timestamped name"
        );
        assert_eq!(
            std::fs::read(dir.path().join(&kept[0])).expect("kept copy"),
            b"{ not json"
        );
    }

    #[test]
    fn an_oversized_settings_file_is_kept_aside_like_a_corrupt_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let oversized = vec![b' '; (MAX_SETTINGS_BYTES + 1) as usize];
        std::fs::write(&path, &oversized).expect("seed");
        let (loaded, warning) = load(&path, AppSettings::default());
        assert_eq!(loaded, AppSettings::default());
        let note = warning.expect("a warning is recorded");
        assert!(note.contains("kept as"), "{note}");
        assert!(!path.exists(), "the oversized file is moved aside");
        let kept: Vec<String> = std::fs::read_dir(dir.path())
            .expect("list")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("settings.json.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::metadata(dir.path().join(&kept[0]))
                .expect("kept copy")
                .len(),
            MAX_SETTINGS_BYTES + 1,
            "the bytes are kept, not truncated"
        );
    }

    #[test]
    fn a_second_corrupt_file_never_overwrites_the_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let first = corrupt_copy_path(&path);
        std::fs::write(&first, b"first").expect("seed");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = corrupt_copy_path(&path);
        assert_ne!(first, second);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut settings = AppSettings::default();
        settings.cache.ram_budget_mb = 1;
        settings.cache.max_disk_gb = f64::NAN;
        settings.general.recent_limit = 10_000;
        settings.general.default_export_dir = Some("   ".into());
        let clean = sanitize(settings);
        assert_eq!(clean.cache.ram_budget_mb, MIN_RAM_BUDGET_MB);
        assert_eq!(clean.cache.max_disk_gb, 10.0);
        assert_eq!(clean.general.recent_limit, MAX_RECENT);
        assert_eq!(clean.general.default_export_dir, None);

        let mut big = AppSettings::default();
        big.cache.max_disk_gb = 1e9;
        assert_eq!(sanitize(big).cache.max_disk_gb, MAX_DISK_GB);
    }

    #[test]
    fn default_ram_budget_follows_memory_size() {
        assert_eq!(defaults_for_ram(4 * 1024 * MIB).cache.ram_budget_mb, 1024);
        assert_eq!(defaults_for_ram(64 * 1024 * MIB).cache.ram_budget_mb, 2048);
        assert_eq!(defaults_for_ram(0).cache.ram_budget_mb, MIN_RAM_BUDGET_MB);
    }
}

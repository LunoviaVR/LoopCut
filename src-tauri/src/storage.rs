//! File helpers shared by the commands: atomic writes, size-capped reads, cache walking and timestamps.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::tokens::random_hex_suffix;

/// Writes `bytes` to `path` atomically: a temporary file in the same folder is flushed to disk and then renamed over
/// the target. A crash leaves either the old file or the new one, never a truncated mix.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent folder"))?;
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut temp_name = name.to_os_string();
    temp_name.push(format!(".tmp-{}", random_hex_suffix()));
    let temp = dir.join(temp_name);

    let written = (|| -> io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// Reads a whole file, refusing anything larger than `max_bytes` before allocating.
pub fn read_capped(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let len = fs::metadata(path)?.len();
    if len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file is {len} bytes; the limit is {max_bytes}"),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    File::open(path)?.take(max_bytes).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Current time as `YYYY-MM-DDTHH:MM:SSZ` (UTC, whole seconds).
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601(secs)
}

/// Formats seconds since the Unix epoch as an ISO 8601 UTC timestamp.
pub fn format_iso8601(unix_secs: u64) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(i64::MAX / 2);
    let rem = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Converts days since 1970-01-01 to a proleptic Gregorian (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m_wide = if mp < 10 { mp + 3 } else { mp - 9 };
    let m = m_wide as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Total size and file count below `root`. Unreadable entries are skipped.
pub fn dir_usage(root: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// Every regular file below `root`.
pub fn list_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(meta) if meta.is_dir() => stack.push(path),
                Ok(meta) if meta.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out
}

/// Deletes stale `*.partial` files below `root` (left behind by a crash or an interrupted export). Returns how many
/// were removed.
pub fn sweep_partials(root: &Path) -> usize {
    list_files(root)
        .into_iter()
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("partial"))
        })
        .filter(|p| fs::remove_file(p).is_ok())
        .count()
}

/// Removes every file below `root` for which `keep` returns false, then the folders left empty. Returns
/// `(removed, failed)` counts; a file that is locked by another process counts as failed.
pub fn remove_files_except(root: &Path, keep: impl Fn(&Path) -> bool) -> (usize, usize) {
    let mut removed = 0;
    let mut failed = 0;
    for file in list_files(root) {
        if keep(&file) {
            continue;
        }
        match fs::remove_file(&file) {
            Ok(()) => removed += 1,
            Err(_) => failed += 1,
        }
    }
    prune_empty_dirs(root);
    (removed, failed)
}

fn prune_empty_dirs(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            prune_empty_dirs(&path);
            let _ = fs::remove_dir(&path);
        }
    }
}

/// Removes the `\\?\` verbatim prefix that `fs::canonicalize` adds on Windows, so paths look and compare like the
/// ones users see. `\\?\UNC\server\share` becomes `\\server\share`. Other paths are returned unchanged.
pub fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        // Only drive-letter paths are unwrapped; device namespaces stay as they are.
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_timestamps() {
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_iso8601(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn atomic_write_replaces_the_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("settings.json");
        fs::write(&target, b"old").expect("seed");
        atomic_write(&target, b"new contents").expect("write");
        assert_eq!(fs::read(&target).expect("read"), b"new contents");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("list")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            leftovers.len(),
            1,
            "no temporary files remain: {leftovers:?}"
        );
    }

    #[test]
    fn atomic_write_creates_missing_folders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("a").join("b").join("file.txt");
        atomic_write(&target, b"x").expect("write");
        assert_eq!(fs::read(&target).expect("read"), b"x");
    }

    #[test]
    fn capped_reads_refuse_large_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("big.bin");
        fs::write(&file, vec![7u8; 100]).expect("seed");
        assert!(read_capped(&file, 99).is_err());
        assert_eq!(read_capped(&file, 100).expect("read").len(), 100);
    }

    #[test]
    fn sweep_removes_only_partials() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("proxies");
        fs::create_dir_all(&sub).expect("mkdir");
        fs::write(sub.join("a.mp4.partial"), b"x").expect("seed");
        fs::write(sub.join("a.mp4"), b"x").expect("seed");
        assert_eq!(sweep_partials(dir.path()), 1);
        assert!(sub.join("a.mp4").exists());
        assert!(!sub.join("a.mp4.partial").exists());
    }

    #[test]
    fn usage_and_selective_removal() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("keep.bin"), [0u8; 10]).expect("seed");
        fs::write(dir.path().join("drop.bin"), [0u8; 20]).expect("seed");
        assert_eq!(dir_usage(dir.path()), (30, 2));
        let keep = dir.path().join("keep.bin");
        let (removed, failed) = remove_files_except(dir.path(), |p| p == keep);
        assert_eq!((removed, failed), (1, 0));
        assert_eq!(dir_usage(dir.path()), (10, 1));
    }

    #[test]
    fn verbatim_prefixes_are_removed() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\videos\a.mp4")),
            PathBuf::from(r"C:\videos\a.mp4")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\nas\share\a.mp4")),
            PathBuf::from(r"\\nas\share\a.mp4")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"C:\plain.mp4")),
            PathBuf::from(r"C:\plain.mp4")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\Volume{1}\x")),
            PathBuf::from(r"\\?\Volume{1}\x")
        );
    }
}

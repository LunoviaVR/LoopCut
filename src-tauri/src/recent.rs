//! The recent-projects list (`recent.json` in the app data folder), most recent first and capped by settings.

use std::path::Path;

use crate::dto::RecentProject;
use crate::error::AppError;
use crate::storage;

/// Larger than any sane list; protects against a hand-edited file.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Reads the list. A missing or unreadable file yields an empty list (the list is a convenience, never an error).
pub fn load(path: &Path, limit: usize) -> Vec<RecentProject> {
    let Ok(bytes) = storage::read_capped(path, MAX_FILE_BYTES) else {
        return Vec::new();
    };
    let mut list: Vec<RecentProject> = serde_json::from_slice(&bytes).unwrap_or_default();
    list.truncate(limit);
    list
}

/// Writes the list atomically.
pub fn store(path: &Path, list: &[RecentProject]) -> Result<(), AppError> {
    let json = serde_json::to_vec_pretty(list).map_err(|err| {
        AppError::new(
            crate::error::code::IO,
            format!("recent projects could not be encoded: {err}"),
        )
    })?;
    storage::atomic_write(path, &json)
        .map_err(|err| crate::error::io_error(&err, "recent projects could not be saved"))
}

/// Puts `entry` first, removes older entries for the same file, and keeps at most `limit` entries.
pub fn record(list: &mut Vec<RecentProject>, entry: RecentProject, limit: usize) {
    let key = identity(&entry.path);
    list.retain(|existing| identity(&existing.path) != key);
    list.insert(0, entry);
    list.truncate(limit.max(1));
}

/// Case-insensitive on Windows, where file names are case-insensitive.
fn identity(path: &str) -> String {
    if cfg!(windows) {
        path.to_ascii_lowercase()
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> RecentProject {
        RecentProject {
            path: path.into(),
            name: "p".into(),
            opened_at: "2026-10-10T00:00:00Z".into(),
        }
    }

    #[test]
    fn list_is_capped_and_most_recent_first() {
        let mut list = Vec::new();
        for i in 0..15 {
            record(&mut list, entry(&format!("C:\\p\\{i}.loopcut")), 10);
        }
        assert_eq!(list.len(), 10);
        assert_eq!(list[0].path, "C:\\p\\14.loopcut");
        assert_eq!(list[9].path, "C:\\p\\5.loopcut");
    }

    #[test]
    fn reopening_moves_an_entry_to_the_top_without_duplicates() {
        let mut list = Vec::new();
        record(&mut list, entry("C:\\a.loopcut"), 10);
        record(&mut list, entry("C:\\b.loopcut"), 10);
        record(&mut list, entry("C:\\A.loopcut"), 10);
        assert_eq!(list.len(), 2);
        if cfg!(windows) {
            assert_eq!(list[0].path, "C:\\A.loopcut");
        }
        assert!(list.iter().any(|e| e.path.ends_with("b.loopcut")));
    }

    #[test]
    fn limit_of_zero_still_keeps_one_entry() {
        let mut list = Vec::new();
        record(&mut list, entry("C:\\a.loopcut"), 0);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn store_then_load_respects_the_current_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent.json");
        let list: Vec<RecentProject> = (0..5).map(|i| entry(&format!("C:\\{i}.loopcut"))).collect();
        store(&path, &list).expect("store");
        assert_eq!(load(&path, 3).len(), 3);
        assert_eq!(load(&path, 50).len(), 5);
    }

    #[test]
    fn missing_or_corrupt_files_load_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load(&dir.path().join("none.json"), 10).is_empty());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"[{").expect("seed");
        assert!(load(&bad, 10).is_empty());
    }
}

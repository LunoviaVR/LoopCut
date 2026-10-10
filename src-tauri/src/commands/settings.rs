//! Settings and cache commands.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;

use crate::commands::blocking;
use crate::dto::{AppSettings, CacheUsage};
use crate::error::AppError;
use crate::render;
use crate::settings;
use crate::state::AppState;
use crate::storage;

/// Current settings. Always a complete document: missing fields are filled with defaults.
#[tauri::command]
pub async fn settings_get(state: State<'_, Arc<AppState>>) -> Result<AppSettings, AppError> {
    Ok(state.settings.read().clone())
}

/// Validates, clamps and saves the settings atomically. Takes effect immediately, except the frame-cache budgets,
/// which apply the next time LoopCut starts. A change to the hardware settings rebuilds the GPU context and the preview
/// engine on their next use, and clears a software fallback that followed a device loss.
#[tauri::command]
pub async fn settings_set(
    state: State<'_, Arc<AppState>>,
    settings: AppSettings,
) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let clean = settings::sanitize(settings);
        settings::save(&st.dirs.settings_file(), &clean)?;
        let hardware_changed = st.settings.read().hw != clean.hw;
        *st.settings.write() = clean;
        if hardware_changed {
            render::reset(&st);
        }
        Ok(())
    })
    .await
}

/// Size and file count of the cache folder (proxies, frame spill files, partial exports).
#[tauri::command]
pub async fn cache_usage(state: State<'_, Arc<AppState>>) -> Result<CacheUsage, AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let (bytes, files) = storage::dir_usage(&st.dirs.cache);
        Ok(CacheUsage {
            bytes,
            files,
            path: st.dirs.cache.to_string_lossy().into_owned(),
        })
    })
    .await
}

/// Deletes cache files. Proxies still referenced by an open media token and partial files of running jobs are kept.
/// Files another process holds open are skipped rather than failing the whole clear.
#[tauri::command]
pub async fn cache_clear(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        if let Some(store) = &st.framestore {
            store.clear();
        }
        if let Some(store) = st.session_frames.lock().as_ref() {
            store.clear();
        }
        let in_use: HashSet<PathBuf> = st
            .media
            .read()
            .values()
            .filter_map(|entry| entry.state.lock().proxy.clone())
            .collect();
        let jobs_running = !st.jobs.is_empty();
        // The frame store's folders (its spill files and the `owner.lock` that proves a live owner) belong to the
        // store, which removes its own frames above. They are never deleted from outside.
        let frames_root = st.dirs.frames.clone();
        let (removed, failed) = storage::remove_files_except(&st.dirs.cache, |path| {
            keep_on_cache_clear(path, &frames_root, &in_use, jobs_running)
        });
        tracing::info!(removed, failed, "cache cleared");
        Ok(())
    })
    .await
}

/// Whether `cache_clear` keeps a file: anything in the frame store's folders (including its owner lock), a proxy a
/// live token uses, and a partial export while jobs run.
pub(crate) fn keep_on_cache_clear(
    path: &std::path::Path,
    frames_root: &std::path::Path,
    in_use: &HashSet<PathBuf>,
    jobs_running: bool,
) -> bool {
    let partial = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("partial"));
    path.starts_with(frames_root) || in_use.contains(path) || (partial && jobs_running)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_clear_keeps_the_frame_store_folder_and_its_owner_lock() {
        let frames = std::path::Path::new(r"C:\cache\frames");
        let none = HashSet::new();
        assert!(keep_on_cache_clear(
            &frames.join("abc123").join("owner.lock"),
            frames,
            &none,
            false
        ));
        assert!(keep_on_cache_clear(
            &frames.join("abc123").join("0001.bin"),
            frames,
            &none,
            false
        ));
        assert!(!keep_on_cache_clear(
            std::path::Path::new(r"C:\cache\proxies\old.mp4"),
            frames,
            &none,
            false
        ));
        assert!(keep_on_cache_clear(
            std::path::Path::new(r"C:\cache\proxies\new.mp4.partial"),
            frames,
            &none,
            true
        ));
    }

    #[test]
    fn clear_keeps_protected_files_and_removes_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let proxy = dir.path().join("proxies").join("keep.mp4");
        std::fs::create_dir_all(proxy.parent().expect("parent")).expect("mkdir");
        std::fs::write(&proxy, b"keep").expect("seed");
        std::fs::write(dir.path().join("proxies").join("drop.mp4"), b"drop").expect("seed");
        let in_use: HashSet<PathBuf> = [proxy.clone()].into_iter().collect();
        let (removed, failed) = storage::remove_files_except(dir.path(), |p| in_use.contains(p));
        assert_eq!((removed, failed), (1, 0));
        assert!(proxy.exists());
    }
}

//! Locating the FFmpeg and ffprobe executables.
//!
//! In the shipped app the executables live in the Tauri resource directory and the app passes that folder to
//! [`FfmpegPaths::from_dir`]. During development and in tests [`FfmpegPaths::discover_dev`] finds the pinned build
//! staged in the repository at `src-tauri/resources/ffmpeg/<version>/`, where `<version>` comes from
//! `scripts/ffmpeg.lock.json`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::MediaError;

/// Environment variable that overrides the development FFmpeg folder.
pub const FFMPEG_DIR_ENV: &str = "LOOPCUT_FFMPEG_DIR";

/// Absolute paths of the two executables LoopCut drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPaths {
    /// The `ffmpeg` executable.
    pub ffmpeg: PathBuf,
    /// The `ffprobe` executable.
    pub ffprobe: PathBuf,
}

/// The subset of `scripts/ffmpeg.lock.json` needed to locate the staged build.
#[derive(Debug, Deserialize)]
struct LockFile {
    version: String,
}

impl FfmpegPaths {
    /// Uses `ffmpeg` and `ffprobe` (`.exe` on Windows) from `dir`.
    ///
    /// Fails with [`MediaError::BinaryMissing`] if either executable is not a regular file there.
    pub fn from_dir(dir: &Path) -> Result<Self, MediaError> {
        let ffmpeg = dir.join(exe_name("ffmpeg"));
        let ffprobe = dir.join(exe_name("ffprobe"));
        for candidate in [&ffmpeg, &ffprobe] {
            if !candidate.is_file() {
                return Err(MediaError::BinaryMissing(candidate.clone()));
            }
        }
        Ok(Self { ffmpeg, ffprobe })
    }

    /// Finds the FFmpeg build staged in this repository, for development and tests.
    ///
    /// `LOOPCUT_FFMPEG_DIR` wins when it points at a valid folder. Otherwise the repository root is found by
    /// walking up from this crate's manifest directory to the first ancestor containing `src-tauri/resources/ffmpeg/`,
    /// and the folder named by `scripts/ffmpeg.lock.json` `version` is used. Returns `None` when nothing usable
    /// exists, so callers can skip FFmpeg-dependent work.
    pub fn discover_dev() -> Option<Self> {
        if let Some(dir) = std::env::var_os(FFMPEG_DIR_ENV) {
            match Self::from_dir(Path::new(&dir)) {
                Ok(paths) => return Some(paths),
                Err(err) => tracing::warn!(error = %err, "ignoring {FFMPEG_DIR_ENV}"),
            }
        }
        Self::discover_from_manifest(Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    /// Walks up from `start` looking for the repository root that holds the staged build.
    fn discover_from_manifest(start: &Path) -> Option<Self> {
        start
            .ancestors()
            .find(|dir| {
                dir.join("src-tauri")
                    .join("resources")
                    .join("ffmpeg")
                    .is_dir()
            })
            .and_then(Self::from_repo_root)
    }

    /// Resolves the staged build for a repository root, using the version pinned in the lock file.
    fn from_repo_root(root: &Path) -> Option<Self> {
        let lock_text = fs::read_to_string(root.join("scripts").join("ffmpeg.lock.json")).ok()?;
        let lock: LockFile = serde_json::from_str(&lock_text).ok()?;
        if !is_plain_folder_name(&lock.version) {
            return None;
        }
        let dir = root
            .join("src-tauri")
            .join("resources")
            .join("ffmpeg")
            .join(lock.version);
        Self::from_dir(&dir).ok()
    }
}

/// Platform file name of an executable (`ffmpeg.exe` on Windows).
fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

/// Rejects version strings that could escape the resource folder (separators, `.`, `..`, drive prefixes).
fn is_plain_folder_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, b"stub").expect("write");
    }

    #[test]
    fn from_dir_reports_missing_binaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = FfmpegPaths::from_dir(dir.path()).expect_err("empty folder must fail");
        assert!(matches!(err, MediaError::BinaryMissing(_)), "{err:?}");
    }

    #[test]
    fn from_dir_finds_both_binaries() {
        let dir = tempfile::tempdir().expect("tempdir");
        touch(&dir.path().join(exe_name("ffmpeg")));
        touch(&dir.path().join(exe_name("ffprobe")));
        let paths = FfmpegPaths::from_dir(dir.path()).expect("both present");
        assert_eq!(paths.ffmpeg, dir.path().join(exe_name("ffmpeg")));
        assert_eq!(paths.ffprobe, dir.path().join(exe_name("ffprobe")));
    }

    #[test]
    fn repo_root_uses_lock_version_folder() {
        let root = tempfile::tempdir().expect("tempdir");
        let version = "9.0.2-test";
        touch(
            &root
                .path()
                .join("src-tauri/resources/ffmpeg")
                .join(version)
                .join(exe_name("ffmpeg")),
        );
        touch(
            &root
                .path()
                .join("src-tauri/resources/ffmpeg")
                .join(version)
                .join(exe_name("ffprobe")),
        );
        // A decoy folder must not be chosen when the lock names another version.
        touch(
            &root
                .path()
                .join("src-tauri/resources/ffmpeg/0.0.0/")
                .join(exe_name("ffmpeg")),
        );
        touch(&root.path().join("scripts/ffmpeg.lock.json"));
        fs::write(
            root.path().join("scripts/ffmpeg.lock.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .expect("lock");

        let paths = FfmpegPaths::discover_from_manifest(root.path()).expect("resolves");
        assert!(
            paths
                .ffmpeg
                .ends_with(Path::new(version).join(exe_name("ffmpeg"))),
            "{paths:?}"
        );
    }

    #[test]
    fn lock_version_cannot_escape_resource_folder() {
        assert!(!is_plain_folder_name(""));
        assert!(!is_plain_folder_name(".."));
        assert!(!is_plain_folder_name("../evil"));
        assert!(!is_plain_folder_name("C:x"));
        assert!(is_plain_folder_name("9.0.2-25-g67b60c310b"));
    }

    #[test]
    fn repo_discovery_finds_staged_build_when_present() {
        // Only meaningful on a checkout with the FFmpeg bundle staged; otherwise discovery must simply return None.
        if let Some(paths) = FfmpegPaths::discover_dev() {
            assert!(paths.ffmpeg.is_file());
            assert!(paths.ffprobe.is_file());
        }
    }
}

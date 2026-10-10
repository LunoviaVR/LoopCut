//! Build script: exposes the pinned FFmpeg version (from `scripts/ffmpeg.lock.json`) to the app as
//! `LOOPCUT_FFMPEG_VERSION`, which names the bundled resource folder `ffmpeg/<version>`.

use std::path::{Path, PathBuf};

/// Version used when the lock file cannot be read. Resolving it fails at runtime with a clear message.
const UNKNOWN_VERSION: &str = "unknown";

fn lock_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("version")?.as_str().map(str::to_owned)
}

fn main() {
    tauri_build::build();

    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_default();
    let lock = manifest_dir
        .join("..")
        .join("scripts")
        .join("ffmpeg.lock.json");
    println!("cargo:rerun-if-changed={}", lock.display());

    let version = lock_version(&lock).unwrap_or_else(|| {
        println!(
            "cargo:warning=could not read FFmpeg version from {}",
            lock.display()
        );
        UNKNOWN_VERSION.to_owned()
    });
    println!("cargo:rustc-env=LOOPCUT_FFMPEG_VERSION={version}");
}

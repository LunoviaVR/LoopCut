//! M0 spike harness and self-test hooks: preview-frame transport benchmarks, webview diagnostics and the opt-in self-test.
//!
//! The benchmark endpoints (`bench_channel`, `bench_invoke_frame`, the `loopcut` protocol's `bench/` route) exist only in
//! debug builds. `spike_flags` and `spike_report` stay in every build. `spike_flags` tells the webview two things:
//! `run` (LOOPCUT_RUN_SPIKES=1 asks for the benchmarks) and the self-test input. The self-test names a video file in
//! LOOPCUT_SELFTEST and is honored in debug builds, or in any build when LOOPCUT_ALLOW_SELFTEST=1. The webview runs it
//! through the normal commands and reports the result with `spike_report`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use ts_rs::TS;

use crate::error::{AppError, code};

/// Largest benchmark report `spike_report` writes.
const MAX_REPORT_BYTES: usize = 1024 * 1024;
/// Environment variable that names the video file the self-test runs on.
pub const SELFTEST_ENV: &str = "LOOPCUT_SELFTEST";
/// Environment variable that allows the self-test in a release build when it is `1`.
pub const SELFTEST_ALLOW_ENV: &str = "LOOPCUT_ALLOW_SELFTEST";
/// Folder under the temp folder where self-test outputs are written.
const SELFTEST_DIR: &str = "loopcut-selftest";

/// What the webview reads at startup from the environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpikeFlags {
    /// True when LOOPCUT_RUN_SPIKES is 1: the benchmarks should run.
    pub run: bool,
    /// The video file for the self-test: set when the self-test is allowed and LOOPCUT_SELFTEST names a file.
    pub selftest: Option<String>,
    /// A writable folder for the self-test's outputs. Set only when `selftest` is set and the folder could be created.
    pub selftest_out: Option<String>,
}

/// The startup flags, read from the environment.
#[tauri::command]
pub fn spike_flags() -> SpikeFlags {
    let selftest = selftest_input(cfg!(debug_assertions), |name| std::env::var(name).ok());
    let selftest_out = selftest
        .as_ref()
        .and_then(|_| selftest_output_dir(&std::env::temp_dir()))
        .map(|dir| dir.to_string_lossy().into_owned());
    SpikeFlags {
        run: std::env::var("LOOPCUT_RUN_SPIKES").is_ok_and(|v| v == "1"),
        selftest,
        selftest_out,
    }
}

/// True when the self-test may run: always in a debug build, and in a release build only when
/// `LOOPCUT_ALLOW_SELFTEST` is `1`. `allow` is that variable's value, if it is set.
pub fn selftest_allowed(debug_build: bool, allow: Option<&str>) -> bool {
    debug_build || allow.is_some_and(|value| value.trim() == "1")
}

/// The self-test's video file, read through `env`. `None` unless the self-test is allowed and `LOOPCUT_SELFTEST` names a
/// non-empty path. The path is only a candidate: the webview opens it through `media_import`, which checks it.
pub fn selftest_input(debug_build: bool, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    if !selftest_allowed(debug_build, env(SELFTEST_ALLOW_ENV).as_deref()) {
        return None;
    }
    env(SELFTEST_ENV)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Creates the self-test output folder under `root` and returns it. `None` when it cannot be created, which is logged.
fn selftest_output_dir(root: &Path) -> Option<PathBuf> {
    let dir = root.join(SELFTEST_DIR);
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(err) => {
            tracing::warn!(error = %err, "the self-test output folder could not be created");
            None
        }
    }
}

/// Writes the frontend's benchmark results to the app log directory and returns the file path. The self-test reports
/// its results the same way.
#[tauri::command]
pub fn spike_report(app: AppHandle, json: String) -> Result<String, AppError> {
    if json.len() > MAX_REPORT_BYTES {
        return Err(AppError::invalid_argument(
            "The benchmark report is larger than 1 MiB and was not written.",
        ));
    }
    let dir = app
        .path()
        .app_log_dir()
        .map_err(|err| AppError::new(code::IO, err.to_string()))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("spike-results.json");
    crate::storage::atomic_write(&path, json.as_bytes())?;
    tracing::info!("spike results written to {}", path.display());
    Ok(path.to_string_lossy().into_owned())
}

/// Benchmark commands and routes. Compiled into debug builds only.
#[cfg(debug_assertions)]
pub mod bench {
    use std::sync::OnceLock;

    use tauri::ipc::{Channel, InvokeResponseBody, Response};

    use crate::error::{AppError, code};

    /// Widest benchmark frame accepted (8K).
    pub const MAX_WIDTH: u32 = 7680;
    /// Tallest benchmark frame accepted (8K).
    pub const MAX_HEIGHT: u32 = 4320;
    /// Most frames one channel benchmark may push.
    pub const MAX_COUNT: u32 = 600;

    /// True when a benchmark frame size is within the accepted bounds.
    pub fn frame_size_allowed(width: u32, height: u32) -> bool {
        (1..=MAX_WIDTH).contains(&width) && (1..=MAX_HEIGHT).contains(&height)
    }

    /// Deterministic RGBA test frame; the frame index is encoded in the first two bytes.
    pub fn test_frame(width: u32, height: u32, index: u32) -> Vec<u8> {
        static BASE: OnceLock<parking_lot::Mutex<(u32, u32, Vec<u8>)>> = OnceLock::new();
        let lock = BASE.get_or_init(|| parking_lot::Mutex::new((0, 0, Vec::new())));
        let mut guard = lock.lock();
        if guard.0 != width || guard.1 != height {
            let mut v = vec![0u8; (width as usize) * (height as usize) * 4];
            for y in 0..height {
                for x in 0..width {
                    let i = ((y * width + x) * 4) as usize;
                    v[i] = (x * 255 / width.max(1)) as u8;
                    v[i + 1] = (y * 255 / height.max(1)) as u8;
                    v[i + 2] = 128;
                    v[i + 3] = 255;
                }
            }
            *guard = (width, height, v);
        }
        let mut frame = guard.2.clone();
        if frame.len() >= 2 {
            frame[0] = (index & 0xff) as u8;
            frame[1] = ((index >> 8) & 0xff) as u8;
        }
        frame
    }

    fn limits_error() -> AppError {
        AppError::invalid_argument(format!(
            "benchmark limits: width <= {MAX_WIDTH}, height <= {MAX_HEIGHT}, count <= {MAX_COUNT}"
        ))
    }

    /// Pushes `count` raw RGBA frames to the webview over an IPC channel.
    #[tauri::command]
    pub async fn bench_channel(
        width: u32,
        height: u32,
        count: u32,
        on_frame: Channel<InvokeResponseBody>,
    ) -> Result<(), AppError> {
        if !frame_size_allowed(width, height) || count > MAX_COUNT {
            return Err(limits_error());
        }
        for i in 0..count {
            let frame = test_frame(width, height, i);
            on_frame
                .send(InvokeResponseBody::Raw(frame))
                .map_err(|err| {
                    AppError::new(code::CANCELLED, "The benchmark channel was closed.")
                        .with_detail(err.to_string())
                })?;
        }
        Ok(())
    }

    /// Returns a single raw RGBA frame as the command response (request/response latency test).
    #[tauri::command]
    pub fn bench_invoke_frame(width: u32, height: u32, index: u32) -> Result<Response, AppError> {
        if !frame_size_allowed(width, height) {
            return Err(limits_error());
        }
        Ok(Response::new(test_frame(width, height, index)))
    }

    /// Serves `bench/<w>/<h>/<i>` raw RGBA frames for the protocol-pull benchmark.
    pub fn handle_bench_request(path: &str) -> Option<Vec<u8>> {
        let mut parts = path.trim_start_matches('/').split('/');
        if parts.next()? != "bench" {
            return None;
        }
        let w: u32 = parts.next()?.parse().ok()?;
        let h: u32 = parts.next()?.parse().ok()?;
        let i: u32 = parts.next()?.parse().ok()?;
        if !frame_size_allowed(w, h) {
            return None;
        }
        Some(test_frame(w, h, i))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bench_routes_parse_and_reject() {
            assert_eq!(
                handle_bench_request("/bench/4/2/1").map(|f| f.len()),
                Some(32)
            );
            assert!(handle_bench_request("/bench/0/2/1").is_none());
            assert!(handle_bench_request("/other/4/2/1").is_none());
            assert!(handle_bench_request("/bench/4/2").is_none());
        }

        #[test]
        fn bench_limits_are_invalid_arguments() {
            let err = bench_invoke_frame(0, 1, 0)
                .err()
                .expect("zero width is refused");
            assert_eq!(err.code, "invalid_argument");
            assert!(err.message.contains("7680"), "{}", err.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_self_test_runs_in_debug_builds_and_with_the_opt_in_only() {
        assert!(selftest_allowed(true, None));
        assert!(!selftest_allowed(false, None));
        assert!(!selftest_allowed(false, Some("0")));
        assert!(selftest_allowed(false, Some(" 1 ")));
    }

    #[test]
    fn the_self_test_input_needs_the_gate_and_a_path() {
        let env = |allow: Option<&'static str>, path: Option<&'static str>| {
            move |name: &str| match name {
                SELFTEST_ALLOW_ENV => allow.map(str::to_owned),
                SELFTEST_ENV => path.map(str::to_owned),
                _ => None,
            }
        };
        assert_eq!(
            selftest_input(false, env(Some("1"), Some(r"C:\clips\a.mp4 "))),
            Some(r"C:\clips\a.mp4".to_owned())
        );
        assert_eq!(selftest_input(false, env(None, Some(r"C:\a.mp4"))), None);
        assert_eq!(selftest_input(false, env(Some("1"), Some("  "))), None);
        assert_eq!(
            selftest_input(true, env(None, Some(r"C:\a.mp4"))),
            Some(r"C:\a.mp4".to_owned()),
            "debug builds need no opt-in"
        );
    }

    #[test]
    fn the_self_test_output_folder_is_created_under_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let made = selftest_output_dir(dir.path()).expect("created");
        assert!(made.is_dir());
        assert_eq!(made, dir.path().join(SELFTEST_DIR));
    }

    #[test]
    fn spike_flags_serialize_in_camel_case() {
        let flags = SpikeFlags {
            run: false,
            selftest: Some(r"C:\a.mp4".into()),
            selftest_out: None,
        };
        let json = serde_json::to_value(&flags).expect("serialize");
        assert_eq!(json["selftest"], r"C:\a.mp4");
        assert!(json["selftestOut"].is_null());
        assert_eq!(json["run"], false);
    }
}

//! M0 spike (b): preview-frame transport benchmarks and webview environment diagnostics.
//!
//! Started with `LOOPCUT_RUN_SPIKES=1 npm run tauri dev`; the frontend runs the benchmarks on launch
//! and reports results through [`spike_report`], which writes `spike-results.json` to the app log dir.

use std::sync::OnceLock;

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpikeFlags {
    pub run: bool,
}

#[tauri::command]
pub fn spike_flags() -> SpikeFlags {
    SpikeFlags { run: std::env::var("LOOPCUT_RUN_SPIKES").is_ok_and(|v| v == "1") }
}

/// Deterministic RGBA test frame; the frame index is encoded in the first pixel.
pub fn test_frame(width: u32, height: u32, index: u32) -> Vec<u8> {
    static BASE: OnceLock<parking_lot::Mutex<(u32, u32, Vec<u8>)>> = OnceLock::new();
    let lock = BASE.get_or_init(|| parking_lot::Mutex::new((0, 0, Vec::new())));
    let mut guard = lock.lock();
    if guard.0 != width || guard.1 != height {
        let mut v = vec![0u8; (width * height * 4) as usize];
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
    frame[0] = (index & 0xff) as u8;
    frame[1] = ((index >> 8) & 0xff) as u8;
    frame
}

/// Pushes `count` raw RGBA frames to the webview over an IPC channel.
#[tauri::command]
pub async fn bench_channel(width: u32, height: u32, count: u32, on_frame: Channel<InvokeResponseBody>) -> Result<(), String> {
    for i in 0..count {
        let frame = test_frame(width, height, i);
        on_frame.send(InvokeResponseBody::Raw(frame)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Returns a single raw RGBA frame as the command response (request/response latency test).
#[tauri::command]
pub fn bench_invoke_frame(width: u32, height: u32, index: u32) -> Response {
    Response::new(test_frame(width, height, index))
}

/// Writes the frontend's benchmark results to the app log directory.
#[tauri::command]
pub fn spike_report(app: AppHandle, json: String) -> Result<String, String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("spike-results.json");
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    tracing::info!("spike results written to {}", path.display());
    Ok(path.to_string_lossy().into_owned())
}

/// Serves `/bench/<w>/<h>/<i>` raw RGBA frames for the protocol-pull benchmark.
pub fn handle_bench_request(path: &str) -> Option<Vec<u8>> {
    let mut parts = path.trim_start_matches('/').split('/');
    if parts.next()? != "bench" {
        return None;
    }
    let w: u32 = parts.next()?.parse().ok()?;
    let h: u32 = parts.next()?.parse().ok()?;
    let i: u32 = parts.next()?.parse().ok()?;
    if w == 0 || h == 0 || w > 7680 || h > 4320 {
        return None;
    }
    Some(test_frame(w, h, i))
}

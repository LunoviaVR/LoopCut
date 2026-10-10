//! Hardware acceleration: the capability record, the probe that fills it, and the startup work that restores it.
//!
//! At startup a background thread reads the FFmpeg version and the adapters. A cached record is used when its key
//! (FFmpeg version, adapters, drivers, indices) still matches; otherwise a probe runs with the default timeouts and no
//! benchmark. `hw_probe` always runs a fresh probe, with the speed benchmark when `force` is set, and replaces the
//! cached record. Probing starts FFmpeg children and blocks, so it always runs on a worker thread and registers as the
//! `hw-probe` job, which `app_prepare_update` cancels.

use std::sync::Arc;

use loopcut_hw::cache::{self, cache_key};
use loopcut_hw::{
    Capabilities, GpuDevice, HwError, ProbeEvent, ProbeOptions, enumerate_gpus, probe_capabilities,
};
use loopcut_media::runner::CancelToken;
use tauri::State;
use tauri::ipc::Channel;

use crate::commands::app::read_ffmpeg_version;
use crate::commands::blocking;
use crate::error::{AppError, code};
use crate::state::AppState;

/// Job id of a running capability probe.
pub const HW_PROBE_JOB: &str = "hw-probe";
/// Job id of the startup `ffmpeg -version` read.
pub const FFMPEG_VERSION_JOB: &str = "ffmpeg-version";

/// Cached hardware probe results, or `null` while the startup probe is still running.
#[tauri::command]
pub async fn hw_capabilities(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<Capabilities>, AppError> {
    Ok(state.caps.read().clone())
}

/// Runs a fresh hardware probe and returns its result. Progress arrives as `ProbeEvent`s on `onProgress`. `force` also
/// runs the speed benchmark (slower, more accurate). The result replaces the cached record.
///
/// # Errors
///
/// `busy` while another probe runs (the startup probe included), `cancelled` when the app is updating, and
/// `ffmpeg_missing` when FFmpeg could not be located.
#[tauri::command]
pub async fn hw_probe(
    state: State<'_, Arc<AppState>>,
    force: bool,
    on_progress: Channel<ProbeEvent>,
) -> Result<Capabilities, AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let gpus = enumerate_gpus();
        run_probe(&st, force, &gpus, &mut |event| {
            let _ = on_progress.send(event);
        })
    })
    .await
}

/// Starts the background work that makes hardware capabilities available: the FFmpeg version line, then the cached
/// record or a fresh probe. Failures are logged; the app works on CPU without them.
pub fn spawn_startup(state: Arc<AppState>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("loopcut-startup".to_owned())
        .spawn(move || {
            let Some(paths) = state.ffmpeg.as_ref() else {
                tracing::warn!("hardware probe skipped: FFmpeg is not available");
                return;
            };
            let Some(line) = startup_version_line(&state, || read_ffmpeg_version(paths)) else {
                tracing::warn!("startup hardware checks skipped: background work is closed for an update");
                return;
            };
            let _ = state.ffmpeg_version.set(line);

            let gpus = enumerate_gpus();
            // A record is stored under one key: the benchmark key for a benchmarked probe, the plain key otherwise.
            // The startup accepts whichever key matches the stored record, benchmark first because a benchmarked record
            // carries the speed figures as well.
            let version = state.ffmpeg_version_line();
            for benchmark in [true, false] {
                let key = cache_key(&version, &gpus, benchmark);
                if let Some(caps) = cache::load(&state.dirs.hw_cache_file(), &key, &gpus) {
                    tracing::info!("hardware capabilities restored from the cache");
                    *state.caps.write() = Some(caps);
                    return;
                }
            }
            match run_probe(&state, false, &gpus, &mut |_| {}) {
                Ok(_) => tracing::info!("hardware capabilities probed"),
                Err(err) => tracing::warn!(code = %err.code, message = %err.message, "hardware probe failed"),
            }
        })
        .map(|_| ())
}

/// The FFmpeg version line for the startup, read by `read` while registered as the `ffmpeg-version` job, so an update
/// waits for the child or terminates it. `None` while the registry is closed for an update: `read` is not called then.
fn startup_version_line(
    st: &AppState,
    read: impl FnOnce() -> Result<String, AppError>,
) -> Option<String> {
    let (_cancel, _job) = st.start_job(FFMPEG_VERSION_JOB).ok()?;
    Some(match read() {
        Ok(line) if !line.is_empty() => line,
        Ok(_) => "unknown".to_owned(),
        Err(err) => format!("unavailable: {}", err.message),
    })
}

/// Probes `gpus`, stores the result in memory and in the cache file, and returns it. `benchmark` adds the speed
/// measurements. Progress goes to `on_event`.
pub(crate) fn run_probe(
    st: &AppState,
    benchmark: bool,
    gpus: &[GpuDevice],
    on_event: &mut dyn FnMut(ProbeEvent),
) -> Result<Capabilities, AppError> {
    let paths = st.ffmpeg()?;
    let cancel = CancelToken::new();
    let _job = st.register_job(
        HW_PROBE_JOB,
        cancel.clone(),
        "A hardware test is already running.",
    )?;
    let opts = ProbeOptions {
        benchmark,
        ..ProbeOptions::default()
    };
    let caps = probe_capabilities(paths, gpus, &opts, on_event, &cancel).map_err(hw_error)?;

    let key = cache_key(&st.ffmpeg_version_line(), gpus, benchmark);
    if let Err(err) = cache::save(&st.dirs.hw_cache_file(), &key, &caps) {
        tracing::warn!(error = %err, "the hardware capability cache could not be written");
    }
    *st.caps.write() = Some(caps.clone());
    Ok(caps)
}

fn hw_error(err: HwError) -> AppError {
    match err {
        HwError::Cancelled => AppError::new(code::CANCELLED, "The hardware test was cancelled."),
        HwError::Io(io_err) => AppError::io("The hardware test could not start", &io_err),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::state::test_support::state_under;

    #[test]
    fn the_startup_version_read_does_not_start_while_an_update_closes_the_registry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        state.jobs.close();
        let started = Cell::new(false);
        let line = startup_version_line(&state, || {
            started.set(true);
            Ok("ffmpeg version".to_owned())
        });
        assert_eq!(line, None);
        assert!(!started.get(), "the child is not started");
        assert!(state.jobs.is_empty(), "nothing was registered");
        assert!(state.jobs.is_closed(), "the registry stays closed");
    }

    #[test]
    fn the_startup_version_read_is_a_job_only_while_it_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let jobs_during_read = Cell::new(None);
        let line = startup_version_line(&state, || {
            jobs_during_read.set(Some(state.jobs.len()));
            Ok("ffmpeg version 7.1".to_owned())
        });
        assert_eq!(line.as_deref(), Some("ffmpeg version 7.1"));
        assert_eq!(
            jobs_during_read.get(),
            Some(1),
            "the read runs as the registered ffmpeg-version job"
        );
        assert!(state.jobs.is_empty(), "the job ends with the read");
    }

    #[test]
    fn a_failed_or_empty_version_read_still_yields_a_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let failed = startup_version_line(&state, || {
            Err(AppError::new(code::IO, "the child could not start"))
        })
        .expect("the registry is open");
        assert_eq!(failed, "unavailable: the child could not start");
        let empty = startup_version_line(&state, || Ok(String::new())).expect("open");
        assert_eq!(empty, "unknown");
    }
}

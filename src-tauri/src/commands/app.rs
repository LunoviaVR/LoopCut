//! App-level commands: versions and paths, the copyable diagnostics report, update preparation and the launch argument.

use std::fmt::Write as _;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use loopcut_media::FfmpegPaths;
use loopcut_media::runner::{FfmpegCommand, run_capture};
use tauri::State;

use crate::commands::blocking;
use crate::dto::AppInfo;
use crate::error::{AppError, code, recent_errors};
use crate::render;
use crate::state::{AppState, FFMPEG_VERSION};
use crate::storage;

/// How long `app_prepare_update` waits for cancelled jobs to exit before it terminates their child processes.
const JOB_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the update waits after terminating FFmpeg children before it refuses to continue.
const SECOND_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound for `ffmpeg -version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
/// Poll interval while waiting for jobs to unregister.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// Versions, the FFmpeg build and the data folders.
#[tauri::command]
pub async fn app_info(state: State<'_, Arc<AppState>>) -> Result<AppInfo, AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        Ok(AppInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            ffmpeg_version: st.ffmpeg_version_line(),
            webview2_version: tauri::webview_version().ok(),
            log_dir: st.dirs.logs.to_string_lossy().into_owned(),
            data_dir: st.dirs.data.to_string_lossy().into_owned(),
        })
    })
    .await
}

/// A plain-text report to paste into a bug report: versions, GPU adapters, capabilities, settings, startup warnings and
/// recent errors.
#[tauri::command]
pub async fn app_diagnostics(state: State<'_, Arc<AppState>>) -> Result<String, AppError> {
    let st = Arc::clone(&state);
    blocking(move || Ok(diagnostics_report(&st))).await
}

/// Prepares the app for an update. First it closes the job registry and drops the preview engine and the GPU context
/// (stopping playback, so the engine's decode children end). Then it cancels every running job and waits up to five
/// seconds for them to finish. Jobs still running after that have their child processes terminated through LoopCut's
/// Job Object, and the command waits two seconds more. The registry stays closed on success, so the installer runs with
/// nothing started behind it. If the work cannot be stopped, the command fails with `busy` and reopens the registry. The
/// UI saves the project before calling this, and calls `app_resume_background` if the install then fails.
#[tauri::command]
pub async fn app_prepare_update(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || prepare_update(&st)).await
}

/// Reopens background work after an update that did not install, so probes, previews, exports and proxies are accepted
/// again. Nothing is recreated here: the GPU context and the preview engine are built by the next preview or export
/// that needs them. Calling it on an open registry does nothing. Refused with `busy` while an update is being prepared.
#[tauri::command]
pub async fn app_resume_background(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    resume_background(&state)
}

/// Reopens the job registry. Only the registry changes: the renderer is left as [`prepare_update`] dropped it. The
/// update gate must be free: a resume during a prepare would reopen the registry under the update's drain.
fn resume_background(st: &AppState) -> Result<(), AppError> {
    let Some(_gate) = st.update_gate.try_lock() else {
        return Err(AppError::new(
            code::BUSY,
            "An update is already being prepared. Try again in a moment.",
        ));
    };
    st.jobs.reopen();
    tracing::info!("background work reopened after an update attempt");
    Ok(())
}

/// The file passed on the command line at first launch (double-clicking a `.loopcut` or a video), if any.
#[tauri::command]
pub async fn app_open_path_arg(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, AppError> {
    Ok(state.open_path_arg.clone())
}

/// Reads the first line of `ffmpeg -version` through the contained runner.
pub fn read_ffmpeg_version(paths: &FfmpegPaths) -> Result<String, AppError> {
    let command = FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-version")
        .map_err(AppError::from)?;
    let stdout = run_capture(command, VERSION_TIMEOUT).map_err(AppError::from)?;
    let text = String::from_utf8_lossy(&stdout);
    Ok(text.lines().next().unwrap_or("").trim().to_owned())
}

/// The steps `app_prepare_update` takes once the registry is closed and the renderer is dropped. The live implementation
/// waits on the job registry and terminates FFmpeg children through the Job Object. Tests substitute outcomes for the
/// timeouts and the termination, so every branch can be checked without FFmpeg.
trait UpdateSteps {
    /// Requests cancellation of every running job. Returns how many were running.
    fn cancel_all(&self) -> usize;
    /// Waits up to `timeout` for every job to unregister. Returns true when none is left.
    fn wait_for_jobs(&self, timeout: Duration) -> bool;
    /// How many jobs are still registered, for the messages.
    fn jobs_left(&self) -> usize;
    /// Terminates the FFmpeg children that belong to the job object.
    fn terminate_children(&self) -> std::io::Result<()>;
}

/// The steps against the real job registry and job object.
struct LiveSteps<'a> {
    st: &'a AppState,
}

impl UpdateSteps for LiveSteps<'_> {
    fn cancel_all(&self) -> usize {
        self.st.jobs.cancel_all()
    }

    fn wait_for_jobs(&self, timeout: Duration) -> bool {
        let jobs = Arc::clone(&self.st.jobs);
        drain_until(|| jobs.is_empty(), timeout, DRAIN_POLL)
    }

    fn jobs_left(&self) -> usize {
        self.st.jobs.len()
    }

    fn terminate_children(&self) -> std::io::Result<()> {
        loopcut_media::job::terminate_all()
    }
}

/// Closes the job registry, drops the renderer and drains background work for an update. See [`app_prepare_update`].
///
/// The registry stays closed after success: the installer replaces the app, and nothing may start in the meantime.
/// After a failure it is reopened, so the app keeps working if the user postpones the update. A call that overlaps
/// another update is refused with `busy` before it touches the registry, so only the first call reopens it.
fn prepare_update(st: &AppState) -> Result<(), AppError> {
    let Some(_gate) = st.update_gate.try_lock() else {
        return Err(AppError::new(
            code::BUSY,
            "An update is already being prepared. Try again in a moment.",
        ));
    };
    prepare_update_with(st, &LiveSteps { st })
}

/// [`prepare_update`] with its steps supplied. Order: close the registry, drop the preview engine and GPU context (so
/// nothing new is created and the decode children end), then the drain and termination in `steps`. The caller holds
/// `update_gate`, so no other update runs alongside this one.
fn prepare_update_with(st: &AppState, steps: &dyn UpdateSteps) -> Result<(), AppError> {
    st.jobs.close();
    render::shutdown(st);
    let result = stop_background_work(steps);
    if result.is_err() {
        st.jobs.reopen();
    }
    result
}

/// Cancels every job, waits up to [`JOB_DRAIN_TIMEOUT`], then terminates the FFmpeg children and waits up to
/// [`SECOND_DRAIN_TIMEOUT`] more. Each failing step returns `busy` with its reason.
fn stop_background_work(steps: &dyn UpdateSteps) -> Result<(), AppError> {
    let cancelled = steps.cancel_all();
    if steps.wait_for_jobs(JOB_DRAIN_TIMEOUT) {
        tracing::info!(cancelled, "background work stopped before the update");
        return Ok(());
    }
    let remaining = steps.jobs_left();
    tracing::warn!(
        remaining,
        "background work did not stop in time; terminating its FFmpeg children"
    );
    if let Err(err) = steps.terminate_children() {
        return Err(AppError::new(
            code::BUSY,
            "Some background work could not be stopped. Try again in a moment.",
        )
        .with_detail(err.to_string()));
    }
    // Termination ends the children at once; the jobs unregister as their waits notice. Give them a moment, then
    // refuse the update if anything still runs.
    if steps.wait_for_jobs(SECOND_DRAIN_TIMEOUT) {
        tracing::info!(
            remaining,
            "background children terminated before the update"
        );
        return Ok(());
    }
    Err(AppError::new(
        code::BUSY,
        "Some background work did not stop. Try the update again in a moment.",
    )
    .with_detail(format!("{} job(s) still registered", steps.jobs_left())))
}

/// Polls `done` every `poll` until it returns true or `timeout` passes. `done` is checked first, so a zero timeout
/// still reports a condition that already holds. Returns whether `done` became true.
pub(crate) fn drain_until(
    mut done: impl FnMut() -> bool,
    timeout: Duration,
    poll: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(poll);
    }
}

fn diagnostics_report(st: &AppState) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "LoopCut diagnostics");
    let _ = writeln!(out, "generated: {}", storage::now_iso8601());
    let _ = writeln!(out, "app version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        out,
        "os: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let webview = tauri::webview_version().unwrap_or_else(|_| "unknown".to_owned());
    let _ = writeln!(out, "webview2: {webview}");
    let _ = writeln!(out, "ffmpeg (pinned): {FFMPEG_VERSION}");
    let _ = writeln!(out, "ffmpeg: {}", st.ffmpeg_version_line());
    if let Some(problem) = &st.ffmpeg_problem {
        let _ = writeln!(out, "ffmpeg problem: {problem}");
    }
    let _ = writeln!(out, "logs: {}", st.dirs.logs.display());
    let _ = writeln!(out, "data: {}", st.dirs.data.display());
    let _ = writeln!(out, "cache: {}", st.dirs.cache.display());
    match &st.framestore {
        Some(store) => {
            let _ = writeln!(out, "frame store: {}", store.spill_folder().display());
        }
        None => {
            let _ = writeln!(out, "frame store: unavailable");
        }
    }
    let _ = writeln!(
        out,
        "thumbnail cache: {} of {} bytes in {} sets",
        st.thumb_budget.total_bytes(),
        st.thumb_budget.limit(),
        st.thumb_budget.set_count()
    );
    let _ = writeln!(out, "background jobs: {}", st.jobs.len());
    let _ = writeln!(
        out,
        "shader compiler: {}",
        loopcut_render::gpu::shader_compiler_name()
    );

    let _ = writeln!(out, "\nstartup warnings:");
    if st.startup_warnings.is_empty() {
        let _ = writeln!(out, "(none)");
    }
    for warning in &st.startup_warnings {
        let _ = writeln!(out, "- {warning}");
    }

    let _ = writeln!(out, "\nruntime warnings:");
    let runtime = st.runtime_warnings.lock().clone();
    if runtime.is_empty() {
        let _ = writeln!(out, "(none)");
    }
    for warning in &runtime {
        let _ = writeln!(out, "- {warning}");
    }
    let _ = writeln!(out, "renderer: {}", renderer_line(st));
    let _ = writeln!(
        out,
        "background work closed for an update: {}",
        st.jobs.is_closed()
    );

    let _ = writeln!(out, "\ngpu adapters:");
    for adapter in loopcut_render::gpu::enumerate_adapters() {
        let _ = writeln!(
            out,
            "- {} | vendor 0x{:04x} | device 0x{:04x} | {:?} | {} | driver {}",
            adapter.name,
            adapter.vendor,
            adapter.device,
            adapter.kind,
            adapter.backend,
            adapter.driver
        );
    }

    let _ = writeln!(out, "\nhardware capabilities:");
    match st.caps.read().as_ref() {
        Some(caps) => {
            let _ = writeln!(out, "{}", loopcut_hw::select::describe(caps));
        }
        None => {
            let _ = writeln!(out, "(not probed yet, or the probe did not finish)");
        }
    }

    let _ = writeln!(out, "\nsettings:");
    match serde_json::to_string_pretty(&*st.settings.read()) {
        Ok(json) => {
            let _ = writeln!(out, "{json}");
        }
        Err(err) => {
            let _ = writeln!(out, "(unavailable: {err})");
        }
    }

    let _ = writeln!(out, "\nrecent errors:");
    let errors = recent_errors();
    if errors.is_empty() {
        let _ = writeln!(out, "(none)");
    }
    for line in errors {
        let _ = writeln!(out, "- {line}");
    }
    out
}

/// The renderer in use, for the diagnostics report.
fn renderer_line(st: &AppState) -> String {
    let slot = st.gpu.lock();
    match &slot.context {
        Some(context) => {
            let kind = if context.is_software() {
                "software"
            } else {
                "hardware"
            };
            format!("{} ({kind})", context.desc.name)
        }
        None if slot.software_only => "software (after a device loss)".to_owned(),
        None => "not started yet".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::state_under;

    #[test]
    fn drain_timeout_is_bounded() {
        assert!(JOB_DRAIN_TIMEOUT <= Duration::from_secs(10));
        assert!(SECOND_DRAIN_TIMEOUT <= JOB_DRAIN_TIMEOUT);
    }

    #[test]
    fn drain_returns_as_soon_as_the_condition_holds() {
        let mut calls = 0;
        let done = drain_until(
            || {
                calls += 1;
                calls >= 3
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert!(done);
        assert_eq!(calls, 3, "stops polling at the first true");
    }

    #[test]
    fn drain_reports_a_timeout_when_the_condition_never_holds() {
        let started = Instant::now();
        let done = drain_until(
            || false,
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        assert!(!done);
        assert!(started.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn a_zero_timeout_still_checks_once() {
        assert!(drain_until(
            || true,
            Duration::ZERO,
            Duration::from_millis(1)
        ));
        assert!(!drain_until(
            || false,
            Duration::ZERO,
            Duration::from_millis(1)
        ));
    }

    /// Update steps whose outcomes the test scripts. Each wait answers the next scripted value, and `false` once the
    /// script runs out. The steps record the state they saw when they ran.
    struct Scripted<'a> {
        st: &'a AppState,
        waits: std::cell::RefCell<std::collections::VecDeque<bool>>,
        terminate_fails: bool,
        left: usize,
        terminated: std::cell::Cell<usize>,
        saw_closed: std::cell::Cell<Option<bool>>,
        saw_playing: std::cell::Cell<Option<bool>>,
    }

    impl<'a> Scripted<'a> {
        fn new(st: &'a AppState, waits: &[bool], terminate_fails: bool, left: usize) -> Self {
            Self {
                st,
                waits: std::cell::RefCell::new(waits.iter().copied().collect()),
                terminate_fails,
                left,
                terminated: std::cell::Cell::new(0),
                saw_closed: std::cell::Cell::new(None),
                saw_playing: std::cell::Cell::new(None),
            }
        }
    }

    impl UpdateSteps for Scripted<'_> {
        fn cancel_all(&self) -> usize {
            self.saw_closed.set(Some(self.st.jobs.is_closed()));
            self.saw_playing.set(Some(self.st.preview.lock().playing));
            0
        }

        fn wait_for_jobs(&self, _timeout: Duration) -> bool {
            self.waits.borrow_mut().pop_front().unwrap_or(false)
        }

        fn jobs_left(&self) -> usize {
            self.left
        }

        fn terminate_children(&self) -> std::io::Result<()> {
            self.terminated.set(self.terminated.get() + 1);
            if self.terminate_fails {
                Err(std::io::Error::other("the job object refused"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn jobs_that_stop_in_time_are_not_terminated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[true], false, 0);
        stop_background_work(&steps).expect("stopped in time");
        assert_eq!(steps.terminated.get(), 0);
    }

    #[test]
    fn children_are_terminated_when_the_first_drain_times_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[false, true], false, 1);
        stop_background_work(&steps).expect("stopped after termination");
        assert_eq!(steps.terminated.get(), 1);
    }

    #[test]
    fn an_update_is_busy_when_termination_itself_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[false], true, 1);
        let err = stop_background_work(&steps).expect_err("termination failed");
        assert_eq!(err.code, "busy");
        assert!(
            err.message.contains("could not be stopped"),
            "{}",
            err.message
        );
        assert_eq!(steps.terminated.get(), 1);
    }

    #[test]
    fn an_update_is_busy_while_jobs_survive_termination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[false, false], false, 2);
        let err = stop_background_work(&steps).expect_err("still running");
        assert_eq!(err.code, "busy");
        assert!(err.message.contains("did not stop"), "{}", err.message);
        assert_eq!(err.detail.as_deref(), Some("2 job(s) still registered"));
    }

    #[test]
    fn a_failed_update_reopens_the_registry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[false, false], false, 1);
        assert!(prepare_update_with(&state, &steps).is_err());
        assert!(
            !state.jobs.is_closed(),
            "the app keeps working after a postponed update"
        );
        assert!(state.start_job("probe:after").is_ok());
    }

    #[test]
    fn a_ready_update_leaves_the_registry_closed_for_the_installer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[true], false, 0);
        prepare_update_with(&state, &steps).expect("ready");
        assert!(state.jobs.is_closed());
        assert!(state.start_job("probe:late").is_err());
    }

    #[test]
    fn the_renderer_is_dropped_before_the_jobs_are_drained() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        state.preview.lock().playing = true;
        let steps = Scripted::new(&state, &[true], false, 0);
        prepare_update_with(&state, &steps).expect("ready");
        assert_eq!(
            steps.saw_closed.get(),
            Some(true),
            "the registry is closed before the drain"
        );
        assert_eq!(
            steps.saw_playing.get(),
            Some(false),
            "playback is stopped before the drain"
        );
    }

    #[test]
    fn resuming_reopens_the_registry_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let steps = Scripted::new(&state, &[true], false, 0);
        prepare_update_with(&state, &steps).expect("ready");
        assert!(state.jobs.is_closed());
        resume_background(&state).expect("no update is being prepared");
        assert!(!state.jobs.is_closed());
        assert!(
            state.gpu.lock().context.is_none(),
            "no GPU context is recreated"
        );
        assert!(
            state.preview.lock().engine.is_none(),
            "no preview engine is recreated"
        );
        assert!(state.start_job("probe:resumed").is_ok());
    }

    #[test]
    fn the_update_drain_waits_for_an_admitted_preview_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let (_cancel, guard) = state
            .start_job("preview:frame")
            .expect("admitted before the update");
        state.jobs.close();
        render::shutdown(&state);
        let started = Instant::now();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            drop(guard);
        });
        stop_background_work(&LiveSteps { st: &state })
            .expect("drained once the admitted call ends");
        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "the drain waited for the call instead of returning at once"
        );
        release.join().expect("release thread");
        assert!(state.jobs.is_empty());
    }

    #[test]
    fn a_resume_during_an_update_is_refused_and_reopens_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let gate = state.update_gate.lock();
        state.jobs.close();
        let err = resume_background(&state).expect_err("refused while the update runs");
        assert_eq!(err.code, "busy");
        assert!(
            state.jobs.is_closed(),
            "the update's drain still sees a closed registry"
        );
        drop(gate);
        resume_background(&state).expect("resumed once the update is over");
        assert!(!state.jobs.is_closed());
    }

    #[test]
    fn an_overlapping_update_is_refused_and_leaves_the_registry_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        // The first update holds the gate while it has closed the registry and drains the jobs.
        let first = state.update_gate.lock();
        state.jobs.close();
        let err = prepare_update(&state).expect_err("refused while the first update runs");
        assert_eq!(err.code, "busy");
        assert!(
            state.jobs.is_closed(),
            "a refused call does not reopen the registry the first update closed"
        );
        drop(first);
        prepare_update(&state).expect("accepted once the first update is done");
        assert!(state.jobs.is_closed(), "the accepted update closes it");
    }
}

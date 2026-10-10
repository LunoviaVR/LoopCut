//! The renderer's shared pieces: the GPU context, the preview engine and the frame store they use.
//!
//! One GPU context serves the preview and every export, so the editor and the outputs draw on the same device. It is
//! created on first use from the hardware settings (the effects adapter from `loopcut_hw::select::choose_effects`) and
//! replaced when those settings change or the device is lost. An adapter that cannot be opened falls back to the
//! software adapter and records a runtime warning. After a device loss the software adapter is used until the hardware
//! settings change or the app restarts.
//!
//! A creation that fails is remembered for [`GPU_RETRY_AFTER`] (or until the hardware settings change), so a preview that
//! asks again and again does not retry the device on every request. While the job registry is closed for an update, no
//! context or engine is created, and [`shutdown`] drops both before the installer runs.
//!
//! Lock order: `gpu` before `preview`, and `preview` before the session frame store and the settings. Engines and
//! contexts are dropped (which joins decode threads and releases devices) only after the locks are released.

use std::sync::Arc;
use std::time::{Duration, Instant};

use loopcut_hw::select::{EffectsChoice, choose_effects};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use loopcut_preview::{PreviewEngine, PreviewError, Viewport};
use loopcut_render::compositor::CompositorError;
use loopcut_render::gpu::{AdapterChoice, GpuContext, GpuError};

use crate::error::{AppError, code};
use crate::state::{AppState, MIB, temp_root};

/// How long a failed GPU creation is remembered before the next attempt. Changing the hardware settings clears it sooner.
pub const GPU_RETRY_AFTER: Duration = Duration::from_secs(60);

/// A GPU creation that failed: when it did, and the error every request gets until it expires.
#[derive(Debug, Clone)]
pub struct GpuFailure {
    pub at: Instant,
    pub error: AppError,
}

/// The shared GPU context and what it was made for.
#[derive(Default)]
pub struct GpuSlot {
    pub context: Option<Arc<GpuContext>>,
    /// Set by a device loss: every later context is the software adapter until the hardware settings change or the
    /// app restarts.
    pub software_only: bool,
    /// The last creation that failed. See [`GPU_RETRY_AFTER`].
    pub failure: Option<GpuFailure>,
}

/// The preview engine and the state the preview commands keep between calls.
#[derive(Default)]
pub struct PreviewSession {
    /// The engine and the GPU context it was built on. A different current context means the engine is stale.
    pub engine: Option<(Arc<GpuContext>, Arc<PreviewEngine>)>,
    /// The viewport of the last successful `preview_set_project`, used for source frames without an explicit box.
    pub viewport: Option<Viewport>,
    /// Incremented by every play, stop and shutdown. A playback waiter clears `playing` only while its generation is
    /// current.
    pub play_generation: u64,
    /// True while a `preview_play` is running, from the app's side of the playback.
    pub playing: bool,
}

/// The shared GPU context, created on first use. Replaces a lost context with the software adapter. Refuses with `busy`
/// while the registry is closed for an update, and answers the remembered error while a recent creation failed.
pub fn gpu(st: &AppState) -> Result<Arc<GpuContext>, AppError> {
    let mut slot = st.gpu.lock();
    // Checked under the lock: `app_prepare_update` closes the registry before it calls `shutdown`, which takes this lock
    // next. A context made here is therefore either refused, or dropped by that shutdown.
    st.refuse_if_closed()?;
    if let Some(context) = &slot.context {
        if !context.is_lost() {
            return Ok(Arc::clone(context));
        }
        slot.context = None;
        slot.software_only = true;
        note_device_lost(st);
    }
    if let Some(error) = recent_failure(&slot, Instant::now()) {
        return Err(error);
    }
    let choice = if slot.software_only {
        AdapterChoice::Software
    } else {
        adapter_choice(st)
    };
    let context = match GpuContext::new(&choice) {
        Ok(context) => context,
        Err(err) => {
            let error = AppError::new(code::GPU_FAILED, "The GPU renderer could not be started.")
                .with_detail(err.to_string());
            slot.failure = Some(GpuFailure {
                at: Instant::now(),
                error: error.clone(),
            });
            return Err(error);
        }
    };
    slot.failure = None;
    if context.fell_back() {
        let message = format!(
            "The selected GPU was unavailable; LoopCut is rendering on {}.",
            context.desc.name
        );
        tracing::warn!(%message);
        st.push_runtime_warning(message);
    }
    let context = Arc::new(context);
    slot.context = Some(Arc::clone(&context));
    Ok(context)
}

/// The remembered creation failure, when it is still within [`GPU_RETRY_AFTER`] of `now`. The error is a copy, so
/// answering it does not record it again.
pub fn recent_failure(slot: &GpuSlot, now: Instant) -> Option<AppError> {
    let failure = slot.failure.as_ref()?;
    (now.saturating_duration_since(failure.at) < GPU_RETRY_AFTER).then(|| failure.error.clone())
}

/// The adapter the hardware settings ask for, from the current adapter list.
fn adapter_choice(st: &AppState) -> AdapterChoice {
    let hw = st.settings.read().hw.clone();
    let gpus = loopcut_hw::enumerate_gpus();
    match choose_effects(&hw, &gpus) {
        EffectsChoice::Gpu(key) => AdapterChoice::from_gpu_key(&key),
        EffectsChoice::Software => AdapterChoice::Software,
    }
}

/// The frame store renders decode into: the app's store, or a RAM-only store for this session when the cache folder
/// could not host one.
pub fn frame_store(st: &AppState) -> Result<Arc<FrameStore>, AppError> {
    if let Some(store) = &st.framestore {
        return Ok(Arc::clone(store));
    }
    let mut slot = st.session_frames.lock();
    if let Some(store) = slot.as_ref() {
        return Ok(Arc::clone(store));
    }
    let ram_budget_bytes = st.settings.read().cache.ram_budget_mb * MIB;
    let store = FrameStore::new(FrameStoreConfig {
        ram_budget_bytes,
        disk_budget_bytes: 0,
        spill_dir: temp_root().join("frames"),
    })
    .map_err(|err| {
        AppError::new(code::IO, "The frame cache could not be created.")
            .with_detail(err.to_string())
    })?;
    let store = Arc::new(store);
    *slot = Some(Arc::clone(&store));
    Ok(store)
}

/// The preview engine for the current GPU context, created on first use. A new engine replaces the previous one and
/// resets the session's fallback and failure counts.
///
/// The engine is built without the preview lock, because building it takes a while, and `preview_status` and
/// `preview_stop` must still answer meanwhile. The registry and the stored engine are checked again under the lock before
/// the new engine is kept. If another request stored an engine for the same context meanwhile, that one is returned and
/// the new one is dropped.
pub fn preview_engine(st: &AppState) -> Result<Arc<PreviewEngine>, AppError> {
    let context = gpu(st)?;
    if let Some(engine) = engine_for(st, &context)? {
        return Ok(engine);
    }
    let paths = st.ffmpeg()?.clone();
    let store = frame_store(st)?;
    let built = Arc::new(
        PreviewEngine::new(Arc::clone(&context), paths, store, st.caption_renderer())
            .map_err(|err| map_preview_error(&err))?,
    );
    let mut session = st.preview.lock();
    // Checked under the lock again: `shutdown` runs after the registry closes and takes this lock, so an engine stored
    // after a close would survive the update. It is refused here and dropped with `built`.
    if let Err(err) = st.refuse_if_closed() {
        drop(session);
        return Err(err);
    }
    if let Some(engine) = engine_for_context(&session, &context) {
        drop(session);
        return Ok(engine);
    }
    let previous = session.engine.replace((context, Arc::clone(&built)));
    drop(session);
    // Dropping an engine joins its decode thread, which must not happen under a lock.
    drop(previous);
    Ok(built)
}

/// The stored engine when it was built on `context`, or `None`. Refused with `busy` while the registry is closed for an
/// update.
fn engine_for(
    st: &AppState,
    context: &Arc<GpuContext>,
) -> Result<Option<Arc<PreviewEngine>>, AppError> {
    let session = st.preview.lock();
    st.refuse_if_closed()?;
    Ok(engine_for_context(&session, context))
}

/// The engine stored in `session`, when it was built on `context`.
fn engine_for_context(
    session: &PreviewSession,
    context: &Arc<GpuContext>,
) -> Option<Arc<PreviewEngine>> {
    session
        .engine
        .as_ref()
        .filter(|(used, _)| Arc::ptr_eq(used, context))
        .map(|(_, engine)| Arc::clone(engine))
}

/// The engine of the current session, or `preview_no_project` when no project has been set.
pub fn current_engine(st: &AppState) -> Result<Arc<PreviewEngine>, AppError> {
    st.preview
        .lock()
        .engine
        .as_ref()
        .map(|(_, engine)| Arc::clone(engine))
        .ok_or_else(|| map_preview_error(&PreviewError::NoProject))
}

/// Drops the GPU context and the preview engine because the hardware settings changed. The next use builds them again
/// from the new settings, and a remembered creation failure is forgotten.
pub fn reset(st: &AppState) {
    let previous = {
        let mut slot = st.gpu.lock();
        slot.context = None;
        slot.software_only = false;
        slot.failure = None;
        st.preview.lock().engine.take()
    };
    drop(previous);
}

/// Stops playback, then drops the preview engine and the GPU context, so the engine's decode children end and the device
/// is released before an update. Nothing is recreated here: the next preview or export builds what it needs, once the
/// registry is open again. `software_only` is kept, because a device loss is still a device loss.
///
/// The caller closes the job registry first, so no new engine or context can be made after this runs.
pub fn shutdown(st: &AppState) {
    let (previous_engine, previous_context) = {
        let mut slot = st.gpu.lock();
        let context = slot.context.take();
        let mut session = st.preview.lock();
        session.play_generation += 1;
        session.playing = false;
        let engine = session.engine.take();
        if let Some((_, running)) = &engine {
            running.stop();
        }
        (engine, context)
    };
    // The engine's drop joins its decode thread, and the context's drop releases the device; neither runs under a lock.
    drop(previous_engine);
    drop(previous_context);
}

/// Switches to the software renderer after the GPU device was lost. The preview engine is dropped with the device.
pub fn on_device_lost(st: &AppState) {
    let previous = {
        let mut slot = st.gpu.lock();
        slot.context = None;
        slot.software_only = true;
        note_device_lost(st);
        st.preview.lock().engine.take()
    };
    drop(previous);
}

/// Records a device loss as a runtime warning. The caller has already switched the slot.
fn note_device_lost(st: &AppState) {
    tracing::warn!("the GPU device was lost; the software renderer is used from now on");
    st.push_runtime_warning(
        "The GPU device was lost. LoopCut is using the software renderer until the hardware settings change or the app restarts.",
    );
}

/// Maps a preview failure to the app error. A lost device also switches the renderer to software. Decode and playback
/// errors are reported by the engine's own status, so they are not recorded here.
pub fn preview_error(st: &AppState, err: PreviewError) -> AppError {
    if is_device_lost(&err) {
        on_device_lost(st);
    }
    map_preview_error(&err)
}

/// Maps a preview failure to an app error without recording it.
pub fn map_preview_error(err: &PreviewError) -> AppError {
    if is_device_lost(err) {
        return AppError::new(
            code::GPU_LOST,
            "The GPU device was lost. The preview switches to the software renderer.",
        );
    }
    AppError::new(err.code(), err.to_string())
}

/// True when the preview failed because the GPU device is gone.
pub fn is_device_lost(err: &PreviewError) -> bool {
    matches!(
        err,
        PreviewError::Render(CompositorError::Gpu(GpuError::DeviceLost))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::state_under;

    #[test]
    fn device_loss_is_recognized_from_the_compositor_error() {
        assert!(is_device_lost(&PreviewError::Render(CompositorError::Gpu(
            GpuError::DeviceLost
        ))));
        assert!(!is_device_lost(&PreviewError::Cancelled));
        let mapped = map_preview_error(&PreviewError::Render(CompositorError::Gpu(
            GpuError::DeviceLost,
        )));
        assert_eq!(mapped.code, "gpu_lost");
    }

    #[test]
    fn preview_failures_keep_their_stable_codes() {
        assert_eq!(
            map_preview_error(&PreviewError::NoProject).code,
            "preview_no_project"
        );
        assert_eq!(
            map_preview_error(&PreviewError::FrameOutOfRange {
                frame: 3,
                frames: 2
            })
            .code,
            "preview_frame_out_of_range"
        );
    }

    #[test]
    fn decode_failures_keep_the_preview_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let err = preview_error(&state, PreviewError::Decode("bad packet".into()));
        assert_eq!(err.code, "preview_decode");
        assert_eq!(err.message, "decode failed: bad packet");
    }

    #[test]
    fn with_no_project_the_engine_is_reported_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let Err(err) = current_engine(&state) else {
            panic!("no engine exists before a project is set");
        };
        assert_eq!(err.code, "preview_no_project");
    }

    #[test]
    fn session_frame_store_is_created_when_the_cache_folder_has_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = state_under(dir.path());
        // Rebuild the state without the app frame store to exercise the session fallback.
        let inner = std::sync::Arc::get_mut(&mut state).expect("sole owner");
        inner.framestore = None;
        let first = frame_store(inner).expect("session store");
        let second = frame_store(inner).expect("same store reused");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_closed_registry_creates_no_gpu_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        state.jobs.close();
        let err = gpu(&state).expect_err("refused while closed");
        assert_eq!(err.code, "busy");
        assert!(state.gpu.lock().context.is_none(), "nothing was created");
        state.jobs.reopen();
        assert!(
            state.preview.lock().engine.is_none(),
            "reopening creates nothing eagerly"
        );
    }

    #[test]
    fn a_recent_gpu_failure_is_answered_until_it_expires() {
        let start = Instant::now();
        let error = AppError::new(code::GPU_FAILED, "The GPU renderer could not be started.");
        let slot = GpuSlot {
            failure: Some(GpuFailure {
                at: start,
                error: error.clone(),
            }),
            ..GpuSlot::default()
        };
        let within = start + Duration::from_secs(59);
        assert_eq!(recent_failure(&slot, within), Some(error));
        let expired = start + GPU_RETRY_AFTER;
        assert_eq!(
            recent_failure(&slot, expired),
            None,
            "a minute later it retries"
        );
        assert_eq!(recent_failure(&GpuSlot::default(), start), None);
    }

    #[test]
    fn reset_forgets_a_remembered_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        state.gpu.lock().failure = Some(GpuFailure {
            at: Instant::now(),
            error: AppError::new(code::GPU_FAILED, "x"),
        });
        reset(&state);
        assert!(state.gpu.lock().failure.is_none());
    }

    #[test]
    fn shutdown_stops_playback_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        {
            let mut session = state.preview.lock();
            session.playing = true;
            session.play_generation = 4;
        }
        shutdown(&state);
        let session = state.preview.lock();
        assert!(!session.playing, "playback is marked stopped");
        assert_eq!(
            session.play_generation, 5,
            "a waiter of the old playback cannot clear the flag again"
        );
        drop(session);
        assert!(state.gpu.lock().context.is_none());
        assert!(
            !state.gpu.lock().software_only,
            "a device loss is kept, a shutdown is not one"
        );
    }
}

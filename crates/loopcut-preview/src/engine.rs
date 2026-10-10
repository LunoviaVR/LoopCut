//! The preview engine: planning, the public API, and rendering of frames through the shared compositor.
//!
//! [`PreviewEngine::set_project`] plans the sequence and splits the source ranges it needs into chunks. A background
//! worker decodes the chunks inside the decode window, the chunks nearest to the playhead first, into the shared
//! [`FrameStore`]. The window holds at most half the store's RAM budget, and it moves with the playhead.
//! [`PreviewEngine::render_output_frame`] reads one plan frame: the sample, its frames and the snapshot come from one
//! lock acquisition, so they always belong to the same plan.
//!
//! # Locking
//!
//! All planning state lives in one `Core` behind a mutex (see `model`). Decodes never run under that mutex. A decode is
//! *claimed* first (its entry moves to `Decoding`, under the lock), then runs without it, and reports progress back under
//! the lock. Waiters sleep on one condition variable that every state change signals. The frame store's lock is a leaf:
//! it may be taken under the core lock. The compositor and the caption rasterizer have their own mutexes, and neither is
//! taken while the core mutex is held.
//!
//! # Changing the project
//!
//! A change to the decode inputs (source, decode size, chain) starts a new *generation*: the old claims are cancelled
//! and every range is decoded again. A change to the plan keeps the generation: chunks that are still planned keep
//! their state, and chunks that left the plan are cancelled and their frames removed.
//!
//! An update that changes the plan or the decode bumps the project revision. A request that waits on the plan began
//! under an older revision then ends with [`PreviewError::Cancelled`], so it never draws from a plan that has changed
//! under it. A grade, caption or crop edit keeps the plan and the revision: requests in flight draw from the new look.
//!
//! # Waiting
//!
//! A frame whose decode another thread is running waits for it. A wait ends with [`PreviewError::Timeout`] when that
//! decode makes no progress for the wait timeout (fifteen seconds by default, see
//! [`PreviewEngine::set_progress_timeout`]). A claim that is cancelled while the project stays the same (preempted by a
//! playback, or released by the window) is run again, and the same timeout applies to a run of such retries.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use loopcut_core::geometry as core_geometry;
use loopcut_core::gif_timing::clamp_gif_fps;
use loopcut_core::sequencer;
use loopcut_core::{DecodeChain, ExportFormat, NormRect, Project, TimeRange};
use loopcut_media::FfmpegPaths;
use loopcut_media::framestore::{FrameKey, FrameStore, StoredFrame};
use loopcut_render::caption_layers::{CaptionRenderer, OwnedOverlay};
use loopcut_render::compositor::{CompositeParams, Compositor, InputFrame, Overlay};
use loopcut_render::gpu::GpuContext;
use loopcut_render::grade::prepare_grade;
use loopcut_render::lut::{Lut, load_cube};
use parking_lot::{Condvar, Mutex};

use crate::error::PreviewError;
use crate::faults;
#[cfg(test)]
use crate::faults::Faults;
use crate::geometry::{
    HARDWARE_MAX_DECODE_WIDTH, SOFTWARE_MAX_DECODE_WIDTH, decode_size, render_size,
    validate_viewport,
};
use crate::model::{
    Active, Core, FrameRef, Next, Resolved, Snapshot, SourceId, Want, carry_entries, release,
    remove_sources,
};
use crate::source::{chunk_ranges, decode_fps, decode_template, plan_ranges};
use crate::types::{
    DecodeFallback, PreviewInfo, PreviewQuality, PreviewSource, PreviewStatus, RenderedFrame,
    Viewport,
};
use crate::worker::{WORKER_THREAD, run_claim, worker_loop};

/// How long a wait goes without any progress on the entry it awaits before it gives up, unless the engine is set
/// otherwise with [`PreviewEngine::set_progress_timeout`].
pub const DEFAULT_PROGRESS_TIMEOUT: Duration = Duration::from_secs(15);

/// Attempts to resolve one frame when stored frames keep disappearing.
const MAX_ATTEMPTS: u32 = 3;

/// The frame-level preview engine. One engine serves one editor: it owns the shared compositor and one background
/// decode worker, and every method takes `&self`.
pub struct PreviewEngine {
    pub(crate) shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// State shared by the engine, its worker and any playback thread.
pub(crate) struct Shared {
    pub(crate) gpu: Arc<GpuContext>,
    pub(crate) paths: FfmpegPaths,
    pub(crate) store: Arc<FrameStore>,
    /// RAM budget of `store`, taken from it when the engine is created. It sizes the decode window.
    pub(crate) ram_budget: u64,
    /// How long a wait may go without progress before it fails with [`PreviewError::Timeout`].
    pub(crate) wait_timeout: Mutex<Duration>,
    pub(crate) compositor: Mutex<Compositor>,
    /// The caption rasterizer the app shares with its exports, so the installed fonts are scanned once.
    pub(crate) captions: Arc<std::sync::Mutex<CaptionRenderer>>,
    /// The last LUT loaded, so that a grade edit does not parse the file again.
    pub(crate) lut: Mutex<Option<LutCache>>,
    pub(crate) core: Mutex<Core>,
    pub(crate) changed: Condvar,
    pub(crate) decode_sessions: AtomicU64,
    /// Stop flag of the running playback, if any. Cleared when the playback thread ends.
    pub(crate) playback: Mutex<Option<Arc<AtomicBool>>>,
    /// Faults armed by the engine's unit tests.
    #[cfg(test)]
    pub(crate) faults: Faults,
}

impl Shared {
    /// The wait timeout in effect. Read at the start of each wait.
    fn progress_timeout(&self) -> Duration {
        *self.wait_timeout.lock()
    }
}

/// A LUT loaded from `path`, valid while the file keeps the same length and modification time.
pub(crate) struct LutCache {
    path: PathBuf,
    stamp: Option<(u64, SystemTime)>,
    lut: Arc<Lut>,
}

/// The file's length and modification time, when the platform reports them.
fn file_stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Loads the `.cube` file at `path`, reusing the last load when the file is unchanged. A file that cannot be loaded is
/// logged and treated as no LUT, so the grade still applies without it.
fn load_lut(shared: &Shared, path: &Path) -> Option<Arc<Lut>> {
    let stamp = file_stamp(path);
    if let Some(cached) = shared.lut.lock().as_ref()
        && cached.path == path
        && stamp.is_some()
        && cached.stamp == stamp
    {
        return Some(Arc::clone(&cached.lut));
    }
    match load_cube(path) {
        Ok(lut) => {
            let lut = Arc::new(lut);
            *shared.lut.lock() = Some(LutCache {
                path: path.to_path_buf(),
                stamp,
                lut: Arc::clone(&lut),
            });
            Some(lut)
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "LUT could not be loaded; grading without it");
            None
        }
    }
}

impl PreviewEngine {
    /// Creates an engine on `gpu`, decoding with `paths` into `store`, rasterizing captions with `captions`, and starts
    /// the decode worker. The RAM budget is the store's own: the decode window holds at most half of it, and source-time
    /// windows outside the plan at most a quarter. `captions` is the app's shared renderer, so the installed fonts are
    /// not scanned again for every engine.
    ///
    /// # Errors
    ///
    /// [`PreviewError::Render`] when the compositor cannot be built on `gpu`; [`PreviewError::Spawn`] when the worker
    /// thread cannot start.
    pub fn new(
        gpu: Arc<GpuContext>,
        paths: FfmpegPaths,
        store: Arc<FrameStore>,
        captions: Arc<std::sync::Mutex<CaptionRenderer>>,
    ) -> Result<Self, PreviewError> {
        let ram_budget = store.ram_budget();
        let mut compositor = Compositor::new(Arc::clone(&gpu))?;
        warm_up(&mut compositor);
        let shared = Arc::new(Shared {
            gpu,
            paths,
            store,
            ram_budget,
            wait_timeout: Mutex::new(DEFAULT_PROGRESS_TIMEOUT),
            compositor: Mutex::new(compositor),
            captions,
            lut: Mutex::new(None),
            core: Mutex::new(Core::default()),
            changed: Condvar::new(),
            decode_sessions: AtomicU64::new(0),
            playback: Mutex::new(None),
            #[cfg(test)]
            faults: Faults::default(),
        });
        let worker = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(WORKER_THREAD.to_owned())
                .spawn(move || worker_loop(&shared))
                .map_err(PreviewError::Spawn)?
        };
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Sets how long a wait for a frame may go without progress before it fails with [`PreviewError::Timeout`]. It applies
    /// to the waits that start after the call. A zero timeout fails a wait at once. The default is
    /// [`DEFAULT_PROGRESS_TIMEOUT`].
    pub fn set_progress_timeout(&self, timeout: Duration) {
        *self.shared.wait_timeout.lock() = timeout;
    }

    /// The wait timeout in effect. See [`PreviewEngine::set_progress_timeout`].
    pub fn progress_timeout(&self) -> Duration {
        *self.shared.wait_timeout.lock()
    }

    /// Plans `project` for `source` at `viewport` and makes the decodes it needs current.
    ///
    /// Cheap when only the grade, crop, orientation or timing changed: the plan is rebuilt and nothing is decoded.
    /// Decoding starts again only when the source, the decode size (which follows the crop and the viewport) or the chain
    /// changed. The returned info is for full quality.
    ///
    /// # Errors
    ///
    /// [`PreviewError::InvalidViewport`], [`PreviewError::InvalidProject`] (no clip, or another stream) and the
    /// sequencer's errors for an empty clip, a bad frame rate or too many frames.
    pub fn set_project(
        &self,
        source: PreviewSource,
        project: &Project,
        viewport: Viewport,
    ) -> Result<PreviewInfo, PreviewError> {
        validate_viewport(viewport).map_err(PreviewError::InvalidViewport)?;
        let clip = project
            .clip()
            .ok_or_else(|| PreviewError::InvalidProject("the project has no clip".to_owned()))?;
        let video = &source.media.video;
        if clip.stream_index != video.index {
            return Err(PreviewError::InvalidProject(format!(
                "the clip uses stream {} but the probed video stream is {}",
                clip.stream_index, video.index
            )));
        }
        let shared = &*self.shared;

        // Output timing follows export: GIF output is clamped to a rate a GIF can play.
        let is_gif = project.export.format == ExportFormat::Gif;
        let fps = if is_gif {
            clamp_gif_fps(project.output.fps)
        } else {
            project.output.fps
        };
        let plan = sequencer::plan(clip, &project.timing, fps)?;
        let plan_len = plan.frames.len();
        // The grade is prepared outside the core lock: the LUT file may be read here.
        let lut = project
            .grade
            .lut_path
            .as_deref()
            .and_then(|path| load_lut(shared, Path::new(path)));
        let grade = Arc::new(prepare_grade(&project.grade, lut.as_deref()));

        let orientation = loopcut_core::Orientation::new(
            video.rotation,
            project.transform.rotation,
            project.transform.flip_h,
            project.transform.flip_v,
        );
        let crop = if project.transform.crop.is_full() {
            NormRect::FULL
        } else {
            project.transform.crop.clamped()
        };
        let sar = if video.sample_aspect.is_valid() {
            video.sample_aspect.as_f64()
        } else {
            1.0
        };
        let display = core_geometry::display_size(video.width, video.height, sar, orientation);
        let output = core_geometry::output_dimensions(
            f64::from(display.0) * crop.w,
            f64::from(display.1) * crop.h,
            project.output.size,
        );
        let render_full = render_size(output, viewport, PreviewQuality::Full);
        let render_reduced = render_size(output, viewport, PreviewQuality::Reduced);
        let decode_fps = decode_fps(video);
        let ranges = plan_ranges(clip, decode_fps);
        let cap = if shared.gpu.is_software() {
            SOFTWARE_MAX_DECODE_WIDTH
        } else {
            HARDWARE_MAX_DECODE_WIDTH
        };
        let duration = if video.duration > 0.0 {
            video.duration
        } else {
            source.media.duration
        };
        let source_id = SourceId {
            path: source.path.clone(),
            stream: video.index,
            requested: source.decode,
        };
        let info = PreviewInfo {
            output_width: output.0,
            output_height: output.1,
            render_width: render_full.0,
            render_height: render_full.1,
            frames: u32::try_from(plan_len).unwrap_or(u32::MAX),
            fps: plan.fps,
            quality: PreviewQuality::Full,
            delays_cs: plan.frames.iter().map(|frame| frame.delay_cs).collect(),
        };
        let snapshot = Snapshot {
            color: video.color,
            orientation,
            crop,
            background: project.output.background,
            display,
            render_full,
            render_reduced,
        };

        let mut guard = shared.core.lock();
        let core = &mut *guard;
        if core.last_source.as_ref() != Some(&source_id) {
            core.cpu_fallback = false;
            core.last_source = Some(source_id.clone());
        }
        let chain = if core.cpu_fallback {
            DecodeChain::CPU
        } else {
            source.decode
        };
        let current = core
            .active
            .as_ref()
            .filter(|active| active.source == source_id)
            .map(|active| active.decode_size);
        let size = decode_size((video.width, video.height), crop, render_full, cap, current);
        let template = decode_template(&source.path, video, size, chain, decode_fps);
        let same = core
            .active
            .as_ref()
            .is_some_and(|active| active.source == source_id && active.template == template);
        // Requests that wait on the plan end when the plan or the decode changes. A grade, caption or crop edit keeps the
        // plan, so it leaves the requests in flight alone: they draw from the new look at once.
        let plan_changed = core
            .active
            .as_ref()
            .is_none_or(|active| *active.plan != plan);
        if !same || plan_changed {
            core.project_revision += 1;
        }

        // Chunks: each planned range, cut into pieces that fit a quarter of the window.
        let frame_bytes = template.frame_bytes().max(1) as u64;
        let window_budget = shared.ram_budget / 2;
        let chunk_frames = (window_budget / frame_bytes / 4).max(2);
        let chunks: Vec<(usize, TimeRange)> = ranges
            .iter()
            .enumerate()
            .flat_map(|(group, range)| {
                chunk_ranges(*range, decode_fps, chunk_frames)
                    .into_iter()
                    .map(move |chunk| (group, chunk))
            })
            .collect();

        let old = core.active.take();
        let (generation, playhead, plan_entries, adhoc, tick) = match old {
            Some(mut old) if same => {
                let entries = carry_entries(
                    std::mem::take(&mut old.plan_entries),
                    &chunks,
                    decode_fps,
                    frame_bytes,
                    &mut core.next_id,
                    &shared.store,
                );
                let playhead = old
                    .playhead
                    .min(u32::try_from(plan_len.saturating_sub(1)).unwrap_or(0));
                (
                    old.generation,
                    playhead,
                    entries,
                    std::mem::take(&mut old.adhoc),
                    old.tick,
                )
            }
            old => {
                if let Some(mut old) = old {
                    old.drop_all(&shared.store);
                }
                core.generation += 1;
                // A new generation starts with a clean slate: the error of the old decode describes inputs that are gone.
                core.last_error = None;
                let entries = carry_entries(
                    Vec::new(),
                    &chunks,
                    decode_fps,
                    frame_bytes,
                    &mut core.next_id,
                    &shared.store,
                );
                (core.generation, 0, entries, Vec::new(), 0)
            }
        };
        let mut active = Active {
            generation,
            source: source_id,
            template,
            decode_fps,
            decode_size: size,
            coded: (video.width, video.height),
            cap,
            snapshot,
            duration,
            plan: Arc::new(plan),
            is_gif,
            speed: project.timing.speed,
            captions: Arc::new(project.captions.clone()),
            grade: Some(grade),
            plan_entries,
            adhoc,
            playhead,
            window_budget,
            adhoc_budget: shared.ram_budget / 4,
            tick,
        };
        active.retarget(&shared.store, playhead);
        core.active = Some(active);
        shared.changed.notify_all();
        Ok(info)
    }

    /// Moves the decode window to output frame `frame` of the plan. The app calls it on seek and on play; playback
    /// moves it as it advances. Ignored without a project.
    pub fn set_playhead(&self, frame: u32) {
        move_playhead(&self.shared, frame);
    }

    /// The output frame `frame` of the current plan, rendered at `quality`.
    ///
    /// The decode window moves to `frame` first, as a seek does, so frames rendered without a call to
    /// [`PreviewEngine::set_playhead`] keep the store within the window's budget. Waits for the source frames it needs,
    /// decoding them on this thread when the worker has not reached them.
    ///
    /// # Errors
    ///
    /// [`PreviewError::NoProject`], [`PreviewError::FrameOutOfRange`], [`PreviewError::Timeout`] when the awaited decode
    /// makes no progress for the wait timeout, [`PreviewError::Decode`], [`PreviewError::FrameUnavailable`] when the frame
    /// lies in a range that decoded to nothing and no neighbouring range can supply a frame, [`PreviewError::Cancelled`]
    /// when the project changed under the request or the engine shut down, and [`PreviewError::Render`].
    pub fn render_output_frame(
        &self,
        frame: u32,
        quality: PreviewQuality,
    ) -> Result<RenderedFrame, PreviewError> {
        // A frame past the end of the plan leaves the window where it is; the render reports it.
        if plan_frames(&self.shared)? > frame {
            move_playhead(&self.shared, frame);
        }
        let mut out = RenderedFrame::default();
        render(
            &self.shared,
            Want::Plan {
                index: frame,
                quality,
            },
            false,
            &mut out,
        )?;
        Ok(out)
    }

    /// The uncropped source at `source_time`, oriented for display (source rotation, then the project's rotation and
    /// flips) and fitted into `max_width`×`max_height` without enlarging. The crop tool and source scrubbing use it.
    ///
    /// The plan's decode serves the frame when it is wide enough for the box. Otherwise a source-time window is decoded
    /// at the box's size, so a large box is never filled from a small decode. Times outside the plan are decoded in
    /// windows of up to one second.
    ///
    /// # Errors
    ///
    /// [`PreviewError::NoProject`], [`PreviewError::InvalidRequest`] for a non-finite time or an empty box, and the
    /// decode and render errors of [`PreviewEngine::render_output_frame`].
    pub fn render_source_frame(
        &self,
        source_time: f64,
        max_width: u32,
        max_height: u32,
    ) -> Result<RenderedFrame, PreviewError> {
        if !source_time.is_finite() {
            return Err(PreviewError::InvalidRequest(format!(
                "source time {source_time} is not finite"
            )));
        }
        if max_width == 0 || max_height == 0 {
            return Err(PreviewError::InvalidRequest(format!(
                "the box {max_width}x{max_height} is empty"
            )));
        }
        let mut out = RenderedFrame::default();
        render(
            &self.shared,
            Want::Source {
                time: source_time,
                max: (max_width, max_height),
            },
            false,
            &mut out,
        )?;
        Ok(out)
    }

    /// Fraction of the planned source time decoded for the current project, from 0 to 1. Partly decoded ranges count by
    /// their decoded frames, empty ranges count as decoded, and ranges whose decode failed are left out.
    pub fn decode_progress(&self) -> f32 {
        let core = self.shared.core.lock();
        core.active
            .as_ref()
            .map_or(0.0, |active| active.progress().0)
    }

    /// What the app shows about background work: progress, failed ranges, fallbacks, playback and the last error.
    pub fn status(&self) -> PreviewStatus {
        let (decode_progress, failed_ranges, fallbacks, last_error) = {
            let core = self.shared.core.lock();
            let (progress, failed) = core
                .active
                .as_ref()
                .map_or((0.0, 0), |active| active.progress());
            (
                progress,
                failed,
                core.fallbacks.clone(),
                core.last_error.clone(),
            )
        };
        PreviewStatus {
            decode_progress,
            failed_ranges,
            fallbacks,
            playing: self.shared.playback.lock().is_some(),
            last_error,
        }
    }

    /// True when the current project exports GIF. GIF plans play at the centisecond delays of their frames; other formats
    /// play at the plan's frame rate. False without a project.
    pub fn is_gif(&self) -> bool {
        self.shared
            .core
            .lock()
            .active
            .as_ref()
            .is_some_and(|active| active.is_gif)
    }

    /// Number of decode sessions started since the engine was created. Tests use it to check that an edit did not
    /// re-decode.
    pub fn decode_sessions_started(&self) -> u64 {
        self.shared.decode_sessions.load(Ordering::Relaxed)
    }

    /// Returns the hardware-to-CPU decode fallbacks recorded since the last call, oldest first.
    pub fn take_fallbacks(&self) -> Vec<DecodeFallback> {
        std::mem::take(&mut self.shared.core.lock().fallbacks)
    }
}

impl Drop for PreviewEngine {
    fn drop(&mut self) {
        {
            let mut core = self.shared.core.lock();
            core.shutdown = true;
            if let Some(active) = core.active.as_mut() {
                for entry in active.plan_entries.iter().chain(active.adhoc.iter()) {
                    if let Some(running) = entry.running.as_ref() {
                        running.cancel.cancel();
                    }
                }
            }
        }
        if let Some(flag) = self.shared.playback.lock().take() {
            flag.store(true, Ordering::Release);
        }
        self.shared.changed.notify_all();
        // The worker re-checks `shutdown` under the core lock, and the lock is released above, so this cannot hang on
        // a lock the worker needs.
        if let Some(handle) = self.worker.lock().take() {
            let _ = handle.join();
        }
        // The worker has stopped, so no decode of this plan will store frames again: remove the plan's frames.
        let mut core = self.shared.core.lock();
        if let Some(mut active) = core.active.take() {
            active.drop_all(&self.shared.store);
        }
    }
}

/// Moves the window to `frame` when that changes the playhead. Shared by the public method and playback.
pub(crate) fn move_playhead(shared: &Shared, frame: u32) {
    let mut guard = shared.core.lock();
    let Some(active) = guard.active.as_mut() else {
        return;
    };
    let len = active.plan.frames.len();
    if len == 0 {
        return;
    }
    let frame = u32::try_from((frame as usize).min(len - 1)).unwrap_or(0);
    if frame != active.playhead {
        active.retarget(&shared.store, frame);
        shared.changed.notify_all();
    }
}

/// Number of output frames in the current plan.
pub(crate) fn plan_frames(shared: &Shared) -> Result<u32, PreviewError> {
    let core = shared.core.lock();
    let active = core.active.as_ref().ok_or(PreviewError::NoProject)?;
    Ok(u32::try_from(active.plan.frames.len()).unwrap_or(u32::MAX))
}

/// Resolves `want`, decodes what it needs, and composites the frame into `out`.
pub(crate) fn render(
    shared: &Shared,
    want: Want,
    playback: bool,
    out: &mut RenderedFrame,
) -> Result<(), PreviewError> {
    let (resolved, frames) = fetch(shared, want, playback)?;
    compose(shared, &resolved, &frames, out)
}

/// Progress state of one wait: the entry awaited, its decoded count, and when that count last changed.
struct Stall {
    entry: u64,
    decoded: u64,
    since: Instant,
}

impl Stall {
    fn new() -> Self {
        Self {
            entry: u64::MAX,
            decoded: 0,
            since: Instant::now(),
        }
    }
}

/// Resolves `want` and loads its frames. A frame that has left the store is decoded again, a few times.
fn fetch(
    shared: &Shared,
    want: Want,
    playback: bool,
) -> Result<(Resolved, Vec<StoredFrame>), PreviewError> {
    let mut attempts = 0;
    let mut stall = Stall::new();
    // The request belongs to the project it starts under. A later update ends it.
    let revision = shared.core.lock().project_revision;
    loop {
        let resolved = resolve(shared, want, playback, &mut stall, revision)?;
        let mut frames = Vec::with_capacity(resolved.refs.len());
        let mut missing = None;
        for frame in &resolved.refs {
            match shared.store.get(&FrameKey {
                source: frame.key,
                index: frame.index,
            }) {
                Some(stored) => frames.push(stored),
                None => {
                    missing = Some(*frame);
                    break;
                }
            }
        }
        let Some(frame) = missing else {
            return Ok((resolved, frames));
        };
        forget(shared, frame);
        attempts += 1;
        if attempts >= MAX_ATTEMPTS {
            return Err(PreviewError::FrameUnavailable { time: frame.time });
        }
    }
}

/// Looks at the plan until `want` is ready. Runs the claims it is handed on this thread, and waits for decodes made
/// by others.
///
/// A claim cancelled while the project keeps the same revision is run again. A wait, or a run of cancelled claims, ends
/// with [`PreviewError::Timeout`] when nothing progresses for the wait timeout. A change to the plan or the decode, a shutdown, or the
/// cancellation of the request by the playback thread ends it with [`PreviewError::Cancelled`].
fn resolve(
    shared: &Shared,
    want: Want,
    playback: bool,
    stall: &mut Stall,
    revision: u64,
) -> Result<Resolved, PreviewError> {
    let timeout = shared.progress_timeout();
    loop {
        let mut core = shared.core.lock();
        if core.shutdown || core.project_revision != revision {
            return Err(PreviewError::Cancelled);
        }
        match core.next(&shared.store, want, playback)? {
            Next::Ready(resolved) => return Ok(resolved),
            Next::Run(claim) => {
                drop(core);
                let started_at = claim.range.start;
                faults::claim_cancel(shared, &claim);
                match run_claim(shared, claim) {
                    Ok(()) => *stall = Stall::new(),
                    Err(PreviewError::Cancelled) => {
                        // Preempted or released while the project stayed the same: run the frame again, unless nothing
                        // has finished for the whole timeout.
                        if stall.since.elapsed() >= timeout {
                            return Err(PreviewError::Timeout { time: started_at });
                        }
                    }
                    Err(err) => return Err(err),
                }
            }
            Next::Wait {
                entry,
                decoded,
                time,
            } => {
                let now = Instant::now();
                if stall.entry != entry || stall.decoded != decoded {
                    *stall = Stall {
                        entry,
                        decoded,
                        since: now,
                    };
                }
                let idle = now.duration_since(stall.since);
                if idle >= timeout {
                    return Err(PreviewError::Timeout { time });
                }
                if playback {
                    // A playback waits for the frame it needs, so a decode of a farther chunk must not hold it up.
                    core.preempt_farther(entry);
                }
                shared.changed.wait_for(&mut core, timeout - idle);
            }
        }
    }
}

/// Marks a frame's entry as pending again after its frame went missing, so that the next resolve decodes it.
fn forget(shared: &Shared, frame: FrameRef) {
    {
        let mut guard = shared.core.lock();
        let mut stale = Vec::new();
        if let Some(active) = guard.active.as_mut()
            && let Some(entry) = active.entry_mut(frame.entry)
            && entry.key() == Some(frame.key)
        {
            stale.extend(release(entry));
        }
        remove_sources(&shared.store, stale);
    }
    shared.changed.notify_all();
}

/// Composites `frames` (one frame, or the two of a crossfade) into `out`. Plan frames get the project's grade and
/// captions; source frames get neither.
fn compose(
    shared: &Shared,
    resolved: &Resolved,
    frames: &[StoredFrame],
    out: &mut RenderedFrame,
) -> Result<(), PreviewError> {
    let first = frames
        .first()
        .ok_or_else(|| PreviewError::Decode("no source frame was resolved".to_owned()))?;
    let snapshot = &resolved.snapshot;
    let input_a = input_frame(first, snapshot.color);
    let input_b = match (frames.get(1), resolved.weight) {
        (Some(second), Some(_)) => Some(input_frame(second, snapshot.color)),
        _ => None,
    };
    let blend = input_b.as_ref().zip(resolved.weight);
    let params = CompositeParams {
        output_width: resolved.size.0,
        output_height: resolved.size.1,
        crop: resolved.crop,
        orientation: snapshot.orientation,
        background: snapshot.background,
    };
    let owned: Vec<OwnedOverlay> = match &resolved.look {
        Some(look) => shared
            .captions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .layers_at(
                &look.captions,
                look.time,
                look.speed,
                resolved.size.0,
                resolved.size.1,
            ),
        None => Vec::new(),
    };
    let overlays: Vec<Overlay<'_>> = owned.iter().map(OwnedOverlay::as_overlay).collect();
    // Source frames are drawn ungraded: set_grade(None) reverts the compositor for them, and a plan frame sets its own.
    let grade = resolved
        .look
        .as_ref()
        .and_then(|look| look.grade.as_deref());
    let mut compositor = shared.compositor.lock();
    compositor.set_grade(grade)?;
    compositor.render_into(&input_a, blend, &overlays, &params, &mut out.rgba)?;
    out.width = resolved.size.0;
    out.height = resolved.size.1;
    Ok(())
}

/// The first composite on a device pays a one-time setup cost (about a second on WARP), whatever the frame size. A
/// two-pixel frame pays it here, when the engine is created, rather than on the first preview frame. Failure is logged
/// and ignored: the next real render reports its own error.
fn warm_up(compositor: &mut Compositor) {
    let data = [0u8; 6];
    let input = InputFrame {
        layout: loopcut_core::PixelLayout::Nv12,
        width: 2,
        height: 2,
        data: &data,
        color: loopcut_core::ColorInfo::SDR_BT709,
    };
    let params = CompositeParams {
        output_width: 2,
        output_height: 2,
        crop: NormRect::FULL,
        orientation: loopcut_core::Orientation::default(),
        background: [0, 0, 0, 255],
    };
    let mut out = Vec::new();
    if let Err(err) = compositor.render_into(&input, None, &[], &params, &mut out) {
        tracing::warn!(error = %err, "the compositor warm-up failed");
    }
}

fn input_frame(frame: &StoredFrame, color: loopcut_core::ColorInfo) -> InputFrame<'_> {
    InputFrame {
        layout: frame.layout,
        width: frame.width,
        height: frame.height,
        data: &frame.data[..],
        color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::decode_limit;

    #[test]
    fn take_id_counts_up() {
        let mut next = 0;
        assert_eq!(crate::model::take_id(&mut next), 0);
        assert_eq!(crate::model::take_id(&mut next), 1);
        assert_eq!(next, 2);
    }

    #[test]
    fn the_engine_can_be_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PreviewEngine>();
        assert_send_sync::<crate::PlaybackHandle>();
    }

    #[test]
    fn the_stall_timer_starts_fresh_for_each_entry() {
        let stall = Stall::new();
        assert_eq!(stall.entry, u64::MAX);
        assert!(Instant::now().duration_since(stall.since) < Duration::from_secs(1));
    }

    #[test]
    fn decode_limit_follows_coded_width_and_cap() {
        assert_eq!(decode_limit(1280, 640), 640);
        assert_eq!(decode_limit(1919, 1920), 1918);
    }

    #[test]
    fn the_default_wait_is_fifteen_seconds() {
        assert_eq!(DEFAULT_PROGRESS_TIMEOUT, Duration::from_secs(15));
    }
}

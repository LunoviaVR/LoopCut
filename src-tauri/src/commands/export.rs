//! Export commands: the platform presets, size estimates, and cancellable exports that report through a channel.
//!
//! An export runs on its own thread and is registered as an `export:` job, so an update can cancel it and nothing can
//! start while the registry is closed. The exporter does the work. This module resolves the clip's media, picks the
//! decode chain and the video encoder from the hardware settings, maps the exporter's events to the wire format and its
//! errors to stable codes.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use loopcut_core::geometry::{self, NormRect, Orientation};
use loopcut_core::hw::EncoderBackend;
use loopcut_core::media::MediaInfo;
use loopcut_core::migrate;
use loopcut_core::presets::{PlatformPreset, platform_presets};
use loopcut_core::project::{Clip, Project};
use loopcut_export::video::video_target;
use loopcut_export::{
    ExportContext, ExportError, ExportEvent as JobEvent, ExportRequest, ExportResult, SizeEstimate,
    estimate_size_with_cancel, run_export,
};
use loopcut_hw::select::choose_encoder;
use loopcut_media::runner::{CancelToken, is_url_like};
use tauri::State;
use tauri::ipc::Channel;

use crate::commands::blocking;
use crate::commands::media::entry_for_clip;
use crate::dto::{ExportEvent, FallbackStage};
use crate::error::{AppError, code};
use crate::netpath::has_accepted_prefix;
use crate::render;
use crate::state::{AppState, EstimateSlot, MediaEntry};
use crate::tokens::new_token;

/// Length of each sampled window of an estimate, in seconds of output.
pub const ESTIMATE_WINDOW_SECONDS: f64 = 0.5;

/// The platform export presets, as versioned data. Each one says whether its limits were verified and when they were
/// last checked.
#[tauri::command]
pub async fn export_presets() -> Result<Vec<PlatformPreset>, AppError> {
    Ok(platform_presets())
}

/// Predicts the export's size from real encodes of up to three short windows. A newer estimate for the same clip
/// supersedes this one: the older estimate's cancel token is set, its encode stops, and it returns `cancelled`.
#[tauri::command]
pub async fn export_estimate(
    state: State<'_, Arc<AppState>>,
    project: Project,
) -> Result<SizeEstimate, AppError> {
    let st = Arc::clone(&state);
    blocking(move || estimate(&st, &project)).await
}

fn estimate(st: &Arc<AppState>, project: &Project) -> Result<SizeEstimate, AppError> {
    migrate::validate(project)?;
    let clip_path = clip_path_of(project)?;
    let key = clip_key(clip_path);
    let (cancel, _job) = st.start_job(&format!("estimate:{key}"))?;
    // Dropped at the end of this call, whichever way it ends, so no finished or failed estimate leaves a slot behind.
    let claim = EstimateClaim::new(st, key, cancel.clone());
    let entry = entry_for_clip(st, clip_path)?;
    let request = request_for(st, project, &entry, PathBuf::new());
    let context = context(st, project)?;
    let result = run_estimate(&context, &request, &cancel);
    if !claim.is_newest() {
        return Err(superseded());
    }
    result.map_err(export_error)
}

/// The estimator for one request, stopped by `cancel`. Windows are sampled at [`ESTIMATE_WINDOW_SECONDS`].
fn run_estimate(
    context: &ExportContext,
    request: &ExportRequest,
    cancel: &CancelToken,
) -> Result<SizeEstimate, ExportError> {
    estimate_size_with_cancel(context, request, ESTIMATE_WINDOW_SECONDS, cancel)
}

/// A clip's claim on its newest size estimate. Claiming it cancels the estimate it supersedes. Dropping the claim forgets
/// it, so only the newest estimate of a clip is ever recorded.
struct EstimateClaim<'a> {
    st: &'a AppState,
    key: String,
    generation: u64,
}

impl<'a> EstimateClaim<'a> {
    fn new(st: &'a AppState, key: String, cancel: CancelToken) -> Self {
        let generation = claim_estimate(st, &key, cancel);
        Self {
            st,
            key,
            generation,
        }
    }

    /// True while no newer estimate of this clip has been claimed.
    fn is_newest(&self) -> bool {
        is_newest_estimate(self.st, &self.key, self.generation)
    }
}

impl Drop for EstimateClaim<'_> {
    fn drop(&mut self) {
        release_estimate(self.st, &self.key, self.generation);
    }
}

/// Records a new estimate for `key` with its cancel token, cancels the token of the estimate it supersedes, and returns
/// the new generation. The generation is taken under the lock, so the newest generation is always the one recorded last.
fn claim_estimate(st: &AppState, key: &str, cancel: CancelToken) -> u64 {
    let mut estimates = st.estimates.lock();
    let generation = st.estimate_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let previous = estimates.insert(key.to_owned(), EstimateSlot { generation, cancel });
    drop(estimates);
    if let Some(superseded) = previous {
        superseded.cancel.cancel();
    }
    generation
}

/// True while `generation` is still the newest estimate for `key`.
fn is_newest_estimate(st: &AppState, key: &str, generation: u64) -> bool {
    st.estimates
        .lock()
        .get(key)
        .is_some_and(|slot| slot.generation == generation)
}

/// Forgets `key` when its estimate `generation` is still the newest one.
fn release_estimate(st: &AppState, key: &str, generation: u64) {
    let mut estimates = st.estimates.lock();
    if estimates
        .get(key)
        .is_some_and(|slot| slot.generation == generation)
    {
        estimates.remove(key);
    }
}

fn superseded() -> AppError {
    AppError::new(
        code::CANCELLED,
        "A newer estimate for this clip replaced this one.",
    )
}

/// Starts an export and returns its job id at once. Progress, fallbacks, completion and failure arrive on `onEvent`.
/// The output is written to `<outputPath>.partial` and renamed into place only when the encoder has finished.
#[tauri::command]
pub async fn export_start(
    state: State<'_, Arc<AppState>>,
    project: Project,
    output_path: String,
    on_event: Channel<ExportEvent>,
) -> Result<String, AppError> {
    let st = Arc::clone(&state);
    blocking(move || start(&st, project, &output_path, on_event)).await
}

fn start(
    st: &Arc<AppState>,
    project: Project,
    raw_output: &str,
    channel: Channel<ExportEvent>,
) -> Result<String, AppError> {
    migrate::validate(&project)?;
    let output = output_path(raw_output)?;
    let job_id = format!("export:{}", new_token());
    let cancel = CancelToken::new();
    // Registered before any GPU work, so a closed registry refuses before a device is opened.
    let guard = st.register_job(&job_id, cancel.clone(), "That export is already running.")?;
    let entry = entry_for_clip(st, clip_path_of(&project)?)?;
    let request = request_for(st, &project, &entry, output);
    let context = context(st, &project)?;
    let job_state = Arc::clone(st);
    thread::Builder::new()
        .name("loopcut-export".to_owned())
        .spawn(move || {
            // The guard unregisters the job when the thread ends, however it ends.
            let _guard = guard;
            run_job(&job_state, &context, &request, &channel, &cancel);
        })
        .map_err(|err| AppError::io("The export could not start", &err))?;
    tracing::info!(job = %job_id, "export started");
    Ok(job_id)
}

/// Hands one exporter event to the webview through `send`, which returns false when the channel no longer accepts
/// events. A refused send stops the export (nothing reads the rest), so it is cancelled through `cancel`. Only the
/// first refusal cancels: the export may still report a few events before it stops.
fn forward_event(
    event: &JobEvent,
    send: &mut dyn FnMut(ExportEvent) -> bool,
    cancel: &CancelToken,
) {
    let Some(dto) = progress_event(event) else {
        return;
    };
    if !send(dto) && !cancel.is_cancelled() {
        tracing::warn!("the export's event channel closed; cancelling the export");
        cancel.cancel();
    }
}

/// Runs one export on its thread and reports the outcome on `channel`: progress and fallbacks as they happen, then
/// exactly one of `done` or `error`.
fn run_job(
    st: &AppState,
    context: &ExportContext,
    request: &ExportRequest,
    channel: &Channel<ExportEvent>,
    cancel: &CancelToken,
) {
    let mut sink = |event: JobEvent| {
        forward_event(&event, &mut |dto| channel.send(dto).is_ok(), cancel);
    };
    match run_export(context, request, &mut sink, cancel) {
        Ok(result) => {
            tracing::info!(
                bytes = result.bytes,
                frames = result.frames,
                seconds = result.seconds,
                "export finished"
            );
            let _ = channel.send(done_event(&result));
        }
        Err(err) => {
            if matches!(err, ExportError::GpuLost) {
                render::on_device_lost(st);
            }
            tracing::warn!(error = %err, "export failed");
            let _ = channel.send(ExportEvent::Error {
                error: export_error(err),
            });
        }
    }
}

/// Cancels an export. Cancelling a job that already finished does nothing.
#[tauri::command]
pub async fn export_cancel(
    state: State<'_, Arc<AppState>>,
    job_id: String,
) -> Result<(), AppError> {
    check_export_job_id(&job_id)?;
    state.jobs.cancel_family(&job_id);
    Ok(())
}

/// Only ids that `export_start` returned are accepted, so this command cannot cancel other kinds of job.
pub fn check_export_job_id(job_id: &str) -> Result<(), AppError> {
    if job_id.starts_with("export:") {
        Ok(())
    } else {
        Err(AppError::invalid_argument("That is not an export job id."))
    }
}

/// The exporter's progress and fallback events as wire events. The finished file is reported by [`done_event`], which
/// also carries the frame count.
pub fn progress_event(event: &JobEvent) -> Option<ExportEvent> {
    match event {
        JobEvent::Progress {
            frames_done,
            frames_total,
            bytes,
        } => Some(ExportEvent::Progress {
            frames_done: *frames_done,
            frames_total: *frames_total,
            bytes: *bytes,
        }),
        JobEvent::Fallback {
            stage,
            from,
            to,
            reason,
        } => FallbackStage::parse(stage).map(|stage| ExportEvent::Fallback {
            stage,
            from: from.clone(),
            to: to.clone(),
            reason: reason.clone(),
        }),
        JobEvent::TargetPass {
            pass,
            description,
            bytes,
        } => Some(ExportEvent::TargetPass {
            pass: *pass,
            settings: description.clone(),
            bytes: *bytes,
        }),
        JobEvent::Done { .. } => None,
    }
}

/// The wire event for a finished export.
pub fn done_event(result: &ExportResult) -> ExportEvent {
    ExportEvent::Done {
        path: result.path.to_string_lossy().into_owned(),
        bytes: result.bytes,
        seconds: result.seconds,
        frames: result.frames,
    }
}

/// Maps an export failure to the app error. Cancellation is `cancelled`; a lost device is `gpu_lost`; requests the build
/// does not support are `not_implemented`.
pub fn export_error(err: ExportError) -> AppError {
    match err {
        ExportError::Cancelled => AppError::new(code::CANCELLED, "The export was cancelled."),
        ExportError::InvalidRequest(message) => AppError::invalid_argument(message),
        ExportError::Unsupported(what) => AppError::new(
            code::NOT_IMPLEMENTED,
            format!("{what} is not available yet"),
        ),
        ExportError::Plan(err) => AppError::new(code::INVALID_PROJECT, err.to_string()),
        ExportError::Media(err) => AppError::from(err),
        ExportError::Gif(err) => {
            AppError::new(code::GIF_FAILED, "The GIF encoder failed.").with_detail(err.to_string())
        }
        ExportError::Render(err) => AppError::new(
            code::GPU_FAILED,
            "The GPU renderer failed during the export.",
        )
        .with_detail(err.to_string()),
        ExportError::GpuLost => AppError::new(
            code::GPU_LOST,
            "The GPU device was lost during the export. Export again; the software renderer will be used.",
        ),
        ExportError::DecodeEmpty { start } => AppError::new(
            code::FFMPEG_FAILED,
            format!("The decoder produced no frames from source time {start} s."),
        ),
        ExportError::FrameUnavailable { time } => AppError::new(
            code::FFMPEG_FAILED,
            format!("No decoded frame is available for source time {time} s."),
        ),
        ExportError::TooLarge(message) => AppError::invalid_argument(message),
        ExportError::Lut { path, reason } => {
            AppError::invalid_argument(format!("The LUT {path} cannot be used: {reason}"))
        }
        ExportError::TargetUnreachable {
            target_bytes,
            best_bytes,
        } => AppError::new(
            code::TARGET_UNREACHABLE,
            target_unreachable_message(target_bytes, best_bytes),
        ),
        ExportError::Io(err) => AppError::io("The export could not write its file", &err),
    }
}

/// Message for a size target that no setting reached. `best_bytes` is 0 when every step was ruled out by its estimate,
/// so nothing was encoded and there is no real size to report. Sizes use decimal megabytes, as the frontend does.
fn target_unreachable_message(target_bytes: u64, best_bytes: u64) -> String {
    let megabytes = |bytes: u64| format!("{:.1} MB", bytes as f64 / 1_000_000.0);
    if best_bytes == 0 {
        format!(
            "The output cannot fit in {}: even the smallest settings are estimated to be larger. Shorten the clip or \
             lower the size, frame rate or colors.",
            megabytes(target_bytes)
        )
    } else {
        format!(
            "The output cannot fit in {}; the smallest export reached {}.",
            megabytes(target_bytes),
            megabytes(best_bytes)
        )
    }
}

/// The clip path of a project, which must name the probed media.
fn clip_path_of(project: &Project) -> Result<&str, AppError> {
    project
        .clip()
        .map(|clip: &Clip| clip.path.as_str())
        .ok_or_else(|| AppError::invalid_argument("The project has no clip."))
}

/// Estimate bookkeeping key for a clip: its path, case-folded on Windows.
fn clip_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_ascii_lowercase()
    } else {
        path.to_owned()
    }
}

/// The exporter's shared context: the FFmpeg paths, the GPU renderer, the frame store and, for a project with captions,
/// the app's caption rasterizer (so the installed fonts are scanned once, not per export).
fn context(st: &AppState, project: &Project) -> Result<ExportContext, AppError> {
    Ok(ExportContext {
        paths: st.ffmpeg()?.clone(),
        gpu: render::gpu(st)?,
        framestore: render::frame_store(st)?,
        captions: (!project.captions.is_empty()).then(|| st.caption_renderer()),
    })
}

/// The request for `project` against the probed media of its clip, writing to `output`.
fn request_for(
    st: &AppState,
    project: &Project,
    entry: &MediaEntry,
    output: PathBuf,
) -> ExportRequest {
    ExportRequest {
        project: project.clone(),
        media: (*entry.info).clone(),
        output,
        decode: st.decode_chain_for(&entry.info.video),
        encoder: export_encoder(st, project, &entry.info),
    }
}

/// The video encoder for the project's format: the hardware encoder the settings and probes allow at the output size,
/// else the CPU. GIF has no video encoder, so it gets the CPU value, which the exporter ignores.
fn export_encoder(st: &AppState, project: &Project, info: &MediaInfo) -> EncoderBackend {
    let Some((codec, _container)) = video_target(project.export.format) else {
        return EncoderBackend::Cpu;
    };
    let (width, height) = output_size(project, info);
    let caps = st.caps.read();
    let settings = st.settings.read();
    match caps.as_ref() {
        Some(caps) => choose_encoder(caps, &settings.hw, codec, width, height, false),
        None => EncoderBackend::Cpu,
    }
}

/// The output frame size of a project: the cropped, oriented display size scaled to the project's output size. Matches
/// what the preview engine computes, so the encoder is chosen for the size that is written.
pub fn output_size(project: &Project, info: &MediaInfo) -> (u32, u32) {
    let video = &info.video;
    let sample_aspect = if video.sample_aspect.is_valid() {
        video.sample_aspect.as_f64()
    } else {
        1.0
    };
    let transform = &project.transform;
    let orientation = Orientation::new(
        video.rotation,
        transform.rotation,
        transform.flip_h,
        transform.flip_v,
    );
    let (display_w, display_h) =
        geometry::display_size(video.width, video.height, sample_aspect, orientation);
    let crop = if transform.crop.is_full() {
        NormRect::FULL
    } else {
        transform.crop.clamped()
    };
    geometry::output_dimensions(
        f64::from(display_w) * crop.w,
        f64::from(display_h) * crop.h,
        project.output.size,
    )
}

/// Checks an output path named by the webview: a drive-letter or UNC path, not a URL, not a LoopCut project file. The
/// exporter checks more (the source clip itself, the folder) before it writes anything.
pub fn output_path(raw: &str) -> Result<PathBuf, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument("No output file was given."));
    }
    if is_url_like(trimmed) || !has_accepted_prefix(trimmed) {
        return Err(AppError::invalid_argument(
            "The output must be a local path with a drive letter or a UNC share.",
        ));
    }
    let path = PathBuf::from(trimmed);
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("loopcut"))
    {
        return Err(AppError::invalid_argument(
            "That name is a LoopCut project file; choose another output name.",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::{sample_info, state_under};
    use loopcut_core::hw::VideoCodec;
    use loopcut_core::project::{Clip, ExportFormat};
    use loopcut_core::time::TimeRange;

    fn project() -> Project {
        Project::new(
            "p",
            Clip {
                id: "c1".into(),
                path: r"C:\videos\clip.mp4".into(),
                stream_index: 0,
                segments: vec![TimeRange::new(0.0, 2.0)],
                holds: vec![],
            },
        )
    }

    #[test]
    fn an_unreachable_target_reports_a_real_size_or_says_nothing_was_encoded() {
        let reached = target_unreachable_message(1_000_000, 1_234_567);
        assert!(
            reached.contains("1.0 MB") && reached.contains("1.2 MB"),
            "{reached}"
        );
        let estimated = target_unreachable_message(1_000_000, 0);
        assert!(estimated.contains("estimated"), "{estimated}");
        assert!(!estimated.contains("0.0 MB"), "{estimated}");
    }

    #[test]
    fn progress_and_fallbacks_map_to_wire_events() {
        let progress = progress_event(&JobEvent::Progress {
            frames_done: 3,
            frames_total: 10,
            bytes: 99,
        });
        assert_eq!(
            serde_json::to_value(progress.expect("mapped")).expect("json"),
            serde_json::json!({"kind": "progress", "framesDone": 3, "framesTotal": 10, "bytes": 99})
        );
        let fallback = progress_event(&JobEvent::Fallback {
            stage: "encode".into(),
            from: "amf".into(),
            to: "cpu".into(),
            reason: "device".into(),
        });
        assert_eq!(
            serde_json::to_value(fallback.expect("mapped")).expect("json"),
            serde_json::json!({"kind": "fallback", "stage": "encode", "from": "amf", "to": "cpu", "reason": "device"})
        );
    }

    #[test]
    fn a_refused_event_channel_cancels_the_export_once() {
        let cancel = CancelToken::new();
        let progress = JobEvent::Progress {
            frames_done: 1,
            frames_total: 2,
            bytes: 3,
        };
        let mut accepted = 0;
        forward_event(
            &progress,
            &mut |_| {
                accepted += 1;
                true
            },
            &cancel,
        );
        assert_eq!(accepted, 1);
        assert!(!cancel.is_cancelled(), "an accepted event cancels nothing");

        forward_event(&progress, &mut |_| false, &cancel);
        assert!(cancel.is_cancelled(), "a refused event stops the export");

        let mut later = 0;
        forward_event(
            &progress,
            &mut |_| {
                later += 1;
                false
            },
            &cancel,
        );
        assert_eq!(
            later, 1,
            "the send is still attempted, but nothing else changes"
        );
    }

    #[test]
    fn the_finished_file_is_not_forwarded_as_a_progress_event() {
        let cancel = CancelToken::new();
        let done = JobEvent::Done {
            path: PathBuf::from(r"C:\out\a.gif"),
            bytes: 10,
            seconds: 0.5,
        };
        let mut sent = false;
        forward_event(
            &done,
            &mut |_| {
                sent = true;
                false
            },
            &cancel,
        );
        assert!(!sent, "the terminal event is sent by run_job itself");
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn unknown_fallback_stages_are_not_shown_as_real_stages() {
        let event = progress_event(&JobEvent::Fallback {
            stage: "mux".into(),
            from: "a".into(),
            to: "b".into(),
            reason: "c".into(),
        });
        assert!(event.is_none());
    }

    #[test]
    fn the_finished_file_is_reported_once_with_its_frame_count() {
        let done = JobEvent::Done {
            path: PathBuf::from(r"C:\out\a.gif"),
            bytes: 10,
            seconds: 0.5,
        };
        assert!(
            progress_event(&done).is_none(),
            "the crate's Done is not forwarded"
        );
        let result = ExportResult {
            path: PathBuf::from(r"C:\out\a.gif"),
            bytes: 10,
            frames: 4,
            seconds: 0.5,
        };
        assert_eq!(
            serde_json::to_value(done_event(&result)).expect("json"),
            serde_json::json!({"kind": "done", "path": r"C:\out\a.gif", "bytes": 10, "seconds": 0.5, "frames": 4})
        );
    }

    #[test]
    fn export_failures_keep_their_codes() {
        assert_eq!(export_error(ExportError::Cancelled).code, "cancelled");
        assert_eq!(export_error(ExportError::GpuLost).code, "gpu_lost");
        assert_eq!(
            export_error(ExportError::InvalidRequest("x".into())).code,
            "invalid_argument"
        );
        assert_eq!(
            export_error(ExportError::Unsupported("the Fast engine".into())).code,
            "not_implemented"
        );
        assert_eq!(
            export_error(ExportError::TooLarge("too big".into())).code,
            "invalid_argument"
        );
        let io = export_error(ExportError::Io(std::io::Error::other("disk full")));
        assert_eq!(io.code, "io");
        assert!(io.detail.is_some());
    }

    #[test]
    fn output_paths_must_be_local_and_not_project_files() {
        assert!(output_path(r"C:\out\clip.gif").is_ok());
        assert!(output_path(r"\\nas\share\clip.gif").is_ok());
        for bad in [
            "",
            "   ",
            r"\\.\C:\x.gif",
            "https://e.example/x.gif",
            r"C:\out\p.loopcut",
            "clip.gif",
        ] {
            assert_eq!(
                output_path(bad).err().map(|e| e.code),
                Some("invalid_argument".to_owned()),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn only_export_job_ids_can_be_cancelled_here() {
        assert!(check_export_job_id("export:0123abcd").is_ok());
        assert_eq!(
            check_export_job_id("media:0123:proxy")
                .err()
                .map(|e| e.code),
            Some("invalid_argument".to_owned())
        );
    }

    #[test]
    fn a_newer_estimate_supersedes_the_older_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let key = clip_key(r"C:\videos\clip.mp4");
        let first = claim_estimate(&state, &key, CancelToken::new());
        assert!(is_newest_estimate(&state, &key, first));
        let second = claim_estimate(&state, &key, CancelToken::new());
        assert!(
            !is_newest_estimate(&state, &key, first),
            "the older one is superseded"
        );
        assert!(is_newest_estimate(&state, &key, second));
        release_estimate(&state, &key, first);
        assert!(
            is_newest_estimate(&state, &key, second),
            "releasing an old one keeps the new"
        );
        release_estimate(&state, &key, second);
        assert!(state.estimates.lock().is_empty());
    }

    #[test]
    fn claiming_an_estimate_cancels_the_token_of_the_one_it_supersedes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let key = clip_key(r"C:\videos\clip.mp4");
        let old = CancelToken::new();
        let _first = claim_estimate(&state, &key, old.clone());
        let fresh = CancelToken::new();
        let _second = claim_estimate(&state, &key, fresh.clone());
        assert!(old.is_cancelled(), "the superseded encode is stopped");
        assert!(!fresh.is_cancelled());
    }

    #[test]
    fn another_clip_is_not_superseded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let mine = CancelToken::new();
        let _a = claim_estimate(&state, "c:\\a.mp4", mine.clone());
        let _b = claim_estimate(&state, "c:\\b.mp4", CancelToken::new());
        assert!(!mine.is_cancelled());
    }

    #[test]
    fn dropping_a_claim_releases_its_slot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let key = clip_key(r"C:\videos\clip.mp4");
        {
            let claim = EstimateClaim::new(&state, key.clone(), CancelToken::new());
            assert!(claim.is_newest());
            assert_eq!(state.estimates.lock().len(), 1);
        }
        assert!(
            state.estimates.lock().is_empty(),
            "a claim that ends, by success or failure, leaves nothing behind"
        );
    }

    #[test]
    fn a_superseded_claim_does_not_remove_the_newer_slot_when_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let key = clip_key(r"C:\videos\clip.mp4");
        let old = EstimateClaim::new(&state, key.clone(), CancelToken::new());
        let new = EstimateClaim::new(&state, key.clone(), CancelToken::new());
        assert!(!old.is_newest());
        drop(old);
        assert!(new.is_newest(), "the older claim leaves the newer one");
        drop(new);
        assert!(state.estimates.lock().is_empty());
    }

    #[test]
    fn a_gif_encoder_failure_has_its_own_stable_code() {
        let err = export_error(ExportError::Gif(
            loopcut_export::gif::GifError::WriterStopped,
        ));
        assert_eq!(err.code, "gif_failed");
        assert!(err.detail.is_some());
    }

    #[test]
    fn clip_keys_ignore_case_on_windows() {
        if cfg!(windows) {
            assert_eq!(clip_key(r"C:\A.mp4"), clip_key(r"c:\a.MP4"));
        }
    }

    #[test]
    fn output_size_follows_the_crop_and_stays_even() {
        let info = sample_info();
        let full = project();
        let (w, h) = output_size(&full, &info);
        assert!(w > 0 && h > 0 && w % 2 == 0 && h % 2 == 0, "{w}x{h}");
        let mut cropped = project();
        cropped.transform.crop = NormRect {
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let (cw, _) = output_size(&cropped, &info);
        assert!(cw <= w, "a narrower crop is not wider");
    }

    #[test]
    fn the_encoder_for_a_gif_is_the_cpu_placeholder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let gif = project();
        assert_eq!(gif.export.format, ExportFormat::Gif);
        assert_eq!(
            export_encoder(&state, &gif, &sample_info()),
            EncoderBackend::Cpu
        );
    }

    #[test]
    fn presets_come_from_the_versioned_list() {
        let presets = platform_presets();
        assert!(!presets.is_empty());
        assert!(presets.iter().all(|p| !p.checked_on.is_empty()));
    }

    #[test]
    fn the_codec_for_each_video_format_is_known() {
        assert_eq!(
            video_target(ExportFormat::Mp4H264).map(|(codec, _)| codec),
            Some(VideoCodec::H264)
        );
        assert!(video_target(ExportFormat::Gif).is_none());
    }
}

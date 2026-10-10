//! Preview commands: the project shown in the editor's preview panel, frames on demand, playback and status.
//!
//! The engine lives in `loopcut-preview`. This module resolves the clip's media, checks the arguments and moves frames
//! to the webview as raw bytes: an 8-byte header (`u32` little-endian width and height), then straight-alpha RGBA.
//! Renders run on the blocking pool, because a frame can wait for the decoder.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use loopcut_core::migrate;
use loopcut_core::project::Project;
use loopcut_preview::{
    FrameSink, PlaybackHandle, PreviewInfo, PreviewQuality, PreviewSource, PreviewStatus,
    RenderedFrame, Viewport,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use ts_rs::TS;

use crate::commands::blocking;
use crate::commands::media::entry_for_clip;
use crate::error::AppError;
use crate::render;
use crate::state::{AppState, JobGuard};

/// Largest source box a preview frame may ask for, per side, in pixels.
pub const MAX_SOURCE_BOX: u32 = 8192;
/// Source box used when a request names no size and no viewport is known.
const DEFAULT_SOURCE_BOX: (u32, u32) = (1920, 1080);

/// Which frame `preview_frame` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PreviewMode {
    /// Exactly what export produces for the output frame: crop, rotation, grade and captions, fitted into the viewport.
    Output,
    /// The uncropped, display-oriented source at a source time, fitted into the requested box.
    Source,
}

/// A checked `preview_frame` request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameRequest {
    /// Output frame `frame` of the plan, at full quality.
    Output { frame: u32 },
    /// The source at `time` seconds, fitted into `max_width` by `max_height` pixels.
    Source {
        time: f64,
        max_width: u32,
        max_height: u32,
    },
}

/// Checks a `preview_frame` request. Source frames need a finite, non-negative `sourceTime`. Without an explicit box, the
/// viewport of the last `preview_set_project` gives the size (scaled by its device pixel ratio).
///
/// # Errors
///
/// `invalid_argument` for a bad time or a box side outside `1..=MAX_SOURCE_BOX`.
pub fn frame_request(
    frame: u32,
    mode: PreviewMode,
    source_time: Option<f64>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    viewport: Option<Viewport>,
) -> Result<FrameRequest, AppError> {
    match mode {
        PreviewMode::Output => Ok(FrameRequest::Output { frame }),
        PreviewMode::Source => {
            let time = source_time
                .filter(|t| t.is_finite() && *t >= 0.0)
                .ok_or_else(|| {
                    AppError::invalid_argument(
                        "sourceTime must be a finite, non-negative number of seconds.",
                    )
                })?;
            let (fallback_w, fallback_h) = viewport_box(viewport);
            Ok(FrameRequest::Source {
                time,
                max_width: box_side(max_width.unwrap_or(fallback_w))?,
                max_height: box_side(max_height.unwrap_or(fallback_h))?,
            })
        }
    }
}

/// One side of a source box, checked against `1..=MAX_SOURCE_BOX`.
fn box_side(value: u32) -> Result<u32, AppError> {
    if (1..=MAX_SOURCE_BOX).contains(&value) {
        Ok(value)
    } else {
        Err(AppError::invalid_argument(format!(
            "maxWidth and maxHeight must be between 1 and {MAX_SOURCE_BOX} pixels."
        )))
    }
}

/// The device-pixel size of a viewport, clamped to a usable source box. Without a viewport, [`DEFAULT_SOURCE_BOX`].
fn viewport_box(viewport: Option<Viewport>) -> (u32, u32) {
    let Some(viewport) = viewport else {
        return DEFAULT_SOURCE_BOX;
    };
    let scale = |css: u32| {
        let pixels = (f64::from(css) * f64::from(viewport.dpr)).round();
        if pixels.is_finite() {
            (pixels as u32).clamp(1, MAX_SOURCE_BOX)
        } else {
            1
        }
    };
    (scale(viewport.width), scale(viewport.height))
}

/// Sets the project the preview shows. The clip's media must have been opened or imported, so its probe is known. The
/// decode chain comes from the hardware settings. Returns the planned sequence (frames, rate, delays) and the sizes to
/// draw at.
#[tauri::command]
pub async fn preview_set_project(
    state: State<'_, Arc<AppState>>,
    project: Project,
    viewport: Viewport,
) -> Result<PreviewInfo, AppError> {
    let st = Arc::clone(&state);
    blocking(move || set_project(&st, &project, viewport)).await
}

fn set_project(
    st: &AppState,
    project: &Project,
    viewport: Viewport,
) -> Result<PreviewInfo, AppError> {
    // Registered first: an update that closes the registry after this point waits for the call, and one that closed it
    // earlier refuses the call here.
    let (_cancel, _job) = st.start_job("preview:project")?;
    migrate::validate(project)?;
    let clip = project
        .clip()
        .ok_or_else(|| AppError::invalid_argument("The project has no clip."))?;
    let entry = entry_for_clip(st, &clip.path)?;
    let source = PreviewSource {
        path: entry.path.clone(),
        media: (*entry.info).clone(),
        decode: st.decode_chain_for(&entry.info.video),
    };
    let engine = render::preview_engine(st)?;
    let info = engine
        .set_project(source, project, viewport)
        .map_err(|err| render::preview_error(st, err))?;
    st.preview.lock().viewport = Some(viewport);
    Ok(info)
}

/// One preview frame as raw bytes (8-byte header, then RGBA). `mode: "output"` renders what export produces for the frame
/// at full quality; `mode: "source"` renders the uncropped source at `sourceTime`, fitted into `maxWidth` by
/// `maxHeight` (default: the viewport). The viewport is read and the request checked on the blocking pool with the
/// render. Refused with `busy` while the registry is closed for an update.
#[tauri::command]
pub async fn preview_frame(
    state: State<'_, Arc<AppState>>,
    frame: u32,
    mode: PreviewMode,
    source_time: Option<f64>,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> Result<Response, AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let (_cancel, _job) = st.start_job("preview:frame")?;
        let viewport = st.preview.lock().viewport;
        let request = frame_request(frame, mode, source_time, max_width, max_height, viewport)?;
        let rendered = render_frame(&st, request)?;
        Ok(Response::new(rendered.to_wire()))
    })
    .await
}

fn render_frame(st: &AppState, request: FrameRequest) -> Result<RenderedFrame, AppError> {
    let engine = render::current_engine(st)?;
    let result = match request {
        FrameRequest::Output { frame } => {
            // A seek: the decoder moves its window to the frame it is asked for.
            engine.set_playhead(frame);
            engine.render_output_frame(frame, PreviewQuality::Full)
        }
        FrameRequest::Source {
            time,
            max_width,
            max_height,
        } => engine.render_source_frame(time, max_width, max_height),
    };
    result.map_err(|err| render::preview_error(st, err))
}

/// How often `preview_play` checks whether its playback thread has ended.
const PLAYBACK_POLL: Duration = Duration::from_millis(10);

/// Plays the preview from `fromFrame` at the plan's pace, sending each frame to `onFrame` in the `preview_frame` format.
/// The command resolves when playback ends: the end of the plan (unless `loop`), `preview_stop`, a newer `preview_play`,
/// or the webview closing the channel. If playback stopped on an error, the command rejects with that error, mapped to
/// its stable code (a lost device is `gpu_lost` and switches the renderer to software).
#[tauri::command]
pub async fn preview_play(
    state: State<'_, Arc<AppState>>,
    from_frame: u32,
    r#loop: bool,
    on_frame: Channel<InvokeResponseBody>,
) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    let (handle, generation, job) = {
        let st = Arc::clone(&st);
        blocking(move || start_playback(&st, from_frame, r#loop, on_frame)).await?
    };
    let outcome = {
        let st = Arc::clone(&st);
        blocking(move || wait_playback(&st, handle, job)).await
    };
    finish_playback(&st, generation);
    outcome
}

/// Waits until the playback thread has ended, then reports the error that stopped it, if one did. A cancelled playback
/// (a stop, a newer play, the engine dropped at an update) has no error and resolves. `job` is the playback's registry
/// guard. It is held until the thread has ended, so an update's drain waits for the whole playback.
fn wait_playback(st: &AppState, handle: PlaybackHandle, job: JobGuard) -> Result<(), AppError> {
    while handle.is_running() {
        thread::sleep(PLAYBACK_POLL);
    }
    drop(job);
    match handle.take_error() {
        None => Ok(()),
        Some(err) => Err(render::preview_error(st, err)),
    }
}

/// Starts playback on the engine's thread. Returns the handle, the generation that owns the `playing` flag and the
/// registry guard of the playback. The flag and the generation change only once the engine has started the playback, so
/// a refused start leaves the previous state as it was.
fn start_playback(
    st: &AppState,
    from: u32,
    looping: bool,
    channel: Channel<InvokeResponseBody>,
) -> Result<(PlaybackHandle, u64, JobGuard), AppError> {
    let (_cancel, job) = st.start_job("preview:play")?;
    let engine = render::current_engine(st)?;
    engine.set_playhead(from);
    let sink: FrameSink = Box::new(move |frame: &RenderedFrame| {
        channel
            .send(InvokeResponseBody::Raw(frame.to_wire()))
            .is_ok()
    });
    let handle = engine
        .play(from, looping, sink)
        .map_err(|err| render::preview_error(st, err))?;
    let generation = {
        let mut session = st.preview.lock();
        session.play_generation += 1;
        session.playing = true;
        session.play_generation
    };
    Ok((handle, generation, job))
}

/// Clears the `playing` flag, unless a newer play or a stop has taken over since.
fn finish_playback(st: &AppState, generation: u64) {
    let mut session = st.preview.lock();
    if session.play_generation == generation {
        session.playing = false;
    }
}

/// Stops the playback started by `preview_play`, if one runs. Returns without waiting for it to end.
#[tauri::command]
pub async fn preview_stop(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let engine = st
            .preview
            .lock()
            .engine
            .as_ref()
            .map(|(_, engine)| Arc::clone(engine));
        if let Some(engine) = engine {
            engine.stop();
        }
        let mut session = st.preview.lock();
        session.play_generation += 1;
        session.playing = false;
        Ok(())
    })
    .await
}

/// Decode progress, failed ranges, hardware fallbacks, playback state and the last error of the preview. Fallbacks are
/// reported by the engine; `playing` follows the app's playback, which starts and ends with `preview_play`.
#[tauri::command]
pub async fn preview_status(state: State<'_, Arc<AppState>>) -> Result<PreviewStatus, AppError> {
    let st = Arc::clone(&state);
    blocking(move || Ok(status(&st))).await
}

fn status(st: &AppState) -> PreviewStatus {
    let engine = st
        .preview
        .lock()
        .engine
        .as_ref()
        .map(|(_, engine)| Arc::clone(engine));
    let playing = st.preview.lock().playing;
    match engine {
        Some(engine) => PreviewStatus {
            playing,
            ..engine.status()
        },
        None => PreviewStatus {
            decode_progress: 0.0,
            failed_ranges: 0,
            fallbacks: Vec::new(),
            playing,
            last_error: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(width: u32, height: u32, dpr: f32) -> Viewport {
        Viewport { width, height, dpr }
    }

    #[test]
    fn output_frames_need_only_an_index() {
        assert_eq!(
            frame_request(7, PreviewMode::Output, None, None, None, None).expect("output"),
            FrameRequest::Output { frame: 7 }
        );
    }

    #[test]
    fn source_frames_need_a_finite_non_negative_time() {
        for bad in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-0.5)] {
            let err =
                frame_request(0, PreviewMode::Source, bad, None, None, None).expect_err("rejected");
            assert_eq!(err.code, "invalid_argument", "{bad:?}");
        }
        assert!(frame_request(0, PreviewMode::Source, Some(0.0), None, None, None).is_ok());
    }

    #[test]
    fn source_boxes_are_checked_and_default_to_the_viewport() {
        let vp = viewport(640, 360, 2.0);
        let request = frame_request(0, PreviewMode::Source, Some(1.5), None, None, Some(vp))
            .expect("default box");
        assert_eq!(
            request,
            FrameRequest::Source {
                time: 1.5,
                max_width: 1280,
                max_height: 720
            }
        );
        let explicit = frame_request(
            0,
            PreviewMode::Source,
            Some(0.0),
            Some(200),
            Some(100),
            Some(vp),
        )
        .expect("explicit box");
        assert_eq!(
            explicit,
            FrameRequest::Source {
                time: 0.0,
                max_width: 200,
                max_height: 100
            }
        );
        for side in [0, MAX_SOURCE_BOX + 1] {
            assert_eq!(
                frame_request(0, PreviewMode::Source, Some(0.0), Some(side), None, None)
                    .expect_err("side out of range")
                    .code,
                "invalid_argument"
            );
        }
    }

    #[test]
    fn without_a_viewport_source_frames_use_the_default_box() {
        let request =
            frame_request(0, PreviewMode::Source, Some(0.0), None, None, None).expect("default");
        assert_eq!(
            request,
            FrameRequest::Source {
                time: 0.0,
                max_width: DEFAULT_SOURCE_BOX.0,
                max_height: DEFAULT_SOURCE_BOX.1
            }
        );
    }

    #[test]
    fn viewport_boxes_are_clamped_to_a_usable_range() {
        assert_eq!(viewport_box(Some(viewport(0, 0, 1.0))), (1, 1));
        assert_eq!(
            viewport_box(Some(viewport(100_000, 10, 1.0))).0,
            MAX_SOURCE_BOX
        );
        assert_eq!(viewport_box(None), DEFAULT_SOURCE_BOX);
    }

    #[test]
    fn a_closed_registry_refuses_the_project_before_any_engine_is_built() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = crate::state::test_support::state_under(dir.path());
        state.jobs.close();
        let project = loopcut_core::project::Project::new(
            "p",
            loopcut_core::project::Clip {
                id: "c1".into(),
                path: r"C:\videos\clip.mp4".into(),
                stream_index: 0,
                segments: vec![loopcut_core::time::TimeRange::new(0.0, 2.0)],
                holds: vec![],
            },
        );
        let err = set_project(&state, &project, viewport(640, 360, 1.0)).expect_err("refused");
        assert_eq!(err.code, "busy");
        assert!(state.preview.lock().engine.is_none());
        assert!(state.gpu.lock().context.is_none(), "no device was opened");
    }

    #[test]
    fn modes_use_the_camel_case_names_of_the_contract() {
        assert_eq!(
            serde_json::to_value(PreviewMode::Output).expect("serialize"),
            serde_json::json!("output")
        );
        let source: PreviewMode = serde_json::from_str("\"source\"").expect("parse");
        assert_eq!(source, PreviewMode::Source);
    }
}

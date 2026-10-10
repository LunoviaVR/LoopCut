//! Caption commands: SubRip import, and the font families the caption rasterizer can draw with.

use std::sync::{Arc, PoisonError};

use loopcut_core::project::{Caption, CaptionStyle};
use loopcut_core::srt::parse_srt;
use loopcut_core::time::TimeRange;
use tauri::State;

use crate::commands::{blocking, existing_local_file};
use crate::error::{AppError, io_error};
use crate::state::AppState;
use crate::storage;
use crate::tokens::new_token;

/// Largest subtitle file read. A subtitle file for a GIF is a few kilobytes.
pub const MAX_SRT_BYTES: u64 = 1024 * 1024;
/// Most captions a project may hold (the limit `migrate::validate` enforces).
pub const MAX_CAPTIONS: usize = 1000;
/// Horizontal anchor of imported captions: centred.
pub const IMPORT_X: f64 = 0.5;
/// Vertical anchor of imported captions: near the bottom of the frame.
pub const IMPORT_Y: f64 = 0.88;
/// Highest frame rate imported timings can snap to.
const MAX_FPS: f64 = 240.0;

/// Reads a SubRip file and returns its cues as captions with the default style. Start and end times snap to the frame
/// grid of `fps`, so a caption begins and ends on a frame of the output.
///
/// # Errors
///
/// `invalid_argument` for a bad `fps`, a malformed file, a file over [`MAX_SRT_BYTES`] (the message gives the limit)
/// or too many cues; `network_path` for a network location; `not_found` or `io` for a file that cannot be read.
#[tauri::command]
pub async fn captions_import_srt(path: String, fps: f64) -> Result<Vec<Caption>, AppError> {
    blocking(move || import_srt(&path, fps)).await
}

fn import_srt(raw: &str, fps: f64) -> Result<Vec<Caption>, AppError> {
    check_fps(fps)?;
    let path = existing_local_file(raw, "subtitle file")?;
    let bytes = storage::read_capped(&path, MAX_SRT_BYTES).map_err(|err| srt_read_error(&err))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let cues = parse_srt(text).map_err(|err| AppError::invalid_argument(err.to_string()))?;
    captions_from_cues(cues, fps)
}

/// The error for a subtitle file that cannot be read whole. An oversized file is the caller's argument, so it is
/// `invalid_argument` and names the limit; anything else is an operating-system failure.
fn srt_read_error(err: &std::io::Error) -> AppError {
    if err.kind() == std::io::ErrorKind::InvalidData {
        AppError::invalid_argument(format!(
            "The subtitle file is larger than the {MAX_SRT_BYTES}-byte limit (1 MiB)."
        ))
        .with_detail(err.to_string())
    } else {
        io_error(err, "The subtitle file could not be read")
    }
}

fn check_fps(fps: f64) -> Result<(), AppError> {
    if fps.is_finite() && fps > 0.0 && fps <= MAX_FPS {
        Ok(())
    } else {
        Err(AppError::invalid_argument(format!(
            "fps must be above 0 and at most {MAX_FPS}."
        )))
    }
}

/// Turns parsed cues into captions with the default style. Times snap to the frame grid of `fps`. Cues with empty text,
/// or whose snapped range is not a valid range, are skipped.
pub fn captions_from_cues(
    cues: Vec<(TimeRange, String)>,
    fps: f64,
) -> Result<Vec<Caption>, AppError> {
    if cues.len() > MAX_CAPTIONS {
        return Err(AppError::invalid_argument(format!(
            "The file has {} captions; a project holds at most {MAX_CAPTIONS}.",
            cues.len()
        )));
    }
    let mut captions = Vec::with_capacity(cues.len());
    for (range, text) in cues {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let snapped = TimeRange::new(snap(range.start, fps).max(0.0), snap(range.end, fps));
        if !snapped.is_valid() {
            continue;
        }
        captions.push(Caption {
            id: format!("cap-{}", &new_token()[..12]),
            text: text.to_owned(),
            range: snapped,
            x: IMPORT_X,
            y: IMPORT_Y,
            style: CaptionStyle::default(),
            animation: Default::default(),
        });
    }
    Ok(captions)
}

/// `seconds` moved to the nearest frame boundary of `fps`.
fn snap(seconds: f64, fps: f64) -> f64 {
    (seconds * fps).round() / fps
}

/// The font families captions can use: system fonts and the bundled ones, sorted. The rasterizer is shared with the
/// exports and is created on first use.
#[tauri::command]
pub async fn fonts_list(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let renderer = st.caption_renderer();
        let mut guard = renderer.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(guard.font_families())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> (TimeRange, String) {
        (TimeRange::new(start, end), text.to_owned())
    }

    #[test]
    fn cues_snap_to_the_frame_grid_and_keep_their_text() {
        let captions =
            captions_from_cues(vec![cue(0.02, 1.49, " HELLO ")], 10.0).expect("captions");
        assert_eq!(captions.len(), 1);
        let caption = &captions[0];
        assert_eq!(caption.text, "HELLO");
        assert!((caption.range.start - 0.0).abs() < 1e-9);
        assert!((caption.range.end - 1.5).abs() < 1e-9);
        assert!((caption.x - IMPORT_X).abs() < 1e-12);
        assert!((caption.y - IMPORT_Y).abs() < 1e-12);
        assert_eq!(caption.style, CaptionStyle::default());
    }

    #[test]
    fn empty_and_collapsed_cues_are_skipped() {
        let captions = captions_from_cues(
            vec![
                cue(1.0, 2.0, "   "),
                cue(3.0, 3.01, "gone"),
                cue(4.0, 5.0, "kept"),
            ],
            10.0,
        )
        .expect("captions");
        assert_eq!(captions.len(), 1);
        assert_eq!(captions[0].text, "kept");
    }

    #[test]
    fn captions_get_distinct_ids() {
        let captions = captions_from_cues(
            vec![cue(0.0, 1.0, "a"), cue(1.0, 2.0, "b"), cue(2.0, 3.0, "c")],
            20.0,
        )
        .expect("captions");
        let mut ids: Vec<_> = captions.iter().map(|c| c.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| id.starts_with("cap-")));
    }

    #[test]
    fn too_many_cues_are_refused() {
        let cues: Vec<_> = (0..=MAX_CAPTIONS)
            .map(|i| cue(i as f64, i as f64 + 1.0, "x"))
            .collect();
        assert_eq!(
            captions_from_cues(cues, 10.0).err().map(|e| e.code),
            Some("invalid_argument".to_owned())
        );
    }

    #[test]
    fn frame_rates_must_be_positive_and_bounded() {
        for bad in [0.0, -1.0, f64::NAN, MAX_FPS + 1.0] {
            assert_eq!(
                check_fps(bad).err().map(|e| e.code),
                Some("invalid_argument".to_owned()),
                "{bad}"
            );
        }
        assert!(check_fps(20.0).is_ok());
    }

    #[test]
    fn an_oversized_subtitle_file_is_an_invalid_argument_that_names_the_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("big.srt");
        std::fs::write(&file, vec![b'\n'; (MAX_SRT_BYTES + 1) as usize]).expect("seed");
        let err = import_srt(&file.to_string_lossy(), 20.0).expect_err("too large");
        assert_eq!(err.code, "invalid_argument");
        assert!(err.message.contains("1048576"), "{}", err.message);
    }

    #[test]
    fn a_missing_subtitle_file_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone.srt");
        let err = import_srt(&missing.to_string_lossy(), 20.0).expect_err("missing");
        assert_eq!(err.code, "not_found");
    }
}

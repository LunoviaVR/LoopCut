//! Cancellable ffprobe scans for the media commands.
//!
//! `loopcut_media::probe::probe` and `keyframe_index` run to completion or to their timeout, with no way to stop them
//! early. The app registers these scans as jobs, so cancelling a media token or an update must reach them. These
//! versions build the same commands from the same public pieces (`FfmpegCommand`, `parse_ffprobe_json`,
//! `parse_packet_csv`) and wait with `wait_cancellable`, which kills the child when the token is set.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::thread;
use std::time::Duration;

use loopcut_core::media::MediaInfo;
use loopcut_media::keyframes::{KEYFRAME_SCAN_TIMEOUT, parse_packet_csv};
use loopcut_media::probe::PROBE_TIMEOUT;
use loopcut_media::runner::{CancelToken, FfmpegCommand, InputOptions, SpawnIo};
use loopcut_media::{FfmpegPaths, MediaError, parse_ffprobe_json};

/// Probes `file` like `loopcut_media::probe::probe`, but stops early when `cancel` is set.
///
/// # Errors
///
/// [`MediaError::Cancelled`] when cancelled, otherwise the errors of `probe`.
pub fn probe_info(
    paths: &FfmpegPaths,
    file: &Path,
    cancel: &CancelToken,
) -> Result<MediaInfo, MediaError> {
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(file, &InputOptions::default())?
        .output_args(["-print_format", "json", "-show_format", "-show_streams"])?;
    let size_bytes = fs::metadata(file)?.len();
    let stdout = capture_cancellable(command, PROBE_TIMEOUT, cancel)?;
    let json = std::str::from_utf8(&stdout)
        .map_err(|err| MediaError::ProbeParse(format!("output is not UTF-8: {err}")))?;
    parse_ffprobe_json(json, size_bytes)
}

/// Keyframe times of one stream like `loopcut_media::keyframes::keyframe_index`, but stops early when `cancel` is set.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] for a non-finite `start_time`, [`MediaError::Cancelled`] when cancelled, otherwise
/// the errors of `keyframe_index`.
pub fn keyframe_times(
    paths: &FfmpegPaths,
    input: &Path,
    stream_index: u32,
    start_time: f64,
    cancel: &CancelToken,
) -> Result<Vec<f64>, MediaError> {
    if !start_time.is_finite() {
        return Err(MediaError::InvalidArgument(format!(
            "start_time must be finite, got {start_time}"
        )));
    }
    let args = [
        "-select_streams".to_owned(),
        stream_index.to_string(),
        "-show_entries".to_owned(),
        "packet=pts_time,flags".to_owned(),
        "-of".to_owned(),
        "csv=p=0".to_owned(),
    ];
    let command = FfmpegCommand::new_ffprobe(paths)
        .input(input, &InputOptions::default())?
        .output_args(args)?;
    let stdout = capture_cancellable(command, KEYFRAME_SCAN_TIMEOUT, cancel)?;
    let text = std::str::from_utf8(&stdout)
        .map_err(|err| MediaError::ProbeParse(format!("keyframe output is not UTF-8: {err}")))?;
    Ok(parse_packet_csv(text, start_time))
}

/// Runs `command`, collects its stdout and returns it. The child is killed after `timeout` or when `cancel` is set.
fn capture_cancellable(
    command: FfmpegCommand,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Vec<u8>, MediaError> {
    // A scan cancelled before it starts never spawns its child.
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let mut process = command.spawn(SpawnIo {
        stdin: false,
        stdout: true,
    })?;
    let mut stdout = process
        .take_stdout()
        .ok_or_else(|| MediaError::Io(io::Error::other("stdout pipe is missing")))?;
    // Drain on another thread so a large output cannot stall the child while the wait below polls it.
    let reader = thread::Builder::new()
        .name("ffprobe-stdout".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })?;
    process.wait_cancellable(cancel, Some(timeout))?;
    let bytes = reader
        .join()
        .map_err(|_| MediaError::Io(io::Error::other("stdout reader thread panicked")))??;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_start_times_are_rejected_before_any_process_starts() {
        // The paths are never used: validation runs first. A placeholder install directory is enough.
        let paths = FfmpegPaths {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        };
        let err = keyframe_times(
            &paths,
            Path::new("C:\\clip.mp4"),
            0,
            f64::NAN,
            &CancelToken::new(),
        )
        .expect_err("NaN start time");
        assert!(matches!(err, MediaError::InvalidArgument(_)));
    }

    /// Placeholder executables: a cancelled scan must not start them, so their paths never matter.
    fn placeholder_paths() -> FfmpegPaths {
        FfmpegPaths {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }

    #[test]
    fn a_probe_cancelled_before_it_starts_reports_cancelled_without_a_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("clip.bin");
        std::fs::write(&file, [0u8; 64]).expect("seed");
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = probe_info(&placeholder_paths(), &file, &cancel).expect_err("cancelled");
        assert!(matches!(err, MediaError::Cancelled), "{err:?}");
    }

    #[test]
    fn a_keyframe_scan_cancelled_before_it_starts_reports_cancelled() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = keyframe_times(
            &placeholder_paths(),
            Path::new("C:\\clip.mp4"),
            0,
            0.0,
            &cancel,
        )
        .expect_err("cancelled");
        assert!(matches!(err, MediaError::Cancelled), "{err:?}");
    }
}

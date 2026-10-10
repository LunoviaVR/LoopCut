//! Keyframe index (packet scan) used for fast seeking and proxy decisions.
//!
//! The index is built by ffprobe from packet flags only: no frame is decoded, so the scan is bounded by demuxing
//! speed. Times are source seconds, measured from the stream's start time, matching the rest of the editor.

use std::path::Path;
use std::time::Duration;

use crate::error::MediaError;
use crate::paths::FfmpegPaths;
use crate::runner::{FfmpegCommand, InputOptions, run_capture};

/// Upper bound for one packet scan. Demuxing a long feature file is I/O bound but finishes well within this.
pub const KEYFRAME_SCAN_TIMEOUT: Duration = Duration::from_secs(900);

/// Lists the keyframes of one stream as source seconds, ascending and without duplicates.
///
/// `stream_index` is the absolute container index (ffprobe `-select_streams <n>`). `start_time` is the stream start
/// in seconds, as reported by [`loopcut_core::VideoStreamInfo::start_time`]; it is subtracted from each packet PTS.
/// Keyframes that precede `start_time` are clamped to `0.0`.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] for a non-finite `start_time`, [`MediaError::UrlNotAllowed`] or
/// [`MediaError::Failed`] from the runner, and [`MediaError::ProbeParse`] if ffprobe output is not UTF-8.
pub fn keyframe_index(
    paths: &FfmpegPaths,
    input: &Path,
    stream_index: u32,
    start_time: f64,
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
    let stdout = run_capture(command, KEYFRAME_SCAN_TIMEOUT)?;
    let text = std::str::from_utf8(&stdout)
        .map_err(|err| MediaError::ProbeParse(format!("keyframe output is not UTF-8: {err}")))?;
    Ok(parse_packet_csv(text, start_time))
}

/// Parses `pts_time,flags` CSV rows (one per packet) into keyframe times relative to `start_time`.
///
/// Rows without a usable PTS (`N/A`) or without the `K` flag are skipped, as are rows that do not parse. The result is
/// sorted and de-duplicated.
pub fn parse_packet_csv(text: &str, start_time: f64) -> Vec<f64> {
    let mut keys: Vec<f64> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(',');
            let pts = fields.next()?.trim();
            let flags = fields.next()?.trim();
            if !flags.contains('K') {
                return None;
            }
            let pts: f64 = pts.parse().ok()?;
            pts.is_finite().then(|| (pts - start_time).max(0.0))
        })
        .collect();
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    keys
}

/// Longest stretch of source time that must be decoded from one keyframe, in seconds.
///
/// `keys` are keyframe times (ascending, from [`keyframe_index`]). The stretches considered are
/// `[0, k0)`, `[k0, k1)`, ..., `[k_last, duration)`, so a stream without keyframes reports `duration`. Non-finite
/// keys are ignored, and a negative `duration` counts as zero.
pub fn max_gop_seconds(keys: &[f64], duration: f64) -> f64 {
    let end = if duration.is_finite() {
        duration.max(0.0)
    } else {
        0.0
    };
    let mut longest = 0.0_f64;
    let mut previous = 0.0_f64;
    for &key in keys.iter().filter(|key| key.is_finite()) {
        longest = longest.max(key - previous);
        previous = key;
    }
    longest.max(end - previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flagged_rows_only() {
        let csv = "0.000000,K__\n0.033000,___\n0.066000,___\n0.400000,K__\n0.433000,___\n";
        assert_eq!(parse_packet_csv(csv, 0.0), vec![0.0, 0.4]);
    }

    #[test]
    fn subtracts_start_time_and_clamps_leading_keys() {
        let csv = "-0.010000,K__\n0.500000,K__\n1.000000,K__\n";
        assert_eq!(parse_packet_csv(csv, 0.25), vec![0.0, 0.25, 0.75]);
    }

    #[test]
    fn skips_missing_pts_and_trailing_fields() {
        let csv = "N/A,K__\n0.000000,K__,\nbogus,K__\n1.500000,KD_,\n";
        assert_eq!(parse_packet_csv(csv, 0.0), vec![0.0, 1.5]);
    }

    #[test]
    fn sorts_and_dedups() {
        let csv = "2.000000,K\n1.000000,K\n1.000000,K\n0.000000,K\n";
        assert_eq!(parse_packet_csv(csv, 0.0), vec![0.0, 1.0, 2.0]);
    }

    #[test]
    fn max_gop_covers_leading_and_trailing_spans() {
        assert_eq!(max_gop_seconds(&[], 5.0), 5.0);
        assert_eq!(max_gop_seconds(&[0.0], 2.0), 2.0);
        assert_eq!(max_gop_seconds(&[0.0, 0.4, 0.8], 1.0), 0.4);
        assert!((max_gop_seconds(&[0.0, 0.4, 0.8], 3.0) - 2.2).abs() < 1e-9);
        assert_eq!(max_gop_seconds(&[1.0, 1.5], 2.0), 1.0);
    }

    #[test]
    fn max_gop_ignores_bad_input() {
        assert_eq!(max_gop_seconds(&[0.0, f64::NAN, 1.0], f64::NAN), 1.0);
        assert_eq!(max_gop_seconds(&[], -3.0), 0.0);
    }
}

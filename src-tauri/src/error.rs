//! The error type returned by every command: `AppError { code, message, detail? }`.
//!
//! Codes are stable identifiers the webview may branch on; messages are for people. Every error that is
//! constructed is also appended to a small in-memory ring buffer so that "Copy diagnostics" can include the last
//! failures without a log file.

use std::collections::VecDeque;
use std::io;
use std::sync::{Mutex, OnceLock};

use loopcut_core::migrate::ProjectError;
use loopcut_media::MediaError;
use serde::Serialize;
use ts_rs::TS;

/// Stable error codes (see docs/ipc.md). Adding a code is allowed; renaming or removing one is not.
pub mod code {
    pub const NOT_FOUND: &str = "not_found";
    pub const NETWORK_PATH: &str = "network_path";
    pub const UNSUPPORTED_MEDIA: &str = "unsupported_media";
    pub const FFMPEG_FAILED: &str = "ffmpeg_failed";
    /// The bundled FFmpeg executables are missing or unusable.
    pub const FFMPEG_MISSING: &str = "ffmpeg_missing";
    pub const IO: &str = "io";
    pub const INVALID_PROJECT: &str = "invalid_project";
    pub const NEWER_PROJECT: &str = "newer_project";
    pub const CANCELLED: &str = "cancelled";
    pub const NOT_IMPLEMENTED: &str = "not_implemented";
    /// An argument is out of range or malformed.
    pub const INVALID_ARGUMENT: &str = "invalid_argument";
    /// The same job is already running, or new work is refused while an update stops background work.
    pub const BUSY: &str = "busy";
    /// The GPU renderer could not be created or failed while drawing (no adapter, validation error, readback).
    pub const GPU_FAILED: &str = "gpu_failed";
    /// The GPU device was lost. The software renderer is used from then on.
    pub const GPU_LOST: &str = "gpu_lost";
    /// Target-size mode tried every setting and none fitted the requested size.
    pub const TARGET_UNREACHABLE: &str = "target_unreachable";
    /// The GIF encoder (gifski, the FFmpeg palette path or the GPU dither) failed while writing an export.
    pub const GIF_FAILED: &str = "gif_failed";
}

/// Longest detail text sent to the webview; FFmpeg stderr tails can be large.
const MAX_DETAIL_CHARS: usize = 4_000;
/// Number of recent errors kept for diagnostics.
const RECENT_ERRORS: usize = 50;

/// Error returned to the webview. Never a panic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppError {
    pub code: String,
    pub message: String,
    /// Technical detail (tool output, OS error text). Omitted from the JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
}

impl AppError {
    /// Builds an error with a stable `code` and a human-readable message, and records it for diagnostics.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let error = Self {
            code: code.to_owned(),
            message: message.into(),
            detail: None,
        };
        record(&error);
        error
    }

    /// Attaches technical detail (tool output, OS error text). Truncated to a sane size.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let text: String = detail.into().chars().take(MAX_DETAIL_CHARS).collect();
        self.detail = Some(text);
        self
    }

    /// A required argument is missing, malformed or out of range.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_ARGUMENT, message)
    }

    /// The bundled FFmpeg could not be located or started.
    pub fn ffmpeg_missing(reason: impl Into<String>) -> Self {
        Self::new(
            code::FFMPEG_MISSING,
            "FFmpeg is not available. Reinstall LoopCut to restore it.",
        )
        .with_detail(reason)
    }

    /// The file does not exist (or was removed since it was opened).
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(code::NOT_FOUND, format!("{} was not found", what.into()))
    }

    /// Any other operating-system failure.
    pub fn io(context: impl Into<String>, err: &io::Error) -> Self {
        Self::new(code::IO, context).with_detail(err.to_string())
    }
}

impl From<MediaError> for AppError {
    fn from(err: MediaError) -> Self {
        match &err {
            MediaError::BinaryMissing(path) => {
                Self::ffmpeg_missing(format!("missing executable: {}", path.display()))
            }
            MediaError::UrlNotAllowed(_)
            | MediaError::LavfiRejected(_)
            | MediaError::InvalidArgument(_) => Self::new(code::INVALID_ARGUMENT, err.to_string()),
            MediaError::Unsupported(_)
            | MediaError::ProbeParse(_)
            | MediaError::NoVideoStream
            | MediaError::Json(_) => Self::new(code::UNSUPPORTED_MEDIA, err.to_string()),
            MediaError::Failed { stderr_tail, .. } => {
                Self::new(code::FFMPEG_FAILED, err.to_string()).with_detail(stderr_tail.clone())
            }
            MediaError::Spawn { .. } | MediaError::Timeout(_) => {
                Self::new(code::FFMPEG_FAILED, err.to_string())
            }
            MediaError::Cancelled => Self::new(code::CANCELLED, "The operation was cancelled"),
            MediaError::Io(io_err) => io_error(io_err, "A media file could not be read"),
        }
    }
}

impl From<ProjectError> for AppError {
    fn from(err: ProjectError) -> Self {
        match &err {
            ProjectError::NewerVersion { .. } => Self::new(code::NEWER_PROJECT, err.to_string()),
            _ => Self::new(code::INVALID_PROJECT, err.to_string()),
        }
    }
}

impl From<io::Error> for AppError {
    fn from(err: io::Error) -> Self {
        io_error(&err, "A file operation failed")
    }
}

/// Maps `io::Error` to `not_found` for missing files and `io` otherwise.
pub fn io_error(err: &io::Error, context: &str) -> AppError {
    if err.kind() == io::ErrorKind::NotFound {
        AppError::new(
            code::NOT_FOUND,
            format!("{context}: the file was not found"),
        )
        .with_detail(err.to_string())
    } else {
        AppError::io(context, err)
    }
}

fn recent() -> &'static Mutex<VecDeque<String>> {
    static RECENT: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    RECENT.get_or_init(|| Mutex::new(VecDeque::with_capacity(RECENT_ERRORS)))
}

fn record(error: &AppError) {
    let mut ring = match recent().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if ring.len() == RECENT_ERRORS {
        ring.pop_front();
    }
    let line = match &error.detail {
        Some(detail) => format!(
            "[{}] {} ({})",
            error.code,
            error.message,
            detail.lines().next().unwrap_or("")
        ),
        None => format!("[{}] {}", error.code, error.message),
    };
    ring.push_back(line);
}

/// The most recent errors, oldest first, for the diagnostics report.
pub fn recent_errors() -> Vec<String> {
    match recent().lock() {
        Ok(guard) => guard.iter().cloned().collect(),
        Err(poisoned) => poisoned.into_inner().iter().cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_strings() {
        assert_eq!(code::NETWORK_PATH, "network_path");
        assert_eq!(code::NEWER_PROJECT, "newer_project");
        assert_eq!(code::GIF_FAILED, "gif_failed");
    }

    #[test]
    fn detail_is_omitted_from_json_when_absent() {
        let json = serde_json::to_value(AppError::new(code::IO, "x")).expect("serialize");
        assert_eq!(json, serde_json::json!({"code": "io", "message": "x"}));
        let with =
            serde_json::to_value(AppError::new(code::IO, "x").with_detail("y")).expect("serialize");
        assert_eq!(with["detail"], "y");
    }

    #[test]
    fn media_errors_map_to_codes() {
        let cancelled: AppError = MediaError::Cancelled.into();
        assert_eq!(cancelled.code, "cancelled");
        let missing: AppError = MediaError::BinaryMissing("ffmpeg.exe".into()).into();
        assert_eq!(missing.code, "ffmpeg_missing");
        let failed: AppError = MediaError::Failed {
            code: Some(1),
            stderr_tail: "bad".into(),
        }
        .into();
        assert_eq!(failed.code, "ffmpeg_failed");
        assert_eq!(failed.detail.as_deref(), Some("bad"));
        let probe: AppError = MediaError::NoVideoStream.into();
        assert_eq!(probe.code, "unsupported_media");
        let io: AppError = MediaError::Io(io::Error::from(io::ErrorKind::NotFound)).into();
        assert_eq!(io.code, "not_found");
    }

    #[test]
    fn project_errors_map_to_codes() {
        let newer: AppError = ProjectError::NewerVersion {
            found: 2,
            supported: 1,
        }
        .into();
        assert_eq!(newer.code, "newer_project");
        let invalid: AppError = ProjectError::MissingVersion.into();
        assert_eq!(invalid.code, "invalid_project");
    }

    #[test]
    fn long_detail_is_truncated() {
        let long = "x".repeat(MAX_DETAIL_CHARS + 500);
        let error = AppError::new(code::IO, "x").with_detail(long);
        assert_eq!(
            error.detail.map(|d| d.chars().count()),
            Some(MAX_DETAIL_CHARS)
        );
    }

    #[test]
    fn recent_ring_is_bounded() {
        // The ring is shared by every test in the process, so only its bound is asserted.
        for i in 0..(RECENT_ERRORS + 5) {
            let _ = AppError::new(code::IO, format!("ring-test-{i}"));
        }
        assert!(recent_errors().len() <= RECENT_ERRORS);
    }
}

//! Error type shared by every module of `loopcut-media`.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Everything that can go wrong while running FFmpeg/ffprobe or interpreting their output.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// The bundled `ffmpeg` or `ffprobe` executable is missing from the expected folder.
    #[error("FFmpeg executable not found: {}", .0.display())]
    BinaryMissing(PathBuf),
    /// An input or output looks like a URL or a protocol specifier. Only local paths are accepted.
    #[error("refusing URL-like path {0:?}: only local file paths are allowed")]
    UrlNotAllowed(String),
    /// A lavfi filtergraph was rejected: lavfi was not requested, or the graph reads other files.
    #[error("refusing lavfi input: {0}")]
    LavfiRejected(String),
    /// A builder argument was invalid (negative seek, non-finite duration, reserved option, ...).
    #[error("invalid FFmpeg argument: {0}")]
    InvalidArgument(String),
    /// The request is well formed but the backend cannot do it: an encoder that lacks the codec, a container that
    /// cannot hold the codec, or a decode chain that cannot carry the requested layout. Callers may offer another
    /// backend instead of reporting a bad argument.
    #[error("unsupported combination: {0}")]
    Unsupported(String),
    /// The process could not be started.
    #[error("could not start {}: {source}", .program.display())]
    Spawn {
        /// The executable that failed to start.
        program: PathBuf,
        /// The underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// The process exited unsuccessfully. `stderr_tail` holds the last diagnostic lines.
    #[error("FFmpeg tool failed (exit code {code:?}): {stderr_tail}")]
    Failed {
        /// Exit code, if the process exited normally.
        code: Option<i32>,
        /// The last diagnostic lines written to stderr, newline separated.
        stderr_tail: String,
    },
    /// The process did not finish within the requested time and was killed.
    #[error("FFmpeg tool did not finish within {0:?}")]
    Timeout(Duration),
    /// The caller cancelled the operation and the process was killed.
    #[error("operation cancelled")]
    Cancelled,
    /// ffprobe output could not be interpreted.
    #[error("unexpected ffprobe output: {0}")]
    ProbeParse(String),
    /// The file has no video stream that LoopCut can edit.
    #[error("no usable video stream found")]
    NoVideoStream,
    /// An operating-system error (pipes, files, job objects).
    #[error(transparent)]
    Io(#[from] io::Error),
    /// ffprobe emitted JSON that does not match the expected shape.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Convenience alias used throughout the crate.
pub type MediaResult<T> = Result<T, MediaError>;

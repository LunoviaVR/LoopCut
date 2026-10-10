//! LoopCut media layer: the FFmpeg/ffprobe process runner, probing and raw frame I/O.
//!
//! FFmpeg is never linked. Every invocation goes through [`runner::FfmpegCommand`], which enforces the sandbox rules:
//! local file paths only, the `file,pipe` protocol whitelist, `-noautorotate` (LoopCut rotates in its compositor),
//! no console windows, and containment in a kill-on-close Windows Job Object.
//!
//! Modules:
//! * [`paths`]: locating the bundled `ffmpeg` and `ffprobe` executables.
//! * [`runner`]: the argument builder, process handles, progress parsing and raw frame reading.
//! * [`probe`]: ffprobe JSON to [`loopcut_core::MediaInfo`].
//! * [`job`]: Windows Job Object containment.

pub mod decode;
pub mod encode;
pub mod error;
pub mod framestore;
pub mod job;
pub mod keyframes;
pub mod paths;
pub mod probe;
pub mod proxy;
pub mod runner;
pub mod thumbnails;

pub use error::{MediaError, MediaResult};
pub use paths::FfmpegPaths;
pub use probe::{parse_ffprobe_json, probe};
pub use runner::{
    CancelToken, ExitInfo, Feed, FfmpegCommand, FfmpegProcess, InputOptions, LogLevel, Progress,
    ProgressParser, RawFrameReader, SpawnIo, is_url_like, run_capture,
};

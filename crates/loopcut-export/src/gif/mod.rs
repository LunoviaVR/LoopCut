//! GIF export engines and the types they share.
//!
//! Every engine takes RGBA frames with a presentation time in seconds and writes a GIF89a stream.
//! Timing follows [`loopcut_core::gif_timing`]: integer-centisecond delays, never below 2 cs.
//!
//! The Quality engine is [`GifskiEncoder`] (gifski 1.34.0). Fast and GPU-dither engines will add
//! sibling modules that reuse [`GifError`], [`GifProgress`] and [`GifStats`].

pub mod gifski_engine;

pub use gifski_engine::{GifskiEncoder, GifskiOptions, pts_from_delays};

use std::io;

use thiserror::Error;

/// Progress of a running GIF encode, reported from the writer thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GifProgress {
    /// GIF frame records written so far.
    ///
    /// gifski merges identical consecutive frames and can drop frames that add nothing, so this
    /// may end below the number of frames added. Its final value equals [`GifStats::frames`].
    pub frames_written: u64,
    /// Logical bytes produced so far (the GIF stream before the final trailer byte).
    pub bytes_written: u64,
}

/// Summary of a finished GIF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GifStats {
    /// Bytes written to the output, including the header and the trailer.
    pub bytes: u64,
    /// GIF frame records in the file.
    pub frames: usize,
}

/// Errors from the GIF engines.
#[derive(Debug, Error)]
pub enum GifError {
    /// The encoder options are out of range.
    #[error("invalid GIF options: {0}")]
    InvalidOptions(String),
    /// A frame does not match the configured size, the pixel buffer length, or the timing rules.
    #[error("invalid frame: {0}")]
    InvalidFrame(String),
    /// The writer thread stopped before it accepted the frame. Call `finish` to learn the cause.
    #[error("the GIF writer stopped before it accepted all frames")]
    WriterStopped,
    /// The encoder was finished without any frame.
    #[error("no frames to encode")]
    NoFrames,
    /// gifski reported an error that LoopCut does not map to a more specific variant.
    #[error("gifski error: {0}")]
    Gifski(String),
    /// The encode was aborted, either by the progress callback returning `false` or by `cancel`.
    #[error("GIF export aborted")]
    Aborted,
    /// Writing the output failed.
    #[error("I/O error while writing GIF: {0}")]
    Io(#[from] io::Error),
    /// An internal invariant failed, for example a panicked writer thread.
    #[error("internal GIF encoder error: {0}")]
    Internal(String),
}

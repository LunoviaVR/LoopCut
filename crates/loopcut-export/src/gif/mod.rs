//! GIF export engines and the types they share.
//!
//! Every engine takes RGBA frames with a presentation time and writes a GIF89a stream. Timing follows
//! [`loopcut_core::gif_timing`]: integer-centisecond delays, never below 2 cs.
//!
//! * [`GifskiEncoder`] is the Quality engine (gifski 1.34.0): per-frame palettes, exact delays.
//! * [`fast`] is the Fast engine: a palette from a sample of the frames piped into FFmpeg's `palettegen`, then every frame
//!   piped through `paletteuse` with that palette. Its delays are uniform, taken from the frame rate and rounded by
//!   FFmpeg.
//! * [`palette`], [`dither`], [`gpu_dither`] and [`indexed`] make the GPU-dither engine: one global palette from
//!   imagequant, ordered dithering on the GPU (or the same arithmetic on the CPU), and a `gif` crate writer that
//!   stores only the changed rectangle of each frame when that is smaller.

pub mod dither;
pub mod fast;
pub mod gifski_engine;
pub mod gpu_dither;
pub mod indexed;
pub mod palette;

pub use gifski_engine::{GifskiEncoder, GifskiOptions, pts_for_exact_delays};

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
    /// A frame does not match the configured size or pixel buffer length, arrives out of index
    /// order, or has a timestamp that does not come after the previous frame's. The encoder is
    /// unchanged, so a corrected frame can be sent at the same index.
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
    /// The encode was aborted: the progress callback returned `false`, or the encoder was
    /// cancelled or dropped before `finish`. The output then has no GIF trailer and must be
    /// deleted.
    #[error("GIF export aborted")]
    Aborted,
    /// Writing the output failed.
    #[error("I/O error while writing GIF: {0}")]
    Io(#[from] io::Error),
    /// The global palette could not be built (imagequant rejected the input or the colour count).
    #[error("palette error: {0}")]
    Palette(String),
    /// The GPU dither pass failed. The caller can retry the frame on the CPU, which gives the same indices.
    #[error("GPU dither error: {0}")]
    Gpu(String),
    /// An internal invariant failed, for example a panicked writer thread.
    #[error("internal GIF encoder error: {0}")]
    Internal(String),
}

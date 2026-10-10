//! LoopCut export orchestration: decodes the source ranges a project uses, grades and captions every output frame on the
//! GPU and encodes it as a GIF or as an MP4 / WebM video (FFmpeg encoders). A project with a size budget is searched
//! for the first setting set that fits.
//!
//! * [`job`]: [`run_export`] and its request, event and error types.
//! * [`frames`]: the source frame provider. It decodes ranges into a frame store, falls back per range, and streams a
//!   range that does not fit the store when the plan plays it in order.
//! * [`estimate`]: [`estimate_size`], a size prediction from real encodes of sampled windows.
//! * [`target`]: the target-size search (private; used by [`run_export`] when a budget is set).
//! * [`video`]: format-to-codec mapping and keyframe interval for video exports.
//! * [`gif`]: the GIF engines: gifski (Quality), FFmpeg palettegen and paletteuse (Fast), and imagequant with GPU ordered
//!   dithering (GPU dither).

pub mod estimate;
pub mod frames;
pub mod gif;
pub mod job;
mod target;
pub mod video;

pub use estimate::{EstimateBasis, SizeEstimate, estimate_size, estimate_size_with_cancel};
pub use job::{
    ExportContext, ExportError, ExportEvent, ExportRequest, ExportResult, partial_path, run_export,
};

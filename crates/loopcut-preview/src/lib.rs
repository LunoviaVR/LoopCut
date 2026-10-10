//! LoopCut preview engine: decodes the source at preview resolution and renders output frames with the compositor
//! that export uses.
//!
//! # What it does
//!
//! * [`PreviewEngine::set_project`] plans the project's sequence (the same [`loopcut_core::sequencer::plan`] that
//!   export uses) and splits the source ranges it needs into chunks.
//! * A background worker decodes the chunks inside a *decode window* into the shared
//!   [`loopcut_media::framestore::FrameStore`], nearest to the playhead first. The window holds at most half of the
//!   store's RAM budget, and it moves with [`PreviewEngine::set_playhead`] and with playback. Chunks that leave the window
//!   are released.
//! * [`PreviewEngine::render_output_frame`] renders exactly the frame the plan shows at that point, through the same
//!   compositor export uses. Crop, orientation, grade, LUT and captions are applied on the GPU.
//! * [`PreviewEngine::render_source_frame`] renders the uncropped source for the crop tool and source scrubbing, without
//!   grade or captions.
//! * [`PreviewEngine::play`] plays the plan on a render thread, paced by the GIF delays (or the plan's rate for other
//!   formats). Each frame is rendered at [`PreviewQuality::Reduced`] (at most 1280×720 pixels) and handed to a sink on
//!   another thread. Late frames are dropped.
//! * [`PreviewEngine::status`] reports decode progress, failed ranges, hardware fallbacks and the last error.
//!
//! # Decode size
//!
//! The source is decoded uncropped, so dragging the crop does not decode again. The decoded width holds enough
//! samples for the crop at the render size, with 25% headroom. The need is the same for both orientations, so turning a
//! frame that keeps the same render does not re-decode. It is capped at 1920 pixels wide on a hardware effects renderer and 640 on WARP, where
//! uploads are slow. The rules are in the `geometry` module.
//!
//! # Frames on the wire
//!
//! [`RenderedFrame::to_wire`] encodes a frame as an 8-byte header (`u32` little-endian width and height) followed by
//! straight-alpha RGBA8, the format `docs/ipc.md` describes.
//!
//! # Platform
//!
//! Windows-first, but nothing here is platform-specific beyond the FFmpeg and GPU crates it calls.

mod engine;
mod error;
mod faults;
mod geometry;
mod model;
mod playback;
mod source;
#[cfg(test)]
mod tests;
mod types;
mod worker;

pub use engine::{DEFAULT_PROGRESS_TIMEOUT, PreviewEngine};
pub use error::PreviewError;
pub use playback::{FrameSink, PlaybackHandle};
pub use types::{
    DecodeFallback, PreviewInfo, PreviewQuality, PreviewSource, PreviewStatus, RenderedFrame,
    Viewport,
};

//! Error type of the preview engine.

use loopcut_core::sequencer::SequenceError;
use loopcut_render::compositor::CompositorError;

/// Everything the preview engine can fail with. [`PreviewError::code`] gives the stable code the app layer maps to
/// `AppError.code`.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// A frame was requested before any project was set.
    #[error("no project is loaded in the preview")]
    NoProject,
    /// The project cannot be previewed: no clip, or a clip that uses another stream.
    #[error("the project cannot be previewed: {0}")]
    InvalidProject(String),
    /// A request argument was invalid (non-finite time, empty frame box).
    #[error("invalid preview request: {0}")]
    InvalidRequest(String),
    /// The viewport is zero-sized or its device pixel ratio is not usable.
    #[error("invalid viewport: {0}")]
    InvalidViewport(String),
    /// The sequencer rejected the clip or frame rate (empty clip, too many frames, ...).
    #[error(transparent)]
    Sequence(#[from] SequenceError),
    /// An output frame index is past the end of the plan.
    #[error("frame {frame} is out of range; the plan has {frames} frames")]
    FrameOutOfRange { frame: u32, frames: u32 },
    /// No source frame arrived within the wait limit (the decoder made no progress).
    #[error("the source frame at {time:.3} s was not decoded in time")]
    Timeout { time: f64 },
    /// A source frame kept disappearing from the frame store (evicted or dropped) after retries.
    #[error("the source frame at {time:.3} s is unavailable after retries")]
    FrameUnavailable { time: f64 },
    /// Decoding failed, with the decoder's own message.
    #[error("decode failed: {0}")]
    Decode(String),
    /// The work was cancelled: the project changed under it, or the engine is shutting down.
    #[error("the preview work was cancelled")]
    Cancelled,
    /// The compositor failed (device lost, validation error, readback failure).
    #[error(transparent)]
    Render(#[from] CompositorError),
    /// A worker thread could not be started.
    #[error("could not start a preview thread: {0}")]
    Spawn(#[source] std::io::Error),
}

impl PreviewError {
    /// Stable machine-readable code for the app layer.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoProject => "preview_no_project",
            Self::InvalidProject(_) => "preview_invalid_project",
            Self::InvalidRequest(_) => "preview_invalid_request",
            Self::InvalidViewport(_) => "preview_invalid_viewport",
            Self::Sequence(_) => "preview_sequence",
            Self::FrameOutOfRange { .. } => "preview_frame_out_of_range",
            Self::Timeout { .. } => "preview_timeout",
            Self::FrameUnavailable { .. } => "preview_frame_unavailable",
            Self::Decode(_) => "preview_decode",
            Self::Cancelled => "preview_cancelled",
            Self::Render(_) => "preview_render",
            Self::Spawn(_) => "preview_spawn",
        }
    }
}

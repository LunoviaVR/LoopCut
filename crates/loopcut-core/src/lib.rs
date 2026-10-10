//! LoopCut core: the project model and pure editing math shared by every other crate.
//!
//! Nothing in this crate performs IO, spawns processes or touches the GPU, so all of it is unit-testable.

pub mod geometry;
pub mod gif_timing;
pub mod hw;
pub mod media;
pub mod migrate;
pub mod presets;
pub mod project;
pub mod sequencer;
pub mod srt;
pub mod time;

pub use geometry::{NormRect, Orientation, OutputSize, PixelRect};
pub use hw::{
    DecodeBackend, DecodeChain, EncoderBackend, GpuKey, GpuScaler, HwSettings, VideoCodec,
};
pub use media::{
    ColorInfo, ColorRange, MediaInfo, PixelLayout, TransferFunction, VideoStreamInfo, YuvMatrix,
};
pub use project::{Clip, Direction, ExportFormat, GifEngine, Project, SCHEMA_VERSION, Timing};
pub use sequencer::{PlannedFrame, Sample, SequencePlan};
pub use time::{Rational, TimeRange};

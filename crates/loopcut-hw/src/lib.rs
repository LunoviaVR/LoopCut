//! LoopCut hardware detection: GPU enumeration (DXGI + CUDA LUID mapping), FFmpeg capability probes,
//! backend selection with automatic CPU fallback, and the on-disk capability cache.
//!
//! Probing starts FFmpeg children and blocks until they finish, so callers run [`probe_capabilities`] on a worker
//! thread and pass a [`loopcut_media::runner::CancelToken`] to stop it.

pub mod cache;
pub mod caps;
pub mod cuda;
pub mod devices;
pub mod probe;
pub mod select;

pub use caps::{CapStatus, Capabilities, DecodeCap, DeviceCaps, EncodeCap, ScalerCap};
pub use devices::{GpuDevice, enumerate_gpus};
pub use probe::{ProbeEvent, ProbeOptions, probe_capabilities};

/// Errors that stop a probe or a cache operation. Individual test failures are not errors: they are recorded as
/// statuses in [`Capabilities`].
#[derive(Debug, thiserror::Error)]
pub enum HwError {
    /// The caller cancelled the probe. Partial results are discarded.
    #[error("hardware probe cancelled")]
    Cancelled,
    /// A file or folder the probe needs could not be created or used.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

//! GPU context: adapter enumeration/selection (incl. the WARP software fallback), device-loss handling and readback helpers.
//!
//! Preview and export both render through [`GpuContext`], so they always share one adapter-selection path.
//! On Windows only the DX12 backend is used; the software fallback is WARP, which wgpu reports as
//! [`DeviceKind::Cpu`].
//!
//! Selection follows the user's [`AdapterChoice`]. When the requested choice cannot be opened it falls back
//! `Specific → Auto → Software`, and [`GpuContext::fell_back`] reports whether that happened.
//!
//! Identical GPUs share vendor and device IDs, so [`AdapterDesc::ordinal`] tells them apart: it counts the
//! adapters with the same IDs that precede this one in enumeration order. A stored [`GpuKey`] therefore keeps
//! naming the same card across launches as long as the set of identical cards does not change.
//!
//! The DX12 shader compiler is chosen by wgpu when the instance is created; see [`shader_compiler_name`].

use std::collections::HashMap;
#[cfg(any(windows, test))]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use loopcut_core::GpuKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How long to wait for a buffer map callback after the device has been polled.
///
/// `Device::poll(Wait)` runs pending map callbacks before it returns, so this only guards against a driver
/// that never invokes the callback; it should never be reached in practice.
const MAP_CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Staging buffers a [`Readback`] keeps at once. When a new size arrives and the pool is full, the pool is dropped.
const MAX_POOLED_STAGING: usize = 4;

/// Broad class of a GPU, mirroring [`wgpu::DeviceType`] in a serializable form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceKind {
    /// Discrete GPU with its own video memory.
    Discrete,
    /// Integrated GPU sharing system memory.
    Integrated,
    /// Virtualized or hosted GPU.
    Virtual,
    /// Software rasterizer (WARP on DX12).
    Cpu,
    /// Unknown or unclassified device.
    Other,
}

impl From<wgpu::DeviceType> for DeviceKind {
    fn from(value: wgpu::DeviceType) -> Self {
        match value {
            wgpu::DeviceType::DiscreteGpu => Self::Discrete,
            wgpu::DeviceType::IntegratedGpu => Self::Integrated,
            wgpu::DeviceType::VirtualGpu => Self::Virtual,
            wgpu::DeviceType::Cpu => Self::Cpu,
            wgpu::DeviceType::Other => Self::Other,
        }
    }
}

/// Serializable description of one GPU adapter, for the device picker and the capability report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterDesc {
    /// Marketing name reported by the driver.
    pub name: String,
    /// PCI vendor ID.
    pub vendor: u32,
    /// PCI device ID.
    pub device: u32,
    /// Index among the enumerated adapters with the same vendor and device IDs, in enumeration order.
    ///
    /// Identical GPUs get 0, 1, and so on. Only [`enumerate_adapters`] assigns this; [`AdapterDesc::from_info`]
    /// leaves it at 0.
    pub ordinal: u32,
    /// Broad device class.
    pub kind: DeviceKind,
    /// wgpu backend name (for example `dx12`).
    pub backend: String,
    /// Driver version string.
    pub driver: String,
    /// Additional driver information (often empty on DX12).
    pub driver_info: String,
    /// PCI bus address such as `0000:03:00.0`, or empty when unknown.
    ///
    /// On DX12 this is the first display device with the same vendor and device IDs, so identical GPUs report the
    /// same value. It is informational only: selection uses [`AdapterDesc::ordinal`], not this field.
    pub pci_bus_id: String,
}

impl AdapterDesc {
    /// Builds a description from wgpu's adapter info. [`AdapterDesc::ordinal`] is 0 until the adapter is placed
    /// in an enumeration by [`enumerate_adapters`].
    #[must_use]
    pub fn from_info(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            vendor: info.vendor,
            device: info.device,
            ordinal: 0,
            kind: DeviceKind::from(info.device_type),
            backend: info.backend.to_string(),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
            pci_bus_id: info.device_pci_bus_id.clone(),
        }
    }

    /// Vendor name for the PCI vendor ID: NVIDIA, AMD, Intel or Microsoft (WARP); otherwise `Unknown`.
    #[must_use]
    pub fn vendor_name(&self) -> &'static str {
        match self.vendor {
            0x10DE => "NVIDIA",
            0x1002 => "AMD",
            0x8086 => "Intel",
            0x1414 => "Microsoft",
            _ => "Unknown",
        }
    }

    /// The [`GpuKey`] that identifies this adapter across launches. Store this, not the adapter's position.
    #[must_use]
    pub fn gpu_key(&self) -> GpuKey {
        GpuKey {
            vendor_id: self.vendor,
            device_id: self.device,
            ordinal: self.ordinal,
            name: self.name.clone(),
        }
    }

    /// True when both descriptions name the same physical device, ignoring the backend.
    fn same_device(&self, other: &Self) -> bool {
        self.vendor == other.vendor
            && self.device == other.device
            && self.pci_bus_id == other.pci_bus_id
            && self.name == other.name
    }
}

/// Which adapter the user asked for.
///
/// Serialized as an internally tagged object whose `mode` is camelCase. The JSON shapes are:
///
/// ```text
/// {"mode":"auto"}
/// {"mode":"software"}
/// {"mode":"specific","vendor":4098,"device":30096,"ordinal":1,"name":"AMD Radeon RX 9060 XT"}
/// ```
///
/// `vendor` and `device` are PCI IDs in decimal (4098 is 0x1002, AMD; 30096 is 0x7590). `ordinal` tells apart
/// identical GPUs. `name` is used only when no adapter reports the stored vendor and device IDs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AdapterChoice {
    /// Highest-performance hardware adapter; falls back to the software adapter if none is usable.
    #[default]
    Auto,
    /// The software (WARP) adapter, regardless of installed hardware.
    Software,
    /// A specific adapter: the `ordinal`-th GPU with this vendor and device ID.
    Specific {
        /// PCI vendor ID.
        vendor: u32,
        /// PCI device ID.
        device: u32,
        /// Index among identical GPUs (same vendor and device IDs), in enumeration order.
        ordinal: u32,
        /// Adapter name recorded when the choice was made. Used only when no adapter has `vendor` and `device`,
        /// and then only among adapters from the same vendor.
        name: String,
    },
}

impl AdapterChoice {
    /// The choice that selects the adapter identified by `key`.
    #[must_use]
    pub fn from_gpu_key(key: &GpuKey) -> Self {
        Self::Specific {
            vendor: key.vendor_id,
            device: key.device_id,
            ordinal: key.ordinal,
            name: key.name.clone(),
        }
    }
}

/// Errors from adapter selection, device creation and GPU readback.
#[derive(Debug, Error)]
pub enum GpuError {
    /// No adapter could be opened for the requested choice or any fallback.
    #[error("no GPU adapter is available")]
    NoAdapter,
    /// No hardware (non-software) adapter is available.
    #[error("no hardware GPU adapter is available")]
    NoHardwareAdapter,
    /// The software (WARP) adapter is not available.
    #[error("no software GPU adapter is available")]
    NoSoftwareAdapter,
    /// No enumerated adapter matches the requested `Specific` choice.
    #[error("no GPU adapter matches \"{0}\"")]
    NoMatchingAdapter(String),
    /// The driver refused to hand out an adapter.
    #[error("failed to request a GPU adapter: {0}")]
    RequestAdapter(#[from] wgpu::RequestAdapterError),
    /// The adapter refused to create a device with the requested features or limits.
    #[error("failed to create a GPU device: {0}")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    /// Polling the device for completion failed.
    #[error("GPU poll failed: {0}")]
    Poll(#[from] wgpu::PollError),
    /// The device was lost, so no further GPU work can run on this context.
    #[error("the GPU device was lost")]
    DeviceLost,
    /// A validation error was raised while the readback was encoded, submitted or mapped.
    #[error("GPU validation error during readback: {0}")]
    Validation(String),
    /// A buffer could not be mapped for reading.
    #[error("GPU buffer mapping failed: {0}")]
    Map(String),
    /// A readback needs a texture format whose bytes are RGBA8.
    #[error("texture format {0:?} is not RGBA8")]
    UnsupportedFormat(wgpu::TextureFormat),
    /// Readbacks copy single-sampled textures only; a multisampled texture must be resolved first.
    #[error("readback needs a single-sampled texture, found {0} samples")]
    UnsupportedSampleCount(u32),
    /// The source resource was not created with `COPY_SRC` usage.
    #[error("the resource was not created with COPY_SRC usage")]
    MissingCopySrc,
    /// The requested readback region is larger than the resource.
    #[error(
        "requested {requested_width}x{requested_height} exceeds the {available_width}x{available_height} resource"
    )]
    OutOfBounds {
        /// Requested width in pixels.
        requested_width: u32,
        /// Requested height in pixels.
        requested_height: u32,
        /// Resource width in pixels.
        available_width: u32,
        /// Resource height in pixels.
        available_height: u32,
    },
    /// The readback size is invalid or exceeds device limits.
    #[error("invalid GPU readback size: {0}")]
    InvalidSize(String),
}

/// A GPU device plus the adapter it came from. Shared by preview and export.
#[derive(Debug)]
pub struct GpuContext {
    /// Logical device used to create resources and pipelines.
    pub device: wgpu::Device,
    /// Queue used to submit work.
    pub queue: wgpu::Queue,
    /// Physical adapter the device was created from.
    pub adapter: wgpu::Adapter,
    /// Description of [`GpuContext::adapter`], including its [`AdapterDesc::ordinal`].
    pub desc: AdapterDesc,
    /// Set by the device-lost callback.
    lost: Arc<AtomicBool>,
    /// True when `new` used a fallback step instead of the requested choice.
    fell_back: bool,
}

/// One step in the adapter-selection chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The adapter named by an `AdapterChoice::Specific`.
    Specific,
    /// The highest-performance non-software adapter.
    Hardware,
    /// The software (WARP) adapter.
    Software,
}

/// Lists every adapter wgpu can see on this machine (DX12 only on Windows), without duplicates.
///
/// Identical GPUs are all listed, each with its [`AdapterDesc::ordinal`]. Duplicate reports of one device under
/// different backends are collapsed to the first backend listed.
#[must_use]
pub fn enumerate_adapters() -> Vec<AdapterDesc> {
    let instance = create_instance();
    let adapters = pollster::block_on(instance.enumerate_adapters(preferred_backends()));
    describe_adapters(&raw_descs(&adapters))
        .into_iter()
        .map(|(_, desc)| desc)
        .collect()
}

impl GpuContext {
    /// Creates a GPU context for `choice`, falling back through the chain described in the module docs.
    ///
    /// The returned context records whether a fallback step was used (see [`GpuContext::fell_back`]).
    /// When every step fails, the error from the first (requested) step is returned.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError`] when no step produced a usable device.
    pub fn new(choice: &AdapterChoice) -> Result<Self, GpuError> {
        let instance = create_instance();
        let steps: &[Step] = match choice {
            AdapterChoice::Specific { .. } => &[Step::Specific, Step::Hardware, Step::Software],
            AdapterChoice::Auto => &[Step::Hardware, Step::Software],
            AdapterChoice::Software => &[Step::Software],
        };

        let mut first_error: Option<GpuError> = None;
        for (index, step) in steps.iter().enumerate() {
            let attempt = select_adapter(&instance, choice, *step).and_then(open_device);
            match attempt {
                Ok(mut context) => {
                    context.fell_back = index > 0;
                    if context.fell_back {
                        tracing::warn!(
                            adapter = %context.desc.name,
                            step = ?step,
                            "requested GPU adapter unavailable; using fallback"
                        );
                    } else {
                        tracing::info!(adapter = %context.desc.name, step = ?step, "GPU context ready");
                    }
                    return Ok(context);
                }
                Err(error) => {
                    tracing::warn!(step = ?step, %error, "GPU adapter step failed");
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or(GpuError::NoAdapter))
    }

    /// True once the device-lost callback has fired. The context must be rebuilt before further use.
    ///
    /// [`wgpu::Device::destroy`] does not fire the callback by itself: it fires from a later device poll, once
    /// the queue is idle. Poll the device before relying on this flag.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// True when this context runs on the software (WARP) adapter.
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.desc.kind == DeviceKind::Cpu
    }

    /// True when `new` did not open the requested choice and used a fallback step instead.
    #[must_use]
    pub fn fell_back(&self) -> bool {
        self.fell_back
    }

    /// Returns [`GpuError::DeviceLost`] if the device has been lost.
    fn ensure_alive(&self) -> Result<(), GpuError> {
        if self.is_lost() {
            Err(GpuError::DeviceLost)
        } else {
            Ok(())
        }
    }
}

/// Name of the DX12 shader compiler wgpu uses in this process: `"DXC"` or `"FXC"` (`"n/a"` off Windows).
///
/// wgpu's `Dx12Compiler::Auto` loads `dxcompiler.dll` when the DLL search order finds it and falls back to FXC
/// (`d3dcompiler_47.dll`, shipped with Windows) otherwise. This function reports that choice by probing the same
/// directories, in the same order, that a bare `LoadLibrary("dxcompiler.dll")` searches: the executable's
/// directory, `%SystemRoot%\System32`, `%SystemRoot%`, the current directory, then `PATH`. It checks that the file
/// exists, not that it loads.
///
/// Static DXC is not compiled in. The `static-dxc` feature of `mach-dxcompiler-rs` downloads an unpinned binary
/// during the build, which breaks offline, reproducible builds. FXC is accepted for v1 (see
/// `docs/architecture.md`); revisit it if a shader needs SM 6 features.
#[must_use]
pub fn shader_compiler_name() -> &'static str {
    #[cfg(windows)]
    {
        compiler_name_in(&dxc_search_dirs())
    }
    #[cfg(not(windows))]
    {
        "n/a"
    }
}

/// `"DXC"` when `dxcompiler.dll` is present in one of `dirs`, otherwise `"FXC"`.
#[cfg(any(windows, test))]
fn compiler_name_in(dirs: &[PathBuf]) -> &'static str {
    if dirs.iter().any(|dir| dir.join(DXC_DLL).is_file()) {
        "DXC"
    } else {
        "FXC"
    }
}

/// The DLL that wgpu's `Auto` DX12 compiler loads when it is present.
#[cfg(any(windows, test))]
const DXC_DLL: &str = "dxcompiler.dll";

/// Directories a bare DLL name is searched in, in the default (SafeDllSearchMode) order.
#[cfg(windows)]
fn dxc_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
    {
        dirs.push(exe_dir);
    }
    if let Some(system_root) = std::env::var_os("SystemRoot").map(PathBuf::from) {
        dirs.push(system_root.join("System32"));
        dirs.push(system_root);
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

/// Reads a whole RGBA8 texture region back to the CPU, using a fresh staging buffer.
///
/// Copies `width` × `height` pixels from the top-left of `texture` and returns tightly packed rows (`width * 4`
/// bytes each, no row padding). The texture must have `COPY_SRC` usage, be single-sampled, and have format
/// `Rgba8Unorm` or `Rgba8UnormSrgb`. This is a convenience wrapper around [`Readback::read_rgba8`]; in a loop, keep
/// a [`Readback`] so the staging buffer is reused.
///
/// This blocks the calling thread until the GPU has finished all submitted work (`Device::poll(Wait)`).
///
/// `texture` must come from `ctx.device` (or another device on the same adapter). A texture from a different
/// wgpu instance, that is from another [`GpuContext`], makes wgpu-core panic on the unknown resource.
///
/// # Errors
///
/// Returns [`GpuError`] for an invalid texture, an out-of-range region, a lost device, a validation error or a
/// failed map.
pub fn read_texture_rgba8(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, GpuError> {
    Readback::new().read_rgba8(ctx, texture, width, height)
}

/// Reusable GPU-to-CPU RGBA8 readback that keeps its staging buffers between calls.
///
/// Allocating a `MAP_READ` buffer for every frame costs a driver round trip, so a `Readback` keeps one buffer per
/// byte size and hands it out again. The buffers belong to the device that created them: use one `Readback` per
/// [`GpuContext`] and drop it when the context is rebuilt. A buffer whose use failed is discarded rather than
/// reused. Dropping the `Readback` releases the buffers.
#[derive(Debug, Default)]
pub struct Readback {
    /// Idle staging buffers, keyed by their size in bytes.
    staging: HashMap<u64, wgpu::Buffer>,
}

impl Readback {
    /// Creates a readback with no staging buffers allocated yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a `width` × `height` region of `texture` back to the CPU as tightly packed RGBA8 rows.
    ///
    /// The texture must have `COPY_SRC` usage, be single-sampled, and have format `Rgba8Unorm` or
    /// `Rgba8UnormSrgb`. Like [`read_texture_rgba8`], this blocks the calling thread until the GPU has finished all
    /// submitted work. `texture` must come from `ctx.device` or another device on the same adapter (see
    /// [`read_texture_rgba8`]).
    ///
    /// # Errors
    ///
    /// Returns [`GpuError`] for an invalid texture, an out-of-range region, a lost device, a validation error
    /// raised while copying or mapping, or a failed map.
    pub fn read_rgba8(
        &mut self,
        ctx: &GpuContext,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, GpuError> {
        ctx.ensure_alive()?;
        check_rgba8_source(texture, width, height)?;
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }

        let row_bytes = to_usize(unpadded_row_bytes(width)?)?;
        let padded_row = padded_row_bytes(width)?;
        let padded = to_usize(padded_row)?;
        let rows = to_usize(height)?;
        let buffer_size = u64::from(padded_row) * u64::from(height);
        check_buffer_size(ctx, buffer_size)?;

        // Every error from the copy, the submit and the map is captured here, not reported to the
        // uncaptured-error handler. The scope is popped on every path, including the early returns.
        let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let staging = self
            .staging
            .remove(&buffer_size)
            .unwrap_or_else(|| create_staging(ctx, buffer_size));
        copy_texture_to_staging(ctx, texture, &staging, padded_row, width, height);
        let outcome = with_mapped(ctx, &staging, buffer_size, |mapped| {
            unpack_rows(mapped, row_bytes, padded, rows)
        });
        let validation = pollster::block_on(scope.pop());
        if let Some(error) = validation {
            return Err(GpuError::Validation(error.to_string()));
        }
        // A failed map or unpack drops `staging` here; only a buffer that completed its map is returned to the pool.
        let pixels = outcome?;
        self.keep(buffer_size, staging);
        Ok(pixels)
    }

    /// Returns an idle staging buffer to the pool, dropping the whole pool first when it is full.
    fn keep(&mut self, size: u64, buffer: wgpu::Buffer) {
        if self.staging.len() >= MAX_POOLED_STAGING {
            self.staging.clear();
        }
        self.staging.insert(size, buffer);
    }
}

/// Reads `size` bytes from the start of `src` back to the CPU.
///
/// `src` must have `COPY_SRC` usage. The copy is queued after any work already submitted to the queue, so call
/// this after submitting the compute or render pass that wrote `src`. `size` must be a multiple of
/// [`wgpu::COPY_BUFFER_ALIGNMENT`] (4 bytes).
///
/// This blocks the calling thread until the GPU has finished all submitted work (`Device::poll(Wait)`).
/// `src` must come from `ctx.device` or another device on the same adapter, as for [`read_texture_rgba8`].
///
/// # Errors
///
/// Returns [`GpuError`] for an invalid buffer, a size mismatch, a lost device, a validation error or a failed map.
pub fn read_buffer(ctx: &GpuContext, src: &wgpu::Buffer, size: u64) -> Result<Vec<u8>, GpuError> {
    ctx.ensure_alive()?;
    if size == 0 {
        return Ok(Vec::new());
    }
    if !size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT) {
        return Err(GpuError::InvalidSize(format!(
            "{size} bytes is not a multiple of {}",
            wgpu::COPY_BUFFER_ALIGNMENT
        )));
    }
    if !src.usage().contains(wgpu::BufferUsages::COPY_SRC) {
        return Err(GpuError::MissingCopySrc);
    }
    if size > src.size() {
        return Err(GpuError::InvalidSize(format!(
            "{size} bytes exceeds the {}-byte buffer",
            src.size()
        )));
    }
    check_buffer_size(ctx, size)?;

    let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let staging = create_staging(ctx, size);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("loopcut-readback-buffer"),
        });
    encoder.copy_buffer_to_buffer(src, 0, &staging, 0, size);
    ctx.queue.submit(std::iter::once(encoder.finish()));

    let outcome = with_mapped(ctx, &staging, size, |mapped| mapped.to_vec());
    let validation = pollster::block_on(scope.pop());
    if let Some(error) = validation {
        return Err(GpuError::Validation(error.to_string()));
    }
    outcome
}

/// Checks the texture requirements of an RGBA8 readback. Order matters: the format and sample count are reported
/// before usage or bounds, so the most specific problem wins.
fn check_rgba8_source(texture: &wgpu::Texture, width: u32, height: u32) -> Result<(), GpuError> {
    let format = texture.format();
    if !matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb
    ) {
        return Err(GpuError::UnsupportedFormat(format));
    }
    let samples = texture.sample_count();
    if samples != 1 {
        return Err(GpuError::UnsupportedSampleCount(samples));
    }
    if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
        return Err(GpuError::MissingCopySrc);
    }
    let available = texture.size();
    if width > available.width || height > available.height {
        return Err(GpuError::OutOfBounds {
            requested_width: width,
            requested_height: height,
            available_width: available.width,
            available_height: available.height,
        });
    }
    Ok(())
}

/// Creates a `MAP_READ` staging buffer of `size` bytes.
fn create_staging(ctx: &GpuContext, size: u64) -> wgpu::Buffer {
    ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("loopcut-readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

/// Queues and submits a copy of the top-left `width` × `height` region of `texture` into `staging`.
fn copy_texture_to_staging(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    staging: &wgpu::Buffer,
    padded_row: u32,
    width: u32,
    height: u32,
) {
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("loopcut-readback-rgba8"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit(std::iter::once(encoder.finish()));
}

/// Copies the first `row_bytes` of each of `rows` padded rows of `mapped` into one tightly packed buffer.
fn unpack_rows(mapped: &[u8], row_bytes: usize, padded_row: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(row_bytes.saturating_mul(rows));
    if padded_row == 0 {
        return out;
    }
    for row in mapped.chunks_exact(padded_row).take(rows) {
        if let Some(bytes) = row.get(..row_bytes) {
            out.extend_from_slice(bytes);
        }
    }
    out
}

/// Opens a device on the adapter selected by `step`.
fn select_adapter(
    instance: &wgpu::Instance,
    choice: &AdapterChoice,
    step: Step,
) -> Result<wgpu::Adapter, GpuError> {
    match (step, choice) {
        (
            Step::Specific,
            AdapterChoice::Specific {
                vendor,
                device,
                ordinal,
                name,
            },
        ) => select_specific(instance, *vendor, *device, *ordinal, name),
        (Step::Specific, _) => Err(GpuError::NoAdapter),
        (Step::Hardware, _) => request_hardware(instance),
        (Step::Software, _) => request_software(instance),
    }
}

/// Finds the enumerated adapter that matches a `Specific` choice.
fn select_specific(
    instance: &wgpu::Instance,
    vendor: u32,
    device: u32,
    ordinal: u32,
    name: &str,
) -> Result<wgpu::Adapter, GpuError> {
    let no_match = || GpuError::NoMatchingAdapter(name.to_owned());
    let adapters = pollster::block_on(instance.enumerate_adapters(preferred_backends()));
    let candidates = describe_adapters(&raw_descs(&adapters));
    let descs: Vec<AdapterDesc> = candidates.iter().map(|(_, desc)| desc.clone()).collect();
    let position = match_specific(&descs, vendor, device, ordinal, name).ok_or_else(no_match)?;
    let index = candidates
        .get(position)
        .map(|(index, _)| *index)
        .ok_or_else(no_match)?;
    adapters.into_iter().nth(index).ok_or_else(no_match)
}

/// Highest-performance adapter that is not a software rasterizer.
///
/// wgpu-core still returns WARP when no hardware adapter exists (it sorts software adapters last), so the
/// device type is checked here.
fn request_hardware(instance: &wgpu::Instance) -> Result<wgpu::Adapter, GpuError> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))?;
    if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        return Err(GpuError::NoHardwareAdapter);
    }
    Ok(adapter)
}

/// The software (WARP on DX12) adapter.
fn request_software(instance: &wgpu::Instance) -> Result<wgpu::Adapter, GpuError> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: true,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))?;
    if adapter.get_info().device_type != wgpu::DeviceType::Cpu {
        return Err(GpuError::NoSoftwareAdapter);
    }
    Ok(adapter)
}

/// Creates the device and queue for `adapter` and installs the device-lost and error handlers.
fn open_device(adapter: wgpu::Adapter) -> Result<GpuContext, GpuError> {
    let desc = AdapterDesc::from_info(&adapter.get_info());
    // `Limits::defaults` (8192 px textures) rather than downlevel limits (2048 px), so 4K sources fit. No
    // optional features are requested, so every device that meets the defaults behaves the same.
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("loopcut-gpu"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::defaults(),
        ..Default::default()
    }))?;

    let lost = Arc::new(AtomicBool::new(false));
    let lost_flag = Arc::clone(&lost);
    device.set_device_lost_callback(move |reason, message| {
        lost_flag.store(true, Ordering::Release);
        match reason {
            wgpu::DeviceLostReason::Destroyed => {
                tracing::debug!(%message, "GPU device destroyed");
            }
            wgpu::DeviceLostReason::Unknown => {
                tracing::error!(%message, "GPU device lost");
            }
        }
    });
    // wgpu panics on uncaptured errors by default. A failed command should be logged, not abort the app.
    device.on_uncaptured_error(Arc::new(|error: wgpu::Error| {
        tracing::error!(%error, "uncaptured GPU error");
    }));

    Ok(GpuContext {
        device,
        queue,
        adapter,
        desc,
        lost,
        fell_back: false,
    })
}

/// Creates a wgpu instance restricted to [`preferred_backends`] with DX12 shader compilation set to `Auto`.
fn create_instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = preferred_backends();
    desc.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Auto;
    wgpu::Instance::new(desc)
}

/// DX12 only on Windows (so WARP is the software adapter); every native backend elsewhere.
#[cfg(windows)]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::DX12
}

/// Every native backend on non-Windows platforms.
#[cfg(not(windows))]
fn preferred_backends() -> wgpu::Backends {
    wgpu::Backends::PRIMARY
}

/// Adapter descriptions in the order wgpu enumerated them, before deduplication or ordinals.
fn raw_descs(adapters: &[wgpu::Adapter]) -> Vec<AdapterDesc> {
    adapters
        .iter()
        .map(|adapter| AdapterDesc::from_info(&adapter.get_info()))
        .collect()
}

/// Keeps the adapters to offer, in enumeration order, and gives each one its ordinal.
///
/// A device reported again under a later backend (same [`AdapterDesc::same_device`] identity) is dropped. Each
/// survivor's ordinal counts the survivors that come before it with the same vendor and device IDs, so the
/// ordinals of identical GPUs are 0, 1, and so on. Returns `(index, desc)` pairs, where `index` is the position in
/// `raw`.
fn describe_adapters(raw: &[AdapterDesc]) -> Vec<(usize, AdapterDesc)> {
    let mut kept: Vec<(usize, AdapterDesc)> = Vec::with_capacity(raw.len());
    for (index, reported) in raw.iter().enumerate() {
        let reported_by_other_backend = kept.iter().any(|(_, existing)| {
            existing.backend != reported.backend && existing.same_device(reported)
        });
        if reported_by_other_backend {
            continue;
        }
        let identical_before = kept
            .iter()
            .filter(|(_, existing)| {
                existing.vendor == reported.vendor && existing.device == reported.device
            })
            .count();
        let mut desc = reported.clone();
        desc.ordinal = u32::try_from(identical_before).unwrap_or(u32::MAX);
        kept.push((index, desc));
    }
    kept
}

/// Index of the candidate that matches a `Specific` choice.
///
/// When any candidate has `vendor` and `device`, the one with `ordinal` is chosen; a missing ordinal does not
/// match, so the caller falls back rather than picking a different identical card. When no candidate has those IDs
/// (for example after a driver change), the adapter name is used as a fallback, within the same vendor. An empty
/// name never matches.
fn match_specific(
    candidates: &[AdapterDesc],
    vendor: u32,
    device: u32,
    ordinal: u32,
    name: &str,
) -> Option<usize> {
    let same_model =
        |candidate: &AdapterDesc| candidate.vendor == vendor && candidate.device == device;
    if candidates.iter().any(same_model) {
        candidates
            .iter()
            .position(|candidate| same_model(candidate) && candidate.ordinal == ordinal)
    } else if name.is_empty() {
        None
    } else {
        candidates
            .iter()
            .position(|candidate| candidate.vendor == vendor && candidate.name == name)
    }
}

/// Bytes per row for `width` RGBA8 pixels, before row padding.
fn unpadded_row_bytes(width: u32) -> Result<u32, GpuError> {
    width
        .checked_mul(4)
        .ok_or_else(|| GpuError::InvalidSize(format!("row of {width} RGBA pixels overflows")))
}

/// Bytes per row for `width` RGBA8 pixels, padded to [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`].
fn padded_row_bytes(width: u32) -> Result<u32, GpuError> {
    unpadded_row_bytes(width)?
        .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or_else(|| GpuError::InvalidSize(format!("padded row of {width} pixels overflows")))
}

/// Rejects readbacks larger than the device's `max_buffer_size`.
fn check_buffer_size(ctx: &GpuContext, size: u64) -> Result<(), GpuError> {
    let max = ctx.device.limits().max_buffer_size;
    if size > max {
        return Err(GpuError::InvalidSize(format!(
            "{size} bytes exceeds the device's {max}-byte buffer limit"
        )));
    }
    Ok(())
}

/// Converts a `u32`/`u64` size to `usize`, failing cleanly on 32-bit targets.
fn to_usize<T>(value: T) -> Result<usize, GpuError>
where
    T: TryInto<usize>,
    T::Error: std::fmt::Display,
{
    value
        .try_into()
        .map_err(|error: T::Error| GpuError::InvalidSize(error.to_string()))
}

/// Maps `buffer[..size]` for reading, runs `read` on the mapped bytes, and unmaps.
///
/// On failure the buffer is left mapped or pending, so the caller must not reuse it.
fn with_mapped<T>(
    ctx: &GpuContext,
    buffer: &wgpu::Buffer,
    size: u64,
    read: impl FnOnce(&[u8]) -> T,
) -> Result<T, GpuError> {
    let slice = buffer.slice(..size);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        // The receiver is alive for the rest of this function, so a failed send only means it was dropped.
        let _ = sender.send(result);
    });
    ctx.device.poll(wgpu::PollType::wait_indefinitely())?;

    let status = receiver
        .recv_timeout(MAP_CALLBACK_TIMEOUT)
        .map_err(|_| GpuError::Map("the map callback did not run".to_owned()))?;
    if let Err(error) = status {
        ctx.ensure_alive()?;
        return Err(GpuError::Map(error.to_string()));
    }

    let view = slice
        .get_mapped_range()
        .map_err(|error| GpuError::Map(error.to_string()))?;
    let value = read(&view);
    drop(view);
    buffer.unmap();
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMD_NAME: &str = "AMD Radeon RX 9060 XT";

    fn desc(backend: &str, vendor: u32, device: u32, pci: &str, name: &str) -> AdapterDesc {
        AdapterDesc {
            name: name.to_owned(),
            vendor,
            device,
            ordinal: 0,
            kind: DeviceKind::Discrete,
            backend: backend.to_owned(),
            driver: String::new(),
            driver_info: String::new(),
            pci_bus_id: pci.to_owned(),
        }
    }

    #[test]
    fn vendor_names_cover_known_ids() {
        let named = |vendor| desc("dx12", vendor, 0, "", "").vendor_name();
        assert_eq!(named(0x10DE), "NVIDIA");
        assert_eq!(named(0x1002), "AMD");
        assert_eq!(named(0x8086), "Intel");
        assert_eq!(named(0x1414), "Microsoft");
        assert_eq!(named(0x1234), "Unknown");
    }

    #[test]
    fn device_types_map_to_kinds() {
        assert_eq!(
            DeviceKind::from(wgpu::DeviceType::DiscreteGpu),
            DeviceKind::Discrete
        );
        assert_eq!(
            DeviceKind::from(wgpu::DeviceType::IntegratedGpu),
            DeviceKind::Integrated
        );
        assert_eq!(
            DeviceKind::from(wgpu::DeviceType::VirtualGpu),
            DeviceKind::Virtual
        );
        assert_eq!(DeviceKind::from(wgpu::DeviceType::Cpu), DeviceKind::Cpu);
        assert_eq!(DeviceKind::from(wgpu::DeviceType::Other), DeviceKind::Other);
    }

    #[test]
    fn identical_gpus_are_told_apart_by_ordinal() {
        let candidates = [
            AdapterDesc {
                ordinal: 0,
                ..desc("dx12", 0x1002, 0x7590, "0000:03:00.0", AMD_NAME)
            },
            AdapterDesc {
                ordinal: 1,
                ..desc("dx12", 0x1002, 0x7590, "0000:03:00.0", AMD_NAME)
            },
        ];
        assert_eq!(
            match_specific(&candidates, 0x1002, 0x7590, 1, AMD_NAME),
            Some(1)
        );
        assert_eq!(
            match_specific(&candidates, 0x1002, 0x7590, 0, AMD_NAME),
            Some(0)
        );
    }

    #[test]
    fn missing_ordinal_does_not_match_another_identical_gpu() {
        let candidates = [AdapterDesc {
            ordinal: 0,
            ..desc("dx12", 0x1002, 0x7590, "", AMD_NAME)
        }];
        assert_eq!(
            match_specific(&candidates, 0x1002, 0x7590, 1, AMD_NAME),
            None
        );
    }

    #[test]
    fn specific_match_falls_back_to_name_when_model_unknown() {
        let candidates = [desc("dx12", 0x10DE, 0x2684, "", "NVIDIA GeForce RTX 4090")];
        // The stored device ID is gone but the vendor and name still match.
        assert_eq!(
            match_specific(&candidates, 0x10DE, 0x2685, 0, "NVIDIA GeForce RTX 4090"),
            Some(0)
        );
        // An empty name never matches.
        assert_eq!(match_specific(&candidates, 0x10DE, 0x2685, 0, ""), None);
    }

    #[test]
    fn specific_match_rejects_other_vendor() {
        let candidates = [desc("dx12", 0x1002, 0x7590, "", AMD_NAME)];
        assert_eq!(
            match_specific(&candidates, 0x10DE, 0x7590, 0, AMD_NAME),
            None
        );
    }

    #[test]
    fn describe_keeps_identical_gpus_and_numbers_them() {
        let pair = [
            desc("dx12", 0x1002, 0x7590, "0000:03:00.0", AMD_NAME),
            desc("dx12", 0x1002, 0x7590, "0000:03:00.0", AMD_NAME),
        ];
        let kept = describe_adapters(&pair);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].0, 0);
        assert_eq!(kept[1].0, 1);
        assert_eq!(kept[0].1.ordinal, 0);
        assert_eq!(kept[1].1.ordinal, 1);
    }

    #[test]
    fn describe_collapses_same_device_across_backends() {
        let list = [
            desc("vulkan", 0x10DE, 0x2684, "0000:01:00.0", "NVIDIA RTX 4090"),
            desc("gl", 0x10DE, 0x2684, "0000:01:00.0", "NVIDIA RTX 4090"),
            desc("vulkan", 0x1002, 0x7590, "0000:03:00.0", AMD_NAME),
        ];
        let kept = describe_adapters(&list);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].0, 0);
        assert_eq!(kept[0].1.backend, "vulkan");
        assert_eq!(kept[1].1.vendor, 0x1002);
        // The duplicate is gone, so the AMD card is the first of its model.
        assert_eq!(kept[1].1.ordinal, 0);
    }

    #[test]
    fn padded_rows_align_to_copy_alignment() {
        assert_eq!(padded_row_bytes(1).expect("1px"), 256);
        assert_eq!(padded_row_bytes(64).expect("64px"), 256);
        assert_eq!(padded_row_bytes(65).expect("65px"), 512);
        assert_eq!(padded_row_bytes(333).expect("333px"), 1536);
        assert_eq!(padded_row_bytes(1920).expect("1920px"), 7680);
    }

    #[test]
    fn padded_rows_reject_overflow() {
        assert!(matches!(
            padded_row_bytes(u32::MAX / 4 + 1),
            Err(GpuError::InvalidSize(_))
        ));
    }

    #[test]
    fn unpack_rows_strips_padding() {
        // Two rows of 3 useful bytes in 4-byte padded rows.
        let mapped: [u8; 8] = [1, 2, 3, 9, 4, 5, 6, 9];
        assert_eq!(unpack_rows(&mapped, 3, 4, 2), vec![1, 2, 3, 4, 5, 6]);
        assert!(unpack_rows(&mapped, 3, 0, 2).is_empty());
    }

    #[test]
    fn adapter_choice_defaults_to_auto() {
        assert_eq!(AdapterChoice::default(), AdapterChoice::Auto);
    }

    #[test]
    fn adapter_choice_json_shape_round_trips() {
        use serde_json::json;

        let specific = AdapterChoice::Specific {
            vendor: 0x1002,
            device: 0x7590,
            ordinal: 1,
            name: AMD_NAME.to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&specific).expect("serialize specific"),
            json!({"mode": "specific", "vendor": 4098, "device": 30096, "ordinal": 1, "name": AMD_NAME})
        );
        assert_eq!(
            serde_json::to_value(AdapterChoice::Auto).expect("serialize auto"),
            json!({"mode": "auto"})
        );
        assert_eq!(
            serde_json::to_value(AdapterChoice::Software).expect("serialize software"),
            json!({"mode": "software"})
        );

        for choice in [AdapterChoice::Auto, AdapterChoice::Software, specific] {
            let text = serde_json::to_string(&choice).expect("serialize");
            let back: AdapterChoice = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(back, choice, "round trip of {text}");
        }

        // The ordinal is required: a specific choice without it does not silently pick the first card.
        let missing_ordinal = r#"{"mode":"specific","vendor":4098,"device":30096,"name":"x"}"#;
        assert!(serde_json::from_str::<AdapterChoice>(missing_ordinal).is_err());
    }

    #[test]
    fn gpu_key_round_trips_through_choice() {
        let adapter = AdapterDesc {
            ordinal: 1,
            ..desc("dx12", 0x1002, 0x7590, "", AMD_NAME)
        };
        let key = adapter.gpu_key();
        assert_eq!(key.vendor_id, 0x1002);
        assert_eq!(key.device_id, 0x7590);
        assert_eq!(key.ordinal, 1);
        assert_eq!(key.name, AMD_NAME);

        let choice = AdapterChoice::from_gpu_key(&key);
        assert_eq!(
            choice,
            AdapterChoice::Specific {
                vendor: 0x1002,
                device: 0x7590,
                ordinal: 1,
                name: AMD_NAME.to_owned(),
            }
        );
        let candidates = [
            AdapterDesc {
                ordinal: 0,
                ..desc("dx12", 0x1002, 0x7590, "", AMD_NAME)
            },
            adapter,
        ];
        let AdapterChoice::Specific {
            vendor,
            device,
            ordinal,
            name,
        } = &choice
        else {
            panic!("from_gpu_key must produce a Specific choice, got {choice:?}");
        };
        assert_eq!(
            match_specific(&candidates, *vendor, *device, *ordinal, name),
            Some(1)
        );
    }

    #[test]
    fn compiler_probe_follows_dxc_dll_presence() {
        assert_eq!(compiler_name_in(&[]), "FXC");

        let dir = std::env::temp_dir().join(format!("loopcut-dxc-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create probe directory");
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).expect("create empty directory");

        // An empty directory does not count, and a present dxcompiler.dll does.
        assert_eq!(compiler_name_in(std::slice::from_ref(&empty)), "FXC");
        std::fs::write(dir.join(DXC_DLL), b"").expect("write probe dll");
        assert_eq!(compiler_name_in(std::slice::from_ref(&dir)), "DXC");
        assert_eq!(compiler_name_in(&[empty.clone(), dir.clone()]), "DXC");

        std::fs::remove_dir_all(&dir).expect("remove probe directory");
        assert_eq!(compiler_name_in(std::slice::from_ref(&dir)), "FXC");
    }

    #[cfg(windows)]
    #[test]
    fn shader_compiler_name_reports_the_live_probe() {
        let name = shader_compiler_name();
        assert!(name == "DXC" || name == "FXC", "unexpected compiler {name}");
        assert_eq!(name, compiler_name_in(&dxc_search_dirs()));
    }
}

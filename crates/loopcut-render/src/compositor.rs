//! The compositor: one wgpu pipeline that turns decoded frames into finished RGBA output for preview and export.
//!
//! [`Compositor::render`] takes one decoded frame (NV12, P010 or RGBA8), an optional second frame for a
//! crossfade, premultiplied overlays and the output parameters, and returns straight-alpha RGBA8 pixels.
//! [`Compositor::render_into`] does the same but writes into a caller-owned buffer, so a per-frame loop can reuse
//! one allocation. Preview and export call the same methods, so what the user sees is what gets encoded.
//!
//! Shading lives in `shaders/composite.wgsl` (one fullscreen colour pass), the colour grade in `shaders/grade.wgsl`
//! (concatenated into that pass) and `shaders/overlay.wgsl` (one quad per overlay). SDR values stay in the encoded
//! domain. [`Compositor::set_grade`] sets the grade that every later render applies. HDR (PQ or HLG) is linearized to display nits, gamut-mapped
//! to BT.709 with negative light clipped, passed through a luminance-preserving highlight rolloff and sRGB encoded.
//! Diffuse white (203 nits) encodes to about 244 of 255, and 1000 nits reaches 255.
//!
//! Known limits of v0:
//! * Downscaling is bilinear without a prefilter, so large reductions alias.
//! * SDR sources tagged with BT.2020 primaries are not gamut-mapped; the wide-gamut conversion only runs for HDR.
//! * YUV frames need even dimensions (the layouts in `loopcut-core` assume them).
//! * A crossfade (frame B) is sampled through frame A's crop and orientation, so B must have A's size and layout.
//! * wgpu error scopes are per thread. A validation error is reported to the thread that calls `render`, and
//!   other threads sharing the device do not see it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use loopcut_core::geometry::normalize_rotation;
use loopcut_core::{
    ColorInfo, ColorRange, NormRect, Orientation, PixelLayout, TransferFunction, YuvMatrix,
};
use thiserror::Error;

use crate::gpu::{GpuContext, GpuError};
use crate::grade::{GradeSetup, GradeUniforms, TABLE_SIZE};

/// The colour pass: `composite.wgsl` followed by `grade.wgsl`, which holds the grade stages that the pass calls.
const COMPOSITE_WGSL: &str = concat!(
    include_str!("../../../shaders/composite.wgsl"),
    "\n",
    include_str!("../../../shaders/grade.wgsl"),
);
const OVERLAY_WGSL: &str = include_str!("../../../shaders/overlay.wgsl");

/// Format of the output, of every overlay texture and of the RGBA8 input plane.
const RGBA8: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Overlay textures that have not been used for this many renders are dropped.
const OVERLAY_TTL_RENDERS: u64 = 60;

/// Longest wait for a buffer map callback after the device has been polled (see `gpu.rs`).
const MAP_CALLBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Serializes pipeline creation within the process.
///
/// Observed on Windows (DX12 with the WARP and RX 9060 XT adapters): building two compositors on different threads
/// at the same time crashed the process with an access violation or segfault, intermittently. Serializing the
/// builds removed the crash in repeated full-suite runs. The shader compiler is the suspected cause; rendering was
/// not affected, so only construction takes this lock.
///
/// The lock covers shader and pipeline creation only. It does not cover [`GpuContext::new`] or the other device
/// calls that build a compositor's buffers. With a device per test, the compositor test binary still segfaulted
/// once in twenty runs at eight threads, even with this lock. The tests therefore share one device per adapter
/// (`tests/compositor.rs`), which is also how the app runs: one device, many compositors.
static PIPELINE_BUILD_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The lock that serializes GPU pipeline construction in this process.
///
/// [`Compositor::new`] holds it while it builds its pipelines. Other crates that build pipelines on the same device
/// (for example the GIF dither in `loopcut-export`) should hold it too, for the same reason. It is a
/// `parking_lot` mutex, so it does not poison, and it is held only for construction, never across a render.
#[must_use]
pub fn pipeline_build_lock() -> &'static parking_lot::Mutex<()> {
    &PIPELINE_BUILD_LOCK
}

/// One decoded frame to composite.
///
/// `data` must hold exactly `layout.frame_bytes(width, height)` bytes in the layout described on
/// [`PixelLayout`]. YUV layouts need even dimensions.
#[derive(Debug, Clone, Copy)]
pub struct InputFrame<'a> {
    /// Memory layout of `data`.
    pub layout: PixelLayout,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Packed pixel data.
    pub data: &'a [u8],
    /// Colour description used for YUV to RGB conversion and HDR handling.
    pub color: ColorInfo,
}

/// A premultiplied RGBA8 overlay (captions, watermarks, guides) drawn over the output after colour conversion.
///
/// The uploaded texture is cached by `id`. A later overlay with the same id and size reuses the texture and skips
/// the upload, so callers must change `id` whenever the pixels change. The visible overlays of one render call
/// must have distinct ids; a repeated id is rejected with [`CompositorError::InvalidOverlay`]. Zero-sized overlays
/// are skipped before this check, so they may share an id.
#[derive(Debug, Clone, Copy)]
pub struct Overlay<'a> {
    /// Premultiplied RGBA8 pixels, `width * height * 4` bytes, top row first.
    pub rgba_premultiplied: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Destination rectangle in normalized output space.
    pub dest: NormRect,
    /// Opacity in `[0, 1]` applied to the whole overlay.
    pub opacity: f32,
    /// Cache key for the uploaded texture.
    pub id: u64,
}

/// Output geometry and display transform for one render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositeParams {
    /// Output width in pixels.
    pub output_width: u32,
    /// Output height in pixels.
    pub output_height: u32,
    /// Crop in display space, after orientation. Use [`NormRect::FULL`] for no crop.
    pub crop: NormRect,
    /// Rotation and flips applied to the stored frame.
    pub orientation: Orientation,
    /// Colour shown behind transparent source pixels (sRGB). The output is always opaque, so the alpha byte is
    /// ignored.
    pub background: [u8; 4],
}

/// Errors from [`Compositor::new`] and [`Compositor::render`].
#[derive(Debug, Error)]
pub enum CompositorError {
    /// The GPU device failed: it was lost, a wgpu validation error was raised during the frame (reported as
    /// [`GpuError::Validation`]), or a readback could not run.
    #[error(transparent)]
    Gpu(#[from] GpuError),
    /// An input frame has the wrong size, layout or colour description, or a crossfade's frame B does not match
    /// frame A in size and layout.
    #[error("invalid input frame: {0}")]
    InvalidFrame(String),
    /// An overlay has the wrong size or pixel count, or two visible overlays of one render share an id.
    #[error("invalid overlay: {0}")]
    InvalidOverlay(String),
    /// The output size is zero, exceeds the device limits, or cannot be read back.
    #[error("invalid output: {0}")]
    InvalidOutput(String),
    /// A shader or pipeline failed validation on this device.
    #[error("shader or pipeline validation failed: {0}")]
    Pipeline(String),
    /// Polling the device for completion failed.
    #[error("GPU poll failed: {0}")]
    Poll(#[from] wgpu::PollError),
    /// The readback buffer could not be mapped.
    #[error("GPU buffer mapping failed: {0}")]
    Map(String),
    /// A colour grade cannot be applied on this device, for example a 3D LUT larger than the device's 3D texture
    /// limit.
    #[error("invalid colour grade: {0}")]
    InvalidGrade(String),
}

/// Per-input block of [`Globals`], mirrored by `InputUniform` in `composite.wgsl` (48 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct InputUniform {
    /// Luma width and height, then two unused components.
    size: [f32; 4],
    /// Kind, matrix, range (1 = full), transfer.
    info: [u32; 4],
    /// Wide-gamut flag, then unused components.
    extra: [u32; 4],
}

/// Uniform block of the colour pass, mirrored by `Globals` in `composite.wgsl` (528 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct Globals {
    out_size: [f32; 4],
    crop: [f32; 4],
    affine0: [f32; 4],
    affine1: [f32; 4],
    background: [f32; 4],
    blend: [f32; 4],
    a: InputUniform,
    b: InputUniform,
    grade: GradeUniforms,
}

/// Uniform block of one overlay draw, mirrored by `OverlayUniform` in `overlay.wgsl` (48 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct OverlayUniform {
    out_size: [f32; 4],
    rect: [f32; 4],
    params: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<InputUniform>() == 48);
const _: () = assert!(std::mem::size_of::<GradeUniforms>() == 336);
const _: () = assert!(std::mem::size_of::<Globals>() == 528);
const _: () = assert!(std::mem::size_of::<OverlayUniform>() == 48);

/// A texture together with the view the shaders bind.
struct PlaneView {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl PlaneView {
    fn new(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        usage: wgpu::TextureUsages,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }

    /// A cubic `size` x `size` x `size` 3D texture. The view takes the texture's own 3D dimension.
    fn new_3d(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        size: u32,
        usage: wgpu::TextureUsages,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: size,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

/// The three plane views of one uploaded input. Planes the layout does not use are 1x1 placeholders so the bind
/// group always has the same shape.
struct InputTextures {
    layout: PixelLayout,
    width: u32,
    height: u32,
    /// `R8Uint` (NV12) or `R16Uint` (P010) luma; a 1x1 placeholder for RGBA8.
    luma: PlaneView,
    /// `Rg8Uint` or `Rg16Uint` interleaved chroma at half resolution; a 1x1 placeholder for RGBA8.
    chroma: PlaneView,
    /// `Rgba8Unorm` pixels; a 1x1 placeholder for YUV.
    rgba: PlaneView,
}

impl InputTextures {
    fn new(device: &wgpu::Device, layout: PixelLayout, width: u32, height: u32) -> Self {
        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let (luma_format, chroma_format, luma_size, chroma_size, rgba_size) = match layout {
            PixelLayout::Nv12 => (
                wgpu::TextureFormat::R8Uint,
                wgpu::TextureFormat::Rg8Uint,
                (width, height),
                (width / 2, height / 2),
                (1, 1),
            ),
            PixelLayout::P010 => (
                wgpu::TextureFormat::R16Uint,
                wgpu::TextureFormat::Rg16Uint,
                (width, height),
                (width / 2, height / 2),
                (1, 1),
            ),
            PixelLayout::Rgba8 => (
                wgpu::TextureFormat::R8Uint,
                wgpu::TextureFormat::Rg8Uint,
                (1, 1),
                (1, 1),
                (width, height),
            ),
        };
        Self {
            layout,
            width,
            height,
            luma: PlaneView::new(
                device,
                "loopcut-input-luma",
                luma_format,
                luma_size.0,
                luma_size.1,
                usage,
            ),
            chroma: PlaneView::new(
                device,
                "loopcut-input-chroma",
                chroma_format,
                chroma_size.0,
                chroma_size.1,
                usage,
            ),
            rgba: PlaneView::new(
                device,
                "loopcut-input-rgba",
                RGBA8,
                rgba_size.0,
                rgba_size.1,
                usage,
            ),
        }
    }

    fn matches(&self, frame: &InputFrame<'_>) -> bool {
        self.layout == frame.layout && self.width == frame.width && self.height == frame.height
    }
}

/// The render target of the colour pass. It is also the source of the readback copy.
struct OutputTarget {
    width: u32,
    height: u32,
    plane: PlaneView,
}

impl OutputTarget {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        Self {
            width,
            height,
            plane: PlaneView::new(
                device,
                "loopcut-composite-output",
                RGBA8,
                width,
                height,
                usage,
            ),
        }
    }
}

/// An uploaded overlay texture and the render index that last used it.
struct OverlayTexture {
    width: u32,
    height: u32,
    plane: PlaneView,
    last_used: u64,
}

/// The shared compositor for one GPU device.
///
/// Create one per [`GpuContext`] and reuse it for every frame. Input textures, the output target, the readback
/// buffer, overlay textures and the uniform buffers are all kept between renders and reallocated only when their
/// size or layout changes.
pub struct Compositor {
    ctx: Arc<GpuContext>,
    composite_layout: wgpu::BindGroupLayout,
    composite_pipeline: wgpu::RenderPipeline,
    overlay_layout: wgpu::BindGroupLayout,
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_sampler: wgpu::Sampler,
    globals: wgpu::Buffer,
    overlay_uniforms: Vec<wgpu::Buffer>,
    inputs: [InputTextures; 2],
    output: OutputTarget,
    staging: wgpu::Buffer,
    overlays: HashMap<u64, OverlayTexture>,
    frame_no: u64,
    /// The grade stages applied by the next render. Zero flags mean no grade (see [`Compositor::set_grade`]).
    grade: GradeUniforms,
    /// Baked tone curves: `TABLE_SIZE` x 1, `Rgba32Float`. A placeholder until a grade with curves is set.
    grade_curve: PlaneView,
    /// Baked 1D LUT: `TABLE_SIZE` x 1, `Rgba32Float`. A placeholder until a grade with a 1D LUT is set.
    grade_lut1d: PlaneView,
    /// The 3D LUT: `grade_lut3d_size` cubed, `Rgba16Float`. A 2-cube placeholder until a grade with a 3D LUT is set.
    grade_lut3d: PlaneView,
    /// Edge length of `grade_lut3d`.
    grade_lut3d_size: u32,
    /// The [`GradeSetup::tables_key`] of the tables currently uploaded, or `None` when none match a setup.
    grade_tables_key: Option<u64>,
}

impl Compositor {
    /// Builds the pipelines and the reusable buffers on the device of `ctx`.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError::Pipeline`] when a shader fails validation on this device.
    pub fn new(ctx: Arc<GpuContext>) -> Result<Self, CompositorError> {
        let device = &ctx.device;
        let (composite_layout, composite_pipeline, overlay_layout, overlay_pipeline) = {
            let _build = pipeline_build_lock().lock();
            let (composite_layout, composite_pipeline) = build_composite_pipeline(device)?;
            let (overlay_layout, overlay_pipeline) = build_overlay_pipeline(device)?;
            (
                composite_layout,
                composite_pipeline,
                overlay_layout,
                overlay_pipeline,
            )
        };
        let overlay_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("loopcut-overlay-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loopcut-composite-globals"),
            size: std::mem::size_of::<Globals>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let inputs = [
            InputTextures::new(device, PixelLayout::Rgba8, 1, 1),
            InputTextures::new(device, PixelLayout::Rgba8, 1, 1),
        ];
        let output = OutputTarget::new(device, 1, 1);
        let staging = create_staging(device, u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
        let table_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let grade_curve = PlaneView::new(
            device,
            "loopcut-grade-curve",
            wgpu::TextureFormat::Rgba32Float,
            TABLE_SIZE as u32,
            1,
            table_usage,
        );
        let grade_lut1d = PlaneView::new(
            device,
            "loopcut-grade-lut1d",
            wgpu::TextureFormat::Rgba32Float,
            TABLE_SIZE as u32,
            1,
            table_usage,
        );
        let grade_lut3d_size = 2;
        let grade_lut3d = PlaneView::new_3d(
            device,
            "loopcut-grade-lut3d",
            wgpu::TextureFormat::Rgba16Float,
            grade_lut3d_size,
            table_usage,
        );
        Ok(Self {
            ctx,
            composite_layout,
            composite_pipeline,
            overlay_layout,
            overlay_pipeline,
            overlay_sampler,
            globals,
            overlay_uniforms: Vec::new(),
            inputs,
            output,
            staging,
            overlays: HashMap::new(),
            frame_no: 0,
            grade: *GradeSetup::identity().uniforms(),
            grade_curve,
            grade_lut1d,
            grade_lut3d,
            grade_lut3d_size,
            grade_tables_key: None,
        })
    }

    /// Sets the colour grade that every later render applies, until it is changed.
    ///
    /// `None` and an identity setup both turn the grade off. Renders then skip the grade stages, which costs no
    /// texture reads. The grade is applied after the crossfade and before the overlays, so captions are not graded.
    /// Preview and export share this compositor, so both apply the same grade. A source-only preview should pass
    /// `None` so that its frames are left ungraded.
    ///
    /// The uniform block is replaced on every call. The baked tables are uploaded only when their key
    /// ([`GradeSetup::tables_key`]) differs from the one already on the GPU, so a slider drag that changes only the
    /// uniforms does not re-upload a 3D LUT. Turning the grade off and on again also keeps the uploaded tables.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError::Gpu`] when the device is lost or the table upload fails validation, and
    /// [`CompositorError::InvalidGrade`] when the 3D LUT is larger than the device's 3D texture limit. On error the
    /// grade is reset to identity, so later renders are ungraded until a valid grade is set. The tables are then
    /// uploaded again by the next successful call, because their key was cleared.
    pub fn set_grade(&mut self, setup: Option<&GradeSetup>) -> Result<(), CompositorError> {
        let setup = match setup {
            Some(setup) if !setup.is_identity() => setup,
            _ => {
                self.grade = *GradeSetup::identity().uniforms();
                return Ok(());
            }
        };
        let ctx = Arc::clone(&self.ctx);
        let uploaded = if ctx.is_lost() {
            Err(GpuError::DeviceLost.into())
        } else if self.grade_tables_key != Some(setup.tables_key()) {
            self.upload_grade_tables(&ctx, setup)
        } else {
            Ok(())
        };
        match uploaded {
            Ok(()) => {
                self.grade = *setup.uniforms();
                Ok(())
            }
            Err(error) => {
                self.grade = *GradeSetup::identity().uniforms();
                Err(error)
            }
        }
    }

    /// Uploads the tables of `setup` and records their key on success. The key is cleared before the first write, so
    /// a failed upload leaves no key behind and the next call uploads again, even when a write went through before the
    /// failure.
    fn upload_grade_tables(
        &mut self,
        ctx: &GpuContext,
        setup: &GradeSetup,
    ) -> Result<(), CompositorError> {
        let device = &ctx.device;
        let queue = &ctx.queue;
        self.grade_tables_key = None;
        if let Some(lut) = setup.lut3d() {
            let size = lut.size();
            let max_dim = device.limits().max_texture_dimension_3d;
            if size > max_dim {
                return Err(CompositorError::InvalidGrade(format!(
                    "a {size}-cube 3D LUT exceeds the device's {max_dim}-texel 3D texture limit"
                )));
            }
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        if let Some(lut) = setup.lut3d() {
            let size = lut.size();
            if self.grade_lut3d_size != size {
                self.grade_lut3d = PlaneView::new_3d(
                    device,
                    "loopcut-grade-lut3d",
                    wgpu::TextureFormat::Rgba16Float,
                    size,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                );
                self.grade_lut3d_size = size;
            }
            let texels = lut.rgba16f();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.grade_lut3d.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(texels.as_slice()),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 8),
                    rows_per_image: Some(size),
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: size,
                },
            );
        }
        if let Some(table) = setup.curve_table() {
            write_table(queue, &self.grade_curve, table);
        }
        if let Some(table) = setup.lut1d_table() {
            write_table(queue, &self.grade_lut1d, table);
        }
        match pollster::block_on(scope.pop()) {
            Some(error) => Err(GpuError::Validation(error.to_string()).into()),
            None => {
                self.grade_tables_key = Some(setup.tables_key());
                Ok(())
            }
        }
    }

    /// Renders one composited frame and reads it back as tightly packed RGBA8 (`width * height * 4` bytes,
    /// alpha 255).
    ///
    /// `a` is the main frame. When `blend` is `Some((b, weight_b))`, the result is `a` mixed with `b` by
    /// `weight_b` in the encoded (gamma) domain: `0.0` shows only `a` and `1.0` only `b`. Frame `b` is sampled
    /// through the crop and orientation in `params`, which are those of `a`, so it must have the same size and
    /// layout as `a`. Overlays are drawn in order after colour conversion and the crossfade.
    ///
    /// This allocates the returned vector. A per-frame loop should call [`Compositor::render_into`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError`] for an invalid frame, overlay or output size, a lost device, or a failed
    /// readback.
    pub fn render(
        &mut self,
        a: &InputFrame<'_>,
        blend: Option<(&InputFrame<'_>, f32)>,
        overlays: &[Overlay<'_>],
        params: &CompositeParams,
    ) -> Result<Vec<u8>, CompositorError> {
        let mut out = Vec::new();
        self.render_into(a, blend, overlays, params, &mut out)?;
        Ok(out)
    }

    /// Renders like [`Compositor::render`] and writes the pixels into `out`.
    ///
    /// `out` is cleared and refilled with the bytes that `render` would return. Its allocation is reused when it is
    /// large enough, so a loop that keeps one buffer does not allocate per frame. On error `out` is left empty.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Compositor::render`].
    pub fn render_into(
        &mut self,
        a: &InputFrame<'_>,
        blend: Option<(&InputFrame<'_>, f32)>,
        overlays: &[Overlay<'_>],
        params: &CompositeParams,
        out: &mut Vec<u8>,
    ) -> Result<(), CompositorError> {
        out.clear();
        let ctx = Arc::clone(&self.ctx);
        if ctx.is_lost() {
            return Err(GpuError::DeviceLost.into());
        }
        let limits = ctx.device.limits();
        let max_dim = limits.max_texture_dimension_2d;
        let (out_w, out_h) = (params.output_width, params.output_height);
        validate_output(out_w, out_h, max_dim)?;
        let row_bytes = padded_row_bytes(out_w)?;
        let staging_size = u64::from(row_bytes) * u64::from(out_h);
        if staging_size > limits.max_buffer_size {
            return Err(CompositorError::InvalidOutput(format!(
                "a readback of {staging_size} bytes exceeds the {}-byte buffer limit",
                limits.max_buffer_size
            )));
        }

        validate_frame(a, "frame A", max_dim)?;
        if let Some((b, _)) = blend {
            validate_frame(b, "frame B", max_dim)?;
            validate_blend_pair(a, b)?;
        }
        for overlay in overlays {
            validate_overlay(overlay, max_dim)?;
        }
        let visible: Vec<&Overlay<'_>> = overlays
            .iter()
            .filter(|overlay| overlay.width > 0 && overlay.height > 0)
            .collect();
        validate_overlay_ids(&visible)?;

        let request = Request {
            a,
            blend,
            overlays: &visible,
            params,
            layout: ReadbackLayout {
                row_bytes,
                staging_size,
            },
        };
        // Every wgpu call from the uploads to the readback runs inside this scope. wgpu then reports a validation
        // failure as a value here, not through the device's uncaptured-error handler. The scope is popped on every
        // path, including when the GPU step failed for another reason.
        let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let outcome = self.encode_and_read(&ctx, &request, out);
        let validation = pollster::block_on(scope.pop());
        match (outcome, validation) {
            (Ok(()), None) => Ok(()),
            (_, Some(error)) => {
                self.reset_readback(&ctx.device, staging_size);
                out.clear();
                Err(GpuError::Validation(error.to_string()).into())
            }
            (Err(error), None) => {
                self.reset_readback(&ctx.device, staging_size);
                out.clear();
                Err(error)
            }
        }
    }

    /// The GPU half of [`Compositor::render_into`]: uploads the planes, draws, copies the output into the staging
    /// buffer and reads it back into `out`.
    fn encode_and_read(
        &mut self,
        ctx: &GpuContext,
        request: &Request<'_, '_>,
        out: &mut Vec<u8>,
    ) -> Result<(), CompositorError> {
        let device = &ctx.device;
        let queue = &ctx.queue;
        let params = request.params;
        let (out_w, out_h) = (params.output_width, params.output_height);
        self.frame_no += 1;
        let frame_no = self.frame_no;

        prepare_input(device, queue, &mut self.inputs[0], request.a)?;
        if let Some((b, _)) = request.blend {
            prepare_input(device, queue, &mut self.inputs[1], b)?;
        }
        if self.output.width != out_w || self.output.height != out_h {
            self.output = OutputTarget::new(device, out_w, out_h);
        }
        if self.staging.size() != request.layout.staging_size {
            self.staging = create_staging(device, request.layout.staging_size);
        }

        let globals = build_globals(request.a, request.blend, params, &self.grade);
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        for (index, overlay) in request.overlays.iter().enumerate() {
            upload_overlay(device, queue, &mut self.overlays, overlay, frame_no);
            while self.overlay_uniforms.len() <= index {
                self.overlay_uniforms.push(create_overlay_uniform(device));
            }
            if let Some(buffer) = self.overlay_uniforms.get(index) {
                let uniform = build_overlay_uniform(overlay, out_w, out_h);
                queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
            }
        }

        let b_slot = if request.blend.is_some() {
            &self.inputs[1]
        } else {
            &self.inputs[0]
        };
        let composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loopcut-composite-bind"),
            layout: &self.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.globals.as_entire_binding(),
                },
                plane_entry(1, &self.inputs[0].luma.view),
                plane_entry(2, &self.inputs[0].chroma.view),
                plane_entry(3, &self.inputs[0].rgba.view),
                plane_entry(4, &b_slot.luma.view),
                plane_entry(5, &b_slot.chroma.view),
                plane_entry(6, &b_slot.rgba.view),
                plane_entry(7, &self.grade_curve.view),
                plane_entry(8, &self.grade_lut1d.view),
                plane_entry(9, &self.grade_lut3d.view),
            ],
        });

        let mut overlay_bind_groups = Vec::with_capacity(request.overlays.len());
        for (index, overlay) in request.overlays.iter().enumerate() {
            let (Some(uniform), Some(texture)) = (
                self.overlay_uniforms.get(index),
                self.overlays.get(&overlay.id),
            ) else {
                return Err(CompositorError::InvalidOverlay(
                    "the overlay texture was not uploaded".to_owned(),
                ));
            };
            overlay_bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("loopcut-overlay-bind"),
                layout: &self.overlay_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    plane_entry(1, &texture.plane.view),
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.overlay_sampler),
                    },
                ],
            }));
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("loopcut-composite"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loopcut-composite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output.plane.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &composite_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        for bind_group in &overlay_bind_groups {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loopcut-overlay-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output.plane.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.output.plane.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(request.layout.row_bytes),
                    rows_per_image: Some(out_h),
                },
            },
            wgpu::Extent3d {
                width: out_w,
                height: out_h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        self.overlays
            .retain(|_, texture| frame_no.saturating_sub(texture.last_used) <= OVERLAY_TTL_RENDERS);
        read_back(
            device,
            &self.staging,
            out_w,
            out_h,
            request.layout.row_bytes,
            out,
        )
    }

    /// Replaces the readback buffer after a failed render. A failed map or poll can leave the old buffer pending
    /// or mapped, so the next render starts with a new one.
    fn reset_readback(&mut self, device: &wgpu::Device, size: u64) {
        self.staging = create_staging(device, size);
    }
}

/// One validated render request, passed from [`Compositor::render_into`] to the GPU step.
struct Request<'r, 'f> {
    /// The main frame.
    a: &'r InputFrame<'f>,
    /// The crossfade partner and its weight.
    blend: Option<(&'r InputFrame<'f>, f32)>,
    /// The overlays to draw, in order, without the zero-sized ones.
    overlays: &'r [&'r Overlay<'f>],
    /// Output geometry and display transform.
    params: &'r CompositeParams,
    /// Readback sizes for the output.
    layout: ReadbackLayout,
}

/// Byte layout of the readback for one output size.
#[derive(Debug, Clone, Copy)]
struct ReadbackLayout {
    /// Bytes per output row, padded to the copy alignment.
    row_bytes: u32,
    /// Size of the staging buffer in bytes: `row_bytes` times the output height.
    staging_size: u64,
}

/// Creates the colour pass pipeline (fullscreen triangle, `composite.wgsl`) and its bind group layout.
fn build_composite_pipeline(
    device: &wgpu::Device,
) -> Result<(wgpu::BindGroupLayout, wgpu::RenderPipeline), CompositorError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("loopcut-composite"),
        source: wgpu::ShaderSource::Wgsl(COMPOSITE_WGSL.into()),
    });
    let uint = wgpu::TextureSampleType::Uint;
    let float = wgpu::TextureSampleType::Float { filterable: true };
    // The grade tables are read with textureLoad, so they are declared unfilterable (Rgba32Float is not filterable).
    let table = wgpu::TextureSampleType::Float { filterable: false };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("loopcut-composite-layout"),
        entries: &[
            uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
            texture_entry(1, uint),
            texture_entry(2, uint),
            texture_entry(3, float),
            texture_entry(4, uint),
            texture_entry(5, uint),
            texture_entry(6, float),
            texture_entry(7, table),
            texture_entry(8, table),
            texture_entry_3d(9, table),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("loopcut-composite-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("loopcut-composite"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: RGBA8,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    if let Some(error) = pollster::block_on(scope.pop()) {
        return Err(CompositorError::Pipeline(error.to_string()));
    }
    Ok((layout, pipeline))
}

/// Creates the overlay pipeline (one premultiplied quad per overlay, `overlay.wgsl`) and its bind group layout.
fn build_overlay_pipeline(
    device: &wgpu::Device,
) -> Result<(wgpu::BindGroupLayout, wgpu::RenderPipeline), CompositorError> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("loopcut-overlay"),
        source: wgpu::ShaderSource::Wgsl(OVERLAY_WGSL.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("loopcut-overlay-layout"),
        entries: &[
            uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
            texture_entry(1, wgpu::TextureSampleType::Float { filterable: true }),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("loopcut-overlay-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("loopcut-overlay"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: RGBA8,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    if let Some(error) = pollster::block_on(scope.pop()) {
        return Err(CompositorError::Pipeline(error.to_string()));
    }
    Ok((layout, pipeline))
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    texture_entry_dim(binding, sample_type, wgpu::TextureViewDimension::D2)
}

fn texture_entry_3d(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    texture_entry_dim(binding, sample_type, wgpu::TextureViewDimension::D3)
}

fn texture_entry_dim(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
    view_dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn plane_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn create_overlay_uniform(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("loopcut-overlay-uniform"),
        size: std::mem::size_of::<OverlayUniform>() as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// The readback buffer. It is written by the colour pass copy and mapped after each submit.
fn create_staging(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("loopcut-composite-readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

/// Uploads the planes of `frame` into `slot`, reallocating them when the layout or size changed.
fn prepare_input(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut InputTextures,
    frame: &InputFrame<'_>,
) -> Result<(), CompositorError> {
    if !slot.matches(frame) {
        *slot = InputTextures::new(device, frame.layout, frame.width, frame.height);
    }
    let (w, h) = (frame.width, frame.height);
    match frame.layout {
        PixelLayout::Nv12 => {
            let (luma, chroma) = split_planes(frame.data, w as usize * h as usize)?;
            write_plane(queue, &slot.luma, luma, w, h, w);
            write_plane(queue, &slot.chroma, chroma, w / 2, h / 2, w);
        }
        PixelLayout::P010 => {
            let (luma, chroma) = split_planes(frame.data, w as usize * h as usize * 2)?;
            write_plane(queue, &slot.luma, luma, w, h, 2 * w);
            write_plane(queue, &slot.chroma, chroma, w / 2, h / 2, 2 * w);
        }
        PixelLayout::Rgba8 => write_plane(queue, &slot.rgba, frame.data, w, h, 4 * w),
    }
    Ok(())
}

/// Splits a packed YUV frame into its luma and interleaved chroma planes.
fn split_planes(data: &[u8], luma_bytes: usize) -> Result<(&[u8], &[u8]), CompositorError> {
    data.split_at_checked(luma_bytes).ok_or_else(|| {
        CompositorError::InvalidFrame("the planes do not fit in the frame data".to_owned())
    })
}

/// Writes `data` into the top-left of `plane`. `bytes_per_row` is the unpadded row size.
fn write_plane(
    queue: &wgpu::Queue,
    plane: &PlaneView,
    data: &[u8],
    width: u32,
    height: u32,
    bytes_per_row: u32,
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &plane.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Writes a baked table of `TABLE_SIZE` RGBA f32 entries into a `TABLE_SIZE` x 1 texture.
fn write_table(queue: &wgpu::Queue, plane: &PlaneView, table: &[[f32; 4]]) {
    let width = TABLE_SIZE as u32;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &plane.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(table),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 16),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// Uploads an overlay texture, or keeps the cached one when the id and size match.
fn upload_overlay(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cache: &mut HashMap<u64, OverlayTexture>,
    overlay: &Overlay<'_>,
    frame_no: u64,
) {
    let reusable = cache
        .get(&overlay.id)
        .is_some_and(|texture| texture.width == overlay.width && texture.height == overlay.height);
    if reusable {
        if let Some(texture) = cache.get_mut(&overlay.id) {
            texture.last_used = frame_no;
        }
        return;
    }
    let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    let plane = PlaneView::new(
        device,
        "loopcut-overlay",
        RGBA8,
        overlay.width,
        overlay.height,
        usage,
    );
    write_plane(
        queue,
        &plane,
        overlay.rgba_premultiplied,
        overlay.width,
        overlay.height,
        4 * overlay.width,
    );
    cache.insert(
        overlay.id,
        OverlayTexture {
            width: overlay.width,
            height: overlay.height,
            plane,
            last_used: frame_no,
        },
    );
}

/// Packs the colour-pass uniforms for one render.
fn build_globals(
    a: &InputFrame<'_>,
    blend: Option<(&InputFrame<'_>, f32)>,
    params: &CompositeParams,
    grade: &GradeUniforms,
) -> Globals {
    let crop = sanitize_crop(params.crop);
    let orientation = Orientation {
        rotation: normalize_rotation(params.orientation.rotation),
        ..params.orientation
    };
    let affine = orientation.display_to_stored_affine();
    let [bg_r, bg_g, bg_b, _] = params.background;
    let (weight, present, b_frame) = match blend {
        Some((b, weight)) => (sanitize_weight(weight), 1.0, b),
        None => (0.0, 0.0, a),
    };
    Globals {
        out_size: [
            params.output_width as f32,
            params.output_height as f32,
            0.0,
            0.0,
        ],
        crop: [crop.x as f32, crop.y as f32, crop.w as f32, crop.h as f32],
        affine0: [affine[0], affine[1], affine[2], 0.0],
        affine1: [affine[3], affine[4], affine[5], 0.0],
        background: [
            f32::from(bg_r) / 255.0,
            f32::from(bg_g) / 255.0,
            f32::from(bg_b) / 255.0,
            1.0,
        ],
        blend: [weight, present, 0.0, 0.0],
        a: input_uniform(a),
        b: input_uniform(b_frame),
        grade: *grade,
    }
}

/// Packs the per-input uniforms (see `InputUniform` in `composite.wgsl`).
fn input_uniform(frame: &InputFrame<'_>) -> InputUniform {
    let kind = match frame.layout {
        PixelLayout::Nv12 => 0,
        PixelLayout::P010 => 1,
        PixelLayout::Rgba8 => 2,
    };
    let matrix = match frame.color.matrix {
        YuvMatrix::Bt601 => 0,
        YuvMatrix::Bt709 | YuvMatrix::Rgb => 1,
        YuvMatrix::Bt2020 => 2,
    };
    let full_range = u32::from(frame.color.range == ColorRange::Full);
    let transfer = match frame.color.transfer {
        TransferFunction::Sdr => 0,
        TransferFunction::Pq => 1,
        TransferFunction::Hlg => 2,
    };
    InputUniform {
        size: [frame.width as f32, frame.height as f32, 0.0, 0.0],
        info: [kind, matrix, full_range, transfer],
        extra: [u32::from(frame.color.wide_gamut), 0, 0, 0],
    }
}

/// Packs the uniforms of one overlay quad, in output pixels.
fn build_overlay_uniform(overlay: &Overlay<'_>, out_w: u32, out_h: u32) -> OverlayUniform {
    let (ow, oh) = (f64::from(out_w), f64::from(out_h));
    let dest = overlay.dest;
    let opacity = if overlay.opacity.is_finite() {
        overlay.opacity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    OverlayUniform {
        out_size: [out_w as f32, out_h as f32, 0.0, 0.0],
        rect: [
            finite_or_zero(dest.x * ow),
            finite_or_zero(dest.y * oh),
            finite_or_zero(dest.w * ow),
            finite_or_zero(dest.h * oh),
        ],
        params: [opacity, 0.0, 0.0, 0.0],
    }
}

fn finite_or_zero(value: f64) -> f32 {
    if value.is_finite() { value as f32 } else { 0.0 }
}

/// Keeps a crop inside the unit square. Non-finite values fall back to the full frame.
fn sanitize_crop(crop: NormRect) -> NormRect {
    if [crop.x, crop.y, crop.w, crop.h]
        .iter()
        .all(|value| value.is_finite())
    {
        crop.clamped()
    } else {
        NormRect::FULL
    }
}

fn sanitize_weight(weight: f32) -> f32 {
    if weight.is_finite() {
        weight.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn validate_output(width: u32, height: u32, max_dim: u32) -> Result<(), CompositorError> {
    if width == 0 || height == 0 {
        return Err(CompositorError::InvalidOutput(
            "the output has no pixels".to_owned(),
        ));
    }
    if width > max_dim || height > max_dim {
        return Err(CompositorError::InvalidOutput(format!(
            "{width}x{height} exceeds the device's {max_dim}-pixel texture limit"
        )));
    }
    Ok(())
}

/// Bytes per row for `width` RGBA8 pixels, padded to the copy alignment.
fn padded_row_bytes(width: u32) -> Result<u32, CompositorError> {
    width
        .checked_mul(4)
        .and_then(|row| row.checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT))
        .ok_or_else(|| CompositorError::InvalidOutput(format!("a row of {width} pixels overflows")))
}

fn validate_frame(frame: &InputFrame<'_>, what: &str, max_dim: u32) -> Result<(), CompositorError> {
    let invalid = |reason: String| CompositorError::InvalidFrame(format!("{what}: {reason}"));
    if frame.width == 0 || frame.height == 0 {
        return Err(invalid("the frame has no pixels".to_owned()));
    }
    if frame.width > max_dim || frame.height > max_dim {
        return Err(invalid(format!(
            "{}x{} exceeds the {max_dim}-pixel texture limit",
            frame.width, frame.height
        )));
    }
    if frame.layout != PixelLayout::Rgba8 {
        if !frame.width.is_multiple_of(2) || !frame.height.is_multiple_of(2) {
            return Err(invalid("YUV frames need even dimensions".to_owned()));
        }
        if frame.color.matrix == YuvMatrix::Rgb {
            return Err(invalid(
                "a YUV layout cannot carry the RGB matrix".to_owned(),
            ));
        }
    }
    let expected = frame.layout.frame_bytes(frame.width, frame.height);
    if frame.data.len() != expected {
        return Err(invalid(format!(
            "{} bytes were given, {expected} expected",
            frame.data.len()
        )));
    }
    Ok(())
}

fn validate_overlay(overlay: &Overlay<'_>, max_dim: u32) -> Result<(), CompositorError> {
    if overlay.width == 0 || overlay.height == 0 {
        return Ok(());
    }
    if overlay.width > max_dim || overlay.height > max_dim {
        return Err(CompositorError::InvalidOverlay(format!(
            "{}x{} exceeds the {max_dim}-pixel texture limit",
            overlay.width, overlay.height
        )));
    }
    let expected = overlay.width as usize * overlay.height as usize * 4;
    if overlay.rgba_premultiplied.len() != expected {
        return Err(CompositorError::InvalidOverlay(format!(
            "{} bytes were given, {expected} expected",
            overlay.rgba_premultiplied.len()
        )));
    }
    Ok(())
}

/// Rejects a crossfade whose second frame differs from the first in size or layout. Frame B is sampled through
/// frame A's crop and orientation, and the readback shares one output geometry, so the two must match.
fn validate_blend_pair(a: &InputFrame<'_>, b: &InputFrame<'_>) -> Result<(), CompositorError> {
    if a.width != b.width || a.height != b.height {
        return Err(CompositorError::InvalidFrame(format!(
            "frame B is {}x{} but frame A is {}x{}; a crossfade needs equal sizes",
            b.width, b.height, a.width, a.height
        )));
    }
    if a.layout != b.layout {
        return Err(CompositorError::InvalidFrame(format!(
            "frame B is {:?} but frame A is {:?}; a crossfade needs equal layouts",
            b.layout, a.layout
        )));
    }
    Ok(())
}

/// Rejects two visible overlays that share an id. The texture cache is keyed by id, so the second upload would
/// replace the first texture in the same render.
fn validate_overlay_ids(visible: &[&Overlay<'_>]) -> Result<(), CompositorError> {
    let mut seen = HashSet::with_capacity(visible.len());
    for overlay in visible {
        if !seen.insert(overlay.id) {
            return Err(CompositorError::InvalidOverlay(format!(
                "overlay id {} is used by more than one visible overlay",
                overlay.id
            )));
        }
    }
    Ok(())
}

/// Copies the output rows out of the mapped staging buffer into `out`, dropping the row padding, then unmaps it.
fn read_back(
    device: &wgpu::Device,
    staging: &wgpu::Buffer,
    width: u32,
    height: u32,
    row_bytes: u32,
    out: &mut Vec<u8>,
) -> Result<(), CompositorError> {
    let size = u64::from(row_bytes) * u64::from(height);
    let slice = staging.slice(..size);
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |status| {
        // The receiver lives until this function returns, so a failed send only means it was dropped.
        let _ = sender.send(status);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let status = receiver
        .recv_timeout(MAP_CALLBACK_TIMEOUT)
        .map_err(|_| CompositorError::Map("the map callback did not run".to_owned()))?;
    status.map_err(|error| CompositorError::Map(error.to_string()))?;

    let unpadded = width as usize * 4;
    let expected = unpadded * height as usize;
    let result = match slice.get_mapped_range() {
        Ok(view) => {
            let padded = row_bytes as usize;
            out.clear();
            out.reserve(expected);
            for row in view.chunks_exact(padded).take(height as usize) {
                if let Some(pixel_row) = row.get(..unpadded) {
                    out.extend_from_slice(pixel_row);
                }
            }
            if out.len() == expected {
                Ok(())
            } else {
                Err(CompositorError::Map(format!(
                    "the readback held {} bytes, {expected} expected",
                    out.len()
                )))
            }
        }
        Err(error) => Err(CompositorError::Map(error.to_string())),
    };
    staging.unmap();
    result
}

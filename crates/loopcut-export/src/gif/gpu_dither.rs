//! GPU ordered dithering: one compute pass maps each composited RGBA frame onto the global palette.
//!
//! [`GpuDitherer`] runs `dither.wgsl`, which does the same integer arithmetic as [`super::dither::map_frame`], so GPU
//! and CPU give the same indices. [`DitherMapper`] is what the export uses. It takes the GPU when the device is
//! hardware, and drops to the CPU for the rest of the export after a GPU failure. Software (WARP) devices always use
//! the CPU path, because the CPU path is the same work without the readback.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use loopcut_render::gpu::{GpuContext, read_buffer};

use super::GifError;
use super::dither::{DitherSpec, MATRIX_CAPACITY, map_frame};
use super::palette::MAX_PALETTE_COLORS;

const DITHER_WGSL: &str = include_str!("dither.wgsl");

/// Side of a compute workgroup. Matches `@workgroup_size(8, 8, 1)` in the shader.
const WORKGROUP: u32 = 8;

/// Uniform block. Layout matches `struct Params` in `dither.wgsl` (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    width: u32,
    height: u32,
    palette_len: u32,
    amplitude: u32,
    matrix_size: u32,
    pad: [u32; 3],
}

/// The compute pipeline of the dither pass and its bind group layout. They depend on the device only, not on the frame
/// size, the palette or the dither spec, so a run builds them once and every [`GpuDitherer`] of the run shares them.
#[derive(Clone)]
pub struct DitherPipeline {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl DitherPipeline {
    /// Compiles the dither shader and builds its pipeline on the device of `ctx`.
    ///
    /// # Errors
    ///
    /// [`GifError::Gpu`] when a validation error is raised while the pipeline is built.
    pub fn new(ctx: &GpuContext) -> Result<Self, GifError> {
        let device = &ctx.device;
        // Error scopes are per thread in wgpu: this scope sees only errors raised on this thread.
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (layout, pipeline) = {
            // Shared with the compositor's pipeline builds: building pipelines on several threads at once crashed the
            // process on Windows.
            let _build = loopcut_render::compositor::pipeline_build_lock().lock();
            build_pipeline(device)
        };
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(GifError::Gpu(error.to_string()));
        }
        Ok(Self { layout, pipeline })
    }
}

/// Ordered dithering on one device for one frame size and palette.
pub struct GpuDitherer {
    ctx: Arc<GpuContext>,
    width: u32,
    height: u32,
    /// RGBA8 input, `width * height * 4` bytes.
    input: wgpu::Buffer,
    /// One `u32` palette index per pixel.
    output: wgpu::Buffer,
    output_bytes: u64,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::ComputePipeline,
}

impl GpuDitherer {
    /// Uploads the palette, thresholds and parameters for a `width` by `height` frame, on the shared `pipeline`.
    ///
    /// # Errors
    ///
    /// [`GifError::InvalidOptions`] for an empty frame, a palette outside `1..=255` or a malformed threshold matrix.
    /// [`GifError::Gpu`] when the device is lost, a buffer is larger than the device allows, or a validation error is
    /// raised.
    pub fn new(
        ctx: Arc<GpuContext>,
        pipeline: &DitherPipeline,
        width: u32,
        height: u32,
        palette: &[[u8; 3]],
        spec: &DitherSpec,
    ) -> Result<Self, GifError> {
        if width == 0 || height == 0 {
            return Err(GifError::InvalidOptions(format!(
                "dither frame {width}x{height} must not be empty"
            )));
        }
        if palette.is_empty() || palette.len() > usize::from(MAX_PALETTE_COLORS) {
            return Err(GifError::InvalidOptions(format!(
                "palette of {} entries must be 1..={MAX_PALETTE_COLORS}",
                palette.len()
            )));
        }
        let cells = (spec.matrix_size as usize) * (spec.matrix_size as usize);
        if spec.matrix_size == 0 || cells > MATRIX_CAPACITY || spec.thresholds.len() != cells {
            return Err(GifError::InvalidOptions(format!(
                "threshold matrix of side {} does not match its {} values",
                spec.matrix_size,
                spec.thresholds.len()
            )));
        }
        if ctx.is_lost() {
            return Err(GifError::Gpu("the GPU device was lost".to_owned()));
        }
        let pixel_bytes = u64::from(width) * u64::from(height) * 4;
        let limit = ctx.device.limits().max_storage_buffer_binding_size;
        if pixel_bytes > limit {
            return Err(GifError::Gpu(format!(
                "a {width}x{height} frame needs {pixel_bytes} bytes, above the {limit}-byte storage limit"
            )));
        }

        let device = &ctx.device;
        // Only this thread's errors reach this scope (see `DitherPipeline::new`).
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let storage = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let input = storage("loopcut-dither-input", pixel_bytes);
        let output = storage("loopcut-dither-output", pixel_bytes);
        let palette_buffer = storage("loopcut-dither-palette", (256 * 16) as u64);
        let thresholds = storage("loopcut-dither-thresholds", (MATRIX_CAPACITY * 4) as u64);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loopcut-dither-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut entries = [[0i32; 4]; 256];
        for (entry, color) in entries.iter_mut().zip(palette) {
            *entry = [
                i32::from(color[0]),
                i32::from(color[1]),
                i32::from(color[2]),
                0,
            ];
        }
        let queue = &ctx.queue;
        queue.write_buffer(&palette_buffer, 0, bytemuck::cast_slice(&entries));
        queue.write_buffer(
            &thresholds,
            0,
            bytemuck::cast_slice(&spec.padded_thresholds()),
        );
        let block = Params {
            width,
            height,
            palette_len: palette.len() as u32,
            amplitude: spec.amplitude,
            matrix_size: spec.matrix_size,
            pad: [0; 3],
        };
        queue.write_buffer(&params, 0, bytemuck::bytes_of(&block));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loopcut-dither-bind-group"),
            layout: &pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: palette_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: thresholds.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(GifError::Gpu(error.to_string()));
        }
        Ok(Self {
            ctx,
            width,
            height,
            input,
            output,
            output_bytes: pixel_bytes,
            bind_group,
            pipeline: pipeline.pipeline.clone(),
        })
    }

    /// Maps one RGBA8 frame (`width * height * 4` bytes) onto the palette. `out` receives one index per pixel.
    ///
    /// # Errors
    ///
    /// [`GifError::InvalidFrame`] for a buffer of the wrong length, [`GifError::Gpu`] for a lost device, a validation
    /// error or a failed readback. After a `Gpu` error the caller should map the frame on the CPU.
    pub fn dither(&mut self, rgba: &[u8], out: &mut Vec<u8>) -> Result<(), GifError> {
        let expected = (self.width as usize) * (self.height as usize) * 4;
        if rgba.len() != expected {
            return Err(GifError::InvalidFrame(format!(
                "frame has {} bytes but {}x{} RGBA needs {expected}",
                rgba.len(),
                self.width,
                self.height
            )));
        }
        if self.ctx.is_lost() {
            return Err(GifError::Gpu("the GPU device was lost".to_owned()));
        }
        let device = &self.ctx.device;
        // Only this thread's errors reach this scope (see `DitherPipeline::new`).
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        self.ctx.queue.write_buffer(&self.input, 0, rgba);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("loopcut-dither"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("loopcut-dither-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(
                self.width.div_ceil(WORKGROUP),
                self.height.div_ceil(WORKGROUP),
                1,
            );
        }
        self.ctx.queue.submit(std::iter::once(encoder.finish()));
        let read = read_buffer(&self.ctx, &self.output, self.output_bytes)
            .map_err(|err| GifError::Gpu(err.to_string()));
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(GifError::Gpu(error.to_string()));
        }
        let bytes = read?;
        out.clear();
        // Indices are below 256, so the low byte of each little-endian u32 is the index.
        out.extend(bytes.as_chunks::<4>().0.iter().map(|word| word[0]));
        Ok(())
    }
}

/// The compute pipeline and its bind group layout.
fn build_pipeline(device: &wgpu::Device) -> (wgpu::BindGroupLayout, wgpu::ComputePipeline) {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("loopcut-gif-dither"),
        source: wgpu::ShaderSource::Wgsl(DITHER_WGSL.into()),
    });
    let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("loopcut-gif-dither-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            storage(1, true),
            storage(2, true),
            storage(3, true),
            storage(4, false),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("loopcut-gif-dither-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("loopcut-gif-dither"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    (layout, pipeline)
}

/// The run's dither pipeline: the one in `slot`, or a new one built into `slot` on first use.
fn shared_pipeline(
    ctx: &GpuContext,
    slot: &mut Option<DitherPipeline>,
) -> Result<DitherPipeline, GifError> {
    if let Some(pipeline) = slot {
        return Ok(pipeline.clone());
    }
    let pipeline = DitherPipeline::new(ctx)?;
    *slot = Some(pipeline.clone());
    Ok(pipeline)
}

/// Maps frames for the GPU-dither engine: the GPU when it is usable, the CPU otherwise.
pub struct DitherMapper {
    width: u32,
    height: u32,
    palette: Vec<[u8; 3]>,
    spec: DitherSpec,
    gpu: Option<GpuDitherer>,
}

impl DitherMapper {
    /// Prepares the mapper. `gpu` is the device to try, or `None` to use the CPU from the start. A software device is
    /// treated as `None`. `pipeline` is the run's dither pipeline: it is built into the slot on first use, and later
    /// mappers of the run reuse it. When the GPU cannot be set up, the returned reason says why and the CPU is used.
    pub fn new(
        gpu: Option<Arc<GpuContext>>,
        pipeline: &mut Option<DitherPipeline>,
        width: u32,
        height: u32,
        palette: &[[u8; 3]],
        spec: DitherSpec,
    ) -> (Self, Option<String>) {
        let mut reason = None;
        let gpu = match gpu {
            Some(ctx) if !ctx.is_software() => {
                let built = shared_pipeline(&ctx, pipeline).and_then(|shared| {
                    GpuDitherer::new(ctx, &shared, width, height, palette, &spec)
                });
                match built {
                    Ok(dither) => Some(dither),
                    Err(err) => {
                        reason = Some(err.to_string());
                        None
                    }
                }
            }
            _ => None,
        };
        (
            Self {
                width,
                height,
                palette: palette.to_vec(),
                spec,
                gpu,
            },
            reason,
        )
    }

    /// True while frames are mapped on the GPU.
    pub fn uses_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// Maps one RGBA8 frame to palette indices in `out`. Returns the reason when the GPU pass failed for this frame: the
    /// frame is then mapped on the CPU, and every later frame uses the CPU too. Returns `None` otherwise.
    pub fn map(&mut self, rgba: &[u8], out: &mut Vec<u8>) -> Option<String> {
        if let Some(gpu) = self.gpu.as_mut() {
            match gpu.dither(rgba, out) {
                Ok(()) => return None,
                Err(err) => {
                    self.gpu = None;
                    map_frame(
                        rgba,
                        self.width,
                        self.height,
                        &self.palette,
                        &self.spec,
                        out,
                    );
                    return Some(err.to_string());
                }
            }
        }
        map_frame(
            rgba,
            self.width,
            self.height,
            &self.palette,
            &self.spec,
            out,
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gif::dither::{map_frame, spec_for};
    use loopcut_core::project::Dither;
    use loopcut_render::gpu::AdapterChoice;

    /// Set to `1` to make a missing software adapter a failure, so the GPU tests cannot pass by skipping.
    const REQUIRE_ENV: &str = "LOOPCUT_REQUIRE_GPU_TESTS";

    /// The software (WARP) device, or `None` (after a note) when no software adapter exists. Under [`REQUIRE_ENV`] a
    /// missing adapter fails the test.
    fn software_gpu() -> Option<Arc<GpuContext>> {
        match GpuContext::new(&AdapterChoice::Software) {
            Ok(ctx) => Some(Arc::new(ctx)),
            Err(err) => {
                let required = std::env::var(REQUIRE_ENV).is_ok_and(|value| value.trim() == "1");
                assert!(
                    !required,
                    "{REQUIRE_ENV}=1 but there is no software GPU adapter: {err}"
                );
                eprintln!("skipping: no software GPU adapter: {err}");
                None
            }
        }
    }

    /// A deterministic RGBA frame with varied colours and alpha.
    fn frame(width: u32, height: u32, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..width * height * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xff) as u8
            })
            .collect()
    }

    const PALETTE: [[u8; 3]; 8] = [
        [0, 0, 0],
        [255, 255, 255],
        [200, 30, 30],
        [30, 200, 90],
        [40, 40, 220],
        [128, 128, 128],
        [250, 220, 40],
        [90, 40, 120],
    ];

    #[test]
    fn gpu_indices_match_the_cpu_mapping_for_every_dither_setting() {
        let Some(ctx) = software_gpu() else {
            return;
        };
        // A size that is not a multiple of the workgroup, so the bounds check runs.
        let (width, height) = (37u32, 23u32);
        let rgba = frame(width, height, 7);
        // One pipeline serves every setting below, as it does in a run.
        let pipeline = DitherPipeline::new(&ctx).expect("dither pipeline");
        for dither in [
            Dither::None,
            Dither::Bayer,
            Dither::FloydSteinberg,
            Dither::Sierra2_4a,
            Dither::BlueNoise,
        ] {
            let spec = spec_for(dither, PALETTE.len());
            let mut gpu =
                GpuDitherer::new(Arc::clone(&ctx), &pipeline, width, height, &PALETTE, &spec)
                    .expect("GPU ditherer");
            let mut from_gpu = Vec::new();
            gpu.dither(&rgba, &mut from_gpu).expect("GPU dither");
            let mut from_cpu = Vec::new();
            map_frame(&rgba, width, height, &PALETTE, &spec, &mut from_cpu);
            assert_eq!(from_gpu, from_cpu, "{dither:?}");
            assert!(from_gpu.iter().all(|&i| usize::from(i) < PALETTE.len()));
        }
    }

    /// Runs on the machine's own adapter (the RX 9060 XT on the dev machine), where the compute pass is not WARP. Run it
    /// with `cargo test -p loopcut-export --lib -- --ignored`. It skips when the best adapter is software.
    #[test]
    #[ignore = "needs a hardware GPU; run on the target machine"]
    fn gpu_indices_match_the_cpu_mapping_on_hardware() {
        let ctx = match GpuContext::new(&AdapterChoice::Auto) {
            Ok(ctx) if !ctx.is_software() => Arc::new(ctx),
            _ => {
                eprintln!("skipping: no hardware GPU adapter");
                return;
            }
        };
        let (width, height) = (320u32, 180u32);
        let rgba = frame(width, height, 11);
        let pipeline = DitherPipeline::new(&ctx).expect("dither pipeline");
        for dither in [Dither::Bayer, Dither::BlueNoise] {
            let spec = spec_for(dither, PALETTE.len());
            let mut gpu =
                GpuDitherer::new(Arc::clone(&ctx), &pipeline, width, height, &PALETTE, &spec)
                    .expect("GPU ditherer");
            let mut from_gpu = Vec::new();
            gpu.dither(&rgba, &mut from_gpu).expect("GPU dither");
            let mut from_cpu = Vec::new();
            map_frame(&rgba, width, height, &PALETTE, &spec, &mut from_cpu);
            assert_eq!(from_gpu, from_cpu, "{dither:?} on {}", ctx.desc.name);
        }
    }

    #[test]
    fn gpu_output_is_deterministic_for_the_same_input() {
        let Some(ctx) = software_gpu() else {
            return;
        };
        let spec = spec_for(Dither::BlueNoise, PALETTE.len());
        let rgba = frame(20, 12, 3);
        let pipeline = DitherPipeline::new(&ctx).expect("dither pipeline");
        let mut gpu =
            GpuDitherer::new(ctx, &pipeline, 20, 12, &PALETTE, &spec).expect("GPU ditherer");
        let mut first = Vec::new();
        let mut second = Vec::new();
        gpu.dither(&rgba, &mut first).expect("first");
        gpu.dither(&rgba, &mut second).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn a_software_device_maps_on_the_cpu() {
        let Some(ctx) = software_gpu() else {
            return;
        };
        let spec = spec_for(Dither::Bayer, PALETTE.len());
        let (mapper, reason) = DitherMapper::new(Some(ctx), &mut None, 8, 8, &PALETTE, spec);
        assert!(reason.is_none());
        assert!(!mapper.uses_gpu(), "WARP is mapped on the CPU");
    }

    #[test]
    fn a_wrong_frame_length_is_rejected_before_the_device_is_used() {
        let Some(ctx) = software_gpu() else {
            return;
        };
        let spec = spec_for(Dither::None, PALETTE.len());
        let pipeline = DitherPipeline::new(&ctx).expect("dither pipeline");
        let mut gpu =
            GpuDitherer::new(ctx, &pipeline, 4, 4, &PALETTE, &spec).expect("GPU ditherer");
        let mut out = Vec::new();
        assert!(matches!(
            gpu.dither(&[0; 10], &mut out),
            Err(GifError::InvalidFrame(_))
        ));
    }

    /// A GPU failure part way through a clip. The frame that fails is mapped on the CPU and the reason is reported once.
    /// Every later frame is mapped on the CPU, with the same indices as [`map_frame`].
    #[test]
    fn a_gpu_failure_maps_that_frame_on_the_cpu_and_the_rest_too() {
        let Some(ctx) = software_gpu() else {
            return;
        };
        let (width, height) = (16u32, 12u32);
        let spec = spec_for(Dither::Bayer, PALETTE.len());
        let pipeline = DitherPipeline::new(&ctx).expect("dither pipeline");
        let mut gpu =
            GpuDitherer::new(ctx, &pipeline, width, height, &PALETTE, &spec).expect("GPU ditherer");
        // The pass was built for another size, so it refuses each frame of this size the way a failed dispatch fails.
        gpu.width = 8;
        let mut mapper = DitherMapper {
            width,
            height,
            palette: PALETTE.to_vec(),
            spec: spec.clone(),
            gpu: Some(gpu),
        };
        let mut out = Vec::new();
        let mut reasons = Vec::new();
        for seed in 0..4 {
            let rgba = frame(width, height, seed);
            if let Some(reason) = mapper.map(&rgba, &mut out) {
                reasons.push(reason);
            }
            let mut expected = Vec::new();
            map_frame(&rgba, width, height, &PALETTE, &spec, &mut expected);
            assert_eq!(out, expected, "frame {seed}");
        }
        assert_eq!(
            reasons.len(),
            1,
            "the failure is reported once: {reasons:?}"
        );
        assert!(reasons[0].contains("frame has"), "{}", reasons[0]);
        assert!(!mapper.uses_gpu(), "later frames stay on the CPU");
    }

    #[test]
    fn invalid_setup_is_refused() {
        let Some(ctx) = software_gpu() else {
            return;
        };
        let spec = spec_for(Dither::None, 2);
        let pipeline = DitherPipeline::new(&ctx).expect("dither pipeline");
        assert!(matches!(
            GpuDitherer::new(Arc::clone(&ctx), &pipeline, 0, 4, &PALETTE[..2], &spec),
            Err(GifError::InvalidOptions(_))
        ));
        assert!(matches!(
            GpuDitherer::new(ctx, &pipeline, 4, 4, &[], &spec),
            Err(GifError::InvalidOptions(_))
        ));
    }
}

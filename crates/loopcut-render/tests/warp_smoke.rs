//! Smoke tests for the GPU context on the software (WARP) adapter and on the hardware adapter.
//!
//! Run with `cargo test -p loopcut-render --test warp_smoke -- --nocapture` to see the adapter list.
//! The timing benchmark is `#[ignore]`d; run it with
//! `cargo test -p loopcut-render --test warp_smoke --release -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use loopcut_render::gpu::{
    self, AdapterChoice, DeviceKind, GpuContext, GpuError, Readback, read_buffer,
    read_texture_rgba8,
};

/// Writes `i * 3` into element `i` of a storage buffer of 1024 `u32`s (16 workgroups of 64).
const COMPUTE_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read_write> out_buf: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < arrayLength(&out_buf)) {
        out_buf[i] = i * 3u;
    }
}
"#;

/// Fullscreen triangle (no vertex buffer). The fragment color encodes the pixel's own position:
/// R = x mod 256, G = y mod 256, B = 0, A = 1.
const GRADIENT_WGSL: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(index & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let x = u32(floor(frag.x)) % 256u;
    let y = u32(floor(frag.y)) % 256u;
    return vec4<f32>(f32(x) / 255.0, f32(y) / 255.0, 0.0, 1.0);
}
"#;

/// Expected RGBA8 bytes of the gradient at pixel `(x, y)`.
fn expected_gradient(x: u32, y: u32) -> [u8; 4] {
    [(x % 256) as u8, (y % 256) as u8, 0, 255]
}

/// Opaque red, the clear color used to tell a fresh readback from a stale one.
const RED: [u8; 4] = [255, 0, 0, 255];

/// Clear color whose readback is [`RED`].
const RED_CLEAR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// Opens the software adapter and checks that it really is the software device.
fn software_context() -> GpuContext {
    let ctx = GpuContext::new(&AdapterChoice::Software)
        .expect("the software (WARP) adapter should open on Windows");
    assert!(
        ctx.is_software(),
        "software choice must land on DeviceKind::Cpu, got {:?}",
        ctx.desc
    );
    assert!(!ctx.fell_back(), "the software choice has no fallback step");
    ctx
}

/// Runs [`COMPUTE_WGSL`] and returns the 1024 output words.
fn run_compute(ctx: &GpuContext) -> Vec<u32> {
    const ELEMENTS: u64 = 1024;
    const BYTES: u64 = ELEMENTS * 4;
    let device = &ctx.device;

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("loopcut-smoke-compute"),
        source: wgpu::ShaderSource::Wgsl(COMPUTE_WGSL.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("loopcut-smoke-compute"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let storage = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("loopcut-smoke-out"),
        size: BYTES,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let layout = pipeline.get_bind_group_layout(0);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("loopcut-smoke-bind"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: storage.as_entire_binding(),
        }],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("loopcut-smoke-compute"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("loopcut-smoke-compute"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(16, 1, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));

    let bytes = read_buffer(ctx, &storage, BYTES).expect("compute readback should succeed");
    let (words, _) = bytes.as_chunks::<4>();
    words.iter().map(|word| u32::from_le_bytes(*word)).collect()
}

/// The fullscreen gradient pipeline, built once per device.
struct Gradient {
    pipeline: wgpu::RenderPipeline,
}

impl Gradient {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("loopcut-smoke-gradient"),
            source: wgpu::ShaderSource::Wgsl(GRADIENT_WGSL.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("loopcut-smoke-gradient"),
            layout: None,
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline }
    }

    /// Clears `target` and draws the gradient over it, then submits.
    fn render(&self, ctx: &GpuContext, target: &wgpu::Texture) {
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("loopcut-smoke-render"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loopcut-smoke-render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            pass.set_pipeline(&self.pipeline);
            pass.draw(0..3, 0..1);
        }
        ctx.queue.submit(Some(encoder.finish()));
    }
}

/// Clears `target` to `color` and submits.
fn clear_to(ctx: &GpuContext, target: &wgpu::Texture, color: wgpu::Color) {
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("loopcut-smoke-clear"),
        });
    {
        // The pass exists only to apply the clear load op when it ends.
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("loopcut-smoke-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    ctx.queue.submit(Some(encoder.finish()));
}

/// A `width` x `height` texture of the given format, sample count and usage.
fn texture_of(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    sample_count: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("loopcut-smoke-texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// A `width` x `height` RGBA8 render target usable for rendering and readback.
fn target_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    texture_of(
        device,
        wgpu::TextureFormat::Rgba8Unorm,
        width,
        height,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    )
}

/// A `size`-byte buffer with the given usage.
fn buffer_of(device: &wgpu::Device, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("loopcut-smoke-buffer"),
        size,
        usage,
        mapped_at_creation: false,
    })
}

/// Renders the gradient at 333x77 (odd width, so each row needs 256-byte padding) and checks every pixel.
fn check_gradient_333x77(ctx: &GpuContext) {
    const WIDTH: u32 = 333;
    const HEIGHT: u32 = 77;

    let gradient = Gradient::new(&ctx.device);
    let texture = target_texture(&ctx.device, WIDTH, HEIGHT);
    gradient.render(ctx, &texture);

    let pixels = read_texture_rgba8(ctx, &texture, WIDTH, HEIGHT).expect("readback should succeed");
    assert_eq!(
        pixels.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "readback must strip row padding"
    );

    for (x, y) in [
        (0, 0),
        (1, 0),
        (255, 10),
        (256, 10),
        (332, 0),
        (0, 76),
        (332, 76),
    ] {
        let offset = ((y * WIDTH + x) * 4) as usize;
        assert_eq!(
            &pixels[offset..offset + 4],
            &expected_gradient(x, y),
            "pixel ({x}, {y})"
        );
    }

    let mismatches: Vec<(u32, u32)> = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let offset = ((y * WIDTH + x) * 4) as usize;
            pixels[offset..offset + 4] != expected_gradient(x, y)
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} pixels differ from the gradient, first few: {:?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(8)]
    );
}

/// Asserts the compute results written by [`run_compute`].
fn check_compute(ctx: &GpuContext) {
    let values = run_compute(ctx);
    assert_eq!(values.len(), 1024);
    for (index, value) in values.iter().enumerate() {
        assert_eq!(*value, index as u32 * 3, "element {index}");
    }
}

#[test]
fn enumerate_adapters_lists_devices() {
    let adapters = gpu::enumerate_adapters();
    for adapter in &adapters {
        println!(
            "adapter: \"{}\" | vendor {} (0x{:04X}) | device 0x{:04X} | ordinal {} | kind {:?} | backend {} | driver {} {} | pci \"{}\"",
            adapter.name,
            adapter.vendor_name(),
            adapter.vendor,
            adapter.device,
            adapter.ordinal,
            adapter.kind,
            adapter.backend,
            adapter.driver,
            adapter.driver_info,
            adapter.pci_bus_id,
        );
    }
    assert!(
        !adapters.is_empty(),
        "at least one adapter should be enumerated"
    );

    // Each (vendor, device, ordinal) names exactly one adapter.
    let mut keys: Vec<(u32, u32, u32)> = adapters
        .iter()
        .map(|adapter| (adapter.vendor, adapter.device, adapter.ordinal))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        adapters.len(),
        "ordinals must be unique per model"
    );

    #[cfg(windows)]
    assert!(
        adapters
            .iter()
            .any(|adapter| adapter.kind == DeviceKind::Cpu),
        "the DX12 WARP adapter should be listed"
    );
}

#[test]
fn software_compute_writes_i_times_3() {
    let ctx = software_context();
    println!("software adapter: {:?}", ctx.desc);
    check_compute(&ctx);
}

#[test]
fn software_render_gradient_333x77() {
    let ctx = software_context();
    check_gradient_333x77(&ctx);
}

#[test]
fn auto_hardware_compute_and_render() {
    let ctx = GpuContext::new(&AdapterChoice::Auto).expect("the Auto choice should always open");
    if ctx.is_software() {
        println!(
            "SKIP: no hardware GPU is available; Auto selected {:?}",
            ctx.desc
        );
        return;
    }
    assert!(
        !ctx.fell_back(),
        "Auto should pick hardware without falling back"
    );
    println!("hardware adapter: {:?}", ctx.desc);
    check_compute(&ctx);
    check_gradient_333x77(&ctx);
}

#[test]
fn specific_choice_matches_enumerated_hardware() {
    let adapters = gpu::enumerate_adapters();
    let Some(target) = adapters
        .iter()
        .find(|adapter| adapter.kind != DeviceKind::Cpu)
    else {
        println!("SKIP: no hardware GPU is enumerated");
        return;
    };
    let choice = AdapterChoice::from_gpu_key(&target.gpu_key());
    let ctx = GpuContext::new(&choice).expect("the enumerated adapter should open");
    assert_eq!(ctx.desc.name, target.name);
    assert_eq!(ctx.desc.vendor, target.vendor);
    assert_eq!(ctx.desc.device, target.device);
    assert_eq!(ctx.desc.ordinal, target.ordinal);
    assert!(
        !ctx.fell_back(),
        "an enumerated adapter must not need a fallback"
    );
}

#[test]
fn unknown_specific_choice_falls_back_and_reports_it() {
    let choice = AdapterChoice::Specific {
        vendor: 0xFFFF,
        device: 0xFFFF,
        ordinal: 0,
        name: "no such adapter".to_owned(),
    };
    let ctx = GpuContext::new(&choice).expect("a fallback adapter should still open");
    assert!(
        ctx.fell_back(),
        "an unmatched Specific choice must report a fallback"
    );
    println!("fallback adapter: {:?}", ctx.desc);
}

#[test]
fn read_texture_rejects_unsupported_requests() {
    let ctx = software_context();

    let half = texture_of(
        &ctx.device,
        wgpu::TextureFormat::Rgba16Float,
        16,
        16,
        1,
        wgpu::TextureUsages::COPY_SRC,
    );
    assert!(
        matches!(
            read_texture_rgba8(&ctx, &half, 16, 16),
            Err(GpuError::UnsupportedFormat(
                wgpu::TextureFormat::Rgba16Float
            ))
        ),
        "a float format must be rejected"
    );

    let bgra = texture_of(
        &ctx.device,
        wgpu::TextureFormat::Bgra8Unorm,
        16,
        16,
        1,
        wgpu::TextureUsages::COPY_SRC,
    );
    assert!(
        matches!(
            read_texture_rgba8(&ctx, &bgra, 16, 16),
            Err(GpuError::UnsupportedFormat(wgpu::TextureFormat::Bgra8Unorm))
        ),
        "a BGRA format must be rejected, not reinterpreted as RGBA"
    );

    let multisampled = texture_of(
        &ctx.device,
        wgpu::TextureFormat::Rgba8Unorm,
        16,
        16,
        4,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    assert!(
        matches!(
            read_texture_rgba8(&ctx, &multisampled, 16, 16),
            Err(GpuError::UnsupportedSampleCount(4))
        ),
        "a multisampled texture must be rejected"
    );

    let without_copy = texture_of(
        &ctx.device,
        wgpu::TextureFormat::Rgba8Unorm,
        16,
        16,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    assert!(
        matches!(
            read_texture_rgba8(&ctx, &without_copy, 16, 16),
            Err(GpuError::MissingCopySrc)
        ),
        "a texture without COPY_SRC must be rejected"
    );

    let small = texture_of(
        &ctx.device,
        wgpu::TextureFormat::Rgba8Unorm,
        16,
        16,
        1,
        wgpu::TextureUsages::COPY_SRC,
    );
    assert!(
        matches!(
            read_texture_rgba8(&ctx, &small, 17, 16),
            Err(GpuError::OutOfBounds {
                requested_width: 17,
                requested_height: 16,
                available_width: 16,
                available_height: 16,
            })
        ),
        "a region wider than the texture must be rejected"
    );
    assert!(
        matches!(
            read_texture_rgba8(&ctx, &small, 16, 17),
            Err(GpuError::OutOfBounds { .. })
        ),
        "a region taller than the texture must be rejected"
    );
    assert!(
        read_texture_rgba8(&ctx, &small, 0, 0)
            .expect("an empty region is not an error")
            .is_empty()
    );
}

#[test]
fn read_buffer_rejects_bad_requests() {
    let ctx = software_context();
    let readable = buffer_of(&ctx.device, 64, wgpu::BufferUsages::COPY_SRC);
    let unreadable = buffer_of(&ctx.device, 64, wgpu::BufferUsages::STORAGE);

    assert!(
        matches!(
            read_buffer(&ctx, &readable, 6),
            Err(GpuError::InvalidSize(_))
        ),
        "a size that is not a multiple of 4 must be rejected"
    );
    assert!(
        matches!(
            read_buffer(&ctx, &readable, 128),
            Err(GpuError::InvalidSize(_))
        ),
        "a size larger than the buffer must be rejected"
    );
    assert!(
        matches!(
            read_buffer(&ctx, &unreadable, 64),
            Err(GpuError::MissingCopySrc)
        ),
        "a buffer without COPY_SRC must be rejected"
    );
    assert!(
        read_buffer(&ctx, &readable, 0)
            .expect("a zero-size read is not an error")
            .is_empty()
    );
}

#[test]
fn read_rejects_texture_from_another_device() {
    // A second device on the same adapter is a validation error. A texture from a different wgpu Instance
    // (a different GpuContext) is not: wgpu-core panics on the unknown resource id, so callers must read
    // textures created on `ctx.device`.
    let ctx = software_context();
    let (other, _queue) = pollster::block_on(ctx.adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("loopcut-smoke-other"),
        ..Default::default()
    }))
    .expect("a second device on the same adapter");
    let foreign = target_texture(&other, 16, 16);
    let result = read_texture_rgba8(&ctx, &foreign, 16, 16);
    assert!(
        matches!(result, Err(GpuError::Validation(_))),
        "a texture from another device must fail validation, got {result:?}"
    );
}

#[test]
fn read_on_destroyed_device_fails_instead_of_returning_blank_pixels() {
    let ctx = software_context();
    let texture = target_texture(&ctx.device, 16, 16);
    clear_to(&ctx, &texture, RED_CLEAR);

    // No poll before the read: the device is invalid, but the lost callback has not run yet.
    ctx.device.destroy();
    let result = read_texture_rgba8(&ctx, &texture, 16, 16);
    assert!(
        result.is_err(),
        "a destroyed device must not return pixels; the read gave {} bytes",
        result.as_ref().map_or(0, Vec::len)
    );
}

#[test]
fn device_loss_turns_reads_into_errors() {
    let ctx = software_context();
    let texture = target_texture(&ctx.device, 16, 16);
    let buffer = buffer_of(&ctx.device, 64, wgpu::BufferUsages::COPY_SRC);

    ctx.device.destroy();
    // destroy() only marks the device invalid. The lost callback runs from a later poll, once the queue is idle.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ctx.is_lost() {
        assert!(
            Instant::now() < deadline,
            "the device-lost callback never ran after destroy()"
        );
        let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        matches!(
            read_texture_rgba8(&ctx, &texture, 16, 16),
            Err(GpuError::DeviceLost)
        ),
        "a texture read on a lost device must report DeviceLost"
    );
    assert!(
        matches!(read_buffer(&ctx, &buffer, 64), Err(GpuError::DeviceLost)),
        "a buffer read on a lost device must report DeviceLost"
    );
    assert!(
        matches!(
            Readback::new().read_rgba8(&ctx, &texture, 16, 16),
            Err(GpuError::DeviceLost)
        ),
        "a pooled read on a lost device must report DeviceLost"
    );
}

#[test]
fn readback_reuses_staging_without_stale_data() {
    let ctx = software_context();
    let gradient = Gradient::new(&ctx.device);
    let texture = target_texture(&ctx.device, 64, 8);
    let mut readback = Readback::new();

    gradient.render(&ctx, &texture);
    let first = readback
        .read_rgba8(&ctx, &texture, 64, 8)
        .expect("first readback");
    assert_eq!(&first[..4], &expected_gradient(0, 0));
    assert_eq!(&first[(63 + 7 * 64) * 4..][..4], &expected_gradient(63, 7));

    // Same size, so the pooled staging buffer is reused. The new contents must show, not the previous frame.
    clear_to(&ctx, &texture, RED_CLEAR);
    let second = readback
        .read_rgba8(&ctx, &texture, 64, 8)
        .expect("second readback");
    assert!(
        second.as_chunks::<4>().0.iter().all(|pixel| *pixel == RED),
        "reused staging buffer returned stale pixels"
    );

    // A different size gets its own buffer; the first size still reads correctly afterwards.
    let small = readback
        .read_rgba8(&ctx, &texture, 33, 5)
        .expect("small readback");
    assert_eq!(small.len(), 33 * 5 * 4, "readback must strip row padding");
    assert!(small.as_chunks::<4>().0.iter().all(|pixel| *pixel == RED));

    gradient.render(&ctx, &texture);
    let third = readback
        .read_rgba8(&ctx, &texture, 64, 8)
        .expect("third readback");
    assert_eq!(third, first, "the gradient must read back identically");
}

/// Times render + readback at 1920x1080 over 30 frames (after 3 warm-up frames).
fn bench_context(ctx: &GpuContext, label: &str) {
    const WIDTH: u32 = 1920;
    const HEIGHT: u32 = 1080;
    const FRAMES: u32 = 30;

    let gradient = Gradient::new(&ctx.device);
    let texture = target_texture(&ctx.device, WIDTH, HEIGHT);
    let mut readback = Readback::new();
    let frame = |readback: &mut Readback, render_ms: &mut f64, readback_ms: &mut f64| {
        let start = Instant::now();
        gradient.render(ctx, &texture);
        ctx.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll should succeed");
        let after_render = Instant::now();
        let pixels = readback
            .read_rgba8(ctx, &texture, WIDTH, HEIGHT)
            .expect("readback");
        std::hint::black_box(&pixels);
        let end = Instant::now();
        *render_ms += (after_render - start).as_secs_f64() * 1000.0;
        *readback_ms += (end - after_render).as_secs_f64() * 1000.0;
    };

    let (mut ignored_render, mut ignored_readback) = (0.0, 0.0);
    for _ in 0..3 {
        frame(&mut readback, &mut ignored_render, &mut ignored_readback);
    }
    let (mut render_ms, mut readback_ms) = (0.0, 0.0);
    for _ in 0..FRAMES {
        frame(&mut readback, &mut render_ms, &mut readback_ms);
    }
    let frames = f64::from(FRAMES);
    println!(
        "{label} ({}): 1920x1080 render {:.2} ms + readback {:.2} ms = {:.2} ms/frame over {FRAMES} frames",
        ctx.desc.name,
        render_ms / frames,
        readback_ms / frames,
        (render_ms + readback_ms) / frames,
    );
}

#[test]
#[ignore = "timing benchmark; run with --ignored --nocapture"]
fn bench_1080p_render_readback() {
    let software = software_context();
    bench_context(&software, "WARP");

    match GpuContext::new(&AdapterChoice::Auto) {
        Ok(hardware) if !hardware.is_software() => bench_context(&hardware, "hardware"),
        Ok(_) => println!("SKIP: no hardware GPU for the hardware benchmark"),
        Err(error) => println!("SKIP: hardware GPU unavailable: {error}"),
    }
}

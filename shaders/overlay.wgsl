// LoopCut compositor, overlay pass.
//
// Draws one premultiplied RGBA overlay as a quad over the output, scaled to its destination rectangle with
// linear filtering. The pipeline blends with premultiplied "over" and the overlay is applied after colour
// conversion, so overlays are not graded.

struct OverlayUniform {
    // x: output width, y: output height (pixels).
    out_size: vec4<f32>,
    // Destination rectangle in output pixels: x, y, w, h.
    rect: vec4<f32>,
    // x: opacity in [0, 1] (multiplies the premultiplied colour and alpha).
    params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> overlay: OverlayUniform;
@group(0) @binding(1) var overlay_tex: texture_2d<f32>;
@group(0) @binding(2) var overlay_samp: sampler;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Two triangles (six vertices) covering the destination rectangle.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // Corners in order: (0,0) (1,0) (0,1) | (0,1) (1,0) (1,1).
    let cx = f32(index == 1u || index == 4u || index == 5u);
    let cy = f32(index == 2u || index == 3u || index == 5u);
    let uv = vec2<f32>(cx, cy);
    let px = overlay.rect.xy + uv * overlay.rect.zw;
    let ndc = vec2<f32>(
        px.x / overlay.out_size.x * 2.0 - 1.0,
        1.0 - px.y / overlay.out_size.y * 2.0,
    );
    var result: VsOut;
    result.position = vec4<f32>(ndc, 0.0, 1.0);
    result.uv = uv;
    return result;
}

@fragment
fn fs_main(vin: VsOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(overlay_tex, overlay_samp, vin.uv, 0.0);
    return c * overlay.params.x;
}

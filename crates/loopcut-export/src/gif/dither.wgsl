// Ordered dithering onto a global palette. This is the GPU half of `crate::gif::dither::map_frame`: the arithmetic is
// integer-only and breaks ties on the lowest palette index, so both paths return the same indices.
//
// Bindings:
//   0  Params (uniform)
//   1  pixels: RGBA8 bytes read as little-endian u32 (R in the low byte)
//   2  palette: up to 256 entries, RGB in xyz
//   3  thresholds: row-major matrix, matrix_size squared entries used
//   4  indices: one u32 per pixel, the palette index

struct Params {
    width: u32,
    height: u32,
    palette_len: u32,
    amplitude: u32,
    matrix_size: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> pixels: array<u32>;
@group(0) @binding(2) var<storage, read> palette: array<vec4<i32>, 256>;
@group(0) @binding(3) var<storage, read> thresholds: array<u32, 256>;
@group(0) @binding(4) var<storage, read_write> indices: array<u32>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    let i = y * params.width + x;
    let packed = pixels[i];
    let r = i32(packed & 255u);
    let g = i32((packed >> 8u) & 255u);
    let b = i32((packed >> 16u) & 255u);

    let cell = (y % params.matrix_size) * params.matrix_size + (x % params.matrix_size);
    let threshold = i32(thresholds[cell]);
    let amplitude = i32(params.amplitude);
    let offset = ((threshold * amplitude) >> 8u) - (amplitude >> 1u);

    let ar = clamp(r + offset, 0, 255);
    let ag = clamp(g + offset, 0, 255);
    let ab = clamp(b + offset, 0, 255);

    var best: u32 = 0u;
    var best_distance: i32 = 2147483647;
    for (var k: u32 = 0u; k < params.palette_len; k = k + 1u) {
        let entry = palette[k];
        let dr = ar - entry.x;
        let dg = ag - entry.y;
        let db = ab - entry.z;
        let distance = dr * dr + dg * dg + db * db;
        if (distance < best_distance) {
            best_distance = distance;
            best = k;
        }
    }
    indices[i] = best;
}

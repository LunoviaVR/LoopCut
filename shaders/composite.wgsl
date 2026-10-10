// LoopCut compositor, colour pass.
//
// One fullscreen triangle writes the RGBA8 output. For every output pixel:
//   output UV -> crop (display space) -> display-to-stored affine -> bilinear sample of frame A (and B)
//   -> YUV to RGB (matrix and range) -> HDR to SDR (PQ or HLG EOTF, BT.2020 to BT.709 with negative light
//      clipped, luminance-preserving highlight rolloff, sRGB encode)
//   -> straight alpha over the background -> crossfade with frame B
//   -> colour grade (grade.wgsl; sharpen and vignette here) -> overlays, drawn afterwards by overlay.wgsl.
//
// Portable WGSL only: no texture_external, f16, subgroups, override constants or extensions.
// Integer planes (R8Uint, R16Uint, Rg8Uint, Rg16Uint) are read with textureLoad and filtered here, because
// unsigned integer textures are not filterable. Chroma is sampled with centre siting.

// Per-input parameters. Mirrors `InputUniform` in src/compositor.rs (48 bytes).
struct InputUniform {
    // x: luma width, y: luma height (samples); z and w unused.
    size: vec4<f32>,
    // x: kind (0 = NV12, 1 = P010, 2 = RGBA8)
    // y: matrix (0 = BT.601, 1 = BT.709, 2 = BT.2020; RGBA8 ignores it)
    // z: range (0 = limited, 1 = full)
    // w: transfer (0 = SDR, 1 = PQ, 2 = HLG)
    info: vec4<u32>,
    // x: wide gamut (BT.2020 primaries); y, z, w unused.
    extra: vec4<u32>,
}

struct Globals {
    // x: output width, y: output height (pixels).
    out_size: vec4<f32>,
    // Crop in display space: x, y, w, h (normalized).
    crop: vec4<f32>,
    // Display-to-stored affine, row-major 2x3:
    //   stored.x = dot(affine0.xyz, (u, v, 1)),  stored.y = dot(affine1.xyz, (u, v, 1)).
    affine0: vec4<f32>,
    affine1: vec4<f32>,
    // Background behind alpha sources (sRGB encoded, rgb used).
    background: vec4<f32>,
    // x: weight of frame B for the crossfade; y: 1 when frame B is present.
    blend: vec4<f32>,
    a: InputUniform,
    b: InputUniform,
    grade: GradeUniforms,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var a_luma: texture_2d<u32>;
@group(0) @binding(2) var a_chroma: texture_2d<u32>;
@group(0) @binding(3) var a_rgba: texture_2d<f32>;
@group(0) @binding(4) var b_luma: texture_2d<u32>;
@group(0) @binding(5) var b_chroma: texture_2d<u32>;
@group(0) @binding(6) var b_rgba: texture_2d<f32>;
// Colour grade tables (see grade.wgsl). Both curve tables hold TABLE_SIZE entries over [0, 1]; the 3D LUT is
// rgba16float. All three are read with textureLoad, so they need no sampler.
@group(0) @binding(7) var curve_tex: texture_2d<f32>;
@group(0) @binding(8) var lut1d_tex: texture_2d<f32>;
@group(0) @binding(9) var lut3d_tex: texture_3d<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(index & 2u) * 2.0 - 1.0;
    var result: VsOut;
    result.position = vec4<f32>(x, y, 0.0, 1.0);
    return result;
}

// Keeps the ten significant bits of a P010 sample (the low six bits are padding).
const P010_MASK: u32 = 0xFFC0u;
const ALL_BITS: u32 = 0xFFFFFFFFu;

// Loads one integer texel with `mask` applied, as floats.
fn load_uint(tex: texture_2d<u32>, texel: vec2<i32>, mask: u32) -> vec4<f32> {
    return vec4<f32>(textureLoad(tex, texel, 0) & vec4<u32>(mask));
}

// Bilinear filter over an integer plane. `uv` is normalized over the plane; texel centres sit at (i + 0.5).
fn bilinear_uint(tex: texture_2d<u32>, uv: vec2<f32>, mask: u32) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(tex));
    let p = uv * vec2<f32>(dims) - vec2<f32>(0.5, 0.5);
    let base = floor(p);
    let f = p - base;
    let top_left = vec2<i32>(base);
    let max_index = dims - vec2<i32>(1, 1);
    let x0 = clamp(top_left.x, 0, max_index.x);
    let x1 = clamp(top_left.x + 1, 0, max_index.x);
    let y0 = clamp(top_left.y, 0, max_index.y);
    let y1 = clamp(top_left.y + 1, 0, max_index.y);
    let row0 = mix(load_uint(tex, vec2<i32>(x0, y0), mask), load_uint(tex, vec2<i32>(x1, y0), mask), f.x);
    let row1 = mix(load_uint(tex, vec2<i32>(x0, y1), mask), load_uint(tex, vec2<i32>(x1, y1), mask), f.x);
    return mix(row0, row1, f.y);
}

// Loads one RGBA8 texel as premultiplied colour, so interpolation does not bleed colour into transparent texels.
fn load_rgba_premultiplied(tex: texture_2d<f32>, texel: vec2<i32>) -> vec4<f32> {
    let c = textureLoad(tex, texel, 0);
    return vec4<f32>(c.rgb * c.a, c.a);
}

// Bilinear filter over an RGBA8 texture, returning premultiplied colour and alpha.
fn bilinear_rgba(tex: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(tex));
    let p = uv * vec2<f32>(dims) - vec2<f32>(0.5, 0.5);
    let base = floor(p);
    let f = p - base;
    let top_left = vec2<i32>(base);
    let max_index = dims - vec2<i32>(1, 1);
    let x0 = clamp(top_left.x, 0, max_index.x);
    let x1 = clamp(top_left.x + 1, 0, max_index.x);
    let y0 = clamp(top_left.y, 0, max_index.y);
    let y1 = clamp(top_left.y + 1, 0, max_index.y);
    let row0 = mix(load_rgba_premultiplied(tex, vec2<i32>(x0, y0)), load_rgba_premultiplied(tex, vec2<i32>(x1, y0)), f.x);
    let row1 = mix(load_rgba_premultiplied(tex, vec2<i32>(x0, y1)), load_rgba_premultiplied(tex, vec2<i32>(x1, y1)), f.x);
    return mix(row0, row1, f.y);
}

// Converts normalized YUV codes (Y, Cb, Cr, each in [0, 1] before range expansion) to RGB in the
// encoded domain. Limited range maps Y 16..235 and C 16..240 (scaled by 4 for 10-bit).
fn yuv_to_rgb(codes: vec3<f32>, inp: InputUniform) -> vec3<f32> {
    let p010 = inp.info.x == 1u;
    let depth = select(255.0, 1023.0, p010);
    let y_black = select(16.0, 64.0, p010) / depth;
    let c_mid = select(128.0, 512.0, p010) / depth;
    let y_span = select(219.0, 876.0, p010);
    let c_span = select(224.0, 896.0, p010);

    var y = codes.x;
    var cb = codes.y - c_mid;
    var cr = codes.z - c_mid;
    if inp.info.z == 0u {
        y = (codes.x - y_black) * depth / y_span;
        cb = cb * depth / c_span;
        cr = cr * depth / c_span;
    }

    // Coefficients: R = Y + a*Cr, B = Y + b*Cb, G = Y - c*Cb - d*Cr.
    var a = 1.5748;
    var b = 1.8556;
    var c = 0.187324;
    var d = 0.468124;
    if inp.info.y == 0u {
        a = 1.402;
        b = 1.772;
        c = 0.344136;
        d = 0.714136;
    } else if inp.info.y == 2u {
        a = 1.4746;
        b = 1.8814;
        c = 0.164553;
        d = 0.571353;
    }
    return vec3<f32>(y + a * cr, y - c * cb - d * cr, y + b * cb);
}

// SMPTE ST 2084 (PQ) EOTF: encoded [0, 1] to relative luminance [0, 1] of a 10000-nit display.
fn pq_eotf(e: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let p = pow(max(e, vec3<f32>(1e-8)), vec3<f32>(1.0 / m2));
    let num = max(p - vec3<f32>(c1), vec3<f32>(0.0));
    let den = vec3<f32>(c2) - vec3<f32>(c3) * p;
    return pow(max(num / den, vec3<f32>(1e-12)), vec3<f32>(1.0 / m1));
}

// ARIB STD-B67 (HLG) inverse OETF for one channel: encoded to scene-linear [0, 1].
fn hlg_scene(e: f32) -> f32 {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    if e <= 0.5 {
        return e * e / 3.0;
    }
    return (exp((e - c) / a) + b) / 12.0;
}

// BT.2020 linear RGB to BT.709 linear RGB (ITU-R BT.2087). Columns are listed in WGSL column order.
const BT2020_TO_BT709 = mat3x3<f32>(
    vec3<f32>(1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876, 1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083, 1.1187),
);

// Assumed HDR mastering peak, in units of SDR reference white (1000 nits / 203 nits).
const HDR_PEAK: f32 = 4.926;
// Below the knee (about 162 nits) light passes unchanged.
const ROLLOFF_KNEE: f32 = 0.8;

// Luminance-preserving, peak-normalized highlight rolloff. `x` is linear light in units of SDR reference white
// (203 nits). Let m be the largest channel and e = m - knee. Above the knee, m is compressed with a rational
// shoulder m' = knee + (1 - knee) * e / (e + (1 - knee)) * (r + 1 - knee) / r, where r = peak - knee, so the
// 1000-nit peak lands exactly on 1.0 and highlights between ~300 and 1000 nits keep visible separation
// (400 nits ≈ 252, 600 nits ≈ 254 after encoding) instead of clipping together. Diffuse white (m = 1) maps to
// about 0.905 linear (≈ 244 of 255). All channels share the ratio m' / m, so hue is preserved and no channel clips.
fn highlight_rolloff(x: vec3<f32>) -> vec3<f32> {
    let v = max(x, vec3<f32>(0.0));
    let m = max(max(v.r, v.g), v.b);
    if m <= ROLLOFF_KNEE {
        return v;
    }
    let head = 1.0 - ROLLOFF_KNEE;
    let range = HDR_PEAK - ROLLOFF_KNEE;
    let e = min(m, HDR_PEAK) - ROLLOFF_KNEE;
    let shoulder = ROLLOFF_KNEE + head * (e / (e + head)) * ((range + head) / range);
    return v * (min(shoulder, 1.0) / m);
}

// sRGB OETF for [0, 1] linear light.
fn srgb_encode(linear: vec3<f32>) -> vec3<f32> {
    let c = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = c * 12.92;
    let high = 1.055 * pow(max(c, vec3<f32>(1e-6)), vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, c <= vec3<f32>(0.0031308));
}

// Converts colour in the source's encoding to SDR encoded RGB. SDR sources pass through unchanged.
// HDR sources are linearized (display nits), gamut-mapped to BT.709, highlight-rolled off and sRGB encoded.
fn to_sdr(rgb: vec3<f32>, inp: InputUniform) -> vec3<f32> {
    let transfer = inp.info.w;
    if transfer == 0u {
        return rgb;
    }
    let encoded = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    var nits: vec3<f32>;
    if transfer == 1u {
        nits = pq_eotf(encoded) * 10000.0;
    } else {
        // HLG: scene light, then the system OOTF with gamma 1.2 at a 1000-nit display.
        let scene = vec3<f32>(hlg_scene(encoded.r), hlg_scene(encoded.g), hlg_scene(encoded.b));
        let luma = max(dot(scene, vec3<f32>(0.2627, 0.6780, 0.0593)), 1e-6);
        nits = scene * pow(luma, 0.2) * 1000.0;
    }
    var linear = max(nits, vec3<f32>(0.0));
    if inp.extra.x == 1u {
        // Saturated BT.2020 colours fall outside BT.709, so the matrix can produce negative light. Clip it here,
        // before the tone curve, so the curve never sees a negative input.
        linear = max(BT2020_TO_BT709 * linear, vec3<f32>(0.0));
    }
    // 203 nits is SDR reference white.
    return srgb_encode(highlight_rolloff(linear / 203.0));
}

// Samples one source at a stored-space UV. Returns straight-alpha RGB in the SDR encoded domain and alpha.
fn sample_source(
    inp: InputUniform,
    luma: texture_2d<u32>,
    chroma: texture_2d<u32>,
    rgba: texture_2d<f32>,
    uv: vec2<f32>,
) -> vec4<f32> {
    let kind = inp.info.x;
    if kind == 2u {
        let p = bilinear_rgba(rgba, uv);
        let alpha = p.a;
        var rgb = vec3<f32>(0.0);
        if alpha > 0.0 {
            rgb = p.rgb / alpha;
        }
        return vec4<f32>(to_sdr(rgb, inp), alpha);
    }
    let p010 = kind == 1u;
    let mask = select(ALL_BITS, P010_MASK, p010);
    // Full-scale raw value: 255 for 8-bit, 1023 << 6 for P010.
    let raw_scale = select(255.0, 65472.0, p010);
    let y = bilinear_uint(luma, uv, mask).x / raw_scale;
    let c = bilinear_uint(chroma, uv, mask);
    let rgb = yuv_to_rgb(vec3<f32>(y, c.x / raw_scale, c.y / raw_scale), inp);
    return vec4<f32>(to_sdr(rgb, inp), 1.0);
}

// Composites a straight-alpha source over the background (encoded domain).
fn over_background(src: vec4<f32>, background: vec3<f32>) -> vec3<f32> {
    return src.rgb * src.a + background * (1.0 - src.a);
}

// The display-referred SDR colour of one output pixel, before the grade: the sources, their background and the
// crossfade. `frag_xy` is a pixel centre in output space.
fn composite_at(frag_xy: vec2<f32>) -> vec3<f32> {
    let uv_out = frag_xy / globals.out_size.xy;
    let display = globals.crop.xy + uv_out * globals.crop.zw;
    let display_h = vec3<f32>(display, 1.0);
    let stored = vec2<f32>(dot(globals.affine0.xyz, display_h), dot(globals.affine1.xyz, display_h));

    let background = globals.background.rgb;
    let src_a = sample_source(globals.a, a_luma, a_chroma, a_rgba, stored);
    var rgb = over_background(src_a, background);
    if globals.blend.y > 0.5 {
        let src_b = sample_source(globals.b, b_luma, b_chroma, b_rgba, stored);
        rgb = mix(rgb, over_background(src_b, background), globals.blend.x);
    }
    return rgb;
}

// The grade runs after the crossfade and before overlays. A zero flag word is the identity and skips all the
// grade work. Sharpening adds the input's luma detail (against the mean of four neighbours in the same pass). The
// vignette multiplies last.
@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let base = composite_at(frag.xy);
    var rgb = base;
    let flags = globals.grade.flags.x;
    if flags != 0u {
        rgb = grade_color(base, globals.grade);
        if (flags & FLAG_SHARPEN) != 0u {
            let neighbours = 0.25 * (
                luma709(composite_at(frag.xy + vec2<f32>(1.0, 0.0)))
                + luma709(composite_at(frag.xy - vec2<f32>(1.0, 0.0)))
                + luma709(composite_at(frag.xy + vec2<f32>(0.0, 1.0)))
                + luma709(composite_at(frag.xy - vec2<f32>(0.0, 1.0)))
            );
            let detail = luma709(base) - neighbours;
            rgb = rgb + vec3<f32>(globals.grade.color.z * detail);
        }
        if (flags & FLAG_VIGNETTE) != 0u {
            rgb = rgb * vignette_factor(frag.xy, globals.out_size.xy, globals.grade.color.w);
        }
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}


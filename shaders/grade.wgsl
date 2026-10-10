// LoopCut colour grade (M3). Concatenated after composite.wgsl, which declares the grade bindings 7 to 9 and the
// `globals` uniform. The math is documented in crates/loopcut-render/src/grade.rs.
//
// Portable WGSL only (FXC compatible): no derivatives, no f16, no override constants.
// Every texture here is read with textureLoad, so the unfilterable float formats used for the tables are fine.

// Stage flags. Mirror the FLAG_* constants in grade.rs.
const FLAG_LINEAR: u32 = 1u;
const FLAG_TONAL: u32 = 2u;
const FLAG_CURVES: u32 = 4u;
const FLAG_HSL: u32 = 8u;
const FLAG_SATURATION: u32 = 16u;
const FLAG_LUT3D: u32 = 32u;
const FLAG_LUT1D: u32 = 64u;
const FLAG_SHARPEN: u32 = 128u;
const FLAG_VIGNETTE: u32 = 256u;

const PI: f32 = 3.14159265358979;

// Mirrors `GradeUniforms` in crates/loopcut-render/src/grade.rs (336 bytes).
struct GradeUniforms {
    // x: stage flags.
    flags: vec4<u32>,
    // x: exposure gain (2^stops); y: contrast; z: highlights; w: shadows.
    tone: vec4<f32>,
    // x: saturation; y: vibrance; z: sharpen; w: vignette.
    color: vec4<f32>,
    // x: LUT strength.
    lut: vec4<f32>,
    // Per-channel 3D LUT domain minimum, and 1 / (maximum - minimum).
    lut_domain_min: vec4<f32>,
    lut_domain_scale: vec4<f32>,
    // Per-channel lift, 1 / gamma and gain.
    lift: vec4<f32>,
    gamma_inv: vec4<f32>,
    gain: vec4<f32>,
    // White balance matrix in linear RGB, as three columns.
    white_balance: array<vec4<f32>, 3>,
    // Per hue band: x hue shift (degrees), y saturation, z luminance.
    hsl: array<vec4<f32>, 8>,
    // x: 3D LUT edge length (texels).
    lut_size: vec4<f32>,
}

// sRGB EOTF for encoded values in [0, 1].
fn srgb_decode(e: vec3<f32>) -> vec3<f32> {
    let c = clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

// Exposure and white balance in linear light, then sRGB re-encode.
fn linear_stage(c: vec3<f32>, g: GradeUniforms) -> vec3<f32> {
    let wb = mat3x3<f32>(g.white_balance[0].xyz, g.white_balance[1].xyz, g.white_balance[2].xyz);
    let lin = max(wb * (srgb_decode(c) * g.tone.x), vec3<f32>(0.0));
    return srgb_encode(lin);
}

// Moves a luma value by a shadow or highlight amount, with its mask. Positive amounts lift toward white and negative
// amounts scale down.
fn adjust_luma(y: f32, amount: f32, mask: f32) -> f32 {
    if amount >= 0.0 {
        return y + 0.25 * amount * mask * (1.0 - y);
    }
    return y * (1.0 + 0.25 * amount * mask);
}

// Lift, gamma and gain per channel, contrast, then shadows and highlights on Rec.709 luma. Encoded domain.
fn tonal_stage(input: vec3<f32>, g: GradeUniforms) -> vec3<f32> {
    let lifted = clamp(input + g.lift.xyz * (vec3<f32>(1.0) - input), vec3<f32>(0.0), vec3<f32>(1.0));
    var c = clamp(g.gain.xyz * pow(max(lifted, vec3<f32>(1e-6)), g.gamma_inv.xyz), vec3<f32>(0.0), vec3<f32>(1.0));

    let a = g.tone.y;
    if a != 0.0 {
        // x - a sin(2 pi x) / (2 pi): fixes 0, 0.5 and 1, and is monotone for |a| <= 1.
        c = clamp(c - a * sin(2.0 * PI * c) / (2.0 * PI), vec3<f32>(0.0), vec3<f32>(1.0));
    }

    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let shadow_mask = 1.0 - smoothstep(0.0, 0.5, y);
    let highlight_mask = smoothstep(0.5, 1.0, y);
    let y1 = adjust_luma(y, g.tone.w, shadow_mask);
    let y2 = adjust_luma(y1, g.tone.z, highlight_mask);
    return clamp(c + vec3<f32>(y2 - y), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Looks up a 1D table of TABLE_SIZE entries that cover [0, 1], with linear interpolation.
fn table_lookup(tex: texture_2d<f32>, x: f32) -> vec4<f32> {
    let n = textureDimensions(tex).x;
    let p = clamp(x, 0.0, 1.0) * f32(n - 1u);
    let i0 = min(u32(floor(p)), n - 2u);
    let f = p - f32(i0);
    let a = textureLoad(tex, vec2<i32>(i32(i0), 0), 0);
    let b = textureLoad(tex, vec2<i32>(i32(i0 + 1u), 0), 0);
    return mix(a, b, f);
}

// Applies a per-channel table: each channel is looked up in its own column (r in .r, g in .g, b in .b).
fn table_apply(tex: texture_2d<f32>, c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(table_lookup(tex, c.r).r, table_lookup(tex, c.g).g, table_lookup(tex, c.b).b);
}

// Hue of band boundary k in degrees. k = 8 is 360, which closes the wheel.
fn band_center(k: u32) -> f32 {
    if k == 0u {
        return 0.0;
    }
    if k == 1u {
        return 30.0;
    }
    if k == 2u {
        return 60.0;
    }
    if k == 3u {
        return 120.0;
    }
    if k == 4u {
        return 180.0;
    }
    if k == 5u {
        return 240.0;
    }
    if k == 6u {
        return 270.0;
    }
    if k == 7u {
        return 300.0;
    }
    return 360.0;
}

// RGB (encoded, [0, 1]) to HSL. Hue is in degrees in [0, 360).
fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    let l = 0.5 * (mx + mn);
    if d < 1e-6 {
        return vec3<f32>(0.0, 0.0, l);
    }
    let s = d / max(1.0 - abs(2.0 * l - 1.0), 1e-6);
    var h6: f32;
    if mx == c.r {
        h6 = (c.g - c.b) / d;
    } else if mx == c.g {
        h6 = (c.b - c.r) / d + 2.0;
    } else {
        h6 = (c.r - c.g) / d + 4.0;
    }
    h6 = h6 - 6.0 * floor(h6 / 6.0);
    return vec3<f32>(60.0 * h6, s, l);
}

// HSL (hue in degrees) back to encoded RGB.
fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x / 60.0;
    let chroma = (1.0 - abs(2.0 * hsl.z - 1.0)) * hsl.y;
    let x = chroma * (1.0 - abs((h - 2.0 * floor(h / 2.0)) - 1.0));
    let m = hsl.z - 0.5 * chroma;
    let sector = min(u32(floor(h)), 5u);
    var rgb: vec3<f32>;
    if sector == 0u {
        rgb = vec3<f32>(chroma, x, 0.0);
    } else if sector == 1u {
        rgb = vec3<f32>(x, chroma, 0.0);
    } else if sector == 2u {
        rgb = vec3<f32>(0.0, chroma, x);
    } else if sector == 3u {
        rgb = vec3<f32>(0.0, x, chroma);
    } else if sector == 4u {
        rgb = vec3<f32>(x, 0.0, chroma);
    } else {
        rgb = vec3<f32>(chroma, 0.0, x);
    }
    return rgb + vec3<f32>(m);
}

// Eight hue bands with raised-cosine blending between neighbours. Achromatic pixels are not adjusted.
fn hsl_stage(c: vec3<f32>, g: GradeUniforms) -> vec3<f32> {
    let hsl = rgb_to_hsl(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)));
    let gate = smoothstep(0.02, 0.10, hsl.y);
    var shift = 0.0;
    var sat = 0.0;
    var lum = 0.0;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let a = band_center(k);
        let b = band_center(k + 1u);
        if hsl.x >= a && hsl.x < b {
            let t = (hsl.x - a) / (b - a);
            let cw = cos(0.5 * PI * t);
            let wa = cw * cw;
            let wb = 1.0 - wa;
            let band_a = g.hsl[k];
            let band_b = g.hsl[(k + 1u) % 8u];
            shift = wa * band_a.x + wb * band_b.x;
            sat = wa * band_a.y + wb * band_b.y;
            lum = wa * band_a.z + wb * band_b.z;
        }
    }
    let hue = hsl.x + gate * shift;
    let hue_wrapped = hue - 360.0 * floor(hue / 360.0);
    let s = clamp(hsl.y * (1.0 + gate * sat), 0.0, 1.0);
    let l = clamp(hsl.z + gate * 0.25 * lum, 0.0, 1.0);
    return hsl_to_rgb(vec3<f32>(hue_wrapped, s, l));
}

// Linear sRGB to OKLab (Bjorn Ottosson's matrices).
fn linear_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    let l_ = pow(max(l, 1e-12), 1.0 / 3.0);
    let m_ = pow(max(m, 1e-12), 1.0 / 3.0);
    let s_ = pow(max(s, 1e-12), 1.0 / 3.0);
    return vec3<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    );
}

// OKLab to linear sRGB.
fn oklab_to_linear(lab: vec3<f32>) -> vec3<f32> {
    let l_ = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z;
    let m_ = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z;
    let s_ = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec3<f32>(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    );
}

// Wraps an angle in degrees to [-180, 180).
fn wrap180(d: f32) -> f32 {
    return d - 360.0 * floor((d + 180.0) / 360.0);
}

// OKLab chroma scaling: saturation everywhere, vibrance on low-chroma colours, with a mild skin-tone guard.
fn saturation_stage(c: vec3<f32>, g: GradeUniforms) -> vec3<f32> {
    var lab = linear_to_oklab(srgb_decode(c));
    let chroma = length(lab.yz);
    if chroma < 1e-6 {
        return c;
    }
    let hue = atan2(lab.z, lab.y);
    let hue_deg = degrees(hue);
    let skin_offset = wrap180(hue_deg - 55.0) / 20.0;
    let skin = exp(-0.5 * skin_offset * skin_offset);
    let protect = 1.0 - 0.5 * skin;
    let low_chroma = 1.0 - smoothstep(0.0, 0.25, chroma);
    let scale = (1.0 + g.color.x) * (1.0 + g.color.y * low_chroma * protect);
    let new_chroma = max(chroma * scale, 0.0);
    lab = vec3<f32>(lab.x, new_chroma * cos(hue), new_chroma * sin(hue));
    return srgb_encode(oklab_to_linear(lab));
}

// 3D LUT with tetrahedral interpolation. Input is mapped through the per-channel domain first.
fn lut3d_apply(c: vec3<f32>, g: GradeUniforms) -> vec3<f32> {
    let dims = textureDimensions(lut3d_tex);
    let edge_minus_one = f32(dims.x) - 1.0;
    let t = clamp((c - g.lut_domain_min.xyz) * g.lut_domain_scale.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    let p = t * edge_minus_one;
    let base_f = min(floor(p), vec3<f32>(edge_minus_one - 1.0));
    let f = p - base_f;
    let b = vec3<i32>(base_f);
    let c000 = textureLoad(lut3d_tex, b, 0).rgb;
    let c100 = textureLoad(lut3d_tex, b + vec3<i32>(1, 0, 0), 0).rgb;
    let c010 = textureLoad(lut3d_tex, b + vec3<i32>(0, 1, 0), 0).rgb;
    let c001 = textureLoad(lut3d_tex, b + vec3<i32>(0, 0, 1), 0).rgb;
    let c110 = textureLoad(lut3d_tex, b + vec3<i32>(1, 1, 0), 0).rgb;
    let c101 = textureLoad(lut3d_tex, b + vec3<i32>(1, 0, 1), 0).rgb;
    let c011 = textureLoad(lut3d_tex, b + vec3<i32>(0, 1, 1), 0).rgb;
    let c111 = textureLoad(lut3d_tex, b + vec3<i32>(1, 1, 1), 0).rgb;
    // Walk the cube diagonal in the order of the fractions (six tetrahedra).
    if f.x >= f.y {
        if f.y >= f.z {
            return c000 + f.x * (c100 - c000) + f.y * (c110 - c100) + f.z * (c111 - c110);
        } else if f.x >= f.z {
            return c000 + f.x * (c100 - c000) + f.z * (c101 - c100) + f.y * (c111 - c101);
        }
        return c000 + f.z * (c001 - c000) + f.x * (c101 - c001) + f.y * (c111 - c101);
    }
    if f.z >= f.y {
        return c000 + f.z * (c001 - c000) + f.y * (c011 - c001) + f.x * (c111 - c011);
    } else if f.z >= f.x {
        return c000 + f.y * (c010 - c000) + f.z * (c011 - c010) + f.x * (c111 - c011);
    }
    return c000 + f.y * (c010 - c000) + f.x * (c110 - c010) + f.z * (c111 - c110);
}

fn luma709(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Vignette factor at an output pixel. The corners are at r = 1 and the centre at r = 0.
fn vignette_factor(frag_xy: vec2<f32>, out_size: vec2<f32>, amount: f32) -> f32 {
    let uv = frag_xy / out_size;
    let aspect = out_size.x / out_size.y;
    let p = (uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0);
    let corner = length(vec2<f32>(0.5 * aspect, 0.5));
    let r = length(p) / corner;
    return 1.0 - 0.8 * amount * smoothstep(0.55, 1.0, r);
}

// The grade stages that act on the colour of one pixel. Sharpening and the vignette need neighbours or the
// position, so fs_main applies them.
fn grade_color(input: vec3<f32>, g: GradeUniforms) -> vec3<f32> {
    let flags = g.flags.x;
    var c = clamp(input, vec3<f32>(0.0), vec3<f32>(1.0));
    if (flags & FLAG_LINEAR) != 0u {
        c = linear_stage(c, g);
    }
    if (flags & FLAG_TONAL) != 0u {
        c = tonal_stage(c, g);
    }
    if (flags & FLAG_CURVES) != 0u {
        c = table_apply(curve_tex, c);
    }
    if (flags & FLAG_HSL) != 0u {
        c = hsl_stage(c, g);
    }
    if (flags & FLAG_SATURATION) != 0u {
        c = saturation_stage(c, g);
    }
    if (flags & FLAG_LUT3D) != 0u {
        c = mix(c, lut3d_apply(c, g), g.lut.x);
    }
    if (flags & FLAG_LUT1D) != 0u {
        c = mix(c, table_apply(lut1d_tex, c), g.lut.x);
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

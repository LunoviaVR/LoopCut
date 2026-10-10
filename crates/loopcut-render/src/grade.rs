//! Colour grading: CPU preparation of a [`Grade`] into the uniforms and small tables the compositor's colour pass reads.
//!
//! [`prepare_grade`] is pure CPU work in `f64`, meant to run once per project change. The result is a [`GradeSetup`].
//! [`crate::compositor::Compositor::set_grade`] uploads its tables only when its [`GradeSetup::key`] changes, and the
//! uniform block is written with every render. Preview and export call the same code, so they grade identically.
//!
//! # Grade math
//!
//! The grade runs on the display-referred SDR colour the compositor produced: after the source is converted to SDR, after
//! the crossfade, and before overlays are drawn. Every stage has a flag bit, and a stage whose parameters are neutral is
//! not run at all. An identity grade therefore costs no texture reads and no arithmetic.
//!
//! 1. **Linear light** (`FLAG_LINEAR`). The sRGB EOTF decodes the value. Exposure multiplies by `2^stops`. White balance
//!    is a 3x3 matrix on linear BT.709 RGB. It is a Bradford chromatic adaptation from D65 to a target white. Temperature
//!    moves that white along the Planckian locus (Kim et al. cubic fit) by up to 50 mired, positive is warmer. Tint moves
//!    it across the locus, along its normal in CIE 1960 uv, by up to 0.006, positive is magenta. Both are offsets from
//!    D65, so zero is exact. Negative light is clipped to zero.
//! 2. **Encoded tonal** (`FLAG_TONAL`). The sRGB OETF re-encodes the value. Per channel,
//!    `y = clamp(x + lift * (1 - x), 0, 1)` and `out = clamp(gain * y^(1/gamma), 0, 1)`. Contrast subtracts
//!    `a * sin(2 pi x) / (2 pi)` with `a = contrast`. That keeps 0, 0.5 and 1 fixed and is monotone for `|a| <= 1`, and it
//!    steepens the midtones for `a > 0`. Shadows and highlights act on Rec.709 luma `Y`. The shadow mask is
//!    `1 - smoothstep(0, 0.5, Y)`: 1 at black, falling to 0 at `Y = 0.5` and staying 0 above it. The highlight mask is
//!    `smoothstep(0.5, 1, Y)`: 0 below `Y = 0.5`, rising to 1 at white. For a positive amount the luma moves toward white
//!    by `0.25 * amount * mask * (1 - Y)`. For a negative amount it scales by `1 + 0.25 * amount * mask`. The shadow
//!    change is applied first, then the highlight change, and the total luma change is added equally to each channel.
//! 3. **Tone curves** (`FLAG_CURVES`). `out_c = C_c(M(x_c))`, where `M` is the master curve and `C_c` the red, green or
//!    blue curve. Each curve is a monotone cubic Hermite spline (Fritsch-Carlson) through its points, with implicit anchors
//!    at (0, 0) and (1, 1). Each is baked into [`TABLE_SIZE`] entries per channel and sampled with linear interpolation.
//!    An empty curve is the identity.
//! 4. **Hue bands** (`FLAG_HSL`). RGB is converted to HSL. Eight bands sit at hues 0, 30, 60, 120, 180, 240, 270 and 300
//!    degrees. In the gap between two neighbouring bands, with `t` the position of the hue in that gap, the nearer band
//!    gets weight `cos^2(pi t / 2)` and the other gets `sin^2(pi t / 2)`. The weights sum to one and have zero slope at
//!    each band centre. A band can shift hue by its degrees, scale saturation by `1 + s`, and move lightness by
//!    `0.25 * l`. Pixels with saturation under about 0.02 get no band adjustment, so a band's lightness change does not
//!    lighten neutral greys.
//! 5. **Saturation and vibrance** (`FLAG_SATURATION`). The colour is converted to OKLab. Its chroma `C` becomes
//!    `C * (1 + saturation) * (1 + vibrance * w * p)`. Here `w = 1 - smoothstep(0, 0.25, C)` favours low-chroma colours,
//!    and `p = 1 - 0.5 * exp(-0.5 * ((h - 55) / 20)^2)` protects skin tones, with `h` the OKLab hue in degrees. Saturation
//!    -1 gives exact grey.
//! 6. **LUT** (`FLAG_LUT3D` or `FLAG_LUT1D`). The LUT output is blended with its input by `lut_strength`. A 3D table is
//!    read with tetrahedral interpolation on an `rgba16float` 3D texture, after the input is mapped through the table's
//!    per-channel domain (`DOMAIN_MIN` and `DOMAIN_MAX`). A 1D table is resampled to [`TABLE_SIZE`] entries over `[0, 1]`
//!    and read per channel.
//! 7. **Sharpen** (`FLAG_SHARPEN`). Let `d` be the luma of the input pixel minus the mean luma of its four neighbours,
//!    one output pixel away. The neighbours are read from the same input composite in the same pass. `sharpen * d` is
//!    then added to each channel of the graded value. This is a cheap approximation: it measures detail before the
//!    grade and adds it after, so it is not a true unsharp mask on the graded picture.
//!
//!    **Cost.** The composite is evaluated at five positions per output pixel (the pixel and its four neighbours), not
//!    one. Each evaluation samples and converts the source, and a crossfade samples the second source too. The source
//!    work of a sharpened render is therefore about five times that of a plain one. The grade arithmetic itself runs
//!    once per pixel. Sharpening costs nothing when `sharpen` is zero, and it is off by default.
//! 8. **Vignette** (`FLAG_VIGNETTE`). The distance `r` from the frame centre is aspect-corrected and scaled so the corners
//!    are 1. The colour is multiplied by `1 - 0.8 * vignette * smoothstep(0.55, 1, r)`, so the centre is unchanged.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::Hasher;

use bytemuck::{Pod, Zeroable};
use loopcut_core::project::{CurvePoint, Curves, Grade};

use crate::lut::Lut;

/// Entries in each baked one-dimensional table (tone curves and 1D LUTs).
pub const TABLE_SIZE: usize = 1024;

/// Stage flag: exposure and white balance, computed in linear light.
pub(crate) const FLAG_LINEAR: u32 = 1 << 0;
/// Stage flag: lift, gamma, gain, contrast, shadows and highlights.
pub(crate) const FLAG_TONAL: u32 = 1 << 1;
/// Stage flag: the baked tone curves.
pub(crate) const FLAG_CURVES: u32 = 1 << 2;
/// Stage flag: the eight hue bands.
pub(crate) const FLAG_HSL: u32 = 1 << 3;
/// Stage flag: OKLab saturation and vibrance.
pub(crate) const FLAG_SATURATION: u32 = 1 << 4;
/// Stage flag: a 3D `.cube` LUT.
pub(crate) const FLAG_LUT3D: u32 = 1 << 5;
/// Stage flag: a 1D `.cube` LUT.
pub(crate) const FLAG_LUT1D: u32 = 1 << 6;
/// Stage flag: sharpening.
pub(crate) const FLAG_SHARPEN: u32 = 1 << 7;
/// Stage flag: vignette.
pub(crate) const FLAG_VIGNETTE: u32 = 1 << 8;

/// Temperature range in mired (reciprocal megakelvin) at full scale.
const TEMPERATURE_MIRED: f64 = 50.0;
/// Tint range in CIE 1960 uv at full scale.
const TINT_UV: f64 = 0.006;
/// The CCT of D65, which is the neutral white of the grade.
const D65_CCT: f64 = 6504.0;
/// CIE xy chromaticity of D65.
const D65_X: f64 = 0.3127;
const D65_Y: f64 = 0.3290;
/// Linear sRGB (D65) to CIE XYZ.
const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];
/// The Bradford cone response matrix.
const BRADFORD: [[f64; 3]; 3] = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Uniform block of the colour pass, mirrored by `GradeUniforms` in `shaders/grade.wgsl` (336 bytes).
///
/// Every `vec4` is one WGSL `vec4<f32>`, or `vec4<u32>` for `flags`. Unused lanes are zero.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub(crate) struct GradeUniforms {
    /// `x`: the stage flags (`FLAG_*`). Zero means identity.
    pub(crate) flags: [u32; 4],
    /// `x`: exposure gain `2^stops`; `y`: contrast; `z`: highlights; `w`: shadows.
    pub(crate) tone: [f32; 4],
    /// `x`: saturation; `y`: vibrance; `z`: sharpen; `w`: vignette.
    pub(crate) color: [f32; 4],
    /// `x`: LUT strength in `[0, 1]`.
    pub(crate) lut: [f32; 4],
    /// Per-channel LUT domain minimum (3D).
    pub(crate) lut_domain_min: [f32; 4],
    /// Per-channel `1 / (max - min)` of the LUT domain (3D).
    pub(crate) lut_domain_scale: [f32; 4],
    /// Per-channel lift.
    pub(crate) lift: [f32; 4],
    /// Per-channel `1 / gamma`.
    pub(crate) gamma_inv: [f32; 4],
    /// Per-channel gain.
    pub(crate) gain: [f32; 4],
    /// The white balance matrix in linear RGB, as three columns (`xyz`).
    pub(crate) white_balance: [[f32; 4]; 3],
    /// Per band: `x` hue shift in degrees, `y` saturation, `z` luminance.
    pub(crate) hsl: [[f32; 4]; 8],
    /// `x`: the 3D LUT edge length in texels.
    pub(crate) lut_size: [f32; 4],
}

impl GradeUniforms {
    /// The neutral block: no stage flags, unit gains and an identity white balance.
    fn neutral() -> Self {
        let mut uniforms = Self::zeroed();
        uniforms.white_balance = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        uniforms.tone[0] = 1.0;
        uniforms.gamma_inv = [1.0; 4];
        uniforms.gain = [1.0; 4];
        uniforms
    }
}

/// A grade prepared for [`crate::compositor::Compositor::set_grade`].
///
/// Built by [`prepare_grade`]. It owns the uniform block and any curve or LUT tables, so it can be kept and reused.
#[derive(Clone)]
pub struct GradeSetup {
    uniforms: GradeUniforms,
    /// Per-entry `(R, G, B)` curve output for input `i / (TABLE_SIZE - 1)`, master curve already applied.
    curve: Option<Vec<[f32; 4]>>,
    /// Per-entry `(R, G, B)` 1D LUT output for input `i / (TABLE_SIZE - 1)`, with its domain already applied.
    lut1d: Option<Vec<[f32; 4]>>,
    /// The 3D LUT, shared with the caller. Its `rgba16float` texels are cached inside it.
    lut3d: Option<Lut>,
    tables_key: u64,
    key: u64,
}

impl fmt::Debug for GradeSetup {
    /// Shows the keys, the stage flags and the table sizes. The baked tables themselves are not printed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GradeSetup")
            .field("key", &self.key)
            .field("tables_key", &self.tables_key)
            .field("stages", &format_args!("{:#011b}", self.uniforms.flags[0]))
            .field("curve_entries", &self.curve.as_ref().map(Vec::len))
            .field("lut1d_entries", &self.lut1d.as_ref().map(Vec::len))
            .field("lut3d", &self.lut3d)
            .finish()
    }
}

impl GradeSetup {
    /// The identity grade: no stage runs and no tables are uploaded.
    #[must_use]
    pub fn identity() -> Self {
        Self::from_parts(GradeUniforms::neutral(), None, None, None)
    }

    /// True when the grade leaves every pixel unchanged. The compositor then skips the grade entirely.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.uniforms.flags[0] == 0
    }

    /// A key that changes whenever the prepared content changes. Equal keys mean equal uniforms and tables, so the
    /// compositor can skip re-uploading them.
    #[must_use]
    pub fn key(&self) -> u64 {
        self.key
    }

    /// The baked curve table (`TABLE_SIZE` entries of `(R, G, B, unused)`), or `None` when the curves are the identity.
    #[must_use]
    pub fn curve_table(&self) -> Option<&[[f32; 4]]> {
        self.curve.as_deref()
    }

    /// The baked 1D LUT table (`TABLE_SIZE` entries of `(R, G, B, unused)`), or `None` when there is no 1D LUT.
    #[must_use]
    pub fn lut1d_table(&self) -> Option<&[[f32; 4]]> {
        self.lut1d.as_deref()
    }

    /// The 3D LUT this setup samples, or `None` when there is none.
    #[must_use]
    pub fn lut3d(&self) -> Option<&Lut> {
        self.lut3d.as_ref()
    }

    /// The stage flags in use (`FLAG_*`), for diagnostics and tests.
    #[must_use]
    pub fn stage_flags(&self) -> u32 {
        self.uniforms.flags[0]
    }

    /// The uniform block the colour pass reads.
    pub(crate) fn uniforms(&self) -> &GradeUniforms {
        &self.uniforms
    }

    /// A key for the tables alone: the baked curve and 1D tables, and the identity of the 3D LUT. Two setups with the
    /// same tables key hold the same table data, so the compositor can keep the uploaded tables and only write the
    /// uniforms. Slider changes that touch only the uniforms leave it unchanged.
    #[must_use]
    pub fn tables_key(&self) -> u64 {
        self.tables_key
    }

    fn from_parts(
        uniforms: GradeUniforms,
        curve: Option<Vec<[f32; 4]>>,
        lut1d: Option<Vec<[f32; 4]>>,
        lut3d: Option<Lut>,
    ) -> Self {
        let mut tables = DefaultHasher::new();
        tables.write_u8(u8::from(curve.is_some()));
        if let Some(table) = &curve {
            tables.write(bytemuck::cast_slice(table.as_slice()));
        }
        tables.write_u8(u8::from(lut1d.is_some()));
        if let Some(table) = &lut1d {
            tables.write(bytemuck::cast_slice(table.as_slice()));
        }
        tables.write_u8(u8::from(lut3d.is_some()));
        if let Some(lut) = &lut3d {
            tables.write_u64(lut.id());
        }
        let tables_key = tables.finish();

        let mut hasher = DefaultHasher::new();
        hasher.write(bytemuck::bytes_of(&uniforms));
        hasher.write_u64(tables_key);
        let key = hasher.finish();
        Self {
            uniforms,
            curve,
            lut1d,
            lut3d,
            tables_key,
            key,
        }
    }
}

/// Prepares `grade` (and an optional `.cube` LUT) for [`crate::compositor::Compositor::set_grade`].
///
/// A disabled grade is the identity. A LUT is applied only when `lut` is `Some` and the strength is above zero, and the
/// table kind decides between the 3D and 1D stages. Parameters outside their documented ranges are clamped, and
/// non-finite values are treated as neutral. The work is CPU-only and does not touch the GPU.
#[must_use]
pub fn prepare_grade(grade: &Grade, lut: Option<&Lut>) -> GradeSetup {
    if !grade.enabled {
        return GradeSetup::identity();
    }
    let mut uniforms = GradeUniforms::neutral();
    let mut flags = 0_u32;

    let exposure = param(grade.exposure, -4.0, 4.0, 0.0);
    let temperature = param(grade.temperature, -1.0, 1.0, 0.0);
    let tint = param(grade.tint, -1.0, 1.0, 0.0);
    if exposure != 0.0 || temperature != 0.0 || tint != 0.0 {
        flags |= FLAG_LINEAR;
        uniforms.tone[0] = 2.0_f64.powf(exposure) as f32;
        let matrix = white_balance_matrix(temperature, tint);
        for (column, uniform_column) in uniforms.white_balance.iter_mut().enumerate() {
            *uniform_column = [
                matrix[0][column] as f32,
                matrix[1][column] as f32,
                matrix[2][column] as f32,
                0.0,
            ];
        }
    }

    let lift = grade.lift.map(|value| param(value, -1.0, 1.0, 0.0));
    let gamma = grade.gamma.map(|value| param(value, 0.1, 4.0, 1.0));
    let gain = grade.gain.map(|value| param(value, 0.0, 4.0, 1.0));
    let contrast = param(grade.contrast, -1.0, 1.0, 0.0);
    let highlights = param(grade.highlights, -1.0, 1.0, 0.0);
    let shadows = param(grade.shadows, -1.0, 1.0, 0.0);
    let tonal = lift.iter().any(|&v| v != 0.0)
        || gamma.iter().any(|&v| v != 1.0)
        || gain.iter().any(|&v| v != 1.0)
        || contrast != 0.0
        || highlights != 0.0
        || shadows != 0.0;
    if tonal {
        flags |= FLAG_TONAL;
        uniforms.lift = vec4_from(lift);
        uniforms.gamma_inv = vec4_from(gamma.map(|g| 1.0 / g));
        uniforms.gain = vec4_from(gain);
        uniforms.tone[1] = contrast as f32;
        uniforms.tone[2] = highlights as f32;
        uniforms.tone[3] = shadows as f32;
    }

    let curve = bake_curves(&grade.curves);
    if curve.is_some() {
        flags |= FLAG_CURVES;
    }

    let mut hsl_active = false;
    for (band, adjust) in grade.hsl.iter().enumerate() {
        let hue = param(adjust.hue, -180.0, 180.0, 0.0);
        let saturation = param(adjust.saturation, -1.0, 1.0, 0.0);
        let luminance = param(adjust.luminance, -1.0, 1.0, 0.0);
        uniforms.hsl[band] = [hue as f32, saturation as f32, luminance as f32, 0.0];
        hsl_active |= hue != 0.0 || saturation != 0.0 || luminance != 0.0;
    }
    if hsl_active {
        flags |= FLAG_HSL;
    }

    let saturation = param(grade.saturation, -1.0, 1.0, 0.0);
    let vibrance = param(grade.vibrance, -1.0, 1.0, 0.0);
    if saturation != 0.0 || vibrance != 0.0 {
        flags |= FLAG_SATURATION;
        uniforms.color[0] = saturation as f32;
        uniforms.color[1] = vibrance as f32;
    }

    let mut lut1d = None;
    let mut lut3d = None;
    let strength = param(grade.lut_strength, 0.0, 1.0, 1.0);
    if let Some(lut) = lut.filter(|_| strength > 0.0) {
        uniforms.lut[0] = strength as f32;
        if lut.is_3d() {
            flags |= FLAG_LUT3D;
            let (min, max) = (lut.domain_min(), lut.domain_max());
            for channel in 0..3 {
                uniforms.lut_domain_min[channel] = min[channel];
                uniforms.lut_domain_scale[channel] = 1.0 / (max[channel] - min[channel]);
            }
            uniforms.lut_size[0] = lut.size() as f32;
            lut3d = Some(lut.clone());
        } else {
            flags |= FLAG_LUT1D;
            lut1d = Some(bake_lut1d(lut));
        }
    }

    let sharpen = param(grade.sharpen, 0.0, 1.0, 0.0);
    if sharpen > 0.0 {
        flags |= FLAG_SHARPEN;
        uniforms.color[2] = sharpen as f32;
    }
    let vignette = param(grade.vignette, 0.0, 1.0, 0.0);
    if vignette > 0.0 {
        flags |= FLAG_VIGNETTE;
        uniforms.color[3] = vignette as f32;
    }

    uniforms.flags[0] = flags;
    GradeSetup::from_parts(uniforms, curve, lut1d, lut3d)
}

/// Clamps a control value to its range, with `neutral` for non-finite input.
fn param(value: f32, lo: f64, hi: f64, neutral: f64) -> f64 {
    if value.is_finite() {
        f64::from(value).clamp(lo, hi)
    } else {
        neutral
    }
}

fn vec4_from(values: [f64; 3]) -> [f32; 4] {
    [values[0] as f32, values[1] as f32, values[2] as f32, 0.0]
}

/// The input of table entry `index`, in `[0, 1]`.
fn entry_input(index: usize) -> f64 {
    index as f64 / (TABLE_SIZE - 1) as f64
}

/// Bakes the master curve and the per-channel curves into one table, or returns `None` when they are the identity.
fn bake_curves(curves: &Curves) -> Option<Vec<[f32; 4]>> {
    let master = MonotoneCurve::new(&curves.master);
    let channels = [
        MonotoneCurve::new(&curves.red),
        MonotoneCurve::new(&curves.green),
        MonotoneCurve::new(&curves.blue),
    ];
    if master.is_none() && channels.iter().all(Option::is_none) {
        return None;
    }
    let mut table = Vec::with_capacity(TABLE_SIZE);
    let mut changed = false;
    for index in 0..TABLE_SIZE {
        let x = entry_input(index);
        let mastered = master.as_ref().map_or(x, |curve| curve.eval(x));
        let mut entry = [0.0_f32; 4];
        for (channel, curve) in channels.iter().enumerate() {
            let value = curve
                .as_ref()
                .map_or(mastered, |curve| curve.eval(mastered))
                .clamp(0.0, 1.0);
            entry[channel] = value as f32;
            changed |= (value - x).abs() > 1e-5;
        }
        table.push(entry);
    }
    changed.then_some(table)
}

/// Bakes a 1D `.cube` LUT into `TABLE_SIZE` entries over `[0, 1]`, applying the LUT's per-channel domain.
fn bake_lut1d(lut: &Lut) -> Vec<[f32; 4]> {
    (0..TABLE_SIZE)
        .map(|index| {
            let x = entry_input(index) as f32;
            let [r, g, b] = lut.sample([x, x, x]);
            [r, g, b, 1.0]
        })
        .collect()
}

/// A monotone cubic Hermite spline through a curve's control points (Fritsch-Carlson tangents).
struct MonotoneCurve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    tangents: Vec<f64>,
}

impl MonotoneCurve {
    /// Builds the spline, or returns `None` when there are no usable points. Points are clamped to `[0, 1]`, sorted by
    /// `x`, and a repeated `x` keeps its last point. Anchors at (0, 0) and (1, 1) are added when the points do not reach
    /// those ends, so a single point lifts or lowers the middle of the curve only.
    fn new(points: &[CurvePoint]) -> Option<Self> {
        let mut sorted: Vec<(f64, f64)> = points
            .iter()
            .filter(|point| point.x.is_finite() && point.y.is_finite())
            .map(|point| {
                (
                    f64::from(point.x).clamp(0.0, 1.0),
                    f64::from(point.y).clamp(0.0, 1.0),
                )
            })
            .collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut unique: Vec<(f64, f64)> = Vec::with_capacity(sorted.len() + 2);
        for (x, y) in sorted {
            match unique.last_mut() {
                Some(last) if last.0 == x => last.1 = y,
                _ => unique.push((x, y)),
            }
        }
        if unique.first().is_some_and(|first| first.0 > 0.0) {
            unique.insert(0, (0.0, 0.0));
        }
        if unique.last().is_none_or(|last| last.0 < 1.0) {
            unique.push((1.0, 1.0));
        }
        let xs: Vec<f64> = unique.iter().map(|point| point.0).collect();
        let ys: Vec<f64> = unique.iter().map(|point| point.1).collect();
        let n = xs.len();
        let slopes: Vec<f64> = (0..n - 1)
            .map(|k| (ys[k + 1] - ys[k]) / (xs[k + 1] - xs[k]))
            .collect();
        let mut tangents = vec![0.0_f64; n];
        tangents[0] = slopes[0];
        tangents[n - 1] = slopes[n - 2];
        for k in 1..n - 1 {
            tangents[k] = if slopes[k - 1] * slopes[k] <= 0.0 {
                0.0
            } else {
                (slopes[k - 1] + slopes[k]) / 2.0
            };
        }
        // Fritsch-Carlson limiter: keep each segment inside the circle of radius 3 so the spline stays monotone.
        for (k, &slope) in slopes.iter().enumerate() {
            if slope == 0.0 {
                tangents[k] = 0.0;
                tangents[k + 1] = 0.0;
                continue;
            }
            let a = tangents[k] / slope;
            let b = tangents[k + 1] / slope;
            let norm = a * a + b * b;
            if norm > 9.0 {
                let tau = 3.0 / norm.sqrt();
                tangents[k] = tau * a * slope;
                tangents[k + 1] = tau * b * slope;
            }
        }
        Some(Self { xs, ys, tangents })
    }

    /// Evaluates the spline at `x`, clamped to the curve's span.
    fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        let x = x.clamp(self.xs[0], self.xs[n - 1]);
        let k = match self.xs.partition_point(|&xi| xi <= x) {
            0 => 0,
            p => (p - 1).min(n - 2),
        };
        let h = self.xs[k + 1] - self.xs[k];
        let t = (x - self.xs[k]) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * self.ys[k]
            + h10 * h * self.tangents[k]
            + h01 * self.ys[k + 1]
            + h11 * h * self.tangents[k + 1]
    }
}

/// The CIE 1960 uv chromaticity of a CIE xy point.
fn xy_to_uv(x: f64, y: f64) -> (f64, f64) {
    let d = -2.0 * x + 12.0 * y + 3.0;
    (4.0 * x / d, 6.0 * y / d)
}

/// The CIE xy chromaticity of a CIE 1960 uv point.
fn uv_to_xy(u: f64, v: f64) -> (f64, f64) {
    let d = 2.0 * u - 8.0 * v + 4.0;
    (3.0 * u / d, 2.0 * v / d)
}

/// The chromaticity of the Planckian locus at `kelvin` (Kim et al., 2002; valid from 1667 K to 25000 K).
fn planckian_xy(kelvin: f64) -> (f64, f64) {
    let t = kelvin.clamp(1667.0, 25_000.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let x = if t <= 4000.0 {
        -0.266_123_9e9 / t3 - 0.234_358_9e6 / t2 + 0.877_695_6e3 / t + 0.179_910
    } else {
        -3.025_846_9e9 / t3 + 2.107_037_9e6 / t2 + 0.222_634_7e3 / t + 0.240_390
    };
    let x2 = x * x;
    let x3 = x2 * x;
    let y = if t <= 2222.0 {
        -1.106_381_4 * x3 - 1.348_110_2 * x2 + 2.185_558_32 * x - 0.202_196_83
    } else if t <= 4000.0 {
        -0.954_947_6 * x3 - 1.374_185_93 * x2 + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758 * x3 - 5.873_386_7 * x2 + 3.751_129_97 * x - 0.370_014_83
    };
    (x, y)
}

fn mat_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (col, cell) in out_row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[row][k] * b[k][col]).sum();
        }
    }
    out
}

fn mat_vec(a: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

fn mat_inverse(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv = |a: f64| a / det;
    [
        [
            inv(m[1][1] * m[2][2] - m[1][2] * m[2][1]),
            inv(m[0][2] * m[2][1] - m[0][1] * m[2][2]),
            inv(m[0][1] * m[1][2] - m[0][2] * m[1][1]),
        ],
        [
            inv(m[1][2] * m[2][0] - m[1][0] * m[2][2]),
            inv(m[0][0] * m[2][2] - m[0][2] * m[2][0]),
            inv(m[0][2] * m[1][0] - m[0][0] * m[1][2]),
        ],
        [
            inv(m[1][0] * m[2][1] - m[1][1] * m[2][0]),
            inv(m[0][1] * m[2][0] - m[0][0] * m[2][1]),
            inv(m[0][0] * m[1][1] - m[0][1] * m[1][0]),
        ],
    ]
}

/// The unit direction in CIE 1960 uv that points toward magenta: the normal of the Planckian locus at D65, pointing
/// away from green. The locus runs toward lower u and v as it gets bluer, so its tangent is `hot - cool`, and the normal
/// that points down and to the right is `(-tv, tu)`.
fn magenta_direction() -> (f64, f64) {
    let (cool_x, cool_y) = planckian_xy(D65_CCT / 1.1);
    let (hot_x, hot_y) = planckian_xy(D65_CCT * 1.1);
    let (cool_u, cool_v) = xy_to_uv(cool_x, cool_y);
    let (hot_u, hot_v) = xy_to_uv(hot_x, hot_y);
    let (tu, tv) = (hot_u - cool_u, hot_v - cool_v);
    let norm = (tu * tu + tv * tv).sqrt();
    (-tv / norm, tu / norm)
}

/// The linear RGB white balance matrix for `temperature` and `tint` (both in `[-1, 1]`), mapping D65 to the target white.
///
/// The target white is a Bradford adaptation from D65 (the sRGB white) to the shifted white. Zero temperature and tint
/// give the identity exactly, because the shift is an offset from D65.
fn white_balance_matrix(temperature: f64, tint: f64) -> [[f64; 3]; 3] {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if temperature == 0.0 && tint == 0.0 {
        return identity;
    }
    // Reciprocal colour temperature (mired): positive temperature raises it, which is a lower and warmer CCT.
    let d65_mired = 1.0e6 / D65_CCT;
    let mired = (d65_mired + temperature * TEMPERATURE_MIRED).clamp(60.0, 400.0);
    let (locus_x, locus_y) = planckian_xy(1.0e6 / mired);
    let (reference_x, reference_y) = planckian_xy(D65_CCT);
    let x = locus_x + (D65_X - reference_x);
    let y = locus_y + (D65_Y - reference_y);
    let (u, v) = xy_to_uv(x, y);
    let (magenta_u, magenta_v) = magenta_direction();
    let (x, y) = uv_to_xy(
        u + tint * TINT_UV * magenta_u,
        v + tint * TINT_UV * magenta_v,
    );

    let source_xyz = [D65_X / D65_Y, 1.0, (1.0 - D65_X - D65_Y) / D65_Y];
    let target_xyz = [x / y, 1.0, (1.0 - x - y) / y];
    let source_cone = mat_vec(BRADFORD, source_xyz);
    let target_cone = mat_vec(BRADFORD, target_xyz);
    let gains = [
        target_cone[0] / source_cone[0],
        target_cone[1] / source_cone[1],
        target_cone[2] / source_cone[2],
    ];
    let scaled = [
        [
            gains[0] * BRADFORD[0][0],
            gains[0] * BRADFORD[0][1],
            gains[0] * BRADFORD[0][2],
        ],
        [
            gains[1] * BRADFORD[1][0],
            gains[1] * BRADFORD[1][1],
            gains[1] * BRADFORD[1][2],
        ],
        [
            gains[2] * BRADFORD[2][0],
            gains[2] * BRADFORD[2][1],
            gains[2] * BRADFORD[2][2],
        ],
    ];
    let adapt_xyz = mat_mul(mat_inverse(BRADFORD), scaled);
    mat_mul(mat_inverse(SRGB_TO_XYZ), mat_mul(adapt_xyz, SRGB_TO_XYZ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d65_is_on_the_locus_within_the_expected_offset() {
        let (x, y) = planckian_xy(D65_CCT);
        assert!((x - 0.3134).abs() < 0.002, "x = {x}");
        assert!((y - 0.3237).abs() < 0.002, "y = {y}");
    }

    #[test]
    fn zero_white_balance_is_exact_identity() {
        let m = white_balance_matrix(0.0, 0.0);
        assert_eq!(m, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }

    #[test]
    fn positive_temperature_warms_neutral_light() {
        // The adapted white of the sRGB neutral (1, 1, 1) must be warm: red above blue.
        let m = white_balance_matrix(1.0, 0.0);
        let r = m[0][0] + m[0][1] + m[0][2];
        let b = m[2][0] + m[2][1] + m[2][2];
        assert!(r > b, "r = {r}, b = {b}");
    }

    #[test]
    fn spline_passes_through_points_and_stays_monotone() {
        let points = [
            CurvePoint { x: 0.2, y: 0.1 },
            CurvePoint { x: 0.5, y: 0.75 },
            CurvePoint { x: 0.8, y: 0.85 },
        ];
        let curve = MonotoneCurve::new(&points).expect("points");
        assert!((curve.eval(0.5) - 0.75).abs() < 1e-12);
        let mut previous = -1.0;
        for index in 0..=1000 {
            let value = curve.eval(index as f64 / 1000.0);
            assert!(value >= previous - 1e-12, "not monotone at {index}");
            assert!((0.0..=1.0).contains(&value));
            previous = value;
        }
    }
}

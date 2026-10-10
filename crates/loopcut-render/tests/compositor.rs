//! Compositor tests on the software (WARP) adapter, and on the hardware adapter when one is present.
//!
//! Run with `cargo test -p loopcut-render --test compositor`. The timing benchmark is `#[ignore]`d; run it with
//! `cargo test -p loopcut-render --test compositor --release -- --ignored --nocapture`.
//!
//! Expected colours come from CPU reference code in this file (YCbCr encoding, PQ encoding), so the shader is
//! checked against an independent implementation rather than against itself.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use loopcut_core::geometry::Point;
use loopcut_core::{
    ColorInfo, ColorRange, NormRect, Orientation, PixelLayout, TransferFunction, YuvMatrix,
};
use loopcut_render::compositor::{
    CompositeParams, Compositor, CompositorError, InputFrame, Overlay,
};
use loopcut_render::gpu::{AdapterChoice, GpuContext};

/// Contexts to test: the software adapter always, plus the `Auto` adapter when it is hardware.
///
/// The contexts are created once per test binary and shared by every test, so each adapter has one device, as in the
/// app. With a device per test, eight test threads created devices at the same time, and one run in twenty
/// segfaulted (exit 139). Sharing removed the race, and the pipeline lock in `compositor.rs` covers the rest.
fn contexts() -> Vec<(&'static str, Arc<GpuContext>)> {
    static CONTEXTS: OnceLock<Vec<(&'static str, Arc<GpuContext>)>> = OnceLock::new();
    CONTEXTS
        .get_or_init(|| {
            let software = GpuContext::new(&AdapterChoice::Software)
                .expect("the software (WARP) adapter should open on Windows");
            assert!(software.is_software(), "the software choice must be WARP");
            let mut list = vec![("software", Arc::new(software))];
            match GpuContext::new(&AdapterChoice::Auto) {
                Ok(hardware) if !hardware.is_software() => {
                    list.push(("hardware", Arc::new(hardware)));
                }
                _ => println!("SKIP: no hardware adapter; the hardware pass is not run"),
            }
            list
        })
        .iter()
        .map(|(label, ctx)| (*label, Arc::clone(ctx)))
        .collect()
}

/// Runs `check` once per adapter returned by [`contexts`].
fn for_each_adapter(check: impl Fn(&str, Arc<GpuContext>)) {
    for (label, ctx) in contexts() {
        println!("{label}: {}", ctx.desc.name);
        check(label, ctx);
    }
}

fn params(width: u32, height: u32) -> CompositeParams {
    CompositeParams {
        output_width: width,
        output_height: height,
        crop: NormRect::FULL,
        orientation: Orientation::default(),
        background: [0, 0, 0, 255],
    }
}

/// RGBA8 pixel at (`x`, `y`) of a tightly packed output.
fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// Asserts that the RGB channels are within `tolerance` of `expected` and the pixel is opaque.
fn assert_rgb_near(actual: [u8; 4], expected: [u8; 3], tolerance: i32, context: &str) {
    for (got, want) in actual.iter().take(3).zip(expected.iter()) {
        let diff = (i32::from(*got) - i32::from(*want)).abs();
        assert!(
            diff <= tolerance,
            "{context}: got {actual:?}, expected {expected:?} (tolerance {tolerance})"
        );
    }
    assert_eq!(actual[3], 255, "{context}: the output must be opaque");
}

/// CPU reference: encodes gamma-encoded RGB in `[0, 1]` as integer YCbCr codes.
///
/// `bits` is 8 or 10. Limited range puts Y at 16..235 and C at 16..240 (scaled by 4 for 10-bit).
fn encode_ycbcr(rgb: [f64; 3], matrix: YuvMatrix, range: ColorRange, bits: u32) -> [u32; 3] {
    let (kr, kb) = match matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        YuvMatrix::Bt2020 => (0.2627, 0.0593),
        YuvMatrix::Rgb => panic!("an RGB matrix has no YCbCr encoding"),
    };
    let kg = 1.0 - kr - kb;
    let [r, g, b] = rgb;
    let y = kr * r + kg * g + kb * b;
    let cb = (b - y) / (2.0 * (1.0 - kb));
    let cr = (r - y) / (2.0 * (1.0 - kr));
    let shift = bits - 8;
    let max = f64::from((1u32 << bits) - 1);
    let (y_offset, y_span, c_mid, c_span) = match range {
        ColorRange::Limited => (
            f64::from(16u32 << shift),
            f64::from(219u32 << shift),
            f64::from(128u32 << shift),
            f64::from(224u32 << shift),
        ),
        ColorRange::Full => (0.0, max, f64::from(128u32 << shift), max),
    };
    let quantize = |value: f64| value.round().clamp(0.0, max) as u32;
    [
        quantize(y_offset + y_span * y),
        quantize(c_mid + c_span * cb),
        quantize(c_mid + c_span * cr),
    ]
}

/// CPU reference: SMPTE ST 2084 inverse EOTF. `nits` is absolute luminance (0..10000); returns the PQ code in [0, 1].
fn pq_encode(nits: f64) -> f64 {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 4096.0 * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 4096.0 * 32.0;
    let c3 = 2392.0 / 4096.0 * 32.0;
    let y = (nits / 10000.0).powf(m1);
    ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
}

fn push_sample(out: &mut Vec<u8>, value: u32, p010: bool) {
    if p010 {
        out.extend_from_slice(&((value << 6) as u16).to_le_bytes());
    } else {
        out.push(value as u8);
    }
}

/// Builds a packed NV12 or P010 frame. `codes(x, y)` gives the Y, Cb and Cr codes of each luma sample. Chroma is
/// taken from the top-left luma sample of each 2x2 block, so callers keep colour uniform within those blocks.
fn build_yuv(
    layout: PixelLayout,
    width: u32,
    height: u32,
    codes: impl Fn(u32, u32) -> [u32; 3],
) -> Vec<u8> {
    let p010 = layout == PixelLayout::P010;
    let mut data = Vec::with_capacity(layout.frame_bytes(width, height));
    for y in 0..height {
        for x in 0..width {
            push_sample(&mut data, codes(x, y)[0], p010);
        }
    }
    for cy in 0..height / 2 {
        for cx in 0..width / 2 {
            let sample = codes(cx * 2, cy * 2);
            push_sample(&mut data, sample[1], p010);
            push_sample(&mut data, sample[2], p010);
        }
    }
    data
}

/// Colour patches used by the YUV tests, as 8-bit sRGB.
const PATCHES: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [255, 255, 255],
    [0, 0, 0],
    [128, 128, 128],
];
const PATCH_WIDTH: u32 = 16;

/// Renders a row of colour patches at 1:1 and checks each patch's interior.
///
/// The margin of 2 pixels keeps the checks clear of the bilinear chroma transition at each patch edge.
fn check_patches(
    label: &str,
    ctx: Arc<GpuContext>,
    layout: PixelLayout,
    color: ColorInfo,
    bits: u32,
) {
    let width = PATCH_WIDTH * PATCHES.len() as u32;
    let height = 16;
    let data = build_yuv(layout, width, height, |x, _| {
        let rgb = PATCHES[(x / PATCH_WIDTH) as usize].map(|value| f64::from(value) / 255.0);
        encode_ycbcr(rgb, color.matrix, color.range, bits)
    });
    let frame = InputFrame {
        layout,
        width,
        height,
        data: &data,
        color,
    };
    let mut compositor = Compositor::new(Arc::clone(&ctx)).expect("compositor");
    let pixels = compositor
        .render(&frame, None, &[], &params(width, height))
        .expect("render 1:1");
    assert_eq!(
        pixels.len(),
        (width * height * 4) as usize,
        "{label}: output size"
    );

    for (index, expected) in PATCHES.iter().enumerate() {
        let start = index as u32 * PATCH_WIDTH;
        for x in (start + 2)..(start + PATCH_WIDTH - 2) {
            for y in 2..(height - 2) {
                assert_rgb_near(
                    pixel(&pixels, width, x, y),
                    *expected,
                    3,
                    &format!("{label} {layout:?} {color:?} patch {index} at ({x}, {y})"),
                );
            }
        }
    }
}

#[test]
fn nv12_limited_bt709_color_patches() {
    for_each_adapter(|label, ctx| {
        check_patches(label, ctx, PixelLayout::Nv12, ColorInfo::SDR_BT709, 8);
    });
}

#[test]
fn nv12_full_bt601_color_patches() {
    let color = ColorInfo {
        matrix: YuvMatrix::Bt601,
        range: ColorRange::Full,
        transfer: TransferFunction::Sdr,
        wide_gamut: false,
    };
    for_each_adapter(|label, ctx| check_patches(label, ctx, PixelLayout::Nv12, color, 8));
}

#[test]
fn nv12_limited_bt601_color_patches() {
    let color = ColorInfo {
        matrix: YuvMatrix::Bt601,
        range: ColorRange::Limited,
        transfer: TransferFunction::Sdr,
        wide_gamut: false,
    };
    for_each_adapter(|label, ctx| check_patches(label, ctx, PixelLayout::Nv12, color, 8));
}

#[test]
fn p010_limited_bt709_color_patches() {
    for_each_adapter(|label, ctx| {
        check_patches(label, ctx, PixelLayout::P010, ColorInfo::SDR_BT709, 10);
    });
}

#[test]
fn rgba_alpha_composites_over_background() {
    // Pixel 0 is fully transparent, pixel 1 is white at alpha 128 (about 50%).
    let data = [200, 100, 50, 0, 255, 255, 255, 128];
    for_each_adapter(|label, ctx| {
        let frame = InputFrame {
            layout: PixelLayout::Rgba8,
            width: 2,
            height: 1,
            data: &data,
            color: ColorInfo::SDR_BT709,
        };
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let mut params = params(2, 1);
        params.background = [10, 20, 30, 255];
        let pixels = compositor
            .render(&frame, None, &[], &params)
            .expect("render");

        assert_eq!(
            pixel(&pixels, 2, 0, 0),
            [10, 20, 30, 255],
            "{label}: alpha 0 must show the background exactly"
        );
        let alpha = 128.0 / 255.0;
        let expected =
            [10.0, 20.0, 30.0].map(|bg: f64| (255.0 * alpha + bg * (1.0 - alpha)).round() as u8);
        assert_rgb_near(
            pixel(&pixels, 2, 1, 0),
            expected,
            1,
            &format!("{label}: alpha 128 over the background"),
        );
    });
}

/// Asymmetric 4x2 RGBA image in which every texel has a distinct colour.
fn asymmetric_texel(x: u32, y: u32) -> [u8; 4] {
    [
        (40 + 50 * x) as u8,
        (40 + 100 * y) as u8,
        (30 + 20 * (x + 2 * y)) as u8,
        255,
    ]
}

/// Expected output of an orientation, derived by mapping every stored texel centre with
/// [`Orientation::stored_to_display`] onto the display grid.
fn expected_oriented(
    orientation: Orientation,
    width: u32,
    height: u32,
) -> (u32, u32, Vec<[u8; 4]>) {
    let (out_w, out_h) = if orientation.swaps_axes() {
        (height, width)
    } else {
        (width, height)
    };
    let mut expected = vec![[0u8; 4]; (out_w * out_h) as usize];
    for y in 0..height {
        for x in 0..width {
            let stored = Point {
                x: (f64::from(x) + 0.5) / f64::from(width),
                y: (f64::from(y) + 0.5) / f64::from(height),
            };
            let display = orientation.stored_to_display(stored);
            let ox = ((display.x * f64::from(out_w)).floor() as u32).min(out_w - 1);
            let oy = ((display.y * f64::from(out_h)).floor() as u32).min(out_h - 1);
            expected[(oy * out_w + ox) as usize] = asymmetric_texel(x, y);
        }
    }
    (out_w, out_h, expected)
}

#[test]
fn orientation_rotation_and_flips_map_corners() {
    let orientations = [
        ("rotate 90", Orientation::new(90, 0, false, false)),
        ("flip horizontal", Orientation::new(0, 0, true, false)),
        (
            "rotate 90 and flip horizontal",
            Orientation::new(90, 0, true, false),
        ),
        ("flip vertical", Orientation::new(0, 0, false, true)),
        (
            "rotate 270 and flip vertical",
            Orientation::new(270, 0, false, true),
        ),
    ];
    let (width, height) = (4u32, 2u32);
    let mut data = Vec::new();
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&asymmetric_texel(x, y));
        }
    }
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        for (name, orientation) in orientations {
            let frame = InputFrame {
                layout: PixelLayout::Rgba8,
                width,
                height,
                data: &data,
                color: ColorInfo::SDR_BT709,
            };
            let (out_w, out_h, expected) = expected_oriented(orientation, width, height);
            let mut params = params(out_w, out_h);
            params.orientation = orientation;
            let pixels = compositor
                .render(&frame, None, &[], &params)
                .expect("render");

            // Corner check first, so a failure names the corner.
            let corners = [
                (0, 0),
                (out_w - 1, 0),
                (0, out_h - 1),
                (out_w - 1, out_h - 1),
            ];
            for (x, y) in corners {
                let index = (y * out_w + x) as usize;
                assert_eq!(
                    pixel(&pixels, out_w, x, y),
                    expected[index],
                    "{label} {name}: corner ({x}, {y}) shows the wrong input corner"
                );
            }
            for y in 0..out_h {
                for x in 0..out_w {
                    let index = (y * out_w + x) as usize;
                    assert_rgb_near(
                        pixel(&pixels, out_w, x, y),
                        [expected[index][0], expected[index][1], expected[index][2]],
                        1,
                        &format!("{label} {name} at ({x}, {y})"),
                    );
                }
            }
        }
    });
}

#[test]
fn crop_selects_the_top_right_quadrant() {
    // 8x8 image with four solid quadrants.
    let (width, height) = (8u32, 8u32);
    let quadrant = |x: u32, y: u32| match (x < 4, y < 4) {
        (true, true) => [255, 0, 0, 255],
        (false, true) => [0, 255, 0, 255],
        (true, false) => [0, 0, 255, 255],
        (false, false) => [255, 255, 255, 255],
    };
    let mut data = Vec::new();
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&quadrant(x, y));
        }
    }
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let frame = InputFrame {
            layout: PixelLayout::Rgba8,
            width,
            height,
            data: &data,
            color: ColorInfo::SDR_BT709,
        };
        let mut params = params(4, 4);
        params.crop = NormRect {
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 0.5,
        };
        let pixels = compositor
            .render(&frame, None, &[], &params)
            .expect("render");
        for y in 0..4 {
            for x in 0..4 {
                assert_rgb_near(
                    pixel(&pixels, 4, x, y),
                    [0, 255, 0],
                    1,
                    &format!("{label}: top-right crop at ({x}, {y})"),
                );
            }
        }
    });
}

#[test]
fn crossfade_mixes_in_the_encoded_domain() {
    let black = [0u8, 0, 0, 255].repeat(16);
    let white = [255u8, 255, 255, 255].repeat(16);
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let a = InputFrame {
            layout: PixelLayout::Rgba8,
            width: 4,
            height: 4,
            data: &black,
            color: ColorInfo::SDR_BT709,
        };
        let b = InputFrame { data: &white, ..a };
        let pixels = compositor
            .render(&a, Some((&b, 0.25)), &[], &params(4, 4))
            .expect("render");
        // 0.75 * 0 + 0.25 * 255 = 63.75, which rounds to 64.
        for y in 0..4 {
            for x in 0..4 {
                assert_rgb_near(
                    pixel(&pixels, 4, x, y),
                    [64, 64, 64],
                    1,
                    &format!("{label}: crossfade at ({x}, {y})"),
                );
            }
        }
    });
}

#[test]
fn premultiplied_overlay_blends_at_its_destination() {
    let gray = [128u8, 128, 128, 255].repeat(64);
    // Opaque red, premultiplied (alpha 255 leaves the colour unchanged).
    let red = [255u8, 0, 0, 255].repeat(4);
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let frame = InputFrame {
            layout: PixelLayout::Rgba8,
            width: 8,
            height: 8,
            data: &gray,
            color: ColorInfo::SDR_BT709,
        };
        let overlay = Overlay {
            rgba_premultiplied: &red,
            width: 2,
            height: 2,
            dest: NormRect {
                x: 0.25,
                y: 0.25,
                w: 0.25,
                h: 0.25,
            },
            opacity: 0.5,
            id: 7,
        };
        let pixels = compositor
            .render(&frame, None, &[overlay], &params(8, 8))
            .expect("render");
        // 50% red over gray 128: (0.5 * 255 + 0.5 * 128, 0.5 * 128, 0.5 * 128) = (191.5, 64, 64).
        for y in 0..8 {
            for x in 0..8 {
                let inside = (2..4).contains(&x) && (2..4).contains(&y);
                if inside {
                    assert_rgb_near(
                        pixel(&pixels, 8, x, y),
                        [192, 64, 64],
                        2,
                        &format!("{label}: overlay at ({x}, {y})"),
                    );
                } else {
                    assert_rgb_near(
                        pixel(&pixels, 8, x, y),
                        [128, 128, 128],
                        1,
                        &format!("{label}: outside the overlay at ({x}, {y})"),
                    );
                }
            }
        }
    });
}

/// Checks a P010 HDR frame: the white code must tone map to a bright SDR value, and the black code to black.
fn check_hdr(
    label: &str,
    ctx: Arc<GpuContext>,
    transfer: TransferFunction,
    white_code: u32,
    black_code: u32,
) {
    let color = ColorInfo {
        matrix: YuvMatrix::Bt2020,
        range: ColorRange::Limited,
        transfer,
        wide_gamut: true,
    };
    let mut compositor = Compositor::new(ctx).expect("compositor");
    for (name, code, expected_bright) in [("white", white_code, true), ("black", black_code, false)]
    {
        let data = build_yuv(PixelLayout::P010, 8, 8, |_, _| [code, 512, 512]);
        let frame = InputFrame {
            layout: PixelLayout::P010,
            width: 8,
            height: 8,
            data: &data,
            color,
        };
        let pixels = compositor
            .render(&frame, None, &[], &params(8, 8))
            .expect("render");
        let [r, g, b, _] = pixel(&pixels, 8, 4, 4);
        if expected_bright {
            for value in [r, g, b] {
                assert!(
                    (240..=255).contains(&value),
                    "{label} {transfer:?} {name}: {value} is not a bright SDR value (got {r} {g} {b})"
                );
            }
        } else {
            assert!(
                [r, g, b] == [0, 0, 0],
                "{label} {transfer:?} {name}: expected exact black, got {r} {g} {b}"
            );
        }
    }
}

#[test]
fn pq_1000_nit_white_tone_maps_bright_and_black_stays_black() {
    // Limited-range 10-bit luma: white code = 64 + 876 * PQ(1000 nits); black code = 64. Code 0 is below
    // black and must also clamp to black.
    let white = (64.0 + 876.0 * pq_encode(1000.0)).round() as u32;
    for_each_adapter(|label, ctx| {
        check_hdr(label, Arc::clone(&ctx), TransferFunction::Pq, white, 64);
        check_hdr(label, ctx, TransferFunction::Pq, white, 0);
    });
}

#[test]
fn hlg_white_tone_maps_bright_and_black_stays_black() {
    // HLG reference white (signal 1.0) is limited-range code 940; black is 64.
    for_each_adapter(|label, ctx| {
        check_hdr(label, ctx, TransferFunction::Hlg, 940, 64);
    });
}

/// Renders one 8x8 P010 HDR frame of uniform 10-bit codes (Y, Cb, Cr) and returns its centre pixel.
fn hdr_center_pixel(
    compositor: &mut Compositor,
    transfer: TransferFunction,
    codes: [u32; 3],
) -> [u8; 4] {
    let color = ColorInfo {
        matrix: YuvMatrix::Bt2020,
        range: ColorRange::Limited,
        transfer,
        wide_gamut: true,
    };
    let data = build_yuv(PixelLayout::P010, 8, 8, |_, _| codes);
    let frame = InputFrame {
        layout: PixelLayout::P010,
        width: 8,
        height: 8,
        data: &data,
        color,
    };
    let pixels = compositor
        .render(&frame, None, &[], &params(8, 8))
        .expect("render HDR frame");
    pixel(&pixels, 8, 4, 4)
}

/// Asserts that a neutral pixel is near SDR white: each channel in `240..=255`, the channels within one step of
/// each other, and the pixel opaque.
fn assert_near_white(actual: [u8; 4], context: &str) {
    for value in actual.iter().take(3) {
        assert!(
            (240..=255).contains(value),
            "{context}: {value} is not near SDR white (got {actual:?})"
        );
    }
    let [r, g, b, a] = actual;
    assert!(
        r.abs_diff(g) <= 1 && g.abs_diff(b) <= 1,
        "{context}: a neutral input must stay neutral (got {actual:?})"
    );
    assert_eq!(a, 255, "{context}: the output must be opaque");
}

#[test]
fn pq_diffuse_white_renders_near_sdr_white() {
    // 203 nits is SDR reference white. After the rolloff it is linear 0.908, which sRGB encodes to about 244.
    let codes = encode_ycbcr(
        [pq_encode(203.0); 3],
        YuvMatrix::Bt2020,
        ColorRange::Limited,
        10,
    );
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let pixel = hdr_center_pixel(&mut compositor, TransferFunction::Pq, codes);
        assert_near_white(pixel, &format!("{label}: PQ 203-nit gray"));
    });
}

#[test]
fn hlg_diffuse_white_renders_near_sdr_white() {
    // HLG signal 0.75 is 203 nits on a 1000-nit display (scene light 0.265 through the gamma 1.2 OOTF).
    let codes = encode_ycbcr([0.75; 3], YuvMatrix::Bt2020, ColorRange::Limited, 10);
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let pixel = hdr_center_pixel(&mut compositor, TransferFunction::Hlg, codes);
        assert_near_white(pixel, &format!("{label}: HLG 0.75-signal gray (203 nits)"));
    });
}

#[test]
fn saturated_bt2020_primaries_clip_negative_light() {
    // A pure BT.2020 primary at 1000 nits has negative light in two BT.709 channels after the matrix. Those two
    // must clip to zero, and the primary must reach full SDR scale. Checked for PQ and for HLG at full signal.
    let primaries = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        for (transfer, peak) in [
            (TransferFunction::Pq, pq_encode(1000.0)),
            (TransferFunction::Hlg, 1.0),
        ] {
            for (index, primary) in primaries.iter().enumerate() {
                let rgb = primary.map(|value| value * peak);
                let codes = encode_ycbcr(rgb, YuvMatrix::Bt2020, ColorRange::Limited, 10);
                let pixel = hdr_center_pixel(&mut compositor, transfer, codes);
                for (channel, value) in pixel.iter().take(3).enumerate() {
                    if channel == index {
                        assert!(
                            *value >= 245,
                            "{label} {transfer:?} primary {index}: the primary reached only {value} (got {pixel:?})"
                        );
                    } else {
                        assert!(
                            *value <= 3,
                            "{label} {transfer:?} primary {index}: channel {channel} is {value}, expected black (got {pixel:?})"
                        );
                    }
                }
            }
        }
    });
}

#[test]
fn invalid_inputs_are_rejected() {
    let ctx = Arc::new(GpuContext::new(&AdapterChoice::Software).expect("software adapter"));
    let mut compositor = Compositor::new(ctx).expect("compositor");
    let valid = vec![0u8; 4 * 4 * 4];

    // Odd YUV dimensions are rejected.
    let odd = vec![0u8; 3 * 3 + 2 * 2];
    let frame = InputFrame {
        layout: PixelLayout::Nv12,
        width: 3,
        height: 3,
        data: &odd,
        color: ColorInfo::SDR_BT709,
    };
    assert!(matches!(
        compositor.render(&frame, None, &[], &params(4, 4)),
        Err(CompositorError::InvalidFrame(_))
    ));

    // Wrong data length is rejected.
    let short = vec![0u8; 10];
    let frame = InputFrame {
        layout: PixelLayout::Rgba8,
        width: 4,
        height: 4,
        data: &short,
        color: ColorInfo::SDR_BT709,
    };
    assert!(matches!(
        compositor.render(&frame, None, &[], &params(4, 4)),
        Err(CompositorError::InvalidFrame(_))
    ));

    // A zero-sized output is rejected.
    let frame = InputFrame {
        layout: PixelLayout::Rgba8,
        width: 4,
        height: 4,
        data: &valid,
        color: ColorInfo::SDR_BT709,
    };
    assert!(matches!(
        compositor.render(&frame, None, &[], &params(0, 4)),
        Err(CompositorError::InvalidOutput(_))
    ));

    // An overlay whose pixel count does not match its size is rejected.
    let bad_overlay_pixels = [0u8; 3];
    let overlay = Overlay {
        rgba_premultiplied: &bad_overlay_pixels,
        width: 1,
        height: 1,
        dest: NormRect::FULL,
        opacity: 1.0,
        id: 1,
    };
    assert!(matches!(
        compositor.render(&frame, None, &[overlay], &params(4, 4)),
        Err(CompositorError::InvalidOverlay(_))
    ));
}

#[test]
fn duplicate_visible_overlay_ids_are_rejected() {
    let gray = [128u8, 128, 128, 255].repeat(64);
    let patch = [255u8, 0, 0, 255].repeat(4);
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(Arc::clone(&ctx)).expect("compositor");
        let frame = InputFrame {
            layout: PixelLayout::Rgba8,
            width: 8,
            height: 8,
            data: &gray,
            color: ColorInfo::SDR_BT709,
        };
        let first = Overlay {
            rgba_premultiplied: &patch,
            width: 2,
            height: 2,
            dest: NormRect {
                x: 0.0,
                y: 0.0,
                w: 0.25,
                h: 0.25,
            },
            opacity: 1.0,
            id: 4,
        };
        let second = Overlay {
            dest: NormRect {
                x: 0.5,
                y: 0.5,
                w: 0.25,
                h: 0.25,
            },
            ..first
        };
        let result = compositor.render(&frame, None, &[first, second], &params(8, 8));
        assert!(
            matches!(result, Err(CompositorError::InvalidOverlay(_))),
            "{label}: two visible overlays with id 4 must be rejected"
        );

        // A zero-sized overlay is skipped before the check, so it may repeat an id.
        let empty = Overlay {
            rgba_premultiplied: &[],
            width: 0,
            height: 0,
            dest: NormRect::FULL,
            opacity: 1.0,
            id: 4,
        };
        let plain = compositor
            .render(&frame, None, &[], &params(8, 8))
            .expect("render without overlays");
        let with_empty = compositor
            .render(&frame, None, &[empty, empty], &params(8, 8))
            .expect("zero-sized overlays with a shared id are skipped");
        assert_eq!(
            with_empty, plain,
            "{label}: skipped overlays must not change the output"
        );

        // The rejected call left no state behind: one overlay renders as it does on a fresh compositor.
        let single = compositor
            .render(&frame, None, &[first], &params(8, 8))
            .expect("one visible overlay after a rejection");
        let fresh = render_fresh(&ctx, &frame, None, &[first], &params(8, 8));
        assert_eq!(
            single, fresh,
            "{label}: a rejected render must not corrupt later renders"
        );
    });
}

#[test]
fn crossfade_frames_must_match_size_and_layout() {
    let a_data = [0u8, 0, 0, 255].repeat(16);
    let big = [255u8, 255, 255, 255].repeat(64);
    let nv12 = vec![128u8; PixelLayout::Nv12.frame_bytes(4, 4)];
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(ctx).expect("compositor");
        let a = InputFrame {
            layout: PixelLayout::Rgba8,
            width: 4,
            height: 4,
            data: &a_data,
            color: ColorInfo::SDR_BT709,
        };
        let wrong_size = InputFrame {
            width: 8,
            height: 8,
            data: &big,
            ..a
        };
        let wrong_layout = InputFrame {
            layout: PixelLayout::Nv12,
            data: &nv12,
            ..a
        };
        for (name, b) in [("size", wrong_size), ("layout", wrong_layout)] {
            let result = compositor.render(&a, Some((&b, 0.5)), &[], &params(4, 4));
            assert!(
                matches!(result, Err(CompositorError::InvalidFrame(_))),
                "{label}: frame B with a different {name} must be rejected"
            );
        }
        let same = a;
        compositor
            .render(&a, Some((&same, 0.5)), &[], &params(4, 4))
            .expect("frame B with A's size and layout is accepted");
    });
}

/// NV12 content whose colour changes with `seed`, so two frames of one size never share pixels.
fn nv12_gradient(width: u32, height: u32, seed: u32) -> Vec<u8> {
    build_yuv(PixelLayout::Nv12, width, height, |x, y| {
        [
            16 + (x * 13 + y * 7 + seed * 31) % 220,
            108 + (x * 5 + seed * 11) % 40,
            108 + (y * 3 + seed * 23) % 40,
        ]
    })
}

/// Renders one frame on a new compositor, as the reference for comparisons.
fn render_fresh(
    ctx: &Arc<GpuContext>,
    a: &InputFrame<'_>,
    blend: Option<(&InputFrame<'_>, f32)>,
    overlays: &[Overlay<'_>],
    params: &CompositeParams,
) -> Vec<u8> {
    let mut compositor = Compositor::new(Arc::clone(ctx)).expect("fresh compositor");
    compositor
        .render(a, blend, overlays, params)
        .expect("fresh render")
}

#[test]
fn reused_compositor_matches_fresh_compositor() {
    // One compositor goes through a format change, a size change, a crossfade, a plain render, and overlays that
    // reuse a texture id and then use a new id. Each output must equal what a fresh compositor produces.
    let nv_a_data = nv12_gradient(8, 8, 1);
    let nv_b_data = nv12_gradient(8, 8, 2);
    let nv_c_data = nv12_gradient(8, 8, 3);
    let rgba_data: Vec<u8> = (0..4u32)
        .flat_map(|y| (0..4u32).flat_map(move |x| asymmetric_texel(x, y)))
        .collect();
    let overlay_a: Vec<u8> = (0..16u8)
        .flat_map(|i| {
            let alpha = 40 + 13 * i;
            [alpha, alpha / 2, 0, alpha]
        })
        .collect();
    let overlay_b: Vec<u8> = (0..16u8)
        .flat_map(|i| {
            let alpha = 40 + 13 * i;
            [0, alpha / 4, alpha, alpha]
        })
        .collect();
    let nv_a = InputFrame {
        layout: PixelLayout::Nv12,
        width: 8,
        height: 8,
        data: &nv_a_data,
        color: ColorInfo::SDR_BT709,
    };
    let nv_b = InputFrame {
        data: &nv_b_data,
        ..nv_a
    };
    let nv_c = InputFrame {
        data: &nv_c_data,
        ..nv_a
    };
    let rgba = InputFrame {
        layout: PixelLayout::Rgba8,
        width: 4,
        height: 4,
        data: &rgba_data,
        color: ColorInfo::SDR_BT709,
    };
    let overlay_1 = Overlay {
        rgba_premultiplied: &overlay_a,
        width: 4,
        height: 4,
        dest: NormRect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        },
        opacity: 0.75,
        id: 7,
    };
    let overlay_2 = Overlay {
        rgba_premultiplied: &overlay_b,
        id: 8,
        ..overlay_1
    };
    let p8 = params(8, 8);
    let p4 = params(4, 4);

    for_each_adapter(|label, ctx| {
        let mut reused = Compositor::new(Arc::clone(&ctx)).expect("compositor");

        let got = reused.render(&nv_a, None, &[], &p8).expect("NV12 8x8");
        assert_eq!(
            got,
            render_fresh(&ctx, &nv_a, None, &[], &p8),
            "{label}: NV12 8x8"
        );

        let got = reused.render(&rgba, None, &[], &p4).expect("RGBA 4x4");
        assert_eq!(
            got,
            render_fresh(&ctx, &rgba, None, &[], &p4),
            "{label}: RGBA 4x4"
        );

        let got = reused
            .render(&nv_b, Some((&nv_c, 0.5)), &[], &p8)
            .expect("crossfade 8x8");
        assert_eq!(
            got,
            render_fresh(&ctx, &nv_b, Some((&nv_c, 0.5)), &[], &p8),
            "{label}: crossfade"
        );

        let got = reused
            .render(&nv_c, None, &[], &p8)
            .expect("plain after blend");
        assert_eq!(
            got,
            render_fresh(&ctx, &nv_c, None, &[], &p8),
            "{label}: plain after blend"
        );

        let got = reused
            .render(&nv_c, None, &[overlay_1], &p8)
            .expect("overlay id 7");
        assert_eq!(
            got,
            render_fresh(&ctx, &nv_c, None, &[overlay_1], &p8),
            "{label}: overlay id 7, first upload"
        );

        // Same id and size: the cached texture is reused. render_into also replaces the dirty buffer.
        let mut out = vec![0xAB; 5];
        reused
            .render_into(&nv_c, None, &[overlay_1], &p8, &mut out)
            .expect("overlay id 7, cached");
        assert_eq!(
            out,
            render_fresh(&ctx, &nv_c, None, &[overlay_1], &p8),
            "{label}: overlay id 7, cached texture"
        );

        // A new id with new pixels uploads a new texture.
        let got = reused
            .render(&nv_c, None, &[overlay_2], &p8)
            .expect("overlay id 8");
        assert_eq!(
            got,
            render_fresh(&ctx, &nv_c, None, &[overlay_2], &p8),
            "{label}: overlay id 8 with new pixels"
        );
    });
}

/// Gradient NV12 frame, used by the benchmark.
fn gradient_nv12(width: u32, height: u32) -> Vec<u8> {
    build_yuv(PixelLayout::Nv12, width, height, |x, y| {
        [(x + y) % 256, 128, 128]
    })
}

/// Times render plus readback over 30 frames (after 3 warm-up frames) and prints ms per frame.
fn bench_case(ctx: &Arc<GpuContext>, label: &str, in_size: (u32, u32), out_size: (u32, u32)) {
    const FRAMES: u32 = 30;
    let (in_w, in_h) = in_size;
    let (out_w, out_h) = out_size;
    let data = gradient_nv12(in_w, in_h);
    let frame = InputFrame {
        layout: PixelLayout::Nv12,
        width: in_w,
        height: in_h,
        data: &data,
        color: ColorInfo::SDR_BT709,
    };
    let mut compositor = Compositor::new(Arc::clone(ctx)).expect("compositor");
    let params = params(out_w, out_h);
    for _ in 0..3 {
        compositor
            .render(&frame, None, &[], &params)
            .expect("warm-up render");
    }
    let start = Instant::now();
    for _ in 0..FRAMES {
        let pixels = compositor
            .render(&frame, None, &[], &params)
            .expect("render");
        std::hint::black_box(&pixels);
    }
    let per_frame = start.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES);
    println!(
        "BENCH {label} ({}): {in_w}x{in_h} NV12 -> {out_w}x{out_h} RGBA8 render+readback {per_frame:.2} ms/frame over {FRAMES} frames",
        ctx.desc.name
    );
}

#[test]
#[ignore = "timing benchmark; run with --ignored --nocapture in release"]
fn bench_composite_1080p_and_4k_preview() {
    for (label, ctx) in contexts() {
        bench_case(&ctx, label, (1920, 1080), (1920, 1080));
        bench_case(&ctx, label, (3840, 2160), (1280, 720));
        bench_case(&ctx, label, (1920, 1080), (480, 270));
    }
}

//! Colour grade tests on the software (WARP) adapter, and on the hardware adapter when one is present.
//!
//! Run with `cargo test -p loopcut-render --test grade`. The expected values come from CPU reference code in this file
//! (the sRGB transfer functions), so the shader is checked against an independent implementation.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use loopcut_core::project::{CurvePoint, Curves, Grade, HslAdjust};
use loopcut_core::{ColorInfo, NormRect, Orientation, PixelLayout};
use loopcut_render::compositor::{CompositeParams, Compositor, InputFrame};
use loopcut_render::gpu::{AdapterChoice, GpuContext};
use loopcut_render::grade::{GradeSetup, prepare_grade};
use loopcut_render::lut::{Lut, parse_cube};

/// Contexts to test: the software adapter always, plus the `Auto` adapter when it is hardware. One device per adapter,
/// shared by every test, as in the app (see the note in `tests/compositor.rs` about device creation races).
fn contexts() -> Vec<(&'static str, Arc<GpuContext>)> {
    static CONTEXTS: OnceLock<Vec<(&'static str, Arc<GpuContext>)>> = OnceLock::new();
    CONTEXTS
        .get_or_init(|| {
            let software = GpuContext::new(&AdapterChoice::Software)
                .expect("the software (WARP) adapter should open on Windows");
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

/// An opaque RGBA8 frame, `rgb(x, y)` giving each pixel.
fn rgba_frame(width: u32, height: u32, rgb: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b] = rgb(x, y);
            data.extend_from_slice(&[r, g, b, 255]);
        }
    }
    data
}

/// A frame of every colour axis at once: red along x, green along y and blue along the diagonal.
fn palette(width: u32, height: u32) -> Vec<u8> {
    rgba_frame(width, height, |x, y| {
        [
            (x * 255 / (width - 1)) as u8,
            (y * 255 / (height - 1)) as u8,
            ((x + y) * 255 / (width + height - 2)) as u8,
        ]
    })
}

/// RGB of the pixel at (`x`, `y`) of a tightly packed RGBA8 output.
fn rgb_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * width + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// Largest per-channel difference of two RGBA8 outputs, over every pixel.
fn max_difference(a: &[u8], b: &[u8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .flat_map(|(p, q)| (0..3).map(move |c| (i32::from(p[c]) - i32::from(q[c])).abs()))
        .max()
        .unwrap_or(0)
}

/// Renders `data` once through a fresh compositor with `setup` applied.
fn render_with(
    ctx: &Arc<GpuContext>,
    setup: Option<&GradeSetup>,
    width: u32,
    height: u32,
    data: &[u8],
) -> Vec<u8> {
    let mut compositor = Compositor::new(Arc::clone(ctx)).expect("compositor");
    compositor.set_grade(setup).expect("set_grade");
    render_plain(&mut compositor, width, height, data)
}

fn render_plain(compositor: &mut Compositor, width: u32, height: u32, data: &[u8]) -> Vec<u8> {
    let frame = InputFrame {
        layout: PixelLayout::Rgba8,
        width,
        height,
        data,
        color: ColorInfo::SDR_BT709,
    };
    compositor
        .render(&frame, None, &[], &params(width, height))
        .expect("render")
}

/// The sRGB EOTF, in f64, for the CPU reference.
fn srgb_decode(e: f64) -> f64 {
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB OETF, in f64, for the CPU reference.
fn srgb_encode(l: f64) -> f64 {
    let c = l.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// A `.cube` table of `size` points per axis, with `f` mapping grid coordinates in [0, 1] to output RGB.
fn cube_text(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let mut text = format!("LUT_3D_SIZE {size}\n");
    let scale = (size - 1) as f32;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let rgb = f([r as f32 / scale, g as f32 / scale, b as f32 / scale]);
                writeln!(text, "{:.7} {:.7} {:.7}", rgb[0], rgb[1], rgb[2])
                    .expect("write to string");
            }
        }
    }
    text
}

fn identity_cube(size: usize) -> Lut {
    parse_cube(&cube_text(size, |p| p)).expect("identity cube")
}

fn inverting_cube(size: usize) -> Lut {
    parse_cube(&cube_text(size, |p| p.map(|c| 1.0 - c))).expect("inverting cube")
}

fn grade_with_lut(strength: f32) -> Grade {
    Grade {
        lut_strength: strength,
        ..Grade::default()
    }
}

#[test]
fn identity_grade_is_a_no_op() {
    assert!(prepare_grade(&Grade::default(), None).is_identity());
    let disabled = Grade {
        enabled: false,
        exposure: 2.0,
        saturation: -1.0,
        ..Grade::default()
    };
    assert!(prepare_grade(&disabled, None).is_identity());
    let identity = prepare_grade(&Grade::default(), None);

    let (width, height) = (64, 48);
    let data = palette(width, height);
    for_each_adapter(|label, ctx| {
        let baseline = render_with(&ctx, None, width, height, &data);
        let graded = render_with(&ctx, Some(&identity), width, height, &data);
        let diff = max_difference(&baseline, &graded);
        assert!(
            diff <= 1,
            "{label}: identity grade changed pixels by {diff} levels"
        );

        // Turning a grade off again restores the ungraded output exactly.
        let mut compositor = Compositor::new(Arc::clone(&ctx)).expect("compositor");
        let exposed = Grade {
            exposure: 1.0,
            ..Grade::default()
        };
        compositor
            .set_grade(Some(&prepare_grade(&exposed, None)))
            .expect("set_grade");
        let _ = render_plain(&mut compositor, width, height, &data);
        compositor.set_grade(None).expect("set_grade none");
        assert_eq!(
            render_plain(&mut compositor, width, height, &data),
            baseline,
            "{label}"
        );
    });
}

#[test]
fn exposure_plus_one_doubles_linear_light() {
    let (width, height) = (16, 16);
    for level in [40_u8, 128, 200] {
        let data = rgba_frame(width, height, |_, _| [level; 3]);
        let linear = srgb_decode(f64::from(level) / 255.0);
        let plus_one = prepare_grade(
            &Grade {
                exposure: 1.0,
                ..Grade::default()
            },
            None,
        );
        let minus_two = prepare_grade(
            &Grade {
                exposure: -2.0,
                ..Grade::default()
            },
            None,
        );
        for_each_adapter(|label, ctx| {
            let up = rgb_at(
                &render_with(&ctx, Some(&plus_one), width, height, &data),
                width,
                8,
                8,
            );
            let expected_up = 255.0 * srgb_encode(2.0 * linear);
            assert!(
                (f64::from(up[0]) - expected_up).abs() <= 1.5,
                "{label}: level {level} +1 stop gave {up:?}, expected {expected_up:.2}"
            );
            let down = rgb_at(
                &render_with(&ctx, Some(&minus_two), width, height, &data),
                width,
                8,
                8,
            );
            let expected_down = 255.0 * srgb_encode(0.25 * linear);
            assert!(
                (f64::from(down[0]) - expected_down).abs() <= 1.5,
                "{label}: level {level} -2 stops gave {down:?}, expected {expected_down:.2}"
            );
        });
    }
}

#[test]
fn temperature_warms_grey_and_tint_moves_green() {
    let (width, height) = (16, 16);
    let data = rgba_frame(width, height, |_, _| [128; 3]);
    let warm = prepare_grade(
        &Grade {
            temperature: 1.0,
            ..Grade::default()
        },
        None,
    );
    let cool = prepare_grade(
        &Grade {
            temperature: -1.0,
            ..Grade::default()
        },
        None,
    );
    let magenta = prepare_grade(
        &Grade {
            tint: 1.0,
            ..Grade::default()
        },
        None,
    );
    let green = prepare_grade(
        &Grade {
            tint: -1.0,
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let neutral = rgb_at(&render_with(&ctx, None, width, height, &data), width, 8, 8);
        let w = rgb_at(
            &render_with(&ctx, Some(&warm), width, height, &data),
            width,
            8,
            8,
        );
        let c = rgb_at(
            &render_with(&ctx, Some(&cool), width, height, &data),
            width,
            8,
            8,
        );
        assert!(
            i32::from(w[0]) - i32::from(w[2]) >= 15,
            "{label}: warm grey {w:?}"
        );
        assert!(
            i32::from(c[2]) - i32::from(c[0]) >= 15,
            "{label}: cool grey {c:?}"
        );
        let m = rgb_at(
            &render_with(&ctx, Some(&magenta), width, height, &data),
            width,
            8,
            8,
        );
        let g = rgb_at(
            &render_with(&ctx, Some(&green), width, height, &data),
            width,
            8,
            8,
        );
        // Magenta lowers green and raises red and blue; green does the reverse.
        assert!(
            m[1] < neutral[1] && m[0] > neutral[0] && m[2] > neutral[2],
            "{label}: magenta {m:?} vs {neutral:?}"
        );
        assert!(
            g[1] > neutral[1] && g[0] < neutral[0] && g[2] < neutral[2],
            "{label}: green {g:?} vs {neutral:?}"
        );
    });
}

#[test]
fn saturation_minus_one_gives_grey() {
    let (width, height) = (32, 32);
    let data = palette(width, height);
    let grey = prepare_grade(
        &Grade {
            saturation: -1.0,
            ..Grade::default()
        },
        None,
    );
    let vivid = prepare_grade(
        &Grade {
            saturation: 1.0,
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let out = render_with(&ctx, Some(&grey), width, height, &data);
        for y in (0..height).step_by(5) {
            for x in (0..width).step_by(5) {
                let [r, g, b] = rgb_at(&out, width, x, y);
                let spread = r.max(g).max(b) - r.min(g).min(b);
                assert!(spread <= 2, "{label}: ({x},{y}) is not grey: {r} {g} {b}");
            }
        }
        // Saturation +1 pushes a muted colour apart.
        let muted = rgba_frame(4, 4, |_, _| [200, 150, 130]);
        let before = rgb_at(&render_with(&ctx, None, 4, 4, &muted), 4, 1, 1);
        let after = rgb_at(&render_with(&ctx, Some(&vivid), 4, 4, &muted), 4, 1, 1);
        let spread = |c: [u8; 3]| {
            i32::from(c.iter().max().copied().unwrap_or(0))
                - i32::from(c.iter().min().copied().unwrap_or(0))
        };
        assert!(
            spread(after) > spread(before) + 20,
            "{label}: {before:?} -> {after:?}"
        );
    });
}

#[test]
fn curve_point_lifts_mid_grey() {
    let (width, height) = (16, 16);
    let data = rgba_frame(width, height, |_, _| [128; 3]);
    let master = prepare_grade(
        &Grade {
            curves: Curves {
                master: vec![CurvePoint { x: 0.5, y: 0.75 }],
                ..Curves::default()
            },
            ..Grade::default()
        },
        None,
    );
    let red_only = prepare_grade(
        &Grade {
            curves: Curves {
                red: vec![CurvePoint { x: 0.5, y: 0.25 }],
                ..Curves::default()
            },
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let lifted = rgb_at(
            &render_with(&ctx, Some(&master), width, height, &data),
            width,
            8,
            8,
        );
        let expected = 0.75 * 255.0;
        assert!(
            (f64::from(lifted[0]) - expected).abs() <= 2.5
                && (f64::from(lifted[2]) - expected).abs() <= 2.5,
            "{label}: mid grey through the master curve gave {lifted:?}, expected about {expected}"
        );
        let split = rgb_at(
            &render_with(&ctx, Some(&red_only), width, height, &data),
            width,
            8,
            8,
        );
        assert!(
            (i32::from(split[0]) - 64).abs() <= 2,
            "{label}: red {split:?}"
        );
        assert!(
            (i32::from(split[1]) - 128).abs() <= 1,
            "{label}: green {split:?}"
        );
        assert!(
            (i32::from(split[2]) - 128).abs() <= 1,
            "{label}: blue {split:?}"
        );
    });
}

#[test]
fn red_hue_band_turns_red_toward_orange_and_leaves_blue() {
    let mut hsl = [HslAdjust::default(); 8];
    hsl[0] = HslAdjust {
        hue: 30.0,
        saturation: 0.0,
        luminance: 0.0,
    };
    let shifted = prepare_grade(
        &Grade {
            hsl,
            ..Grade::default()
        },
        None,
    );
    let data = rgba_frame(3, 1, |x, _| match x {
        0 => [255, 0, 0],
        1 => [0, 0, 255],
        _ => [128, 128, 128],
    });
    for_each_adapter(|label, ctx| {
        let out = render_with(&ctx, Some(&shifted), 3, 1, &data);
        let red = rgb_at(&out, 3, 0, 0);
        assert!(red[0] >= 250, "{label}: red {red:?}");
        assert!(
            (i32::from(red[1]) - 128).abs() <= 8,
            "{label}: red should move to orange: {red:?}"
        );
        assert!(red[2] <= 4, "{label}: red {red:?}");
        assert_eq!(
            rgb_at(&out, 3, 1, 0)[..],
            [0, 0, 255][..],
            "{label}: blue must not move"
        );
        let grey = rgb_at(&out, 3, 2, 0);
        assert!(
            grey.iter().all(|&c| (i32::from(c) - 128).abs() <= 1),
            "{label}: grey {grey:?}"
        );
    });
}

#[test]
fn identity_cube_is_a_no_op() {
    let identity = identity_cube(17);
    let setup = prepare_grade(&grade_with_lut(1.0), Some(&identity));
    assert!(!setup.is_identity(), "a 3D LUT is a real grade stage");
    let (width, height) = (64, 48);
    let data = palette(width, height);
    for_each_adapter(|label, ctx| {
        let baseline = render_with(&ctx, None, width, height, &data);
        let graded = render_with(&ctx, Some(&setup), width, height, &data);
        let diff = max_difference(&baseline, &graded);
        assert!(
            diff <= 1,
            "{label}: identity LUT changed pixels by {diff} levels"
        );
    });
}

#[test]
fn inverting_cube_inverts() {
    let inverting = inverting_cube(9);
    let setup = prepare_grade(&grade_with_lut(1.0), Some(&inverting));
    let (width, height) = (64, 48);
    let data = palette(width, height);
    for_each_adapter(|label, ctx| {
        let baseline = render_with(&ctx, None, width, height, &data);
        let graded = render_with(&ctx, Some(&setup), width, height, &data);
        for (src, out) in baseline
            .as_chunks::<4>()
            .0
            .iter()
            .zip(graded.as_chunks::<4>().0.iter())
        {
            for c in 0..3 {
                let diff = (i32::from(src[c]) + i32::from(out[c]) - 255).abs();
                assert!(diff <= 2, "{label}: {src:?} -> {out:?}");
            }
        }
    });
}

#[test]
fn lut_strength_half_is_halfway_to_the_lut() {
    let inverting = inverting_cube(9);
    let half = prepare_grade(&grade_with_lut(0.5), Some(&inverting));
    let data = rgba_frame(8, 8, |x, _| [(x * 36) as u8, 0, 0]);
    for_each_adapter(|label, ctx| {
        let baseline = render_with(&ctx, None, 8, 8, &data);
        let out = render_with(&ctx, Some(&half), 8, 8, &data);
        for (src, mixed) in baseline
            .as_chunks::<4>()
            .0
            .iter()
            .zip(out.as_chunks::<4>().0.iter())
        {
            let expected = (f64::from(src[0]) + (255.0 - f64::from(src[0]))) / 2.0;
            assert!(
                (f64::from(mixed[0]) - expected).abs() <= 1.5,
                "{label}: {} -> {} expected {expected}",
                src[0],
                mixed[0]
            );
        }
    });
}

#[test]
fn one_dimensional_lut_inverts_and_identity_is_no_op() {
    let inverting = parse_cube(
        &(0..256).fold(String::from("LUT_1D_SIZE 256\n"), |mut text, i| {
            let v = 1.0 - i as f32 / 255.0;
            writeln!(text, "{v:.7} {v:.7} {v:.7}").expect("write to string");
            text
        }),
    )
    .expect("1D inverting");
    let inverted = prepare_grade(&grade_with_lut(1.0), Some(&inverting));
    let identity = parse_cube(
        &(0..64).fold(String::from("LUT_1D_SIZE 64\n"), |mut text, i| {
            let v = i as f32 / 63.0;
            writeln!(text, "{v:.7} {v:.7} {v:.7}").expect("write to string");
            text
        }),
    )
    .expect("1D identity");
    let identity_setup = prepare_grade(&grade_with_lut(1.0), Some(&identity));
    let (width, height) = (64, 48);
    let data = palette(width, height);
    for_each_adapter(|label, ctx| {
        let baseline = render_with(&ctx, None, width, height, &data);
        let out = render_with(&ctx, Some(&inverted), width, height, &data);
        for (src, o) in baseline
            .as_chunks::<4>()
            .0
            .iter()
            .zip(out.as_chunks::<4>().0.iter())
        {
            for c in 0..3 {
                assert!(
                    (i32::from(src[c]) + i32::from(o[c]) - 255).abs() <= 2,
                    "{label}: {src:?} -> {o:?}"
                );
            }
        }
        let same = render_with(&ctx, Some(&identity_setup), width, height, &data);
        assert!(
            max_difference(&baseline, &same) <= 1,
            "{label}: identity 1D LUT"
        );
    });
}

#[test]
fn vignette_darkens_corners_and_spares_the_centre() {
    let (width, height) = (128, 72);
    let data = rgba_frame(width, height, |_, _| [200; 3]);
    let vignette = prepare_grade(
        &Grade {
            vignette: 1.0,
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let out = render_with(&ctx, Some(&vignette), width, height, &data);
        let centre = rgb_at(&out, width, width / 2, height / 2);
        assert!(
            (i32::from(centre[0]) - 200).abs() <= 1,
            "{label}: centre {centre:?}"
        );
        let quarter = rgb_at(&out, width, width / 4, height / 2);
        assert!(
            (i32::from(quarter[0]) - 200).abs() <= 1,
            "{label}: quarter point {quarter:?}"
        );
        for (x, y) in [
            (0, 0),
            (width - 1, 0),
            (0, height - 1),
            (width - 1, height - 1),
        ] {
            let corner = rgb_at(&out, width, x, y);
            assert!(corner[0] <= 70, "{label}: corner ({x},{y}) is {corner:?}");
        }
    });
}

#[test]
fn sharpen_increases_edge_contrast() {
    let (width, height) = (64, 16);
    let data = rgba_frame(
        width,
        height,
        |x, _| if x < 32 { [100; 3] } else { [150; 3] },
    );
    let sharpen = prepare_grade(
        &Grade {
            sharpen: 1.0,
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let out = render_with(&ctx, Some(&sharpen), width, height, &data);
        let dark = rgb_at(&out, width, 31, 8)[0];
        let bright = rgb_at(&out, width, 32, 8)[0];
        assert!(
            dark < 95,
            "{label}: the dark side of the edge should darken, got {dark}"
        );
        assert!(
            bright > 155,
            "{label}: the bright side of the edge should lighten, got {bright}"
        );
        // Flat areas are untouched.
        assert!(
            (i32::from(rgb_at(&out, width, 4, 8)[0]) - 100).abs() <= 1,
            "{label}: flat dark"
        );
        assert!(
            (i32::from(rgb_at(&out, width, 60, 8)[0]) - 150).abs() <= 1,
            "{label}: flat bright"
        );
    });
}

#[test]
fn contrast_steepens_midtones_and_shadows_and_highlights_act_at_the_ends() {
    let (width, height) = (8, 8);
    let dark = rgba_frame(width, height, |_, _| [64; 3]);
    let light = rgba_frame(width, height, |_, _| [192; 3]);
    let black = rgba_frame(width, height, |_, _| [0; 3]);
    let white = rgba_frame(width, height, |_, _| [255; 3]);
    let contrast_up = prepare_grade(
        &Grade {
            contrast: 0.5,
            ..Grade::default()
        },
        None,
    );
    let contrast_down = prepare_grade(
        &Grade {
            contrast: -0.5,
            ..Grade::default()
        },
        None,
    );
    let shadows = prepare_grade(
        &Grade {
            shadows: 1.0,
            ..Grade::default()
        },
        None,
    );
    let highlights = prepare_grade(
        &Grade {
            highlights: -1.0,
            ..Grade::default()
        },
        None,
    );
    let gain = prepare_grade(
        &Grade {
            gain: [0.5; 3],
            ..Grade::default()
        },
        None,
    );
    let lift = prepare_grade(
        &Grade {
            lift: [0.5; 3],
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let px = |setup: Option<&GradeSetup>, data: &[u8]| {
            rgb_at(&render_with(&ctx, setup, width, height, data), width, 4, 4)[0]
        };
        assert!(
            px(Some(&contrast_up), &dark) < 64,
            "{label}: S-curve darkens shadows"
        );
        assert!(
            px(Some(&contrast_up), &light) > 192,
            "{label}: S-curve lightens highlights"
        );
        assert!(
            px(Some(&contrast_down), &dark) > 64,
            "{label}: less contrast lifts shadows"
        );
        assert!(
            px(Some(&shadows), &black) > 50,
            "{label}: shadows lift black"
        );
        assert!(
            px(Some(&highlights), &white) < 200,
            "{label}: negative highlights pull white down"
        );
        assert!(
            (i32::from(px(Some(&gain), &white)) - 128).abs() <= 2,
            "{label}: gain 0.5 on white"
        );
        assert!(
            (i32::from(px(Some(&lift), &black)) - 128).abs() <= 2,
            "{label}: lift 0.5 on black"
        );
    });
}

#[test]
fn preview_and_export_paths_grade_identically_and_the_grade_persists() {
    let (width, height) = (48, 32);
    let data = palette(width, height);
    let inverting = inverting_cube(9);
    let setup = prepare_grade(
        &Grade {
            exposure: 0.5,
            saturation: 0.3,
            vignette: 0.4,
            sharpen: 0.2,
            ..grade_with_lut(0.75)
        },
        Some(&inverting),
    );
    for_each_adapter(|label, ctx| {
        // Preview and export are two compositors on one device, given the same grade.
        let mut preview = Compositor::new(Arc::clone(&ctx)).expect("preview compositor");
        let mut export = Compositor::new(Arc::clone(&ctx)).expect("export compositor");
        preview.set_grade(Some(&setup)).expect("preview grade");
        export.set_grade(Some(&setup)).expect("export grade");

        let first = render_plain(&mut preview, width, height, &data);
        // The grade persists: a second render without set_grade is the same.
        assert_eq!(
            render_plain(&mut preview, width, height, &data),
            first,
            "{label}: grade persists"
        );

        // The export path reuses its output buffer through render_into.
        let frame = InputFrame {
            layout: PixelLayout::Rgba8,
            width,
            height,
            data: &data,
            color: ColorInfo::SDR_BT709,
        };
        let mut reused = Vec::new();
        export
            .render_into(&frame, None, &[], &params(width, height), &mut reused)
            .expect("export render_into");
        assert_eq!(reused, first, "{label}: export matches preview");

        // Changing the grade takes effect on the next render, and turning it off restores the ungraded output.
        preview.set_grade(None).expect("grade off");
        let ungraded = render_plain(&mut preview, width, height, &data);
        assert!(
            max_difference(&ungraded, &first) > 20,
            "{label}: the grade should change the picture"
        );
        assert_eq!(
            ungraded,
            render_with(&ctx, None, width, height, &data),
            "{label}: ungraded baseline"
        );

        // Turning the grade back on reuses the uploaded tables and gives the same picture.
        preview.set_grade(Some(&setup)).expect("grade on again");
        assert_eq!(
            render_plain(&mut preview, width, height, &data),
            first,
            "{label}: graded again"
        );
    });
}

#[test]
fn switching_between_curves_and_luts_keeps_each_graded_picture() {
    let (width, height) = (32, 16);
    let data = palette(width, height);
    let inverting = inverting_cube(9);
    let with_curve = prepare_grade(
        &Grade {
            curves: Curves {
                master: vec![CurvePoint { x: 0.5, y: 0.8 }],
                ..Curves::default()
            },
            ..Grade::default()
        },
        None,
    );
    let with_lut = prepare_grade(&grade_with_lut(1.0), Some(&inverting));
    let exposure_only = prepare_grade(
        &Grade {
            exposure: 0.5,
            ..Grade::default()
        },
        None,
    );
    for_each_adapter(|label, ctx| {
        let mut compositor = Compositor::new(Arc::clone(&ctx)).expect("compositor");
        let curve_a = {
            compositor.set_grade(Some(&with_curve)).expect("curve");
            render_plain(&mut compositor, width, height, &data)
        };
        let lut_a = {
            compositor.set_grade(Some(&with_lut)).expect("lut");
            render_plain(&mut compositor, width, height, &data)
        };
        compositor
            .set_grade(Some(&exposure_only))
            .expect("exposure");
        assert_eq!(
            render_plain(&mut compositor, width, height, &data),
            render_with(&ctx, Some(&exposure_only), width, height, &data),
            "{label}: exposure only"
        );
        compositor
            .set_grade(Some(&with_curve))
            .expect("curve again");
        assert_eq!(
            render_plain(&mut compositor, width, height, &data),
            curve_a,
            "{label}: curve table reused"
        );
        compositor.set_grade(Some(&with_lut)).expect("lut again");
        assert_eq!(
            render_plain(&mut compositor, width, height, &data),
            lut_a,
            "{label}: lut table reused"
        );
    });
}

#[test]
fn baked_tables_are_monotone_and_identity_curves_are_dropped() {
    let curved = prepare_grade(
        &Grade {
            curves: Curves {
                master: vec![
                    CurvePoint { x: 0.2, y: 0.1 },
                    CurvePoint { x: 0.5, y: 0.75 },
                    CurvePoint { x: 0.8, y: 0.85 },
                ],
                ..Curves::default()
            },
            ..Grade::default()
        },
        None,
    );
    let table = curved.curve_table().expect("a curve table");
    assert_eq!(table.len(), loopcut_render::grade::TABLE_SIZE);
    assert!(
        (table[511][0] - 0.75).abs() < 0.01,
        "entry at 0.5 is {}",
        table[511][0]
    );
    let mut previous = -1.0_f32;
    for entry in table {
        assert!(entry[0] >= previous - 1e-6, "not monotone");
        assert!((0.0..=1.0).contains(&entry[0]));
        previous = entry[0];
    }

    let identity_curve = prepare_grade(
        &Grade {
            curves: Curves {
                master: vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }],
                ..Curves::default()
            },
            ..Grade::default()
        },
        None,
    );
    assert!(
        identity_curve.curve_table().is_none(),
        "an identity curve uploads nothing"
    );
    assert!(identity_curve.is_identity());
}

#[test]
fn setup_keys_separate_uniform_changes_from_table_changes() {
    let inverting = inverting_cube(9);
    let base = prepare_grade(&grade_with_lut(1.0), Some(&inverting));
    let again = prepare_grade(&grade_with_lut(1.0), Some(&inverting));
    assert_eq!(base.key(), again.key(), "equal grades have equal keys");
    assert_eq!(base.tables_key(), again.tables_key());

    let nudged = prepare_grade(
        &Grade {
            exposure: 0.25,
            ..grade_with_lut(1.0)
        },
        Some(&inverting),
    );
    assert_ne!(base.key(), nudged.key(), "a new exposure changes the key");
    assert_eq!(
        base.tables_key(),
        nudged.tables_key(),
        "the 3D LUT is not re-uploaded for an exposure change"
    );

    let other_lut = prepare_grade(&grade_with_lut(1.0), Some(&identity_cube(9)));
    assert_ne!(
        base.tables_key(),
        other_lut.tables_key(),
        "a different LUT is a different table"
    );
}

#[test]
fn zero_strength_lut_and_oversize_checks() {
    let inverting = inverting_cube(9);
    let zero = prepare_grade(&grade_with_lut(0.0), Some(&inverting));
    assert!(zero.is_identity(), "a LUT at strength zero is not applied");
    assert!(zero.lut3d().is_none());
    let no_lut = prepare_grade(&Grade::default(), None);
    assert!(no_lut.is_identity());
    let applied = prepare_grade(&grade_with_lut(1.0), Some(&inverting));
    assert!(applied.lut3d().is_some());
    assert_eq!(applied.lut3d().map(Lut::size), Some(9));
}

/// Every stage that a grade can run, each set to a value that is numerically neutral but still nonzero, so the stage
/// flag is set and the stage executes. The output must match the ungraded picture.
fn every_stage_neutral() -> Grade {
    let mut hsl = [HslAdjust::default(); 8];
    for band in &mut hsl {
        band.hue = 1e-6;
        band.saturation = 1e-6;
        band.luminance = 1e-6;
    }
    Grade {
        exposure: 1e-6,
        contrast: 1e-6,
        saturation: 1e-6,
        // A curve whose spline sits 1e-4 above the diagonal at its middle: above the baking threshold, far below one level.
        curves: Curves {
            master: vec![CurvePoint { x: 0.5, y: 0.5001 }],
            ..Curves::default()
        },
        hsl,
        sharpen: 1e-6,
        vignette: 1e-6,
        ..Grade::default()
    }
}

#[test]
fn identity_with_every_stage_flag_set_is_numerically_neutral() {
    // Bits 0 to 8 are LINEAR, TONAL, CURVES, HSL, SATURATION, LUT3D, LUT1D, SHARPEN and VIGNETTE. A 3D and a 1D table
    // cannot both be set, so each case leaves out the other table kind.
    const ALL_BUT_LUT1D: u32 = 0x1BF;
    const ALL_BUT_LUT3D: u32 = 0x1DF;
    let (width, height) = (64, 48);
    let data = palette(width, height);
    let neutral_3d = prepare_grade(&every_stage_neutral(), Some(&identity_cube(9)));
    let neutral_1d = prepare_grade(
        &every_stage_neutral(),
        Some(
            &parse_cube(
                &(0..64).fold(String::from("LUT_1D_SIZE 64\n"), |mut text, i| {
                    let v = i as f32 / 63.0;
                    writeln!(text, "{v:.7} {v:.7} {v:.7}").expect("write to string");
                    text
                }),
            )
            .expect("1D identity"),
        ),
    );
    assert_eq!(neutral_3d.stage_flags(), ALL_BUT_LUT1D, "3D case flags");
    assert_eq!(neutral_1d.stage_flags(), ALL_BUT_LUT3D, "1D case flags");
    for_each_adapter(|label, ctx| {
        let baseline = render_with(&ctx, None, width, height, &data);
        for (case, setup) in [("3D", &neutral_3d), ("1D", &neutral_1d)] {
            let graded = render_with(&ctx, Some(setup), width, height, &data);
            let diff = max_difference(&baseline, &graded);
            assert!(
                diff <= 2,
                "{label} {case}: every stage at neutral changed pixels by {diff} levels"
            );
        }
    });
}

/// A non-affine, seeded 9-cube table: per-channel squares, a cross term in each output, and a seeded jitter of
/// about one part in a hundred. No affine map or per-channel curve reproduces it, so the test covers the full
/// 3D lookup.
fn seeded_nonaffine_cube(seed: u64) -> Lut {
    let size = 9_usize;
    let mut state = seed;
    let mut next_bits = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let scale = (size - 1) as f32;
    let mut text = format!("LUT_3D_SIZE {size}\n");
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let p = [r as f32 / scale, g as f32 / scale, b as f32 / scale];
                let jitter = |bits: u64| ((bits >> 40) as f32 / (1_u64 << 24) as f32 - 0.5) * 0.02;
                let rgb = [
                    0.6 * p[0] * p[0] + 0.3 * p[1] * p[2] + jitter(next_bits()),
                    0.5 * p[1] * p[1] + 0.25 * p[0] * p[2] + 0.2 + jitter(next_bits()),
                    0.5 * p[2] * p[2] + 0.2 * p[0] * p[1] + 0.3 + jitter(next_bits()),
                ];
                let [r_out, g_out, b_out] = rgb.map(|v| v.clamp(0.0, 1.0));
                writeln!(text, "{r_out:.7} {g_out:.7} {b_out:.7}").expect("write to string");
            }
        }
    }
    parse_cube(&text).expect("seeded cube")
}

#[test]
fn seeded_nonaffine_nine_cube_matches_the_cpu_lookup_within_two_levels() {
    let lut = seeded_nonaffine_cube(0x5EED_CAFE);
    let setup = prepare_grade(&grade_with_lut(1.0), Some(&lut));
    let (width, height) = (64, 64);
    // Seeded input pixels, so every channel takes values all over its range.
    let mut state = 0xC0FF_EE00_u64;
    let mut next_byte = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 56) as u8
    };
    let pixels: Vec<[u8; 3]> = (0..width * height)
        .map(|_| [next_byte(), next_byte(), next_byte()])
        .collect();
    let data = rgba_frame(width, height, |x, y| pixels[(y * width + x) as usize]);
    for_each_adapter(|label, ctx| {
        let out = render_with(&ctx, Some(&setup), width, height, &data);
        let mut worst = 0.0_f64;
        for (index, pixel) in pixels.iter().enumerate() {
            let input = pixel.map(|c| f32::from(c) / 255.0);
            let expected = lut.sample(input);
            let got = &out[index * 4..index * 4 + 3];
            for c in 0..3 {
                let want = f64::from(expected[c]) * 255.0;
                let error = (f64::from(got[c]) - want).abs();
                worst = worst.max(error);
                assert!(
                    error <= 2.0,
                    "{label}: input {pixel:?} channel {c}: GPU {} vs CPU {want:.2}",
                    got[c]
                );
            }
        }
        println!(
            "{label}: worst difference {worst:.3} levels over {} pixels",
            pixels.len()
        );
    });
}

#[test]
fn grade_setup_debug_shows_stages_and_sizes_not_tables() {
    let lut = seeded_nonaffine_cube(3);
    let setup = prepare_grade(&grade_with_lut(1.0), Some(&lut));
    let text = format!("{setup:?}");
    assert!(text.contains("lut3d"), "{text}");
    assert!(text.contains("stages"), "{text}");
    assert!(text.len() < 500, "debug output is {} bytes", text.len());
}

//! Caption layers through the public API: visibility, animation, caching, scaling and fallbacks.
//!
//! Each test builds its own [`CaptionRenderer`] (creating one scans the installed fonts), and uses the bundled
//! Anton face where the glyph shapes do not matter, so the results do not depend on the machine's fonts.

use std::sync::Arc;

use loopcut_core::project::{Caption, CaptionAnimation, CaptionStyle, SlideEdge};
use loopcut_core::time::TimeRange;
use loopcut_render::caption_layers::{CaptionRenderer, OwnedOverlay};

fn caption(id: &str, text: &str, start: f64, end: f64, animation: CaptionAnimation) -> Caption {
    Caption {
        id: id.to_owned(),
        text: text.to_owned(),
        range: TimeRange::new(start, end),
        x: 0.5,
        y: 0.5,
        style: CaptionStyle {
            font_family: "Anton".to_owned(),
            ..CaptionStyle::default()
        },
        animation,
    }
}

/// Layers for one caption at `t` on a 480x270 output (a GIF-sized frame).
fn one_layer(renderer: &mut CaptionRenderer, caption: &Caption, t: f64) -> OwnedOverlay {
    let layers = renderer.layers_at(std::slice::from_ref(caption), t, 1.0, 480, 270);
    assert_eq!(layers.len(), 1, "expected one layer at t = {t}");
    layers.into_iter().next().expect("one layer")
}

fn assert_close(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not within {tolerance} of {expected}"
    );
}

#[test]
fn visible_only_inside_the_source_range() {
    let mut renderer = CaptionRenderer::new();
    let c = caption("v", "hi", 1.0, 2.0, CaptionAnimation::None);
    let at = |renderer: &mut CaptionRenderer, t: f64| {
        renderer
            .layers_at(std::slice::from_ref(&c), t, 1.0, 480, 270)
            .len()
    };
    assert_eq!(at(&mut renderer, 0.99), 0, "before the range");
    assert_eq!(at(&mut renderer, 1.0), 1, "the start is included");
    assert_eq!(at(&mut renderer, 1.999), 1, "inside the range");
    assert_eq!(at(&mut renderer, 2.0), 0, "the end is excluded");
}

#[test]
fn fade_opacity_at_start_middle_and_end() {
    let mut renderer = CaptionRenderer::new();
    let c = caption(
        "f",
        "fade",
        0.0,
        4.0,
        CaptionAnimation::Fade {
            fade_in: 0.5,
            fade_out: 0.5,
        },
    );
    // At the start the opacity is zero, so the caption is skipped rather than drawn invisibly.
    assert!(
        renderer
            .layers_at(std::slice::from_ref(&c), 0.0, 1.0, 480, 270)
            .is_empty(),
        "a fully transparent caption is not drawn"
    );
    assert_close(
        one_layer(&mut renderer, &c, 0.25).opacity,
        0.5,
        1e-4,
        "fading in",
    );
    assert_close(
        one_layer(&mut renderer, &c, 2.0).opacity,
        1.0,
        1e-4,
        "middle",
    );
    assert_close(
        one_layer(&mut renderer, &c, 3.75).opacity,
        0.5,
        1e-4,
        "fading out",
    );
    assert!(one_layer(&mut renderer, &c, 3.999).opacity < 0.01, "end");
}

#[test]
fn speed_stretches_animation_time() {
    let mut renderer = CaptionRenderer::new();
    let c = caption(
        "s",
        "fast",
        0.0,
        10.0,
        CaptionAnimation::Fade {
            fade_in: 1.0,
            fade_out: 0.0,
        },
    );
    // At speed 2, one output second is two source seconds: 1.0 source second is half the fade-in.
    let at_speed = renderer.layers_at(std::slice::from_ref(&c), 1.0, 2.0, 480, 270);
    assert_close(at_speed[0].opacity, 0.5, 1e-4, "speed 2 at 1 source second");
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let layers = renderer.layers_at(std::slice::from_ref(&c), 1.0, bad, 480, 270);
        assert_close(layers[0].opacity, 1.0, 1e-4, "invalid speed counts as 1");
    }
}

#[test]
fn pop_scales_the_destination_and_never_the_raster() {
    let mut renderer = CaptionRenderer::new();
    let c = caption(
        "p",
        "POP",
        0.0,
        10.0,
        CaptionAnimation::Pop { duration: 1.0 },
    );
    let settled = one_layer(&mut renderer, &c, 5.0);
    let start = one_layer(&mut renderer, &c, 0.0);
    let overshoot = one_layer(&mut renderer, &c, 0.7);
    let end = one_layer(&mut renderer, &c, 1.0);

    assert_close(
        (start.dest.w / settled.dest.w) as f32,
        0.6,
        0.01,
        "scale at start",
    );
    assert_close(
        (overshoot.dest.w / settled.dest.w) as f32,
        1.05,
        0.01,
        "overshoot",
    );
    assert_close((end.dest.w / settled.dest.w) as f32, 1.0, 0.01, "settled");
    assert_close(
        (start.dest.h / settled.dest.h) as f32,
        (start.dest.w / settled.dest.w) as f32,
        0.01,
        "height scales like width",
    );
    // The pixels are the same raster at every scale.
    assert!(Arc::ptr_eq(
        &start.rgba_premultiplied,
        &settled.rgba_premultiplied
    ));
    assert_eq!((start.width, start.height), (settled.width, settled.height));
}

#[test]
fn typewriter_reveals_one_more_character_per_step() {
    let mut renderer = CaptionRenderer::new();
    let c = caption(
        "t",
        "abcd",
        0.0,
        10.0,
        CaptionAnimation::Typewriter {
            chars_per_second: 2.0,
        },
    );
    assert!(
        renderer
            .layers_at(std::slice::from_ref(&c), 0.0, 1.0, 480, 270)
            .is_empty(),
        "no characters yet"
    );
    let times = [0.5, 1.0, 1.5, 2.0];
    let layers: Vec<OwnedOverlay> = times
        .iter()
        .map(|&t| one_layer(&mut renderer, &c, t))
        .collect();
    for pair in layers.windows(2) {
        assert!(
            pair[0].width < pair[1].width,
            "each character widens the raster: {} then {}",
            pair[0].width,
            pair[1].width
        );
        assert_ne!(pair[0].id, pair[1].id, "each prefix is its own texture");
    }
    // Past the end the full text stays, with the same texture.
    let done = one_layer(&mut renderer, &c, 2.5);
    assert_eq!(done.id, layers[3].id);
    assert!(Arc::ptr_eq(
        &done.rgba_premultiplied,
        &layers[3].rgba_premultiplied
    ));
}

#[test]
fn cache_reuses_the_same_id_and_pixels_across_frames() {
    let mut renderer = CaptionRenderer::new();
    let c = caption(
        "cache",
        "reuse",
        0.0,
        10.0,
        CaptionAnimation::Fade {
            fade_in: 1.0,
            fade_out: 0.0,
        },
    );
    let first = one_layer(&mut renderer, &c, 2.0);
    let second = one_layer(&mut renderer, &c, 3.0);
    assert_eq!(first.id, second.id, "same pixels, same texture id");
    assert!(Arc::ptr_eq(
        &first.rgba_premultiplied,
        &second.rgba_premultiplied
    ));
    assert_close(
        first.opacity,
        second.opacity,
        1e-6,
        "opacity is not part of the raster",
    );
}

#[test]
fn font_size_scales_with_the_output_height() {
    let mut renderer = CaptionRenderer::new();
    let c = caption("scale", "SCALE", 0.0, 10.0, CaptionAnimation::None);
    let small = renderer.layers_at(std::slice::from_ref(&c), 1.0, 1.0, 480, 270);
    let large = renderer.layers_at(std::slice::from_ref(&c), 1.0, 1.0, 960, 540);
    let (s, l) = (&small[0], &large[0]);
    // The stroke and font are proportional; only the fixed 2 px margin does not scale.
    assert!(
        (l.width as i64 - 2 * s.width as i64).abs() <= 6,
        "width {} at 2x output vs {} at 1x",
        l.width,
        s.width
    );
    assert!(
        (l.height as i64 - 2 * s.height as i64).abs() <= 6,
        "height {} at 2x output vs {} at 1x",
        l.height,
        s.height
    );
    // Normalized size stays the same at either output size.
    assert_close(
        (s.dest.h * 270.0) as f32,
        s.height as f32,
        0.5,
        "destination height matches the raster at 1x",
    );
    assert_close(
        (l.dest.h * 540.0) as f32,
        l.height as f32,
        0.5,
        "destination height matches the raster at 2x",
    );
}

#[test]
fn a_missing_impact_falls_back_to_bundled_anton_pixel_for_pixel() {
    let mut renderer = CaptionRenderer::with_extra_fonts(Vec::new());
    let impact_installed = renderer
        .font_families()
        .iter()
        .any(|f| f.eq_ignore_ascii_case("Impact"));
    let mut impact = caption("m", "IMPACT", 0.0, 10.0, CaptionAnimation::None);
    impact.style.font_family = "impact".to_owned();
    if impact_installed {
        // This machine has Impact, so it is used and not replaced.
        assert_eq!(renderer.resolved_family("impact"), "impact");
        return;
    }
    assert_eq!(renderer.resolved_family("impact"), "Anton");
    let mut anton = impact.clone();
    anton.style.font_family = "Anton".to_owned();

    let fallback = one_layer(&mut renderer, &impact, 1.0);
    let direct = one_layer(&mut renderer, &anton, 1.0);
    assert_eq!(
        (fallback.width, fallback.height),
        (direct.width, direct.height)
    );
    assert_eq!(
        fallback.rgba_premultiplied.as_slice(),
        direct.rgba_premultiplied.as_slice(),
        "the fallback is the Anton raster, pixel for pixel"
    );
}

#[test]
fn other_missing_families_render_with_the_family_they_resolve_to() {
    let mut renderer = CaptionRenderer::with_extra_fonts(Vec::new());
    let requested = "No Such Display Face";
    let resolved = renderer.resolved_family(requested);
    // Not Impact, so no Anton substitution: the name is one that cosmic-text matched, and it is listed or is the request.
    let families = renderer.font_families();
    assert!(
        families.contains(&resolved) || resolved == requested,
        "{resolved} is neither installed nor the request"
    );
    let mut missing = caption("o", "OTHER", 0.0, 10.0, CaptionAnimation::None);
    missing.style.font_family = requested.to_owned();
    let layer = one_layer(&mut renderer, &missing, 1.0);
    assert!(
        layer.width > 0 && layer.height > 0,
        "the caption still draws"
    );
}

#[test]
fn bundled_families_are_listed_sorted_and_unique() {
    let mut renderer = CaptionRenderer::new();
    let families = renderer.font_families();
    assert!(families.iter().any(|f| f == "Anton"), "Anton is bundled");
    assert!(families.iter().any(|f| f == "Inter"), "Inter is bundled");
    for pair in families.windows(2) {
        assert!(pair[0].to_lowercase() <= pair[1].to_lowercase());
        assert_ne!(pair[0].to_lowercase(), pair[1].to_lowercase());
    }
}

#[test]
fn identical_captions_in_one_frame_get_distinct_ids() {
    let mut renderer = CaptionRenderer::new();
    let a = caption("same", "twin", 0.0, 10.0, CaptionAnimation::None);
    let layers = renderer.layers_at(&[a.clone(), a], 1.0, 1.0, 480, 270);
    assert_eq!(layers.len(), 2);
    assert_ne!(layers[0].id, layers[1].id, "a render rejects repeated ids");
    // The overlays are valid for the compositor: the id is what it keys its textures by.
    assert_eq!(layers[0].as_overlay().id, layers[0].id);
    assert_eq!(
        layers[0].as_overlay().rgba_premultiplied.len(),
        (layers[0].width * layers[0].height * 4) as usize
    );
}

#[test]
fn slide_starts_off_the_frame_and_settles_inside() {
    let mut renderer = CaptionRenderer::new();
    let mut c = caption(
        "slide",
        "IN",
        0.0,
        10.0,
        CaptionAnimation::Slide {
            from: SlideEdge::Bottom,
            duration: 1.0,
        },
    );
    c.y = 0.5;
    let start = one_layer(&mut renderer, &c, 0.0);
    assert!(
        start.dest.y >= 1.0 - 1e-6,
        "starts below the frame: y = {}",
        start.dest.y
    );
    let mid = one_layer(&mut renderer, &c, 0.5);
    assert!(
        mid.dest.y < start.dest.y && mid.dest.y > 0.0,
        "moves up while sliding"
    );
    let rest = one_layer(&mut renderer, &c, 1.0);
    assert!(
        rest.dest.y >= 0.0 && rest.dest.y + rest.dest.h <= 1.0 + 1e-6,
        "rests inside"
    );
}

#[test]
fn captions_stay_inside_the_frame() {
    let mut renderer = CaptionRenderer::new();
    for (x, y) in [(0.0, 0.0), (1.0, 1.0), (0.02, 0.98)] {
        let mut c = caption("edge", "EDGE", 0.0, 10.0, CaptionAnimation::None);
        c.x = x;
        c.y = y;
        let layer = one_layer(&mut renderer, &c, 1.0);
        let d = layer.dest;
        assert!(
            d.x >= -1e-6 && d.y >= -1e-6,
            "({x}, {y}) moved past the top-left: {d:?}"
        );
        assert!(
            d.x + d.w <= 1.0 + 1e-6 && d.y + d.h <= 1.0 + 1e-6,
            "({x}, {y}) moved past the bottom-right: {d:?}"
        );
    }
}

#[test]
fn empty_outputs_and_bad_times_give_no_layers() {
    let mut renderer = CaptionRenderer::new();
    let c = caption("e", "text", 0.0, 10.0, CaptionAnimation::None);
    assert!(
        renderer
            .layers_at(std::slice::from_ref(&c), 1.0, 1.0, 0, 270)
            .is_empty()
    );
    assert!(
        renderer
            .layers_at(std::slice::from_ref(&c), 1.0, 1.0, 480, 0)
            .is_empty()
    );
    assert!(
        renderer
            .layers_at(std::slice::from_ref(&c), f64::NAN, 1.0, 480, 270)
            .is_empty()
    );
    let blank = caption("b", "   ", 0.0, 10.0, CaptionAnimation::None);
    assert!(
        renderer
            .layers_at(std::slice::from_ref(&blank), 1.0, 1.0, 480, 270)
            .is_empty()
    );
}

#[test]
fn inter_bold_uses_the_weight_axis_of_the_variable_font() {
    let mut renderer = CaptionRenderer::new();
    let mut regular = caption("w", "Weight", 0.0, 10.0, CaptionAnimation::None);
    regular.style.font_family = "Inter".to_owned();
    regular.style.bold = false;
    regular.style.stroke_width = 0.0;
    let mut bold = regular.clone();
    bold.style.bold = true;
    let thin = one_layer(&mut renderer, &regular, 1.0);
    let heavy = one_layer(&mut renderer, &bold, 1.0);
    assert!(
        heavy.width > thin.width,
        "bold {} px should be wider than regular {} px",
        heavy.width,
        thin.width
    );
}

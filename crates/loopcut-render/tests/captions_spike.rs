//! Spike f: meme-style captions rasterized with cosmic-text 0.19 + swash outlines + tiny-skia 0.12.
//!
//! Run with `cargo test -p loopcut-render --test captions_spike -- --nocapture` to see the
//! diagnostics (resolved fonts, emoji result, raster timings). PNGs land in `target/spike-out/`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use loopcut_render::captions::{
    CaptionRasterizer, RasterImage, Shadow, Stroke, TextAlign, TextStyle,
};
use tiny_skia::{IntSize, Pixmap};

/// `target/spike-out`, created on demand. Honors `CARGO_TARGET_DIR` when it is set.
fn out_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let dir = target.join("spike-out");
    std::fs::create_dir_all(&dir).expect("create target/spike-out");
    dir
}

/// Writes the premultiplied image as a PNG (tiny-skia un-premultiplies while encoding).
fn save_png(image: &RasterImage, path: &Path) {
    let size = IntSize::from_wh(image.width, image.height).expect("non-empty image");
    let pixmap = Pixmap::from_vec(image.data.clone(), size).expect("buffer matches size");
    std::fs::write(path, pixmap.encode_png().expect("encode png")).expect("write png");
}

fn meme_style(family: &str, size_px: f32) -> TextStyle {
    TextStyle {
        family: family.to_owned(),
        size_px,
        fill: [255, 255, 255, 255],
        stroke: Some(Stroke {
            width_px: 6.0,
            color: [0, 0, 0, 255],
        }),
        shadow: None,
        align: TextAlign::Center,
        line_height: 1.15,
        max_width_px: None,
        ..TextStyle::default()
    }
}

/// True when the pixel is clearly colored: alpha above 200 and not gray (R, G, B not all equal).
fn is_colored(px: &[u8; 4]) -> bool {
    px[3] > 200 && !(px[0] == px[1] && px[1] == px[2])
}

#[test]
fn meme_caption_fill_and_black_stroke() {
    let mut rasterizer = CaptionRasterizer::new();
    let text = "WHEN THE CODE COMPILES";

    // Impact is installed on Windows; fall back to Anton, then Arial, if the family does not resolve.
    let mut chosen = None;
    for candidate in ["Impact", "Anton", "Arial"] {
        let probe = rasterizer.rasterize(text, &meme_style(candidate, 64.0));
        if probe.is_empty() {
            continue;
        }
        let used = rasterizer.font_families_used();
        if used.iter().any(|name| name.eq_ignore_ascii_case(candidate)) {
            chosen = Some(candidate);
            break;
        }
    }
    let family = chosen.unwrap_or("Arial");
    println!(
        "resolved family: {family} (fonts used: {:?})",
        rasterizer.font_families_used()
    );

    let image = rasterizer.rasterize(text, &meme_style(family, 64.0));
    assert!(!image.is_empty(), "caption must rasterize");
    assert_eq!(image.data.len(), (image.width * image.height * 4) as usize);

    let opaque_white = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| **px == [255, 255, 255, 255])
        .count();
    let opaque_black = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| **px == [0, 0, 0, 255])
        .count();
    println!(
        "caption_meme: {}x{}, opaque white px = {opaque_white}, opaque black px = {opaque_black}",
        image.width, image.height
    );
    assert!(opaque_white > 0, "fill must produce opaque white pixels");
    assert!(opaque_black > 0, "stroke must produce opaque black pixels");

    save_png(&image, &out_dir().join("caption_meme.png"));
}

#[test]
fn color_emoji_spike() {
    let mut rasterizer = CaptionRasterizer::new();
    let style = meme_style("Impact", 64.0);
    let image = rasterizer.rasterize("Hello 😀🎉", &style);
    assert!(!image.is_empty(), "emoji caption must rasterize");

    let colored = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| is_colored(px))
        .count();
    println!(
        "caption_emoji: {}x{}, colored px (alpha>200, not gray) = {colored}, fonts used = {:?}",
        image.width,
        image.height,
        rasterizer.font_families_used()
    );
    // Spike result, not a hard requirement: a count of 0 means emoji fell back to the text font or
    // drew as monochrome outlines. Either way, the rest of the caption must still render.
    let opaque_white = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| **px == [255, 255, 255, 255])
        .count();
    assert!(opaque_white > 0, "text beside the emoji must still render");

    save_png(&image, &out_dir().join("caption_emoji.png"));
}

#[test]
fn multiline_wrap_centered_within_300px() {
    let mut rasterizer = CaptionRasterizer::new();
    let style = TextStyle {
        max_width_px: Some(300.0),
        align: TextAlign::Center,
        ..meme_style("Impact", 64.0)
    };
    let image = rasterizer.rasterize("WHEN THE CODE COMPILES, THEN WE SHIP IT", &style);
    assert!(!image.is_empty(), "wrapped caption must rasterize");

    // A single line of this text is far wider than 300 px; wrapping keeps the ink near the box width.
    // Padding (stroke plus margin) is at most about 10 px per side.
    assert!(
        image.width <= 300 + 24,
        "width {} exceeds the wrap box",
        image.width
    );
    // At least two lines: two line boxes of 64 px * 1.15 are already taller than one line.
    assert!(
        image.height as f32 > 2.0 * 64.0 * 1.15,
        "height {} shows one line only",
        image.height
    );
    println!("caption_wrap: {}x{}", image.width, image.height);

    save_png(&image, &out_dir().join("caption_wrap.png"));
}

#[test]
fn shadow_is_blurred_translucent_and_offset() {
    let mut rasterizer = CaptionRasterizer::new();
    let style = TextStyle {
        stroke: None,
        shadow: Some(Shadow {
            dx: 6.0,
            dy: 6.0,
            blur_px: 4.0,
            color: [0, 0, 0, 200],
        }),
        ..meme_style("Impact", 64.0)
    };
    let image = rasterizer.rasterize("SHADOW", &style);
    assert!(!image.is_empty());
    // Shadow pixels: black (premultiplied rgb 0) with partial alpha, which only the blur produces.
    let translucent_black = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] > 0 && px[3] < 200 && px[0] == 0 && px[1] == 0 && px[2] == 0)
        .count();
    let opaque_white = image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| **px == [255, 255, 255, 255])
        .count();
    println!(
        "caption_shadow: {}x{}, translucent black px = {translucent_black}",
        image.width, image.height
    );
    assert!(
        translucent_black > 100,
        "blurred shadow must produce partial alpha"
    );
    assert!(opaque_white > 0, "fill must sit on top of the shadow");
    save_png(&image, &out_dir().join("caption_shadow.png"));
}

#[test]
fn raster_time_two_line_64px_caption() {
    let mut rasterizer = CaptionRasterizer::new();
    let style = TextStyle {
        max_width_px: Some(400.0),
        ..meme_style("Impact", 64.0)
    };
    let unstroked = TextStyle {
        stroke: None,
        ..style.clone()
    };
    let text = "WHEN THE CODE COMPILES";
    let iterations = 20u32;

    // Warm up: the first call fills the glyph and outline caches.
    let warm = rasterizer.rasterize(text, &style);
    assert!(!warm.is_empty());

    let mut last = warm;
    let start = Instant::now();
    for _ in 0..iterations {
        last = rasterizer.rasterize(text, &style);
    }
    let stroked = start.elapsed() / iterations;

    let start = Instant::now();
    for _ in 0..iterations {
        rasterizer.rasterize(text, &unstroked);
    }
    let fill_only = start.elapsed() / iterations;

    // Layout only (cosmic-text shaping + line breaking, no outlines): shows where the time goes.
    let mut font_system = cosmic_text::FontSystem::new();
    let start = Instant::now();
    for _ in 0..iterations {
        let mut buffer = cosmic_text::Buffer::new(
            &mut font_system,
            cosmic_text::Metrics::new(64.0, 64.0 * 1.15),
        );
        buffer.set_size(Some(400.0), None);
        buffer.set_wrap(cosmic_text::Wrap::WordOrGlyph);
        let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name("Impact"));
        buffer.set_text(
            text,
            &attrs,
            cosmic_text::Shaping::Advanced,
            Some(cosmic_text::Align::Center),
        );
        buffer.shape_until_scroll(&mut font_system, false);
        std::hint::black_box(buffer.layout_runs().count());
    }
    let layout_only = start.elapsed() / iterations;

    println!(
        "raster time (warm, 64px, 2 lines, {}x{} px): {stroked:?} per call with 6px stroke, {fill_only:?} without stroke; cosmic-text layout alone {layout_only:?}",
        last.width, last.height
    );
    assert!(!last.is_empty());
}

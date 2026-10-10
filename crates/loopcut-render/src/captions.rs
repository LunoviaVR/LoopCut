//! Caption rasterizer: cosmic-text shaping and layout, glyph outlines filled and stroked with tiny-skia.
//!
//! [`CaptionRasterizer::rasterize`] runs five stages:
//!
//! 1. **Layout.** A cosmic-text [`Buffer`] shapes the text (harfrust), resolves fonts through fontdb
//!    (installed fonts plus any bundled data), wraps at `max_width_px` and aligns each line. The font size is
//!    clamped to [`MAX_FONT_PX`].
//! 2. **Glyph extraction.** Each laid-out glyph is either a *vector* glyph or a *bitmap* glyph.
//!    Vector outlines come from [`SwashCache::get_outline_commands`] (swash/skrifa: font units scaled to
//!    the pixel size, y axis pointing up) and are converted to a [`tiny_skia::Path`] in layout space
//!    (pixels, y axis pointing down). Color glyphs (COLR/CPAL outlines or CBDT/sbix bitmaps, i.e. emoji)
//!    have no single outline, so they are taken as premultiplied RGBA bitmaps from
//!    [`SwashCache::get_image`] and blitted at their glyph position.
//! 3. **Line boxes.** Each line with glyphs gives a box from its advances and its line height. Boxes are
//!    only used for [`Background`].
//! 4. **Shadow.** An offset silhouette of the glyphs and their stroke, box-blurred. A three-pass box blur
//!    approximates the Gaussian; `blur_px` follows the CSS convention (sigma = blur_px / 2).
//! 5. **Compositing.** Onto a premultiplied RGBA8 canvas in this order: background boxes, shadow, every
//!    outline stroke, every outline fill, then bitmap glyphs. The stroke is drawn at twice `width_px` and the
//!    fill covers its inner half, so exactly `width_px` of stroke shows outside the glyph edge. With a
//!    translucent fill the stroke goes into its own layer, the glyph interiors are cleared from that layer
//!    ([`BlendMode::Clear`]), and the layer is composited beneath the fill, so the stroke does not tint the fill.
//!
//! **Bitmap glyphs get no stroke.** A colour glyph (emoji) and a glyph with no outline (a bitmap-only mask) are
//! drawn as bitmaps. The stroke is a path, so these glyphs are not outlined. They still cast the shadow, which is
//! built from their bitmaps.
//!
//! Invalid style values are treated as absent instead of failing: a stroke whose width is not finite or not
//! positive, a shadow with a non-finite offset or blur or a negative blur, and a background whose padding or
//! radius is not finite or is negative (those become zero). A canvas over [`MAX_EDGE_PX`] on either edge or
//! [`MAX_CANVAS_PIXELS`] in total produces an empty image.
//!
//! The glyph caches of cosmic-text grow with every distinct glyph, size and subpixel position they see. Before each
//! rasterization the `SwashCache` is replaced by a fresh one once either of its glyph maps passes 4096 entries
//! (`SWASH_CACHE_LIMIT`). The cost is re-rasterizing the glyphs again, not a different result.

use cosmic_text::{
    Align, Attrs, Buffer, Command, Family, FontSystem, Metrics, PhysicalGlyph, Shaping, Style,
    SwashCache, SwashContent, SwashImage, Weight, Wrap,
};
use swash::scale::Source as SwashSource;
use tiny_skia::{
    BlendMode, FillRule, IntSize, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    Stroke as SkStroke, Transform,
};

/// Largest canvas edge in pixels that the rasterizer allocates. Larger captions produce an empty image.
pub const MAX_EDGE_PX: u32 = 16_384;

/// Largest canvas area in pixels (16 Mpx, so up to 64 MiB of premultiplied RGBA8 per canvas).
pub const MAX_CANVAS_PIXELS: u64 = 16 * 1024 * 1024;

/// Font sizes above this many pixels are clamped before layout.
pub const MAX_FONT_PX: f32 = 4096.0;

/// Layout coordinates beyond this magnitude are rejected before they are converted to integers.
const MAX_COORD_PX: f32 = 1.0e7;

/// Entries in either of cosmic-text's glyph maps (images or outlines) that trigger a fresh [`SwashCache`].
const SWASH_CACHE_LIMIT: usize = 4096;

/// Horizontal alignment of each line inside the caption box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    /// Lines start at the left edge.
    #[default]
    Left,
    /// Lines are centered on the box (or on the widest line when `max_width_px` is `None`).
    Center,
    /// Lines end at the right edge.
    Right,
}

impl From<TextAlign> for Align {
    fn from(align: TextAlign) -> Self {
        match align {
            TextAlign::Left => Align::Left,
            TextAlign::Center => Align::Center,
            TextAlign::Right => Align::Right,
        }
    }
}

/// Outline drawn around glyphs. `width_px` is the visible thickness outside the glyph edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    /// Visible stroke thickness outside the glyph edge, in pixels. Must be finite and positive.
    pub width_px: f32,
    /// Straight (non-premultiplied) RGBA.
    pub color: [u8; 4],
}

/// Offset copy of the text drawn beneath it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    /// Horizontal offset in pixels. Must be finite.
    pub dx: f32,
    /// Vertical offset in pixels (positive is down). Must be finite.
    pub dy: f32,
    /// CSS-style blur radius in pixels. Must be finite and not negative. Approximated with a three-pass box blur.
    pub blur_px: f32,
    /// Straight (non-premultiplied) RGBA.
    pub color: [u8; 4],
}

/// Rounded box drawn behind each line of text.
#[derive(Clone, Debug, PartialEq)]
pub struct Background {
    /// Straight (non-premultiplied) RGBA.
    pub color: [u8; 4],
    /// Distance from the line box to the edge of the drawn box, in pixels. Non-finite or negative means zero.
    pub padding_px: f32,
    /// Corner radius in pixels, limited to half the box size. Non-finite or negative means zero.
    pub radius_px: f32,
}

/// Style of one caption. Sizes are in output pixels, so rasterize at the export resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Family name, for example `"Impact"`. Unknown names fall back through cosmic-text's font matching.
    pub family: String,
    /// Font weight on the CSS scale (400 regular, 700 bold). Variable fonts use it on their weight axis.
    /// Clamped to 1..=1000.
    pub weight: u16,
    /// Use the italic face of the family when one exists.
    pub italic: bool,
    /// Font size in pixels. Clamped to [`MAX_FONT_PX`].
    pub size_px: f32,
    /// Fill color, straight RGBA.
    pub fill: [u8; 4],
    /// Optional outline around the glyphs.
    pub stroke: Option<Stroke>,
    /// Optional shadow beneath the glyphs.
    pub shadow: Option<Shadow>,
    /// Optional rounded box behind each line.
    pub background: Option<Background>,
    /// Alignment of each line.
    pub align: TextAlign,
    /// Line height as a multiple of `size_px`.
    pub line_height: f32,
    /// Wrap width in pixels. `None` lays the text out on one line per explicit line break.
    pub max_width_px: Option<f32>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: "Arial".to_owned(),
            weight: 400,
            italic: false,
            size_px: 48.0,
            fill: [255, 255, 255, 255],
            stroke: None,
            shadow: None,
            background: None,
            align: TextAlign::Center,
            line_height: 1.2,
            max_width_px: None,
        }
    }
}

/// A rasterized caption: `width * height` premultiplied RGBA8 pixels, row-major, no padding.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterImage {
    /// Width in pixels (0 for an empty caption).
    pub width: u32,
    /// Height in pixels (0 for an empty caption).
    pub height: u32,
    /// Premultiplied RGBA8 pixels, `width * height * 4` bytes.
    pub data: Vec<u8>,
    /// Horizontal position of the layout box origin (top-left of the text box) relative to the image's left
    /// edge, in pixels. Anchor on this to place text by its layout box instead of its ink. The image is cropped
    /// to the ink, so the origin can be negative or past the right edge when the ink does not reach the box.
    pub origin_x: f32,
    /// Vertical position of the layout box origin relative to the image's top edge, in pixels. Same caveat
    /// as [`RasterImage::origin_x`].
    pub origin_y: f32,
}

impl RasterImage {
    /// An image with no pixels, returned for empty text or invalid styles.
    fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            data: Vec::new(),
            origin_x: 0.0,
            origin_y: 0.0,
        }
    }

    /// True when the image has no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Owns the font database and glyph caches. Create one per application and reuse it, because
/// [`FontSystem::new`] scans every installed font.
pub struct CaptionRasterizer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Family names of the fonts used by the most recent [`CaptionRasterizer::rasterize`] call.
    last_families: Vec<String>,
}

impl Default for CaptionRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptionRasterizer {
    /// Creates a rasterizer that can use every installed system font.
    pub fn new() -> Self {
        Self::from_font_system(FontSystem::new())
    }

    /// Creates a rasterizer with the system fonts plus extra font files (TTF/OTF/TTC bytes), for
    /// example bundled OFL fonts such as Anton or Inter. The extra fonts are matched by their own names.
    ///
    /// Returns the rasterizer and the number of entries that fontdb accepted as fonts. Data that is not a
    /// readable font is skipped, so the count can be smaller than `fonts.len()`.
    pub fn with_extra_font_data(fonts: Vec<Vec<u8>>) -> (Self, usize) {
        let mut font_system = FontSystem::new();
        let mut accepted = 0;
        for data in fonts {
            let before = font_system.db().len();
            font_system.db_mut().load_font_data(data);
            if font_system.db().len() > before {
                accepted += 1;
            }
        }
        (Self::from_font_system(font_system), accepted)
    }

    fn from_font_system(font_system: FontSystem) -> Self {
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            last_families: Vec::new(),
        }
    }

    /// Every family name known to the font database (system plus loaded data), sorted case-insensitively.
    /// Names that differ only in case count as one family, and the first in sort order is kept.
    pub fn font_family_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .font_system
            .db()
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        names.sort_by(|a, b| {
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        });
        // Names that differ only in case are one family: the sort puts them next to each other, and the first is kept.
        names.dedup_by(|later, earlier| later.to_lowercase() == earlier.to_lowercase());
        names
    }

    /// True when a family with this name (compared case-insensitively) is in the font database.
    pub fn has_family(&self, name: &str) -> bool {
        self.font_system.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(family, _)| family.eq_ignore_ascii_case(name))
        })
    }

    /// Family names of the fonts that the most recent [`rasterize`](Self::rasterize) call used, in the
    /// order they first appeared. Use this to confirm that a requested family resolved (or fell back).
    pub fn font_families_used(&self) -> &[String] {
        &self.last_families
    }

    /// Lays out and rasterizes `text` in `style`.
    ///
    /// Line breaks in `text` start new lines.
    ///
    /// The result is cropped to the ink (glyphs, stroke, shadow and background) plus a small margin.
    /// [`RasterImage::origin_x`] and [`RasterImage::origin_y`] give the layout box origin inside it.
    /// Returns an empty image for empty text, invalid sizes, or a canvas over [`MAX_EDGE_PX`] on an edge or
    /// [`MAX_CANVAS_PIXELS`] in total.
    pub fn rasterize(&mut self, text: &str, style: &TextStyle) -> RasterImage {
        self.last_families.clear();
        if self.swash_cache.image_cache.len() > SWASH_CACHE_LIMIT
            || self.swash_cache.outline_command_cache.len() > SWASH_CACHE_LIMIT
        {
            self.swash_cache = SwashCache::new();
        }
        if text.is_empty()
            || !style.size_px.is_finite()
            || style.size_px <= 0.0
            || !style.line_height.is_finite()
            || style.line_height <= 0.0
        {
            return RasterImage::empty();
        }

        let size = style.size_px.min(MAX_FONT_PX);
        let line_height = (size * style.line_height).clamp(1.0, 4.0 * MAX_FONT_PX);
        let max_width = style.max_width_px.filter(|w| w.is_finite() && *w > 0.0);

        // Raw Buffer methods take the FontSystem as an argument, so the glyph loop below can borrow
        // the swash cache and the font system mutably while the buffer is still alive.
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        buffer.set_size(max_width, None);
        buffer.set_wrap(Wrap::WordOrGlyph);
        let attrs = Attrs::new()
            .family(Family::Name(style.family.as_str()))
            .weight(Weight(style.weight.clamp(1, 1000)))
            .style(if style.italic {
                Style::Italic
            } else {
                Style::Normal
            });
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(style.align.into()));
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut pieces = Vec::new();
        let mut boxes = Vec::new();
        let mut fonts = Vec::new();
        for run in buffer.layout_runs() {
            let mut span: Option<(f32, f32)> = None;
            for glyph in run.glyphs {
                if !fonts.contains(&glyph.font_id) {
                    fonts.push(glyph.font_id);
                }
                span = Some(match span {
                    None => (glyph.x, glyph.x + glyph.w),
                    Some((x0, x1)) => (x0.min(glyph.x), x1.max(glyph.x + glyph.w)),
                });
                // Same placement as cosmic-text's own draw path: baseline at `line_y`, scale 1.
                let physical = glyph.physical((0.0, run.line_y), 1.0);
                self.collect_glyph(&physical, style.fill, &mut pieces);
            }
            if let Some((x0, x1)) = span {
                boxes.push([x0, run.line_top, x1, run.line_top + run.line_height]);
            }
        }

        let db = self.font_system.db();
        self.last_families = fonts
            .iter()
            .filter_map(|id| db.face(*id))
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();

        compose(&pieces, &boxes, style).unwrap_or_else(RasterImage::empty)
    }

    /// Turns one physical glyph into a vector outline or a bitmap piece.
    fn collect_glyph(&mut self, glyph: &PhysicalGlyph, fill: [u8; 4], pieces: &mut Vec<Piece>) {
        let key = glyph.cache_key;

        // Outline in layout space: the outline is relative to the glyph origin with y up, and the
        // origin is the baseline position including the subpixel bin (x_bin/y_bin are quarter pixels).
        // It is built before the image is read, so the two borrows of the swash cache do not overlap. A colour
        // glyph may also have an outline (a base layer), which is then discarded below.
        let origin_x = glyph.x as f32 + key.x_bin.as_float();
        let origin_y = glyph.y as f32 + key.y_bin.as_float();
        let outline = self
            .swash_cache
            .get_outline_commands(&mut self.font_system, key)
            .and_then(|commands| outline_path(commands, origin_x, origin_y));

        // The image is fetched once per glyph. It decides between a colour bitmap, the outline and the mask.
        let image = self
            .swash_cache
            .get_image(&mut self.font_system, key)
            .as_ref();

        // Color glyphs come from the color layers of the font (COLR/CPAL) or from color bitmaps
        // (CBDT/sbix). swash renders them first in its source order, so their image content is `Color`.
        if let Some(image) = image.filter(|image| matches!(image.content, SwashContent::Color)) {
            // swash composites COLR layers into premultiplied RGBA. Its color bitmaps are straight.
            let premultiplied = matches!(image.source, SwashSource::ColorOutline(_));
            pieces.extend(color_piece(image, glyph.x, glyph.y, premultiplied));
            return;
        }

        if let Some(path) = outline {
            pieces.push(Piece::Outline(path));
            return;
        }

        // No outline (for example a bitmap-only mask glyph): use the coverage mask in the fill color.
        if let Some(image) = image {
            pieces.extend(mask_piece(image, glyph.x, glyph.y, fill));
        }
    }
}

/// One drawable part of a caption, in layout space (pixels, y down).
enum Piece {
    /// Vector glyph outline, stroked and filled.
    Outline(Path),
    /// Premultiplied RGBA bitmap at an integer position (color emoji, or a mask without an outline).
    Bitmap { x: i32, y: i32, pixmap: Pixmap },
}

/// Style values after validation. `None` means the effect is not drawn.
struct Resolved {
    /// Visible stroke width and color.
    stroke: Option<(f32, [u8; 4])>,
    shadow: Option<ResolvedShadow>,
    /// Padding, radius and color of the line boxes.
    background: Option<(f32, f32, [u8; 4])>,
}

#[derive(Clone, Copy)]
struct ResolvedShadow {
    dx: f32,
    dy: f32,
    blur: f32,
    color: [u8; 4],
}

/// Applies the validation rules from the module documentation.
fn resolve(style: &TextStyle) -> Resolved {
    let stroke = style
        .stroke
        .as_ref()
        .filter(|s| s.width_px.is_finite() && s.width_px > 0.0)
        .map(|s| (s.width_px, s.color));
    let shadow = style
        .shadow
        .as_ref()
        .filter(|s| {
            s.dx.is_finite() && s.dy.is_finite() && s.blur_px.is_finite() && s.blur_px >= 0.0
        })
        .map(|s| ResolvedShadow {
            dx: s.dx,
            dy: s.dy,
            blur: s.blur_px,
            color: s.color,
        });
    let background = style.background.as_ref().map(|b| {
        let pad = if b.padding_px.is_finite() {
            b.padding_px.max(0.0)
        } else {
            0.0
        };
        let radius = if b.radius_px.is_finite() {
            b.radius_px.max(0.0)
        } else {
            0.0
        };
        (pad, radius, b.color)
    });
    Resolved {
        stroke,
        shadow,
        background,
    }
}

/// Converts swash outline commands (y up, relative to the glyph origin) into a path in layout space.
fn outline_path(commands: &[Command], origin_x: f32, origin_y: f32) -> Option<Path> {
    let mut builder = PathBuilder::new();
    for command in commands {
        match command {
            Command::MoveTo(p) => builder.move_to(origin_x + p.x, origin_y - p.y),
            Command::LineTo(p) => builder.line_to(origin_x + p.x, origin_y - p.y),
            Command::QuadTo(c, p) => builder.quad_to(
                origin_x + c.x,
                origin_y - c.y,
                origin_x + p.x,
                origin_y - p.y,
            ),
            Command::CurveTo(c1, c2, p) => builder.cubic_to(
                origin_x + c1.x,
                origin_y - c1.y,
                origin_x + c2.x,
                origin_y - c2.y,
                origin_x + p.x,
                origin_y - p.y,
            ),
            Command::Close => builder.close(),
        }
    }
    builder.finish()
}

/// Builds a bitmap piece from a color glyph image. `premultiplied` says whether the data is already premultiplied.
fn color_piece(image: &SwashImage, x: i32, y: i32, premultiplied: bool) -> Option<Piece> {
    let (w, h) = (
        image.placement.width as usize,
        image.placement.height as usize,
    );
    let len = w.checked_mul(h)?.checked_mul(4)?;
    if w == 0 || h == 0 || image.data.len() < len {
        return None;
    }
    let mut data = image.data[..len].to_vec();
    if !premultiplied {
        for px in data.as_chunks_mut::<4>().0.iter_mut() {
            let a = px[3];
            px[0] = mul_div255(px[0], a);
            px[1] = mul_div255(px[1], a);
            px[2] = mul_div255(px[2], a);
        }
    }
    Some(Piece::Bitmap {
        x: x + image.placement.left,
        y: y - image.placement.top,
        pixmap: Pixmap::from_vec(data, IntSize::from_wh(w as u32, h as u32)?)?,
    })
}

/// Builds a bitmap piece from an alpha coverage mask, colored with `color`.
fn mask_piece(image: &SwashImage, x: i32, y: i32, color: [u8; 4]) -> Option<Piece> {
    let (w, h) = (
        image.placement.width as usize,
        image.placement.height as usize,
    );
    let len = w.checked_mul(h)?;
    if len == 0 || image.data.len() < len {
        return None;
    }
    let mut data = vec![0u8; len * 4];
    for (dst, &coverage) in data.as_chunks_mut::<4>().0.iter_mut().zip(&image.data) {
        let a = mul_div255(color[3], coverage);
        *dst = [
            mul_div255(color[0], a),
            mul_div255(color[1], a),
            mul_div255(color[2], a),
            a,
        ];
    }
    Some(Piece::Bitmap {
        x: x + image.placement.left,
        y: y - image.placement.top,
        pixmap: Pixmap::from_vec(data, IntSize::from_wh(w as u32, h as u32)?)?,
    })
}

/// Composes all pieces into one canvas: background boxes, shadow, strokes, fills and bitmaps, in that order.
fn compose(pieces: &[Piece], boxes: &[[f32; 4]], style: &TextStyle) -> Option<RasterImage> {
    let resolved = resolve(style);
    let stroke_px = resolved.stroke.map_or(0.0, |(width, _)| width);
    let shadow_extent = resolved.shadow.map_or(0.0, |s| {
        s.dx.abs().max(s.dy.abs()) + 2.0 * s.blur + stroke_px
    });
    let box_pad = resolved.background.map_or(0.0, |(pad, _, _)| pad);
    // Line boxes only matter when a background is drawn; otherwise they must not enlarge the canvas.
    let boxes = if resolved.background.is_some() {
        boxes
    } else {
        &[]
    };

    // Padding keeps the stroke, the shadow (including its stroke and blur) and the margin inside the canvas.
    let pad = (stroke_px.max(shadow_extent) + 2.0).ceil();
    let [min_x, min_y, max_x, max_y] = bounds(pieces, boxes, box_pad)?;

    // Canvas edges as whole pixels. Checked in f32 before any integer conversion, so huge values fail cleanly.
    let (left, top) = (min_x.floor(), min_y.floor());
    let (right, bottom) = (max_x.ceil(), max_y.ceil());
    if ![left, top, right, bottom, pad]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= MAX_COORD_PX)
    {
        return None;
    }
    let width_f = (right - left) + 2.0 * pad;
    let height_f = (bottom - top) + 2.0 * pad;
    let edge = MAX_EDGE_PX as f32;
    if !(width_f >= 1.0 && width_f <= edge && height_f >= 1.0 && height_f <= edge) {
        return None;
    }
    let (width, height) = (width_f as u32, height_f as u32);
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    if pixels > MAX_CANVAS_PIXELS {
        return None;
    }

    // Layout space -> canvas space. Integer, so bitmap glyphs stay pixel aligned.
    let (tx, ty) = (pad - left, pad - top);
    let (tx_px, ty_px) = (tx as i32, ty as i32);
    let transform = Transform::from_translate(tx, ty);

    let mut canvas = Pixmap::new(width, height)?;

    if let Some((_, radius, color)) = resolved.background {
        let paint = solid_paint(color);
        for &[x0, y0, x1, y1] in boxes {
            let (l, t, r, b) = (x0 - box_pad, y0 - box_pad, x1 + box_pad, y1 + box_pad);
            let rad = radius.min((r - l) * 0.5).min((b - t) * 0.5).max(0.0);
            if let Some(path) = rounded_rect(l, t, r, b, rad) {
                canvas.fill_path(&path, &paint, FillRule::Winding, transform, None);
            }
        }
    }

    let stroke = resolved
        .stroke
        .map(|(width, color)| (solid_paint(color), sk_stroke(width)));

    if let Some(shadow) = resolved.shadow {
        draw_shadow(
            &mut canvas,
            pieces,
            shadow,
            stroke.as_ref().map(|(_, sk)| sk),
            (tx_px, ty_px),
        )?;
    }

    if let Some((paint, sk_stroke)) = &stroke {
        let fill_is_opaque = style.fill[3] == 255;
        if fill_is_opaque {
            // The fill covers the inner half of the stroke, so drawing it directly is equivalent.
            for piece in pieces {
                if let Piece::Outline(path) = piece {
                    canvas.stroke_path(path, paint, sk_stroke, transform, None);
                }
            }
        } else {
            // Translucent fill: the stroke's inner half must not show through, so the stroke is drawn into
            // its own layer, the glyph interiors are cleared from it, and the layer goes beneath the fill.
            let mut layer = Pixmap::new(width, height)?;
            for piece in pieces {
                if let Piece::Outline(path) = piece {
                    layer.stroke_path(path, paint, sk_stroke, transform, None);
                }
            }
            let clear = clear_paint();
            for piece in pieces {
                if let Piece::Outline(path) = piece {
                    layer.fill_path(path, &clear, FillRule::Winding, transform, None);
                }
            }
            canvas.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
    }

    let fill = solid_paint(style.fill);
    for piece in pieces {
        if let Piece::Outline(path) = piece {
            canvas.fill_path(path, &fill, FillRule::Winding, transform, None);
        }
    }

    for piece in pieces {
        if let Piece::Bitmap { x, y, pixmap } = piece {
            canvas.draw_pixmap(
                x + tx_px,
                y + ty_px,
                pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
    }

    Some(RasterImage {
        width,
        height,
        data: canvas.take(),
        origin_x: tx,
        origin_y: ty,
    })
}

/// Draws the blurred, offset, colored silhouette of `pieces` (plus `stroke`, when given) onto `canvas`.
fn draw_shadow(
    canvas: &mut Pixmap,
    pieces: &[Piece],
    shadow: ResolvedShadow,
    stroke: Option<&SkStroke>,
    (tx, ty): (i32, i32),
) -> Option<()> {
    let (width, height) = (canvas.width(), canvas.height());
    // The silhouette pixmap is four bytes per pixel. Only its alpha plane is kept, so the pixmap is freed before the
    // blur, which needs the alpha plane and two f32 planes of its own.
    let mut alpha = silhouette_alpha(pieces, shadow, stroke, (tx, ty), (width, height))?;
    blur_alpha(
        &mut alpha,
        width as usize,
        height as usize,
        blur_radius(shadow.blur),
    );

    let mut layer = Pixmap::new(width, height)?;
    for (dst, &a) in layer
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(&alpha)
    {
        let alpha_out = mul_div255(a, shadow.color[3]);
        *dst = [
            mul_div255(shadow.color[0], alpha_out),
            mul_div255(shadow.color[1], alpha_out),
            mul_div255(shadow.color[2], alpha_out),
            alpha_out,
        ];
    }
    canvas.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Some(())
}

/// The alpha plane of the offset silhouette of `pieces` (with `stroke` when given), at canvas size. The silhouette
/// pixmap exists only inside this function, so it is dropped before the caller blurs.
fn silhouette_alpha(
    pieces: &[Piece],
    shadow: ResolvedShadow,
    stroke: Option<&SkStroke>,
    (tx, ty): (i32, i32),
    (width, height): (u32, u32),
) -> Option<Vec<u8>> {
    let mut silhouette = Pixmap::new(width, height)?;
    let black = solid_paint([0, 0, 0, 255]);
    let dx_px = shadow.dx.round() as i32;
    let dy_px = shadow.dy.round() as i32;
    let transform = Transform::from_translate(tx as f32 + shadow.dx, ty as f32 + shadow.dy);

    for piece in pieces {
        match piece {
            Piece::Outline(path) => {
                silhouette.fill_path(path, &black, FillRule::Winding, transform, None);
                if let Some(stroke) = stroke {
                    silhouette.stroke_path(path, &black, stroke, transform, None);
                }
            }
            Piece::Bitmap { x, y, pixmap } => silhouette.draw_pixmap(
                x + tx + dx_px,
                y + ty + dy_px,
                pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            ),
        }
    }

    Some(
        silhouette
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3])
            .collect(),
    )
}

/// Union of the piece bounds and the line boxes (each grown by `box_pad`), as `[left, top, right, bottom]`.
/// `None` when there is nothing to draw.
fn bounds(pieces: &[Piece], boxes: &[[f32; 4]], box_pad: f32) -> Option<[f32; 4]> {
    let mut acc: Option<[f32; 4]> = None;
    let mut include = |rect: [f32; 4]| {
        acc = Some(match acc {
            None => rect,
            Some(b) => [
                b[0].min(rect[0]),
                b[1].min(rect[1]),
                b[2].max(rect[2]),
                b[3].max(rect[3]),
            ],
        });
    };
    for piece in pieces {
        let rect = match piece {
            Piece::Outline(path) => {
                let b = path.bounds();
                [b.left(), b.top(), b.right(), b.bottom()]
            }
            Piece::Bitmap { x, y, pixmap } => {
                let (x0, y0) = (*x as f32, *y as f32);
                [
                    x0,
                    y0,
                    x0 + pixmap.width() as f32,
                    y0 + pixmap.height() as f32,
                ]
            }
        };
        include(rect);
    }
    for &[x0, y0, x1, y1] in boxes {
        include([x0 - box_pad, y0 - box_pad, x1 + box_pad, y1 + box_pad]);
    }
    acc
}

/// Rounded rectangle path with corners of radius `r` (quarter circles as cubics).
fn rounded_rect(l: f32, t: f32, r: f32, b: f32, rad: f32) -> Option<Path> {
    const K: f32 = 0.552_284_8;
    let k = K * rad;
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(r - rad, t);
    pb.cubic_to(r - rad + k, t, r, t + rad - k, r, t + rad);
    pb.line_to(r, b - rad);
    pb.cubic_to(r, b - rad + k, r - rad + k, b, r - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - k, b, l, b - rad + k, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - k, l + rad - k, t, l + rad, t);
    pb.close();
    pb.finish()
}

/// Stroke geometry for a visible outside width: the stroke is centered on the outline, so it is twice as wide.
fn sk_stroke(visible_px: f32) -> SkStroke {
    SkStroke {
        width: visible_px * 2.0,
        line_join: LineJoin::Round,
        line_cap: LineCap::Round,
        ..SkStroke::default()
    }
}

/// Opaque-or-translucent solid paint with anti-aliasing, from straight RGBA.
fn solid_paint(color: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint.anti_alias = true;
    paint
}

/// Paint that erases the destination under the path (used to clear glyph interiors from a stroke layer).
fn clear_paint() -> Paint<'static> {
    Paint {
        blend_mode: BlendMode::Clear,
        anti_alias: true,
        ..Paint::default()
    }
}

/// Box-blur radius for a CSS-style blur. The three-pass box blur then has roughly the variance of a
/// Gaussian with sigma = `blur_px / 2`. Returns 0 (no blur) for non-positive or non-finite radii, and caps the
/// radius so the window arithmetic cannot overflow.
fn blur_radius(blur_px: f32) -> usize {
    if !blur_px.is_finite() || blur_px <= 0.0 {
        return 0;
    }
    let sigma = blur_px / 2.0;
    // Ideal box width for three passes is sqrt(4 sigma^2 + 1); the radius is half of (width - 1).
    let radius = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round();
    // Anything past the largest canvas edge blurs to nothing new, so the cap only protects the arithmetic.
    (radius.min(MAX_EDGE_PX as f32) as usize).max(1)
}

/// Approximates a Gaussian blur of an 8-bit alpha plane with three separable box-blur passes.
/// Pixels outside the plane count as transparent, so the total alpha is preserved up to rounding
/// unless the blur reaches the edge.
fn blur_alpha(alpha: &mut [u8], width: usize, height: usize, radius: usize) {
    if radius == 0 || width == 0 || height == 0 || alpha.len() < width * height {
        return;
    }
    let mut plane: Vec<f32> = alpha[..width * height]
        .iter()
        .map(|&a| f32::from(a))
        .collect();
    let mut scratch = vec![0.0_f32; plane.len()];
    for _ in 0..3 {
        // Horizontal: `height` rows of `width` samples, adjacent samples one apart.
        box_pass(&plane, &mut scratch, height, width, width, 1, radius);
        // Vertical: `width` columns of `height` samples, adjacent samples `width` apart.
        box_pass(&scratch, &mut plane, width, height, 1, width, radius);
    }
    for (dst, src) in alpha.iter_mut().zip(&plane) {
        *dst = src.round().clamp(0.0, 255.0) as u8;
    }
}

/// One box-blur pass over `lines` lines of `len` samples. Within a line, samples are `step` apart;
/// line `i` starts at `i * line_step`. Uses a running sum, so each pass is O(n).
fn box_pass(
    src: &[f32],
    dst: &mut [f32],
    lines: usize,
    len: usize,
    line_step: usize,
    step: usize,
    radius: usize,
) {
    let norm = 1.0 / (2 * radius + 1) as f32;
    for line in 0..lines {
        let base = line * line_step;
        let at = |i: usize| base + i * step;
        // Window for output 0 covers [-radius, radius], clipped to the line.
        let mut sum: f32 = (0..=radius.min(len - 1)).map(|i| src[at(i)]).sum();
        for i in 0..len {
            dst[at(i)] = sum * norm;
            let entering = i + radius + 1;
            if entering < len {
                sum += src[at(entering)];
            }
            if i >= radius {
                sum -= src[at(i - radius)];
            }
        }
    }
}

/// `a * b / 255` with rounding, for 8-bit channels.
fn mul_div255(a: u8, b: u8) -> u8 {
    ((u32::from(a) * u32::from(b) + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use swash::zeno::{Command as ZCommand, Point};

    const ANTON: &[u8] = include_bytes!("../../../assets/fonts/Anton-Regular.ttf");
    const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");

    fn anton_rasterizer() -> CaptionRasterizer {
        CaptionRasterizer::with_extra_font_data(vec![ANTON.to_vec()]).0
    }

    fn anton_style(size_px: f32) -> TextStyle {
        TextStyle {
            family: "Anton".to_owned(),
            size_px,
            ..TextStyle::default()
        }
    }

    fn pixel(image: &RasterImage, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * image.width as usize + x as usize) * 4;
        [
            image.data[i],
            image.data[i + 1],
            image.data[i + 2],
            image.data[i + 3],
        ]
    }

    /// Bounding box `(min_x, min_y, max_x, max_y)` of pixels with any alpha, inclusive.
    fn ink_box(image: &RasterImage) -> Option<(u32, u32, u32, u32)> {
        let mut found: Option<(u32, u32, u32, u32)> = None;
        for y in 0..image.height {
            for x in 0..image.width {
                if pixel(image, x, y)[3] == 0 {
                    continue;
                }
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                });
            }
        }
        found
    }

    #[test]
    fn mul_div255_matches_exact_fractions() {
        assert_eq!(mul_div255(255, 255), 255);
        assert_eq!(mul_div255(255, 0), 0);
        assert_eq!(mul_div255(128, 255), 128);
        assert_eq!(mul_div255(100, 128), 50);
    }

    #[test]
    fn blur_radius_scales_with_blur() {
        assert_eq!(blur_radius(0.0), 0);
        assert_eq!(blur_radius(-3.0), 0);
        assert_eq!(blur_radius(f32::NAN), 0);
        assert_eq!(blur_radius(f32::INFINITY), 0);
        assert_eq!(blur_radius(1.0), 1);
        assert_eq!(blur_radius(6.0), 3);
        assert!(blur_radius(20.0) > blur_radius(6.0));
        assert!(blur_radius(1.0e30) <= MAX_EDGE_PX as usize);
    }

    #[test]
    fn blur_spreads_alpha_and_keeps_mass() {
        let (w, h) = (9usize, 9usize);
        let mut alpha = vec![0u8; w * h];
        alpha[4 * w + 4] = 255;
        let before: u32 = alpha.iter().map(|&a| u32::from(a)).sum();
        blur_alpha(&mut alpha, w, h, 1);
        let after: u32 = alpha.iter().map(|&a| u32::from(a)).sum();
        assert!(alpha[4 * w + 4] < 255, "center should lose intensity");
        assert!(
            alpha[4 * w + 5] > 0 && alpha[3 * w + 4] > 0,
            "neighbors should gain alpha"
        );
        // Rounding each of the 3 passes loses at most a little mass.
        let diff = before.abs_diff(after);
        assert!(diff <= 40, "mass changed by {diff}");
    }

    #[test]
    fn outline_path_flips_y_into_layout_space() {
        // A 10x10 square with its top edge at y = 10 (y up) above the baseline.
        let commands = [
            ZCommand::MoveTo(Point::new(0.0, 0.0)),
            ZCommand::LineTo(Point::new(10.0, 0.0)),
            ZCommand::LineTo(Point::new(10.0, 10.0)),
            ZCommand::LineTo(Point::new(0.0, 10.0)),
            ZCommand::Close,
        ];
        // Baseline origin at (5, 20) in layout space: the square must span y in [10, 20].
        let path = outline_path(&commands, 5.0, 20.0).expect("square has a path");
        let b = path.bounds();
        assert_eq!(
            (b.left(), b.top(), b.right(), b.bottom()),
            (5.0, 10.0, 15.0, 20.0)
        );
    }

    #[test]
    fn empty_text_and_invalid_sizes_give_empty_images() {
        let mut rasterizer = CaptionRasterizer::new();
        let style = TextStyle::default();
        assert!(rasterizer.rasterize("", &style).is_empty());
        let bad = TextStyle {
            size_px: 0.0,
            ..TextStyle::default()
        };
        assert!(rasterizer.rasterize("A", &bad).is_empty());
        let nan = TextStyle {
            size_px: f32::NAN,
            ..TextStyle::default()
        };
        assert!(rasterizer.rasterize("A", &nan).is_empty());
    }

    #[test]
    fn extra_font_data_counts_accepted_fonts() {
        let (rasterizer, accepted) = CaptionRasterizer::with_extra_font_data(vec![
            ANTON.to_vec(),
            vec![1, 2, 3, 4],
            INTER.to_vec(),
        ]);
        assert_eq!(accepted, 2, "garbage bytes must not be counted");
        let families = rasterizer.font_family_names();
        assert!(families.iter().any(|f| f == "Anton"), "Anton: {families:?}");
        assert!(families.iter().any(|f| f == "Inter"), "Inter: {families:?}");
        assert!(rasterizer.has_family("anton"));
        assert!(!rasterizer.has_family("No Such Family Here"));
    }

    #[test]
    fn family_names_are_sorted_and_unique() {
        let (rasterizer, _) = CaptionRasterizer::with_extra_font_data(vec![ANTON.to_vec()]);
        let names = rasterizer.font_family_names();
        assert!(!names.is_empty());
        for pair in names.windows(2) {
            assert!(
                pair[0].to_lowercase() <= pair[1].to_lowercase(),
                "not sorted: {:?} before {:?}",
                pair[0],
                pair[1]
            );
            assert_ne!(
                pair[0].to_lowercase(),
                pair[1].to_lowercase(),
                "duplicate family {}",
                pair[0]
            );
        }
    }

    #[test]
    fn family_names_that_differ_only_in_case_are_one_family() {
        let (rasterizer, _) =
            CaptionRasterizer::with_extra_font_data(vec![ANTON.to_vec(), INTER.to_vec()]);
        let names = rasterizer.font_family_names();
        let mut lower: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
        let total = lower.len();
        lower.sort();
        lower.dedup();
        assert_eq!(lower.len(), total, "a family is listed twice in {names:?}");
    }

    #[test]
    fn swash_cache_is_replaced_once_its_maps_pass_the_limit() {
        // Each new font size makes new glyph keys, so a sweep of sizes over many glyphs fills cosmic-text's maps
        // past the limit. Each call adds at most one entry per glyph, so the cache is never more than one call over.
        let text = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let glyphs = text.chars().count();
        let mut rasterizer = anton_rasterizer();
        let mut peak = 0;
        for step in 0..150 {
            let style = anton_style(8.0 + step as f32 * 0.01);
            assert!(!rasterizer.rasterize(text, &style).is_empty());
            peak = peak.max(rasterizer.swash_cache.image_cache.len());
        }
        assert!(
            rasterizer.swash_cache.image_cache.len() <= SWASH_CACHE_LIMIT + glyphs,
            "the image cache holds {} entries after the sweep",
            rasterizer.swash_cache.image_cache.len()
        );
        assert!(
            peak > SWASH_CACHE_LIMIT,
            "the sweep never filled the cache (peak {peak})"
        );
        // The rasterizer still renders identically after a reset.
        let again = rasterizer.rasterize("I", &anton_style(8.0));
        let fresh = anton_rasterizer().rasterize("I", &anton_style(8.0));
        assert_eq!(again, fresh);
    }

    #[test]
    fn stroke_is_exactly_its_width_outside_an_opaque_glyph() {
        let mut rasterizer = anton_rasterizer();
        let width = 8.0;
        let plain = rasterizer.rasterize("I", &anton_style(200.0));
        let stroked = rasterizer.rasterize(
            "I",
            &TextStyle {
                stroke: Some(Stroke {
                    width_px: width,
                    color: [0, 0, 0, 255],
                }),
                ..anton_style(200.0)
            },
        );
        assert!(!plain.is_empty() && !stroked.is_empty());

        // Scan the middle row of the stroked image. The stroke is the ink outside the white fill, so its
        // thickness on each side is the distance from the first (last) ink pixel to the first (last) white pixel.
        let row = stroked.height / 2;
        let ink = |x: u32| pixel(&stroked, x, row)[3] > 0;
        let white = |x: u32| pixel(&stroked, x, row) == [255, 255, 255, 255];
        let first_white = (0..stroked.width)
            .find(|&x| white(x))
            .expect("the glyph fill must cross the middle row");
        let first_ink = (0..first_white)
            .find(|&x| ink(x))
            .expect("the stroke must reach the middle row");
        let left = (first_white - first_ink) as f32;
        assert!(
            (left - width).abs() <= 1.5,
            "left stroke thickness {left} px, expected about {width} px"
        );

        let last_white = (0..stroked.width)
            .rev()
            .find(|&x| white(x))
            .expect("the glyph fill must cross the middle row");
        let last_ink = (last_white + 1..stroked.width)
            .rev()
            .find(|&x| ink(x))
            .expect("the stroke must reach the middle row");
        let right = (last_ink - last_white) as f32;
        assert!(
            (right - width).abs() <= 1.5,
            "right stroke thickness {right} px, expected about {width} px"
        );
    }

    #[test]
    fn stroke_under_translucent_fill_does_not_tint_the_interior() {
        let mut rasterizer = anton_rasterizer();
        let style = TextStyle {
            fill: [255, 255, 255, 128],
            stroke: Some(Stroke {
                width_px: 8.0,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(200.0)
        };
        let image = rasterizer.rasterize("I", &style);
        assert!(!image.is_empty());
        let row = image.height / 2;
        // Deep inside the stem: the fill alone, 50% white over nothing, premultiplied (128, 128, 128, 128).
        // Fill pixels are gray with rgb equal to alpha; the opaque black stroke has rgb 0 and is skipped.
        let (mut first, mut last) = (None, None);
        for x in 0..image.width {
            let px = pixel(&image, x, row);
            if px[3] >= 120 && px[0] >= 100 && px[0] == px[3] {
                if first.is_none() {
                    first = Some(x);
                }
                last = Some(x);
            }
        }
        let (first, last) = (first.expect("fill present"), last.expect("fill present"));
        let center = (first + last) / 2;
        let px = pixel(&image, center, row);
        assert!(
            px[3].abs_diff(128) <= 4 && px[0].abs_diff(128) <= 4,
            "interior pixel {px:?} should be the bare translucent fill (about 128)"
        );
        // Outside the glyph the stroke is opaque black.
        let outside = pixel(&image, first.saturating_sub(4), row);
        assert_eq!(outside, [0, 0, 0, 255], "stroke should be opaque outside");
    }

    #[test]
    fn centered_lines_share_a_center() {
        let mut rasterizer = anton_rasterizer();
        let style = TextStyle {
            max_width_px: Some(2000.0),
            align: TextAlign::Center,
            ..anton_style(120.0)
        };
        let image = rasterizer.rasterize("I\nIIIIIIIIII", &style);
        assert!(!image.is_empty());
        // Split the ink into vertical bands separated by fully transparent rows.
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let mut current: Option<(u32, u32)> = None;
        for y in 0..image.height {
            let row_ink = (0..image.width).any(|x| pixel(&image, x, y)[3] > 0);
            match (row_ink, current) {
                (true, None) => current = Some((y, y)),
                (true, Some((a, _))) => current = Some((a, y)),
                (false, Some(band)) => {
                    bands.push(band);
                    current = None;
                }
                (false, None) => {}
            }
        }
        if let Some(band) = current {
            bands.push(band);
        }
        assert_eq!(bands.len(), 2, "two lines expected, found bands {bands:?}");
        let centers: Vec<f32> = bands
            .iter()
            .map(|&(y0, y1)| {
                let mut lo = u32::MAX;
                let mut hi = 0;
                for y in y0..=y1 {
                    for x in 0..image.width {
                        if pixel(&image, x, y)[3] > 0 {
                            lo = lo.min(x);
                            hi = hi.max(x);
                        }
                    }
                }
                (lo + hi) as f32 / 2.0
            })
            .collect();
        assert!(
            (centers[0] - centers[1]).abs() <= 2.0,
            "line centers differ: {centers:?}"
        );
    }

    #[test]
    fn origin_moves_with_the_padding_and_the_stroke_grows_the_ink_outward() {
        let mut rasterizer = anton_rasterizer();
        let plain = rasterizer.rasterize("Origin", &anton_style(64.0));
        let stroked = rasterizer.rasterize(
            "Origin",
            &TextStyle {
                stroke: Some(Stroke {
                    width_px: 5.0,
                    color: [0, 0, 0, 255],
                }),
                ..anton_style(64.0)
            },
        );
        assert!(!plain.is_empty() && !stroked.is_empty());
        // The padding is the stroke width plus 2 px, rounded up: 2 px plain and 7 px stroked. The layout is the same,
        // so the canvas grows by 10 px on each axis and the origin moves by exactly 5 px.
        assert_eq!(stroked.origin_x - plain.origin_x, 5.0);
        assert_eq!(stroked.origin_y - plain.origin_y, 5.0);
        assert_eq!(stroked.width, plain.width + 10);
        assert_eq!(stroked.height, plain.height + 10);
        // The origin is not clamped to the image. It can lie outside it when the ink does not reach the box, so the
        // check is on the offset between the origin and the ink, not on the origin's position. The stroke pushes the
        // ink out by 5 px on the left and top, so that offset shrinks by 5 px on each axis.
        let (plain_ink, stroked_ink) = (
            ink_box(&plain).expect("plain ink"),
            ink_box(&stroked).expect("stroked ink"),
        );
        let gap = |ink: u32, origin: f32| ink as f32 - origin;
        let left_gap =
            gap(stroked_ink.0, stroked.origin_x) - (gap(plain_ink.0, plain.origin_x) - 5.0);
        let top_gap =
            gap(stroked_ink.1, stroked.origin_y) - (gap(plain_ink.1, plain.origin_y) - 5.0);
        assert!(left_gap.abs() <= 1.5, "left offset off by {left_gap} px");
        assert!(top_gap.abs() <= 1.5, "top offset off by {top_gap} px");
    }

    #[test]
    fn shadow_blur_spreads_more_than_a_sharp_shadow() {
        let mut rasterizer = anton_rasterizer();
        let shadow = |blur_px: f32| TextStyle {
            stroke: None,
            shadow: Some(Shadow {
                dx: 6.0,
                dy: 6.0,
                blur_px,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(96.0)
        };
        let sharp = rasterizer.rasterize("SHADOW", &shadow(0.0));
        let blurred = rasterizer.rasterize("SHADOW", &shadow(10.0));
        let partial = |image: &RasterImage| {
            image
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] > 0 && px[3] < 255 && px[0] == 0)
                .count()
        };
        let (sharp_partial, blurred_partial) = (partial(&sharp), partial(&blurred));
        assert!(
            blurred_partial > 3 * sharp_partial.max(1),
            "blurred {blurred_partial} vs sharp {sharp_partial} partial-alpha pixels"
        );
        // The blurred silhouette reaches further than the sharp one, so the image is at least as wide.
        assert!(
            blurred.width >= sharp.width && blurred.height >= sharp.height,
            "blur should widen the padded canvas: {}x{} vs {}x{}",
            blurred.width,
            blurred.height,
            sharp.width,
            sharp.height
        );
        assert!(ink_box(&blurred).is_some() && ink_box(&sharp).is_some());
    }

    #[test]
    fn invalid_stroke_and_shadow_values_are_treated_as_none() {
        let mut rasterizer = anton_rasterizer();
        let plain = rasterizer.rasterize("Hi", &anton_style(72.0));
        let invalid_stroke = |width_px: f32| TextStyle {
            stroke: Some(Stroke {
                width_px,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(72.0)
        };
        for width in [f32::NAN, f32::INFINITY, -4.0, 0.0] {
            let image = rasterizer.rasterize("Hi", &invalid_stroke(width));
            assert_eq!(image, plain, "stroke width {width} should draw no stroke");
        }
        let invalid_shadow = |dx: f32, blur_px: f32| TextStyle {
            shadow: Some(Shadow {
                dx,
                dy: 0.0,
                blur_px,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(72.0)
        };
        for (dx, blur) in [(f32::NAN, 2.0), (2.0, f32::NAN), (2.0, -1.0)] {
            let image = rasterizer.rasterize("Hi", &invalid_shadow(dx, blur));
            assert_eq!(
                image, plain,
                "shadow dx {dx} blur {blur} should draw no shadow"
            );
        }
        let bad_padding = rasterizer.rasterize(
            "Hi",
            &TextStyle {
                background: Some(Background {
                    color: [0, 0, 0, 0],
                    padding_px: f32::NAN,
                    radius_px: -3.0,
                }),
                ..anton_style(72.0)
            },
        );
        assert!(
            !bad_padding.is_empty(),
            "NaN padding must not drop the text"
        );
    }

    #[test]
    fn canvas_limits_return_empty_images() {
        let mut rasterizer = anton_rasterizer();
        let huge_edge = TextStyle {
            stroke: Some(Stroke {
                width_px: 9000.0,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(200.0)
        };
        assert!(rasterizer.rasterize("I", &huge_edge).is_empty());

        // Under the edge limit but over the pixel budget: about 6200 x 6300 px.
        let over_budget = TextStyle {
            stroke: Some(Stroke {
                width_px: 3000.0,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(200.0)
        };
        assert!(rasterizer.rasterize("I", &over_budget).is_empty());

        // Within both limits: about 3100 x 3300 px, about 10 Mpx.
        let within = TextStyle {
            stroke: Some(Stroke {
                width_px: 1500.0,
                color: [0, 0, 0, 255],
            }),
            ..anton_style(200.0)
        };
        let image = rasterizer.rasterize("I", &within);
        assert!(!image.is_empty());
        assert!(u64::from(image.width) * u64::from(image.height) <= MAX_CANVAS_PIXELS);
    }

    #[test]
    fn background_box_is_drawn_behind_the_text() {
        let mut rasterizer = anton_rasterizer();
        let image = rasterizer.rasterize(
            "I",
            &TextStyle {
                background: Some(Background {
                    color: [0, 0, 255, 255],
                    padding_px: 12.0,
                    radius_px: 4.0,
                }),
                ..anton_style(120.0)
            },
        );
        assert!(!image.is_empty());
        let row = image.height / 2;
        // Left of the glyph, inside the padded box: opaque blue.
        let first_fill = (0..image.width)
            .find(|&x| pixel(&image, x, row) == [255, 255, 255, 255])
            .expect("glyph crosses the middle row");
        let margin = pixel(&image, first_fill.saturating_sub(6), row);
        assert_eq!(
            margin,
            [0, 0, 255, 255],
            "background must sit behind the glyph"
        );
    }

    #[test]
    #[cfg(windows)]
    fn color_emoji_has_colored_pixels_on_windows() {
        let mut rasterizer = CaptionRasterizer::new();
        let style = TextStyle {
            family: "Segoe UI Emoji".to_owned(),
            size_px: 96.0,
            ..TextStyle::default()
        };
        let image = rasterizer.rasterize("\u{1F600}\u{1F389}", &style);
        assert!(!image.is_empty(), "emoji must rasterize");
        let colored = image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] > 200 && !(px[0] == px[1] && px[1] == px[2]))
            .count();
        assert!(
            colored > 500,
            "expected colored emoji pixels, got {colored}"
        );
    }

    #[test]
    fn caption_rasterize_without_stroke_leaves_no_black() {
        let mut rasterizer = anton_rasterizer();
        let image = rasterizer.rasterize("Hi", &anton_style(64.0));
        assert!(!image.is_empty());
        let black = image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] == 255 && px[0] == 0)
            .count();
        assert_eq!(black, 0, "no stroke or shadow means no black pixels");
    }
}

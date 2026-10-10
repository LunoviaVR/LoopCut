//! Caption layers: converts project captions into compositor overlays at a given output time.
//!
//! [`CaptionRenderer::layers_at`] is called once per output frame. For each caption whose source range contains
//! the current source time it:
//!
//! 1. works out the motion (fade, pop, slide) from the time since the caption appeared and the time left. A caption
//!    whose opacity is zero is skipped here, before any text is built or rasterized,
//! 2. builds the visible text (uppercased when the style asks, and cut to the typewriter prefix),
//! 3. converts the caption style to a [`TextStyle`] at the output size (sizes are fractions of the output
//!    height, clamped to the rasterizer's maximum font size first; the wrap width a fraction of the output width;
//!    stroke, shadow and box sizes fractions of the font size),
//! 4. fetches the raster from a byte-capped LRU cache, rasterizing on a miss. A raster that comes out empty is
//!    remembered, so later frames skip it,
//! 5. places the raster so that its anchor lands on the caption position, clamped inside the frame.
//!
//! Pop scales the destination rectangle around the anchor, so it never re-rasterizes. Typewriter re-rasterizes
//! once per visible character count. Every raster keeps the same overlay id for as long as its pixels stay the
//! same, so the compositor reuses the uploaded texture. A caption at rest sits on whole output pixels.
//!
//! Bundled fonts (OFL, see `assets/fonts`) are compiled into the binary: Anton and Inter (regular and bold, as a
//! variable font). Anton stands in for Impact when Impact is not installed, and for the project's default family
//! when that is missing. Any other missing family goes to cosmic-text's own matching; [`CaptionRenderer::resolved_family`]
//! reports the family that matching picks.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use loopcut_core::NormRect;
use loopcut_core::project::{
    Caption, CaptionAnimation, CaptionStyle, SlideEdge, TextAlign as ProjectTextAlign,
};

use crate::captions::{
    Background, CaptionRasterizer, MAX_FONT_PX, RasterImage, Shadow, Stroke, TextAlign, TextStyle,
};
use crate::compositor::Overlay;

/// Anton (OFL), the display face used when Impact or the default family is not installed.
const ANTON_TTF: &[u8] = include_bytes!("../../../assets/fonts/Anton-Regular.ttf");

/// Inter (OFL) as a variable font: regular and bold come from its weight axis.
const INTER_TTF: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");

/// Family used for Impact, and for the default family, when it is not installed.
const FALLBACK_FAMILY: &str = "Anton";

/// Font size in pixels of the probe that asks cosmic-text which family it matches for a name.
const PROBE_PX: f32 = 16.0;

/// Failed raster keys kept before the set is cleared. A failure is cheap to remember, and this bounds the set.
const FAILED_LIMIT: usize = 4096;

/// Byte budget for cached rasters (premultiplied RGBA8 pixels).
const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Padding of the background box, in em (font sizes).
const BOX_PADDING_EM: f32 = 0.25;

/// Corner radius of the background box, in em.
const BOX_RADIUS_EM: f32 = 0.15;

/// Pop animation: starts at this scale, overshoots to [`POP_OVERSHOOT`], then settles at 1.0.
const POP_START: f32 = 0.6;
const POP_OVERSHOOT: f32 = 1.05;

/// Fraction of the pop duration spent growing to the overshoot.
const POP_GROW_FRACTION: f32 = 0.7;

/// Owns the premultiplied pixels of one caption layer, so the overlay can be borrowed for a render.
#[derive(Debug, Clone)]
pub struct OwnedOverlay {
    /// Premultiplied RGBA8 pixels, `width * height * 4` bytes, top row first. Shared with the cache.
    pub rgba_premultiplied: Arc<Vec<u8>>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Destination rectangle in normalized output space.
    pub dest: NormRect,
    /// Opacity in `[0, 1]`.
    pub opacity: f32,
    /// Texture cache key. Stable while the pixels stay the same; distinct for captions visible together.
    pub id: u64,
}

impl OwnedOverlay {
    /// Borrows the layer as a compositor overlay.
    pub fn as_overlay(&self) -> Overlay<'_> {
        Overlay {
            rgba_premultiplied: self.rgba_premultiplied.as_slice(),
            width: self.width,
            height: self.height,
            dest: self.dest,
            opacity: self.opacity,
            id: self.id,
        }
    }
}

/// Renders project captions to overlays. Create one per application and reuse it: creating one scans the
/// installed fonts, and the raster cache only pays off across frames.
pub struct CaptionRenderer {
    rasterizer: CaptionRasterizer,
    /// Sorted, deduplicated family names (system plus bundled).
    families: Vec<String>,
    /// The same names lowercased, for case-insensitive lookups.
    installed: HashSet<String>,
    /// Families that cosmic-text matched for missing names, keyed by the lowercased request.
    matched: HashMap<String, String>,
    cache: RasterCache,
    /// Keys whose raster came out empty. They are not retried, so a bad caption costs one attempt.
    failed: HashSet<RasterKey>,
}

impl Default for CaptionRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptionRenderer {
    /// Creates a renderer with the system fonts plus the bundled fonts.
    pub fn new() -> Self {
        Self::with_extra_fonts(Vec::new())
    }

    /// Creates a renderer with the bundled fonts plus extra font files (TTF, OTF or TTC bytes). Data that
    /// is not a readable font is ignored.
    pub fn with_extra_fonts(data: Vec<Vec<u8>>) -> Self {
        let mut fonts = vec![ANTON_TTF.to_vec(), INTER_TTF.to_vec()];
        fonts.extend(data);
        let total = fonts.len();
        let (rasterizer, accepted) = CaptionRasterizer::with_extra_font_data(fonts);
        if accepted < total {
            tracing::warn!(accepted, total, "some caption fonts could not be read");
        }
        let families = rasterizer.font_family_names();
        let installed = families.iter().map(|name| name.to_lowercase()).collect();
        Self {
            rasterizer,
            families,
            installed,
            matched: HashMap::new(),
            cache: RasterCache::default(),
            failed: HashSet::new(),
        }
    }

    /// Sorted, deduplicated family names available to captions (system plus bundled).
    pub fn font_families(&mut self) -> Vec<String> {
        self.families.clone()
    }

    /// The family that a caption asking for `family` renders with. The UI shows this, so it matches what is drawn.
    ///
    /// * An installed family (system or bundled, case-insensitive) is used as requested.
    /// * `Impact`, and the project's default family, use the bundled Anton when they are not installed.
    /// * Any other missing family, including an empty name, goes to cosmic-text's own matching. That matching has no
    ///   fixed answer, so this renders a probe glyph in that family once and reads the family it used. The answer is
    ///   kept for later calls.
    pub fn resolved_family(&mut self, family: &str) -> String {
        let name = family.trim();
        let lower = name.to_lowercase();
        if !name.is_empty() && self.installed.contains(&lower) {
            return name.to_owned();
        }
        let is_default = name.eq_ignore_ascii_case("Impact")
            || name.eq_ignore_ascii_case(&CaptionStyle::default().font_family);
        if is_default && self.installed.contains(&FALLBACK_FAMILY.to_lowercase()) {
            return FALLBACK_FAMILY.to_owned();
        }
        if let Some(found) = self.matched.get(&lower) {
            return found.clone();
        }
        let found = self.probe_family(name);
        self.matched.insert(lower, found.clone());
        found
    }

    /// The family cosmic-text uses for `name`, found by rendering one glyph in it.
    fn probe_family(&mut self, name: &str) -> String {
        let style = TextStyle {
            family: name.to_owned(),
            size_px: PROBE_PX,
            ..TextStyle::default()
        };
        self.rasterizer.rasterize("A", &style);
        self.rasterizer
            .font_families_used()
            .first()
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    }

    /// Overlays for the captions visible at `source_time`, in caption order, at the given output size.
    ///
    /// Visibility is `range.start <= source_time < range.end`. Animation time is output time: source time since
    /// the caption started (or left to go), divided by `speed`. A non-finite or non-positive `speed` counts as 1.
    /// Captions with empty visible text, a non-positive font size, a non-finite position or zero opacity are skipped,
    /// and so are captions whose raster is empty. Overlay ids are unique within one call.
    pub fn layers_at(
        &mut self,
        captions: &[Caption],
        source_time: f64,
        speed: f64,
        output_width: u32,
        output_height: u32,
    ) -> Vec<OwnedOverlay> {
        let mut layers = Vec::new();
        if output_width == 0 || output_height == 0 || !source_time.is_finite() {
            return layers;
        }
        let speed = if speed.is_finite() && speed > 0.0 {
            speed
        } else {
            1.0
        };
        let mut used_ids = HashSet::new();
        for caption in captions {
            if !caption.range.contains(source_time)
                || !caption.x.is_finite()
                || !caption.y.is_finite()
            {
                continue;
            }
            if let Some(layer) = self.layer_for(
                caption,
                source_time,
                speed,
                (output_width, output_height),
                &mut used_ids,
            ) {
                layers.push(layer);
            }
        }
        layers
    }

    fn layer_for(
        &mut self,
        caption: &Caption,
        source_time: f64,
        speed: f64,
        (out_w, out_h): (u32, u32),
        used_ids: &mut HashSet<u64>,
    ) -> Option<OwnedOverlay> {
        let (ow, oh) = (out_w as f32, out_h as f32);
        // Output seconds since the caption appeared and until it leaves.
        let elapsed = ((source_time - caption.range.start) / speed) as f32;
        let remaining = ((caption.range.end - source_time) / speed) as f32;

        // Motion needs no raster, so a caption that is fully transparent is skipped before any text is built.
        let motion = motion_at(&caption.animation, elapsed, remaining);
        if motion.opacity <= 0.0 {
            return None;
        }

        let full = if caption.style.uppercase {
            caption.text.to_uppercase()
        } else {
            caption.text.clone()
        };
        let total_chars = full.chars().count();
        let shown_chars = match caption.animation {
            CaptionAnimation::Typewriter { chars_per_second } => {
                let cps = if chars_per_second.is_finite() {
                    chars_per_second.max(0.0)
                } else {
                    0.0
                };
                ((elapsed.max(0.0) * cps).floor() as usize).min(total_chars)
            }
            _ => total_chars,
        };
        if shown_chars == 0 {
            return None;
        }
        let shown: String = if shown_chars == total_chars {
            full
        } else {
            full.chars().take(shown_chars).collect()
        };
        if shown.trim().is_empty() {
            return None;
        }

        let font_px = font_px_for(caption.style.font_size, oh)?;
        let family = self.resolved_family(&caption.style.font_family);
        let style = text_style(&caption.style, family, ow, font_px);

        let key = RasterKey::new(&shown, shown_chars as u32, (out_w, out_h), &style);
        let raster = self.raster_for(key.clone(), &shown, &style)?;

        let dest = place(
            &raster,
            caption.x,
            caption.y,
            style.max_width_px,
            (ow, oh),
            &motion,
        );

        // Two captions with the same key (same text, style and time) must not share an id in one render.
        let mut salt = 0u64;
        let id = loop {
            let id = stable_id(&key, &caption.id, salt);
            if used_ids.insert(id) {
                break id;
            }
            salt += 1;
        };

        Some(OwnedOverlay {
            rgba_premultiplied: Arc::clone(&raster.pixels),
            width: raster.width,
            height: raster.height,
            dest,
            opacity: motion.opacity,
            id,
        })
    }

    /// Cached raster for `key`, or a new one. `None` when the text produces no pixels.
    fn raster_for(
        &mut self,
        key: RasterKey,
        text: &str,
        style: &TextStyle,
    ) -> Option<Arc<CachedRaster>> {
        if let Some(hit) = self.cache.get(&key) {
            return Some(hit);
        }
        if self.failed.contains(&key) {
            return None;
        }
        let image = self.rasterizer.rasterize(text, style);
        if image.is_empty() {
            // Remember the failure, so the frames after this one do not run the rasterizer again for the same key.
            // The text itself is not logged, only its size and style.
            if self.failed.len() >= FAILED_LIMIT {
                self.failed.clear();
            }
            if self.failed.insert(key) {
                tracing::warn!(
                    chars = text.chars().count(),
                    family = %style.family,
                    size_px = style.size_px,
                    "caption raster came out empty; this caption will not be drawn"
                );
            }
            return None;
        }
        let raster = Arc::new(CachedRaster::from_image(image));
        self.cache.insert(key, Arc::clone(&raster));
        Some(raster)
    }
}

/// Font size in output pixels for a caption's `font_size` (a fraction of the output height). The result is clamped to
/// the rasterizer's largest size, and the stroke, shadow and box are then derived from that clamped value. `None` for
/// a size that is not finite or not positive.
fn font_px_for(font_size: f32, out_h: f32) -> Option<f32> {
    let px = font_size * out_h;
    (px.is_finite() && px > 0.0).then(|| px.min(MAX_FONT_PX))
}

/// Converts a project caption style into a rasterizer style at the output size. `font_px` is the font size in
/// output pixels, already clamped by [`font_px_for`]; `out_w` is the output width.
fn text_style(style: &CaptionStyle, family: String, out_w: f32, font_px: f32) -> TextStyle {
    let stroke = (style.stroke_width.is_finite() && style.stroke_width > 0.0).then_some(Stroke {
        width_px: style.stroke_width * font_px,
        color: style.stroke_color,
    });
    let shadow = style.shadow.as_ref().map(|s| Shadow {
        dx: s.dx * font_px,
        dy: s.dy * font_px,
        blur_px: s.blur * font_px,
        color: s.color,
    });
    let background = style.background.map(|color| Background {
        color,
        padding_px: BOX_PADDING_EM * font_px,
        radius_px: BOX_RADIUS_EM * font_px,
    });
    let max_width_px =
        (style.max_width.is_finite() && style.max_width > 0.0).then_some(style.max_width * out_w);
    TextStyle {
        family,
        weight: if style.bold { 700 } else { 400 },
        italic: style.italic,
        size_px: font_px,
        fill: style.fill,
        stroke,
        shadow,
        background,
        align: match style.align {
            ProjectTextAlign::Left => TextAlign::Left,
            ProjectTextAlign::Center => TextAlign::Center,
            ProjectTextAlign::Right => TextAlign::Right,
        },
        line_height: style.line_height,
        max_width_px,
    }
}

/// Animation state of one caption at one output time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Motion {
    /// Opacity in `[0, 1]`.
    opacity: f32,
    /// Scale around the anchor (1.0 for no pop).
    scale: f32,
    /// Slide-in edge and the part of the slide still to do (1.0 at the start, 0.0 at rest).
    slide: Option<(SlideEdge, f32)>,
}

impl Motion {
    const REST: Self = Self {
        opacity: 1.0,
        scale: 1.0,
        slide: None,
    };
}

/// Motion of `animation` at `elapsed` output seconds after the caption appeared, with `remaining` output
/// seconds before it leaves.
fn motion_at(animation: &CaptionAnimation, elapsed: f32, remaining: f32) -> Motion {
    match *animation {
        CaptionAnimation::None | CaptionAnimation::Typewriter { .. } => Motion::REST,
        CaptionAnimation::Fade { fade_in, fade_out } => Motion {
            opacity: ramp(elapsed, fade_in) * ramp(remaining, fade_out),
            ..Motion::REST
        },
        CaptionAnimation::Pop { duration } => Motion {
            scale: pop_scale(ramp(elapsed, duration)),
            ..Motion::REST
        },
        CaptionAnimation::Slide { from, duration } => {
            let eased = ease_out_cubic(ramp(elapsed, duration));
            Motion {
                slide: Some((from, 1.0 - eased)),
                ..Motion::REST
            }
        }
    }
}

/// Progress in `[0, 1]` over `duration` seconds. A missing or invalid duration means no animation (1.0).
fn ramp(t: f32, duration: f32) -> f32 {
    if !(duration.is_finite() && duration > 0.0) {
        return 1.0;
    }
    (t / duration).clamp(0.0, 1.0)
}

/// Pop scale for progress `p` in `[0, 1]`: 0.6 grows to 1.05 over the first 70%, then settles at 1.0.
fn pop_scale(p: f32) -> f32 {
    // Exactly 1 at rest, so placement can treat a settled caption as unscaled.
    if p >= 1.0 {
        1.0
    } else if p < POP_GROW_FRACTION {
        POP_START + (POP_OVERSHOOT - POP_START) * ease_out_cubic(p / POP_GROW_FRACTION)
    } else {
        let t = (p - POP_GROW_FRACTION) / (1.0 - POP_GROW_FRACTION);
        POP_OVERSHOOT + (1.0 - POP_OVERSHOOT) * smoothstep(t)
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Destination rectangle in normalized output space.
///
/// The raster's anchor is the middle of its layout box horizontally (the box is `box_w` wide when wrapping is
/// set, so centered and aligned text both sit where the caption says) and the middle of the image vertically.
/// The anchor lands on `(x, y)` and the raster is clamped inside the frame. Pop scales around the anchor, and the
/// scaled raster is clamped inside the frame again. A slide then offsets the rectangle from its edge by the
/// remaining slide distance, so it is outside the frame until it arrives. At rest, with no scale and no slide, the
/// position is rounded to whole output pixels.
fn place(
    raster: &CachedRaster,
    x: f64,
    y: f64,
    box_w: Option<f32>,
    (ow, oh): (f32, f32),
    motion: &Motion,
) -> NormRect {
    let (w, h) = (raster.width as f32, raster.height as f32);
    let anchor_x = match box_w {
        Some(bw) => raster.origin_x + bw * 0.5,
        None => w * 0.5,
    };
    let anchor_y = h * 0.5;

    let left = clamp_inside((x as f32) * ow - anchor_x, w, ow);
    let top = clamp_inside((y as f32) * oh - anchor_y, h, oh);
    // Anchor point in output pixels after clamping.
    let (px, py) = (left + anchor_x, top + anchor_y);

    // Pop scales around the anchor. The overshoot can push the scaled raster past an edge, so it is clamped again.
    let scale = motion.scale;
    let (sw, sh) = (w * scale, h * scale);
    let mut x0 = clamp_inside(px - anchor_x * scale, sw, ow);
    let mut y0 = clamp_inside(py - anchor_y * scale, sh, oh);

    // A slide starts one full distance off its edge (at remaining 1) and reaches the clamped rest position at 0.
    if let Some((edge, remaining)) = motion.slide {
        match edge {
            SlideEdge::Left => x0 -= (x0 + sw) * remaining,
            SlideEdge::Right => x0 += (ow - x0) * remaining,
            SlideEdge::Top => y0 -= (y0 + sh) * remaining,
            SlideEdge::Bottom => y0 += (oh - y0) * remaining,
        }
    } else if scale == 1.0 {
        // At rest the raster sits on whole output pixels, so it is not resampled. The bounds are whole pixels too, so
        // rounding keeps it inside the frame.
        x0 = x0.round();
        y0 = y0.round();
    }

    NormRect {
        x: f64::from(x0 / ow),
        y: f64::from(y0 / oh),
        w: f64::from(sw / ow),
        h: f64::from(sh / oh),
    }
}

/// Position `v` for a segment of `size` inside `[0, bound]`. A segment larger than the frame sits at 0.
fn clamp_inside(v: f32, size: f32, bound: f32) -> f32 {
    let max = (bound - size).max(0.0);
    v.clamp(0.0, max)
}

/// Raster inputs that determine the pixels. Two captions that agree on all of these share one raster.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RasterKey {
    text: String,
    /// Characters shown: the typewriter prefix length, or the full length.
    prefix: u32,
    output: (u32, u32),
    family: String,
    weight: u16,
    italic: bool,
    size_bits: u32,
    fill: [u8; 4],
    stroke: Option<(u32, [u8; 4])>,
    shadow: Option<(u32, u32, u32, [u8; 4])>,
    background: Option<([u8; 4], u32, u32)>,
    align: TextAlign,
    line_height_bits: u32,
    max_width_bits: Option<u32>,
}

impl RasterKey {
    fn new(text: &str, prefix: u32, output: (u32, u32), style: &TextStyle) -> Self {
        Self {
            text: text.to_owned(),
            prefix,
            output,
            family: style.family.clone(),
            weight: style.weight,
            italic: style.italic,
            size_bits: style.size_px.to_bits(),
            fill: style.fill,
            stroke: style
                .stroke
                .as_ref()
                .map(|s| (s.width_px.to_bits(), s.color)),
            shadow: style
                .shadow
                .as_ref()
                .map(|s| (s.dx.to_bits(), s.dy.to_bits(), s.blur_px.to_bits(), s.color)),
            background: style
                .background
                .as_ref()
                .map(|b| (b.color, b.padding_px.to_bits(), b.radius_px.to_bits())),
            align: style.align,
            line_height_bits: style.line_height.to_bits(),
            max_width_bits: style.max_width_px.map(f32::to_bits),
        }
    }
}

/// FNV-1a, so overlay ids do not depend on the standard library's hasher.
struct Fnv(u64);

impl Fnv {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
}

/// Overlay id for a raster key and the caption's own id. `salt` changes the id when two visible captions collide.
fn stable_id(key: &RasterKey, caption_id: &str, salt: u64) -> u64 {
    let mut hasher = Fnv(Fnv::OFFSET);
    key.hash(&mut hasher);
    caption_id.hash(&mut hasher);
    salt.hash(&mut hasher);
    hasher.finish()
}

/// A rasterized caption shared between overlays and the cache.
struct CachedRaster {
    pixels: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    /// Horizontal position of the layout box origin inside the image (see [`RasterImage::origin_x`]).
    origin_x: f32,
}

impl CachedRaster {
    fn from_image(image: RasterImage) -> Self {
        Self {
            width: image.width,
            height: image.height,
            origin_x: image.origin_x,
            pixels: Arc::new(image.data),
        }
    }

    fn bytes(&self) -> usize {
        self.pixels.len()
    }
}

struct CacheEntry {
    raster: Arc<CachedRaster>,
    last_used: u64,
}

/// LRU cache of rasters, capped at [`CACHE_BYTES`] of pixel data.
#[derive(Default)]
struct RasterCache {
    entries: HashMap<RasterKey, CacheEntry>,
    bytes: usize,
    tick: u64,
}

impl RasterCache {
    fn get(&mut self, key: &RasterKey) -> Option<Arc<CachedRaster>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|entry| {
            entry.last_used = tick;
            Arc::clone(&entry.raster)
        })
    }

    /// Stores `raster` under `key`, evicting least recently used entries to stay under the cap. A raster larger
    /// than the whole cap is not stored.
    fn insert(&mut self, key: RasterKey, raster: Arc<CachedRaster>) {
        let bytes = raster.bytes();
        if bytes > CACHE_BYTES {
            return;
        }
        self.tick += 1;
        let tick = self.tick;
        self.bytes += bytes;
        let previous = self.entries.insert(
            key.clone(),
            CacheEntry {
                raster,
                last_used: tick,
            },
        );
        if let Some(previous) = previous {
            self.bytes -= previous.raster.bytes();
        }
        while self.bytes > CACHE_BYTES {
            let victim = self
                .entries
                .iter()
                .filter(|(candidate, _)| **candidate != key)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(candidate, _)| candidate.clone());
            let Some(victim) = victim else {
                break;
            };
            if let Some(removed) = self.entries.remove(&victim) {
                self.bytes -= removed.raster.bytes();
            }
        }
    }

    #[cfg(test)]
    fn total_bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcut_core::time::TimeRange;

    fn caption(id: &str, text: &str, start: f64, end: f64) -> Caption {
        Caption {
            id: id.to_owned(),
            text: text.to_owned(),
            range: TimeRange::new(start, end),
            x: 0.5,
            y: 0.5,
            style: CaptionStyle {
                font_family: FALLBACK_FAMILY.to_owned(),
                ..CaptionStyle::default()
            },
            animation: CaptionAnimation::None,
        }
    }

    #[test]
    fn motion_fade_ramps_in_and_out() {
        let fade = CaptionAnimation::Fade {
            fade_in: 0.5,
            fade_out: 0.5,
        };
        assert!(motion_at(&fade, 0.0, 4.0).opacity.abs() < 1e-6);
        assert!((motion_at(&fade, 0.25, 3.75).opacity - 0.5).abs() < 1e-6);
        assert!((motion_at(&fade, 2.0, 2.0).opacity - 1.0).abs() < 1e-6);
        assert!((motion_at(&fade, 3.75, 0.25).opacity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pop_scale_runs_from_060_through_overshoot_to_one() {
        assert!((pop_scale(0.0) - 0.6).abs() < 1e-6);
        assert!((pop_scale(0.7) - 1.05).abs() < 1e-6);
        assert!((pop_scale(1.0) - 1.0).abs() < 1e-6);
        assert!(pop_scale(0.5) > 0.6 && pop_scale(0.5) < 1.05);
    }

    #[test]
    fn only_impact_and_the_default_family_fall_back_to_anton() {
        let mut renderer = CaptionRenderer::with_extra_fonts(Vec::new());
        let impact_installed = renderer
            .font_families()
            .iter()
            .any(|f| f.eq_ignore_ascii_case("Impact"));
        let impact = renderer.resolved_family("impact");
        if impact_installed {
            assert_eq!(impact, "impact", "an installed Impact is used as requested");
        } else {
            assert_eq!(
                impact, FALLBACK_FAMILY,
                "a missing Impact falls back to Anton"
            );
        }
        assert_eq!(renderer.resolved_family("anton"), "anton");
        assert_eq!(renderer.resolved_family("  Anton  "), "Anton");
        // Any other missing name goes to cosmic-text: the answer is one name, the same on every call.
        let missing = renderer.resolved_family("No Such Display Face");
        assert!(!missing.is_empty());
        assert_eq!(renderer.resolved_family("no such display face"), missing);
        assert!(!renderer.resolved_family("").is_empty());
    }

    #[test]
    fn font_size_is_clamped_to_the_rasterizer_maximum_and_bad_sizes_are_refused() {
        assert_eq!(font_px_for(0.12, 270.0), Some(0.12 * 270.0));
        assert_eq!(font_px_for(1000.0, 1000.0), Some(MAX_FONT_PX));
        for bad in [f32::NAN, f32::INFINITY, 0.0, -0.1] {
            assert_eq!(font_px_for(bad, 270.0), None, "font size {bad}");
        }
    }

    #[test]
    fn stroke_and_box_follow_the_clamped_font_size() {
        let style = CaptionStyle {
            background: Some([0, 0, 0, 255]),
            ..CaptionStyle::default()
        };
        let clamped = text_style(&style, "Anton".to_owned(), 480.0, MAX_FONT_PX);
        assert_eq!(
            clamped.stroke.expect("stroke").width_px,
            style.stroke_width * MAX_FONT_PX
        );
        assert_eq!(
            clamped.background.expect("box").padding_px,
            BOX_PADDING_EM * MAX_FONT_PX
        );
    }

    #[test]
    fn a_raster_that_comes_out_empty_is_remembered_and_not_retried() {
        let mut renderer = CaptionRenderer::with_extra_fonts(Vec::new());
        // A stroke this wide makes the canvas larger than the rasterizer allows, so the raster is empty.
        let mut c = caption("huge", "HUGE", 0.0, 10.0);
        c.style.stroke_width = 1000.0;
        assert!(
            renderer
                .layers_at(std::slice::from_ref(&c), 1.0, 1.0, 480, 270)
                .is_empty()
        );
        assert_eq!(renderer.failed.len(), 1);
        assert!(
            renderer
                .layers_at(std::slice::from_ref(&c), 1.0, 1.0, 480, 270)
                .is_empty()
        );
        assert_eq!(renderer.failed.len(), 1, "the same key is recorded once");
    }

    /// A raster of the given size with no pixels, for placement tests.
    fn raster(width: u32, height: u32) -> CachedRaster {
        CachedRaster {
            pixels: Arc::new(Vec::new()),
            width,
            height,
            origin_x: 0.0,
        }
    }

    /// `(left, top, right, bottom)` of a normalized rectangle, in output pixels.
    fn pixel_edges(dest: NormRect, (ow, oh): (f32, f32)) -> (f64, f64, f64, f64) {
        let (ow, oh) = (f64::from(ow), f64::from(oh));
        (
            dest.x * ow,
            dest.y * oh,
            (dest.x + dest.w) * ow,
            (dest.y + dest.h) * oh,
        )
    }

    #[test]
    fn pop_keeps_the_raster_inside_the_frame_at_every_step() {
        let frame = (480.0_f32, 270.0_f32);
        let tolerance = 1e-3;
        for (w, h) in [(120_u32, 40_u32), (400, 200)] {
            let r = raster(w, h);
            for (x, y) in [(0.0, 0.0), (1.0, 1.0), (0.02, 0.98), (0.5, 0.5), (1.0, 0.0)] {
                for step in 0..=100 {
                    let p = step as f32 / 100.0;
                    let motion = Motion {
                        opacity: 1.0,
                        scale: pop_scale(p),
                        slide: None,
                    };
                    let (left, top, right, bottom) =
                        pixel_edges(place(&r, x, y, None, frame, &motion), frame);
                    assert!(
                        left >= -tolerance && top >= -tolerance,
                        "{w}x{h} at ({x}, {y}) p = {p}: starts at ({left}, {top})"
                    );
                    assert!(
                        right <= 480.0 + tolerance && bottom <= 270.0 + tolerance,
                        "{w}x{h} at ({x}, {y}) p = {p}: ends at ({right}, {bottom})"
                    );
                }
            }
        }
    }

    #[test]
    fn every_slide_edge_rests_inside_and_moves_monotonically_from_outside() {
        let frame = (480.0_f32, 270.0_f32);
        let tolerance = 1e-3;
        let r = raster(120, 40);
        for edge in [
            SlideEdge::Left,
            SlideEdge::Right,
            SlideEdge::Top,
            SlideEdge::Bottom,
        ] {
            for (x, y) in [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0)] {
                let at = |remaining: f32| {
                    let motion = Motion {
                        opacity: 1.0,
                        scale: 1.0,
                        slide: Some((edge, remaining)),
                    };
                    pixel_edges(place(&r, x, y, None, frame, &motion), frame)
                };
                let rest = at(0.0);
                assert!(
                    rest.0 >= -tolerance
                        && rest.1 >= -tolerance
                        && rest.2 <= 480.0 + tolerance
                        && rest.3 <= 270.0 + tolerance,
                    "{edge:?} at ({x}, {y}) rests outside the frame: {rest:?}"
                );
                let start = at(1.0);
                match edge {
                    SlideEdge::Left => assert!(start.2 <= tolerance, "{start:?}"),
                    SlideEdge::Right => assert!(start.0 >= 480.0 - tolerance, "{start:?}"),
                    SlideEdge::Top => assert!(start.3 <= tolerance, "{start:?}"),
                    SlideEdge::Bottom => assert!(start.1 >= 270.0 - tolerance, "{start:?}"),
                }
                // From the start to the rest, each step moves toward the rest position and never past it.
                let mut previous = start;
                for step in 1..=20 {
                    let now = at(1.0 - step as f32 / 20.0);
                    let ok = match edge {
                        SlideEdge::Left => now.0 >= previous.0 - tolerance,
                        SlideEdge::Top => now.1 >= previous.1 - tolerance,
                        SlideEdge::Right => now.0 <= previous.0 + tolerance,
                        SlideEdge::Bottom => now.1 <= previous.1 + tolerance,
                    };
                    assert!(
                        ok,
                        "{edge:?} at ({x}, {y}) step {step}: {previous:?} -> {now:?}"
                    );
                    previous = now;
                }
                assert!(
                    (previous.0 - rest.0).abs() < tolerance
                        && (previous.1 - rest.1).abs() < tolerance
                );
            }
        }
    }

    #[test]
    fn a_settled_caption_lands_on_whole_output_pixels() {
        let frame = (480.0_f32, 270.0_f32);
        let r = raster(121, 37);
        for x in [0.123, 0.5, 0.777] {
            for y in [0.31, 0.5] {
                let (left, top, _, _) =
                    pixel_edges(place(&r, x, y, Some(100.3), frame, &Motion::REST), frame);
                assert!(
                    (left - left.round()).abs() < 1e-3 && (top - top.round()).abs() < 1e-3,
                    "({x}, {y}) placed at ({left}, {top})"
                );
            }
        }
    }

    #[test]
    fn clamp_inside_keeps_segments_in_the_frame() {
        assert_eq!(clamp_inside(-10.0, 20.0, 100.0), 0.0);
        assert_eq!(clamp_inside(95.0, 20.0, 100.0), 80.0);
        assert_eq!(clamp_inside(5.0, 200.0, 100.0), 0.0);
    }

    #[test]
    fn stable_id_depends_on_key_caption_and_salt() {
        let style = TextStyle::default();
        let key = RasterKey::new("A", 1, (640, 360), &style);
        let other = RasterKey::new("B", 1, (640, 360), &style);
        assert_eq!(stable_id(&key, "c1", 0), stable_id(&key, "c1", 0));
        assert_ne!(stable_id(&key, "c1", 0), stable_id(&other, "c1", 0));
        assert_ne!(stable_id(&key, "c1", 0), stable_id(&key, "c2", 0));
        assert_ne!(stable_id(&key, "c1", 0), stable_id(&key, "c1", 1));
    }

    #[test]
    fn cache_evicts_least_recently_used_under_the_cap() {
        let mut cache = RasterCache::default();
        let big = |fill: u8| {
            Arc::new(CachedRaster {
                pixels: Arc::new(vec![fill; 24 * 1024 * 1024]),
                width: 1,
                height: 1,
                origin_x: 0.0,
            })
        };
        let style = TextStyle::default();
        let key = |text: &str| RasterKey::new(text, 0, (1, 1), &style);
        cache.insert(key("a"), big(1));
        cache.insert(key("b"), big(2));
        assert!(cache.get(&key("a")).is_some(), "touch a so b is the oldest");
        cache.insert(key("c"), big(3));
        assert!(cache.total_bytes() <= CACHE_BYTES);
        assert!(
            cache.get(&key("b")).is_none(),
            "b was the least recently used"
        );
        assert!(cache.get(&key("a")).is_some());
        assert!(cache.get(&key("c")).is_some());
    }

    #[test]
    fn duplicate_captions_get_distinct_ids() {
        let mut renderer = CaptionRenderer::new();
        let captions = vec![
            caption("first", "one", 0.0, 1.0),
            caption("second", "two", 0.0, 1.0),
        ];
        let layers = renderer.layers_at(&captions, 0.5, 1.0, 480, 270);
        assert_eq!(layers.len(), 2);
        assert_ne!(layers[0].id, layers[1].id);
    }
}

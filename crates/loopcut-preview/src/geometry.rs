//! Pure sizing rules of the preview: how big a frame to render, and how wide to decode the source for it.
//!
//! Nothing here touches FFmpeg or the GPU, so every rule is unit-tested directly.
//!
//! # Decode width
//!
//! Preview decodes the whole frame (no crop) and crops in the compositor, so dragging the crop never waits for a
//! decode. The decoded width must still hold enough stored samples for the crop at the render size:
//! `need = max(render_w / crop_w, render_h / crop_h)`, in stored pixels across the width.
//!
//! * SAR cancels. The render size and the crop are both in display space, and the compositor maps the crop onto the
//!   whole stored width. The stored samples across the crop are `stored_w × crop_w`, and they must cover `render_w`
//!   output pixels, so the need is `render_w / crop_w`. The display width is `stored_w × SAR`, and SAR scales the
//!   render's display space and the frame's display width by the same factor, so it never appears in the need.
//! * Both orientations are covered. Upright, the display width is the stored width, so `render_w / crop_w` is the need.
//!   After a quarter turn the display height is the stored width, so `render_h / crop_h` is the need. Taking the larger
//!   one makes the need depend only on the render and the crop, not on which axis is stored horizontally. A turn that
//!   keeps the render the same therefore keeps the decode. A turn that changes the render (a fixed output width with a
//!   different aspect) changes the need, and the 2x window below decides whether the decode is kept.
//!
//! The decode is `need` plus headroom, clamped into `[64, min(coded, cap)]`.
//!
//! Two refinements keep crop drags from re-decoding on every step:
//! * a fresh decode asks for 25% more width than `need` (headroom), so a small crop shrink is covered;
//! * a decode that is still at least `need` and at most twice `need` is kept.

use loopcut_core::NormRect;

use crate::types::{PreviewQuality, Viewport};

/// Pixel count of 1280×720. Playback frames are capped at this many pixels.
pub const PLAYBACK_MAX_PIXELS: u64 = 1280 * 720;

/// Narrowest decode the preview asks for.
pub const MIN_DECODE_WIDTH: u32 = 64;

/// Widest decode when the effects renderer is a hardware adapter.
pub const HARDWARE_MAX_DECODE_WIDTH: u32 = 1920;

/// Widest decode when the effects renderer is WARP: WARP uploads run at about 11 MB/s.
pub const SOFTWARE_MAX_DECODE_WIDTH: u32 = 640;

/// Decode headroom over the width a crop needs.
const DECODE_HEADROOM: f64 = 1.25;

/// A kept decode may be this many times wider than needed before it is replaced.
const DECODE_EXCESS: f64 = 2.0;

/// Smallest crop fraction used in the need. A degenerate crop must not divide by zero.
const MIN_CROP: f64 = 1e-6;

/// Largest accepted viewport side, in device pixels.
const MAX_VIEWPORT_SIDE: u32 = 16_384;

/// Largest accepted device pixel ratio.
const MAX_DPR: f32 = 16.0;

/// Checks a viewport before any sizing uses it.
pub fn validate_viewport(viewport: Viewport) -> Result<(), String> {
    if viewport.width == 0 || viewport.height == 0 {
        return Err(format!(
            "{}x{} has an empty side",
            viewport.width, viewport.height
        ));
    }
    if viewport.width > MAX_VIEWPORT_SIDE || viewport.height > MAX_VIEWPORT_SIDE {
        return Err(format!(
            "{}x{} is larger than {MAX_VIEWPORT_SIDE} pixels",
            viewport.width, viewport.height
        ));
    }
    if !(viewport.dpr.is_finite() && viewport.dpr > 0.0 && viewport.dpr <= MAX_DPR) {
        return Err(format!(
            "device pixel ratio {} is not in (0, {MAX_DPR}]",
            viewport.dpr
        ));
    }
    Ok(())
}

/// The viewport in device pixels, at least 2 on each side.
pub fn viewport_box(viewport: Viewport) -> (u32, u32) {
    let scale = f64::from(viewport.dpr);
    let w = (f64::from(viewport.width) * scale).round().max(2.0) as u32;
    let h = (f64::from(viewport.height) * scale).round().max(2.0) as u32;
    (w, h)
}

/// Rounds to the nearest even size, never below 2.
fn even_size(value: f64) -> u32 {
    let rounded = value.round().max(2.0) as u32;
    (rounded & !1).max(2)
}

/// Largest even size with the aspect of `w`×`h` that fits in `box_w`×`box_h`. Never enlarges.
pub fn fit_within(w: u32, h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let w = w.max(1);
    let h = h.max(1);
    let scale = (f64::from(box_w) / f64::from(w))
        .min(f64::from(box_h) / f64::from(h))
        .min(1.0);
    (
        even_size(f64::from(w) * scale),
        even_size(f64::from(h) * scale),
    )
}

/// Scales `w`×`h` down to at most `max_pixels`, keeping the aspect.
pub fn cap_pixels(w: u32, h: u32, max_pixels: u64) -> (u32, u32) {
    let pixels = u64::from(w) * u64::from(h);
    if pixels <= max_pixels {
        return (w, h);
    }
    let scale = (max_pixels as f64 / pixels as f64).sqrt();
    (
        even_size(f64::from(w) * scale),
        even_size(f64::from(h) * scale),
    )
}

/// Size of one output frame at `quality`: the output fitted into the viewport, and for playback also capped at
/// [`PLAYBACK_MAX_PIXELS`]. Outputs smaller than the panel are not enlarged.
pub fn render_size(output: (u32, u32), viewport: Viewport, quality: PreviewQuality) -> (u32, u32) {
    let (box_w, box_h) = viewport_box(viewport);
    let (w, h) = fit_within(output.0, output.1, box_w, box_h);
    match quality {
        PreviewQuality::Full => (w, h),
        PreviewQuality::Reduced => cap_pixels(w, h, PLAYBACK_MAX_PIXELS),
    }
}

/// Stored width, before headroom, that a crop of `crop` shown at `render` (display pixels) needs. The larger of the two
/// orientations' needs, so the value does not depend on orientation. See the module documentation.
pub fn decode_need(render: (u32, u32), crop: NormRect) -> f64 {
    let crop = crop.clamped();
    let upright = f64::from(render.0) / crop.w.max(MIN_CROP);
    let turned = f64::from(render.1) / crop.h.max(MIN_CROP);
    upright.max(turned)
}

/// The widest stored width the decode can use: the coded width, capped, kept even and at least 2.
pub fn decode_limit(coded_w: u32, cap: u32) -> u32 {
    (coded_w.min(cap) & !1).max(2)
}

/// Decoded width (stored pixels) for a request of `needed` pixels. Keeps `current` when it is still in the window
/// `[needed, DECODE_EXCESS × needed]`, and otherwise picks `needed` plus headroom, clamped to `[floor, limit]`.
fn choose_width(needed: f64, limit: u32, current: Option<u32>) -> u32 {
    let floor = MIN_DECODE_WIDTH.min(limit);
    let need = if needed.is_finite() {
        (needed.ceil().min(f64::from(limit)) as u32).clamp(floor, limit)
    } else {
        limit
    };
    if let Some(kept) = current
        && kept >= need
        && kept <= limit
        && f64::from(kept) <= f64::from(need) * DECODE_EXCESS
    {
        return kept;
    }
    let target = (f64::from(need) * DECODE_HEADROOM)
        .ceil()
        .min(f64::from(limit)) as u32;
    target.clamp(floor, limit)
}

/// Stored-pixel size to decode the source at, for a crop of `crop` shown at `render` (display pixels).
///
/// `coded` is the stored size, `cap` the widest decode the effects renderer accepts, and `current` the size of the
/// decode in use, when the source is the same. The height keeps the coded aspect.
pub fn decode_size(
    coded: (u32, u32),
    crop: NormRect,
    render: (u32, u32),
    cap: u32,
    current: Option<(u32, u32)>,
) -> (u32, u32) {
    let (coded_w, coded_h) = (coded.0.max(2), coded.1.max(2));
    let limit = decode_limit(coded_w, cap);
    let width =
        (choose_width(decode_need(render, crop), limit, current.map(|size| size.0)) & !1).max(2);
    let height = (f64::from(width) * f64::from(coded_h) / f64::from(coded_w)).round() as u32;
    let height = (height & !1).clamp(2, (coded_h & !1).max(2));
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(width: u32, height: u32, dpr: f32) -> Viewport {
        Viewport { width, height, dpr }
    }

    #[test]
    fn viewport_rejects_unusable_values() {
        assert!(validate_viewport(viewport(0, 100, 1.0)).is_err());
        assert!(validate_viewport(viewport(100, 100, 0.0)).is_err());
        assert!(validate_viewport(viewport(100, 100, f32::NAN)).is_err());
        assert!(validate_viewport(viewport(1280, 720, 2.0)).is_ok());
    }

    #[test]
    fn fit_never_enlarges_and_keeps_aspect() {
        assert_eq!(fit_within(480, 270, 640, 360), (480, 270));
        assert_eq!(fit_within(1920, 1080, 640, 360), (640, 360));
        assert_eq!(fit_within(1080, 1920, 640, 360), (202, 360));
        assert_eq!(fit_within(1000, 1000, 1, 1), (2, 2));
    }

    #[test]
    fn render_size_follows_viewport_and_playback_cap() {
        let output = (1920, 1080);
        assert_eq!(
            render_size(output, viewport(960, 540, 2.0), PreviewQuality::Full),
            (1920, 1080)
        );
        assert_eq!(
            render_size(output, viewport(960, 540, 2.0), PreviewQuality::Reduced),
            (1280, 720)
        );
        // A portrait output keeps its aspect under the same pixel budget.
        let (w, h) = render_size(
            (1080, 1920),
            viewport(4000, 4000, 1.0),
            PreviewQuality::Reduced,
        );
        assert!(
            h > w && u64::from(w) * u64::from(h) <= PLAYBACK_MAX_PIXELS,
            "{w}x{h}"
        );
    }

    #[test]
    fn small_clips_decode_at_their_coded_width() {
        let size = decode_size((320, 240), NormRect::FULL, (480, 270), 1920, None);
        assert_eq!(size, (320, 240));
    }

    #[test]
    fn decode_covers_the_crop_with_headroom() {
        let half = NormRect {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        // A 1280-wide render of a half-width crop needs 2560 stored pixels: clamped to the 1920 cap.
        assert_eq!(
            decode_size((1920, 1080), half, (1280, 720), 1920, None),
            (1920, 1080)
        );
        // A full-width crop needs 1280, with 25% headroom: 1600 wide.
        assert_eq!(
            decode_size((1920, 1080), NormRect::FULL, (1280, 720), 1920, None),
            (1600, 900)
        );
        // The software cap wins.
        assert_eq!(
            decode_size((1920, 1080), NormRect::FULL, (1280, 720), 640, None),
            (640, 360)
        );
    }

    #[test]
    fn a_kept_decode_survives_small_crop_changes() {
        let full = NormRect::FULL;
        let first = decode_size((1920, 1080), full, (1280, 720), 1920, None);
        assert_eq!(first.0, 1600);
        // Shrinking the crop a little needs more pixels, but the 25% headroom already covers it.
        let shrunk = NormRect {
            x: 0.1,
            y: 0.1,
            w: 0.9,
            h: 0.9,
        };
        let render = (1280 * 9 / 10, 720 * 9 / 10);
        assert_eq!(
            decode_size((1920, 1080), shrunk, render, 1920, Some(first)),
            first
        );
        // Growing the crop back keeps the wide decode while it is within twice the need.
        assert_eq!(
            decode_size((1920, 1080), full, (1280, 720), 1920, Some(first)),
            first
        );
        // A decode more than twice as wide as needed is replaced.
        assert_eq!(
            decode_size((1920, 1080), full, (400, 225), 1920, Some(first)),
            (500, 280)
        );
    }

    #[test]
    fn the_need_takes_the_larger_of_both_orientations() {
        // Display height maps to stored width after a quarter turn, so the crop's height can drive the decode.
        // The render always keeps the crop's own aspect (the output is cut from the same crop), so the renders here do too.
        let half_tall = NormRect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 0.5,
        };
        // A 1080x1920 frame cropped to its top half is 1080x960 pixels, shown at 540x480. The upright width needs 540
        // stored columns; the turned rows need 480 / 0.5 = 960. The larger one, 960, drives the decode.
        assert_eq!(decode_need((540, 480), half_tall), 960.0);
        assert_eq!(
            decode_size((1080, 1920), half_tall, (540, 480), 1920, None),
            (1080, 1920),
            "960 needed, 1200 with headroom, capped at the 1080 stored width"
        );
        // A 1920x1080 frame cropped the same way is 1920x540, shown at 1280x360. Now the upright width is the larger need.
        assert_eq!(decode_need((1280, 360), half_tall), 1280.0);
        assert_eq!(
            decode_size((1920, 1080), half_tall, (1280, 360), 1920, None),
            (1600, 900)
        );
    }

    #[test]
    fn quarter_turns_share_one_decode() {
        // With the full frame, a portrait render and its landscape turn need the same stored width.
        let portrait = decode_size((1920, 1080), NormRect::FULL, (405, 720), 1920, None);
        let landscape = decode_size((1920, 1080), NormRect::FULL, (720, 405), 1920, None);
        assert_eq!(portrait, landscape);
        assert_eq!(decode_need((405, 720), NormRect::FULL), 720.0);
    }

    #[test]
    fn decode_dimensions_are_even() {
        let (w, h) = decode_size((1919, 1079), NormRect::FULL, (1001, 563), 1920, None);
        assert!(w % 2 == 0 && h % 2 == 0, "{w}x{h}");
        assert!(w <= 1918 && h <= 1078);
    }
}

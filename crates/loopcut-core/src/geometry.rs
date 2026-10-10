//! Crop / rotation / flip math.
//!
//! Conventions:
//! * *Stored* space is the decoded frame as FFmpeg delivers it with `-noautorotate` (coded size).
//! * *Display* space is what the user sees: stored space rotated by the source rotation plus the
//!   user rotation (both clockwise, multiples of 90°), then flipped.
//! * Crop rectangles are normalized `[0,1]` coordinates in display space, because the crop overlay is
//!   drawn on the displayed image.
//!
//! Export crops/scales in stored space inside FFmpeg (cheap, full resolution) and lets the compositor
//! apply the rotation/flip to the small output frames. Preview applies everything in the shader.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Normalized point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Normalized rectangle (display space for crops).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl NormRect {
    pub const FULL: Self = Self { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    /// Clamps into the unit square, keeping a minimum size.
    pub fn clamped(self) -> Self {
        let min = 1e-4;
        let w = self.w.clamp(min, 1.0);
        let h = self.h.clamp(min, 1.0);
        let x = self.x.clamp(0.0, 1.0 - w);
        let y = self.y.clamp(0.0, 1.0 - h);
        Self { x, y, w, h }
    }

    pub fn is_full(&self) -> bool {
        self.x <= 1e-9 && self.y <= 1e-9 && self.w >= 1.0 - 1e-9 && self.h >= 1.0 - 1e-9
    }
}

/// Integer pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Orientation applied on top of the stored frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Orientation {
    /// Total clockwise rotation in degrees: 0, 90, 180 or 270.
    pub rotation: u16,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Orientation {
    /// Combines the source's display-matrix rotation with the user's rotation and flips.
    pub fn new(source_rotation: u16, user_rotation: u16, flip_h: bool, flip_v: bool) -> Self {
        let rotation = (normalize_rotation(source_rotation) + normalize_rotation(user_rotation)) % 360;
        Self { rotation, flip_h, flip_v }
    }

    pub fn swaps_axes(&self) -> bool {
        self.rotation % 180 == 90
    }

    /// Maps a normalized stored-space point to display space.
    pub fn stored_to_display(&self, p: Point) -> Point {
        let mut q = p;
        for _ in 0..(self.rotation / 90) {
            // 90° clockwise: (u, v) -> (1 - v, u)
            q = Point { x: 1.0 - q.y, y: q.x };
        }
        if self.flip_h {
            q.x = 1.0 - q.x;
        }
        if self.flip_v {
            q.y = 1.0 - q.y;
        }
        q
    }

    /// Maps a normalized display-space point back to stored space.
    pub fn display_to_stored(&self, p: Point) -> Point {
        let mut q = p;
        if self.flip_v {
            q.y = 1.0 - q.y;
        }
        if self.flip_h {
            q.x = 1.0 - q.x;
        }
        for _ in 0..(self.rotation / 90) {
            // inverse of 90° clockwise: (x, y) -> (y, 1 - x)
            q = Point { x: q.y, y: 1.0 - q.x };
        }
        q
    }

    /// Maps a display-space rectangle to stored space (exact for multiples of 90°).
    pub fn display_rect_to_stored(&self, r: NormRect) -> NormRect {
        let a = self.display_to_stored(Point { x: r.x, y: r.y });
        let b = self.display_to_stored(Point { x: r.x + r.w, y: r.y + r.h });
        NormRect {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            w: (a.x - b.x).abs(),
            h: (a.y - b.y).abs(),
        }
    }

    /// 2×3 affine (row-major) mapping display UV → stored UV, for the compositor's sampling.
    pub fn display_to_stored_affine(&self) -> [f32; 6] {
        let o = self.display_to_stored(Point { x: 0.0, y: 0.0 });
        let ex = self.display_to_stored(Point { x: 1.0, y: 0.0 });
        let ey = self.display_to_stored(Point { x: 0.0, y: 1.0 });
        [
            (ex.x - o.x) as f32,
            (ey.x - o.x) as f32,
            o.x as f32,
            (ex.y - o.y) as f32,
            (ey.y - o.y) as f32,
            o.y as f32,
        ]
    }
}

pub fn normalize_rotation(deg: u16) -> u16 {
    let d = deg % 360;
    // Snap to the nearest quarter turn (display matrices sometimes carry tiny float errors).
    (((u32::from(d) + 45) / 90 * 90) % 360) as u16
}

/// Size of the displayed image (after rotation and sample aspect) for a coded frame.
pub fn display_size(coded_w: u32, coded_h: u32, sample_aspect: f64, orientation: Orientation) -> (u32, u32) {
    let sar = if sample_aspect.is_finite() && sample_aspect > 0.0 { sample_aspect } else { 1.0 };
    let w = (f64::from(coded_w) * sar).round().max(1.0) as u32;
    if orientation.swaps_axes() { (coded_h, w) } else { (w, coded_h) }
}

fn even_floor(v: f64) -> u32 {
    let v = v.max(0.0).floor() as u32;
    v & !1
}

/// Converts a display-space crop into an even-aligned pixel rectangle in stored (coded) space,
/// suitable for FFmpeg's `crop=w:h:x:y:exact=1` on 4:2:0 video.
pub fn stored_crop_px(coded_w: u32, coded_h: u32, orientation: Orientation, crop: NormRect) -> PixelRect {
    let s = orientation.display_rect_to_stored(crop.clamped());
    let fw = f64::from(coded_w);
    let fh = f64::from(coded_h);
    let mut x = even_floor(s.x * fw);
    let mut y = even_floor(s.y * fh);
    let mut w = even_floor((s.w * fw).round()).max(2);
    let mut h = even_floor((s.h * fh).round()).max(2);
    let max_w = coded_w & !1;
    let max_h = coded_h & !1;
    w = w.min(max_w.max(2));
    h = h.min(max_h.max(2));
    if x + w > coded_w {
        x = coded_w.saturating_sub(w) & !1;
    }
    if y + h > coded_h {
        y = coded_h.saturating_sub(h) & !1;
    }
    PixelRect { x, y, w, h }
}

/// How the user sized the output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "mode", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[ts(export)]
pub enum OutputSize {
    /// Fixed width, height follows the crop aspect.
    Width { width: u32 },
    /// Fit inside a box keeping aspect (platform presets).
    Fit { max_width: u32, max_height: u32 },
    /// Scale factor of the cropped display size.
    Scale { factor: f64 },
}

/// Output pixel size (display orientation, even dimensions, never upscaled beyond 4× or below 16 px).
pub fn output_dimensions(crop_display_w: f64, crop_display_h: f64, size: OutputSize) -> (u32, u32) {
    let aspect = if crop_display_h > 0.0 { crop_display_w / crop_display_h } else { 1.0 };
    let (w, h) = match size {
        OutputSize::Width { width } => {
            let w = f64::from(width);
            (w, w / aspect)
        }
        OutputSize::Fit { max_width, max_height } => {
            let (mw, mh) = (f64::from(max_width), f64::from(max_height));
            let s = (mw / crop_display_w).min(mh / crop_display_h).min(1.0);
            (crop_display_w * s, crop_display_h * s)
        }
        OutputSize::Scale { factor } => (crop_display_w * factor, crop_display_h * factor),
    };
    let even = |v: f64| -> u32 { ((v.round().max(16.0) as u32) + 1) & !1 };
    (even(w), even(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9
    }

    #[test]
    fn rotation_90_moves_top_left_to_top_right() {
        let o = Orientation::new(90, 0, false, false);
        assert!(close(o.stored_to_display(Point { x: 0.0, y: 0.0 }), Point { x: 1.0, y: 0.0 }));
        assert!(o.swaps_axes());
    }

    #[test]
    fn rotations_combine_and_normalize() {
        assert_eq!(Orientation::new(270, 180, false, false).rotation, 90);
        assert_eq!(normalize_rotation(359), 0);
        assert_eq!(normalize_rotation(89), 90);
    }

    #[test]
    fn portrait_phone_crop_maps_to_stored_space() {
        // 1920x1080 coded, displayed rotated 90° (1080x1920 portrait). Crop the top half of the display.
        let o = Orientation::new(90, 0, false, false);
        let r = stored_crop_px(1920, 1080, o, NormRect { x: 0.0, y: 0.0, w: 1.0, h: 0.5 });
        assert_eq!(r, PixelRect { x: 0, y: 0, w: 960, h: 1080 });
    }

    #[test]
    fn crop_is_even_aligned_and_inside() {
        let r = stored_crop_px(1919, 1079, Orientation::default(), NormRect { x: 0.333, y: 0.111, w: 0.5, h: 0.77 });
        assert_eq!(r.x % 2, 0);
        assert_eq!(r.y % 2, 0);
        assert_eq!(r.w % 2, 0);
        assert_eq!(r.h % 2, 0);
        assert!(r.x + r.w <= 1919 && r.y + r.h <= 1079);
    }

    #[test]
    fn output_dimensions_are_even() {
        assert_eq!(output_dimensions(1920.0, 1080.0, OutputSize::Width { width: 480 }), (480, 270));
        assert_eq!(output_dimensions(1000.0, 1000.0, OutputSize::Width { width: 333 }), (334, 334));
        assert_eq!(
            output_dimensions(1920.0, 1080.0, OutputSize::Fit { max_width: 640, max_height: 640 }),
            (640, 360)
        );
    }

    proptest! {
        #[test]
        fn display_stored_roundtrip(rot in 0u16..4, fh: bool, fv: bool, x in 0.0f64..1.0, y in 0.0f64..1.0) {
            let o = Orientation::new(rot * 90, 0, fh, fv);
            let p = Point { x, y };
            prop_assert!(close(o.display_to_stored(o.stored_to_display(p)), p));
        }

        #[test]
        fn affine_matches_point_mapping(rot in 0u16..4, fh: bool, fv: bool, x in 0.0f64..1.0, y in 0.0f64..1.0) {
            let o = Orientation::new(rot * 90, 0, fh, fv);
            let m = o.display_to_stored_affine();
            let s = o.display_to_stored(Point { x, y });
            let ax = f64::from(m[0]) * x + f64::from(m[1]) * y + f64::from(m[2]);
            let ay = f64::from(m[3]) * x + f64::from(m[4]) * y + f64::from(m[5]);
            prop_assert!((ax - s.x).abs() < 1e-6 && (ay - s.y).abs() < 1e-6);
        }
    }
}

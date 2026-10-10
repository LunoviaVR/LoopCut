//! GIF timing rules.
//!
//! GIF frame delays are integer centiseconds. Browsers render delays of 0–1 cs (≤ 10 ms) as 100 ms,
//! so LoopCut never emits a delay below 2 cs, which caps playback at 50 fps.

use crate::time::Rational;

/// Smallest delay browsers honor.
pub const MIN_DELAY_CS: u16 = 2;
/// Highest frame rate a GIF can play back at reliably.
pub const MAX_GIF_FPS: f64 = 50.0;

/// Frame rates whose frame duration is an exact number of centiseconds.
pub const EXACT_GIF_RATES: [Rational; 6] = [
    Rational::integer(50),
    Rational::integer(25),
    Rational::integer(20),
    Rational::new(25, 2),
    Rational::integer(10),
    Rational::integer(5),
];

/// Clamps a requested frame rate to what a GIF can play.
pub fn clamp_gif_fps(fps: Rational) -> Rational {
    if fps.as_f64() > MAX_GIF_FPS {
        Rational::integer(50)
    } else {
        fps
    }
}

/// Returns the exact-centisecond rate closest to `fps` (used by the "snap fps" option).
pub fn snap_fps(fps: Rational) -> Rational {
    let target = fps.as_f64();
    EXACT_GIF_RATES
        .iter()
        .copied()
        .min_by(|a, b| {
            (a.as_f64() - target)
                .abs()
                .total_cmp(&(b.as_f64() - target).abs())
        })
        .unwrap_or(Rational::integer(20))
}

/// Centisecond delays for `count` frames shown at `fps`, rounded cumulatively so the total
/// duration stays exact (e.g. 15 fps → 7,7,6,7,7,6,…) and never below [`MIN_DELAY_CS`].
pub fn delays_cs(count: usize, fps: Rational) -> Vec<u16> {
    let fps = clamp_gif_fps(fps);
    let per_frame_cs = 100.0 / fps.as_f64();
    let mut out = Vec::with_capacity(count);
    let mut prev = 0i64;
    for i in 1..=count {
        let boundary = (i as f64 * per_frame_cs).round() as i64;
        let d = (boundary - prev).max(i64::from(MIN_DELAY_CS));
        out.push(u16::try_from(d).unwrap_or(u16::MAX));
        prev += d;
    }
    out
}

/// The frame rate a viewer actually sees after centisecond rounding.
pub fn effective_fps(fps: Rational) -> f64 {
    let d = delays_cs(100, fps);
    let total: u32 = d.iter().map(|&x| u32::from(x)).sum();
    if total == 0 {
        0.0
    } else {
        100.0 * d.len() as f64 / f64::from(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rates_have_uniform_delays() {
        assert!(delays_cs(10, Rational::integer(20)).iter().all(|&d| d == 5));
        assert!(delays_cs(10, Rational::new(25, 2)).iter().all(|&d| d == 8));
    }

    #[test]
    fn fifteen_fps_alternates_and_keeps_total() {
        let d = delays_cs(15, Rational::integer(15));
        assert_eq!(d.iter().map(|&x| u32::from(x)).sum::<u32>(), 100);
        assert!(d.iter().all(|&x| x == 6 || x == 7));
    }

    #[test]
    fn caps_at_fifty_fps() {
        assert!(
            delays_cs(20, Rational::integer(60))
                .iter()
                .all(|&d| d >= MIN_DELAY_CS)
        );
        assert_eq!(clamp_gif_fps(Rational::integer(60)), Rational::integer(50));
        assert!((effective_fps(Rational::integer(60)) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn snaps_to_nearest_exact_rate() {
        assert_eq!(snap_fps(Rational::integer(24)), Rational::integer(25));
        assert_eq!(snap_fps(Rational::integer(15)), Rational::new(25, 2));
        assert_eq!(snap_fps(Rational::integer(30)), Rational::integer(25));
    }
}

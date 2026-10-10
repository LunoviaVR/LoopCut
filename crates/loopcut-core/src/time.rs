//! Time and frame-rate primitives.
//!
//! All edit decisions are stored in *source time*: seconds measured from the selected video
//! stream's `start_time` (as reported by ffprobe), so that the source monitor and the frame server
//! agree on a single timebase.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A positive rational frame rate, e.g. `30000/1001`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub const fn integer(n: u32) -> Self {
        Self { num: n, den: 1 }
    }

    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            f64::from(self.num) / f64::from(self.den)
        }
    }

    pub fn is_valid(self) -> bool {
        self.num > 0 && self.den > 0
    }

    /// Duration of one frame in seconds.
    pub fn frame_duration(self) -> f64 {
        f64::from(self.den) / f64::from(self.num)
    }

    /// Parses ffprobe-style rates such as `"30000/1001"`, `"25/1"` or `"24"`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (num, den) = match s.split_once('/') {
            Some((n, d)) => (n.trim().parse().ok()?, d.trim().parse().ok()?),
            None => (s.parse().ok()?, 1),
        };
        let r = Self { num, den };
        r.is_valid().then_some(r)
    }

    /// Best rational approximation of `fps` with a small denominator (for UI-entered values like 12.5).
    pub fn from_f64(fps: f64) -> Option<Self> {
        if !fps.is_finite() || fps <= 0.0 {
            return None;
        }
        for den in [1u32, 2, 4, 5, 8, 10, 1001, 100, 1000] {
            let num = (fps * f64::from(den)).round();
            if num >= 1.0 && num <= f64::from(u32::MAX) && ((num / f64::from(den)) - fps).abs() < 1e-6 {
                return Some(Self { num: num as u32, den });
            }
        }
        let num = (fps * 1000.0).round();
        (num >= 1.0).then(|| Self { num: num as u32, den: 1000 })
    }
}

impl std::fmt::Display for Rational {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

/// A half-open range of source time `[start, end)` in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn new(start: f64, end: f64) -> Self {
        Self { start, end }
    }

    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    pub fn is_valid(&self) -> bool {
        self.start.is_finite() && self.end.is_finite() && self.end > self.start && self.start >= 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_rates() {
        assert_eq!(Rational::parse("30000/1001"), Some(Rational::new(30000, 1001)));
        assert_eq!(Rational::parse("25"), Some(Rational::integer(25)));
        assert_eq!(Rational::parse("0/0"), None);
        assert_eq!(Rational::parse("abc"), None);
    }

    #[test]
    fn from_f64_prefers_small_denominators() {
        assert_eq!(Rational::from_f64(12.5), Some(Rational::new(25, 2)));
        assert_eq!(Rational::from_f64(20.0), Some(Rational::integer(20)));
        assert_eq!(Rational::from_f64(29.97002997), Some(Rational::new(30000, 1001)));
        assert_eq!(Rational::from_f64(0.0), None);
    }
}

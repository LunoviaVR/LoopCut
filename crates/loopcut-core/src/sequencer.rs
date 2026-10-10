//! The Sequencer owns output time.
//!
//! Frames are decoded once at the source's own rate; the sequencer decides, for every output frame,
//! which *source time* (or blend of two source times) to show and for how long. Changing the output
//! frame rate, speed, direction or crossfade therefore never requires re-decoding.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

use crate::gif_timing;
use crate::project::{Clip, Direction, Timing};
use crate::time::{Rational, TimeRange};

pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;
/// Hard cap protecting memory and GIF sanity (≈ 2.5 minutes at 20 fps).
pub const MAX_OUTPUT_FRAMES: usize = 3000;

/// What a single output frame shows.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[ts(export)]
pub enum Sample {
    /// The source frame displayed at source time `t`.
    At { t: f64 },
    /// Cross-dissolve: `(1 - weight_b) * frame(a) + weight_b * frame(b)`.
    Blend { a: f64, b: f64, weight_b: f32 },
}

impl Sample {
    pub fn source_times(&self) -> impl Iterator<Item = f64> {
        let (a, b) = match *self {
            Sample::At { t } => (t, None),
            Sample::Blend { a, b, .. } => (a, Some(b)),
        };
        std::iter::once(a).chain(b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlannedFrame {
    pub sample: Sample,
    /// GIF delay in centiseconds (video encoders use the constant `fps` instead).
    pub delay_cs: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SequencePlan {
    pub fps: Rational,
    pub frames: Vec<PlannedFrame>,
    /// Output duration in seconds (frame count / fps).
    pub duration: f64,
}

impl SequencePlan {
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Every distinct source time referenced by the plan, ascending.
    pub fn source_times(&self) -> Vec<f64> {
        let mut v: Vec<f64> = self.frames.iter().flat_map(|f| f.sample.source_times()).collect();
        v.sort_by(f64::total_cmp);
        v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        v
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum SequenceError {
    #[error("the clip has no kept segments")]
    Empty,
    #[error("invalid segment {0:?}")]
    InvalidSegment(TimeRange),
    #[error("invalid output frame rate")]
    InvalidFps,
    #[error("the result would have {0} frames; the limit is {MAX_OUTPUT_FRAMES}")]
    TooLong(usize),
}

/// Ranges of source time that must be decoded for this clip (segments plus held frames).
pub fn required_source_ranges(clip: &Clip) -> Vec<TimeRange> {
    let mut ranges: Vec<TimeRange> = clip.segments.clone();
    for h in &clip.holds {
        ranges.push(TimeRange::new(h.at, h.at + 1e-3));
    }
    ranges.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<TimeRange> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

#[derive(Debug, Clone, Copy)]
enum Piece {
    Play { start: f64, end: f64 },
    Hold { at: f64, duration: f64 },
}

impl Piece {
    fn output_duration(&self, speed: f64) -> f64 {
        match *self {
            Piece::Play { start, end } => (end - start) / speed,
            Piece::Hold { duration, .. } => duration,
        }
    }

    fn source_time(&self, local_out: f64, speed: f64) -> f64 {
        match *self {
            Piece::Play { start, end } => (start + local_out * speed).min(end),
            Piece::Hold { at, .. } => at,
        }
    }
}

fn pieces(clip: &Clip) -> Result<Vec<Piece>, SequenceError> {
    if clip.segments.is_empty() {
        return Err(SequenceError::Empty);
    }
    let mut segments = clip.segments.clone();
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut holds = clip.holds.clone();
    holds.retain(|h| h.duration > 0.0 && h.at.is_finite());
    holds.sort_by(|a, b| a.at.total_cmp(&b.at));

    let mut out = Vec::new();
    for seg in segments {
        if !seg.is_valid() {
            return Err(SequenceError::InvalidSegment(seg));
        }
        let mut cursor = seg.start;
        for h in holds.iter().filter(|h| h.at >= seg.start && h.at < seg.end) {
            if h.at > cursor {
                out.push(Piece::Play { start: cursor, end: h.at });
            }
            out.push(Piece::Hold { at: h.at, duration: h.duration });
            cursor = h.at;
        }
        if seg.end > cursor {
            out.push(Piece::Play { start: cursor, end: seg.end });
        }
    }
    Ok(out)
}

/// Builds the output frame list for a clip.
pub fn plan(clip: &Clip, timing: &Timing, fps: Rational) -> Result<SequencePlan, SequenceError> {
    if !fps.is_valid() || fps.as_f64() > 240.0 {
        return Err(SequenceError::InvalidFps);
    }
    let speed = if timing.speed.is_finite() { timing.speed.clamp(MIN_SPEED, MAX_SPEED) } else { 1.0 };
    let pieces = pieces(clip)?;
    let total: f64 = pieces.iter().map(|p| p.output_duration(speed)).sum();
    let fps_f = fps.as_f64();
    let count = ((total * fps_f) - 1e-6).ceil().max(1.0) as usize;
    if count > MAX_OUTPUT_FRAMES {
        return Err(SequenceError::TooLong(count));
    }

    // Forward samples: output frame k shows the source at output time k / fps.
    let mut forward = Vec::with_capacity(count);
    let mut piece_idx = 0;
    let mut piece_start_out = 0.0;
    for k in 0..count {
        let t_out = k as f64 / fps_f;
        while piece_idx + 1 < pieces.len()
            && t_out >= piece_start_out + pieces[piece_idx].output_duration(speed) - 1e-9
        {
            piece_start_out += pieces[piece_idx].output_duration(speed);
            piece_idx += 1;
        }
        let p = pieces[piece_idx];
        forward.push(p.source_time(t_out - piece_start_out, speed));
    }

    let mut samples: Vec<Sample> = match timing.direction {
        Direction::Forward => forward.iter().map(|&t| Sample::At { t }).collect(),
        Direction::Reverse => forward.iter().rev().map(|&t| Sample::At { t }).collect(),
        Direction::PingPong => {
            let mut v: Vec<Sample> = forward.iter().map(|&t| Sample::At { t }).collect();
            if forward.len() > 2 {
                v.extend(forward[1..forward.len() - 1].iter().rev().map(|&t| Sample::At { t }));
            }
            v
        }
    };

    // Seamless-loop crossfade (not meaningful for ping-pong, which already loops seamlessly).
    if timing.direction != Direction::PingPong && timing.crossfade > 0.0 {
        let n = samples.len();
        let nc = ((timing.crossfade * fps_f).round() as usize).min(n.saturating_sub(1) / 2);
        if nc > 0 {
            let at = |s: &Sample| match *s {
                Sample::At { t } => t,
                Sample::Blend { a, .. } => a,
            };
            let mut blended = Vec::with_capacity(n - nc);
            for i in 0..nc {
                let tail = at(&samples[n - nc + i]);
                let head = at(&samples[i]);
                let w = (i + 1) as f32 / (nc + 1) as f32;
                blended.push(Sample::Blend { a: tail, b: head, weight_b: w });
            }
            blended.extend_from_slice(&samples[nc..n - nc]);
            samples = blended;
        }
    }

    let delays = gif_timing::delays_cs(samples.len(), fps);
    let frames: Vec<PlannedFrame> = samples
        .into_iter()
        .zip(delays)
        .map(|(sample, delay_cs)| PlannedFrame { sample, delay_cs })
        .collect();
    let duration = frames.len() as f64 / fps_f;
    Ok(SequencePlan { fps, frames, duration })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Hold;
    use proptest::prelude::*;

    fn clip(segments: &[(f64, f64)]) -> Clip {
        Clip {
            id: "c".into(),
            path: "x.mp4".into(),
            stream_index: 0,
            segments: segments.iter().map(|&(a, b)| TimeRange::new(a, b)).collect(),
            holds: vec![],
        }
    }

    fn times(p: &SequencePlan) -> Vec<f64> {
        p.frames
            .iter()
            .map(|f| match f.sample {
                Sample::At { t } => t,
                Sample::Blend { a, .. } => a,
            })
            .collect()
    }

    #[test]
    fn forward_one_second_at_10fps() {
        let p = plan(&clip(&[(2.0, 3.0)]), &Timing::default(), Rational::integer(10)).unwrap();
        assert_eq!(p.len(), 10);
        let t = times(&p);
        assert!((t[0] - 2.0).abs() < 1e-9 && (t[9] - 2.9).abs() < 1e-9);
        assert!(p.frames.iter().all(|f| f.delay_cs == 10));
    }

    #[test]
    fn speed_changes_frame_count_not_source_range() {
        let fast = Timing { speed: 2.0, ..Timing::default() };
        let p = plan(&clip(&[(0.0, 2.0)]), &fast, Rational::integer(10)).unwrap();
        assert_eq!(p.len(), 10);
        assert!((times(&p)[9] - 1.8).abs() < 1e-9);
        let slow = Timing { speed: 0.5, ..Timing::default() };
        assert_eq!(plan(&clip(&[(0.0, 2.0)]), &slow, Rational::integer(10)).unwrap().len(), 40);
    }

    #[test]
    fn reverse_and_pingpong() {
        let rev = Timing { direction: Direction::Reverse, ..Timing::default() };
        let p = plan(&clip(&[(0.0, 1.0)]), &rev, Rational::integer(10)).unwrap();
        assert!((times(&p)[0] - 0.9).abs() < 1e-9);
        let pp = Timing { direction: Direction::PingPong, ..Timing::default() };
        let p = plan(&clip(&[(0.0, 1.0)]), &pp, Rational::integer(10)).unwrap();
        assert_eq!(p.len(), 18); // 10 forward + 8 back, endpoints not repeated
        let t = times(&p);
        assert!((t[10] - 0.8).abs() < 1e-9 && (t[17] - 0.1).abs() < 1e-9);
    }

    #[test]
    fn segments_concatenate_in_order() {
        let p = plan(&clip(&[(5.0, 5.5), (1.0, 1.5)]), &Timing::default(), Rational::integer(10)).unwrap();
        let t = times(&p);
        assert_eq!(p.len(), 10);
        assert!((t[0] - 1.0).abs() < 1e-9 && (t[5] - 5.0).abs() < 1e-9);
    }

    #[test]
    fn holds_freeze_the_frame() {
        let mut c = clip(&[(0.0, 1.0)]);
        c.holds.push(Hold { at: 0.5, duration: 0.5 });
        let p = plan(&c, &Timing::default(), Rational::integer(10)).unwrap();
        assert_eq!(p.len(), 15);
        let t = times(&p);
        assert!(t[5..10].iter().all(|&x| (x - 0.5).abs() < 1e-9));
        assert!((t[10] - 0.5).abs() < 1e-9 && (t[11] - 0.6).abs() < 1e-9);
    }

    #[test]
    fn crossfade_shortens_and_blends_head() {
        let xf = Timing { crossfade: 0.3, ..Timing::default() };
        let p = plan(&clip(&[(0.0, 2.0)]), &xf, Rational::integer(10)).unwrap();
        assert_eq!(p.len(), 17);
        match p.frames[0].sample {
            Sample::Blend { a, b, weight_b } => {
                assert!((a - 1.7).abs() < 1e-9 && b.abs() < 1e-9 && (weight_b - 0.25).abs() < 1e-6);
            }
            _ => panic!("expected blend"),
        }
        assert!(matches!(p.frames[3].sample, Sample::At { t } if (t - 0.3).abs() < 1e-9));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(plan(&clip(&[]), &Timing::default(), Rational::integer(10)), Err(SequenceError::Empty));
        assert_eq!(
            plan(&clip(&[(0.0, 1.0)]), &Timing::default(), Rational::new(0, 1)),
            Err(SequenceError::InvalidFps)
        );
        assert!(matches!(
            plan(&clip(&[(0.0, 1000.0)]), &Timing::default(), Rational::integer(50)),
            Err(SequenceError::TooLong(_))
        ));
    }

    #[test]
    fn required_ranges_merge() {
        let mut c = clip(&[(0.0, 1.0), (0.5, 2.0), (3.0, 4.0)]);
        c.holds.push(Hold { at: 10.0, duration: 1.0 });
        let r = required_source_ranges(&c);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], TimeRange::new(0.0, 2.0));
    }

    proptest! {
        #[test]
        fn delays_sum_matches_duration(len in 0.2f64..20.0, speed in 0.25f64..4.0, fps_i in 5u32..50) {
            let timing = Timing { speed, ..Timing::default() };
            let p = plan(&clip(&[(1.0, 1.0 + len)]), &timing, Rational::integer(fps_i)).unwrap();
            let total_cs: u32 = p.frames.iter().map(|f| u32::from(f.delay_cs)).sum();
            let expected = p.len() as f64 * 100.0 / f64::from(fps_i);
            prop_assert!((f64::from(total_cs) - expected).abs() <= 1.0);
            // Every sampled time stays inside the kept range.
            for t in p.source_times() {
                prop_assert!((1.0..=1.0 + len + 1e-9).contains(&t));
            }
        }
    }
}

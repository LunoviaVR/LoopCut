//! Source-side helpers: the decode request every preview range starts from, the ranges to decode, and the mapping
//! from source time to decoded frame.
//!
//! These mirror the export crate's rules (`loopcut-export::frames`) on purpose: a frame decoded for preview and one
//! decoded for export with the same request share one frame-store key, and both map times the same way.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use loopcut_core::media::VideoStreamInfo;
use loopcut_core::sequencer::required_source_ranges;
use loopcut_core::{Clip, DecodeChain, PixelLayout, Rational, TimeRange, YuvMatrix};
use loopcut_media::decode::DecodeRequest;

/// Highest source rate that is decoded. Faster sources are sampled at this rate.
pub const MAX_DECODE_FPS: u32 = 60;

/// Rate used when the stream reports no usable frame rate.
const FALLBACK_DECODE_FPS: u32 = 30;

/// Longest source-time window decoded for a frame outside the plan (the crop tool, source scrubbing). Shorter when the
/// window's frames would exceed its share of the RAM budget.
pub const WINDOW_SECONDS: f64 = 1.0;

/// Tolerance when matching a source time to a range: sample times are sums of floats.
pub const TIME_EPS: f64 = 1e-6;

/// Added before flooring a source-time offset to a frame index, so exact multiples of the frame period stay on their
/// frame.
const INDEX_EPSILON: f64 = 1e-6;

/// The layout the decoder delivers: RGBA when there is an alpha plane or the source is RGB, 10-bit P010 for high bit
/// depths, and 8-bit NV12 otherwise.
pub fn layout_for(video: &VideoStreamInfo) -> PixelLayout {
    if video.has_alpha || video.color.matrix == YuvMatrix::Rgb {
        PixelLayout::Rgba8
    } else if video.bit_depth > 8 {
        PixelLayout::P010
    } else {
        PixelLayout::Nv12
    }
}

/// The rate frames are decoded at: the stream's frame rate, capped at [`MAX_DECODE_FPS`].
pub fn decode_fps(video: &VideoStreamInfo) -> Rational {
    let rate = video.frame_rate;
    if !rate.is_valid() {
        Rational::integer(FALLBACK_DECODE_FPS)
    } else if rate.as_f64() > f64::from(MAX_DECODE_FPS) {
        Rational::integer(MAX_DECODE_FPS)
    } else {
        rate
    }
}

/// RGBA output is CPU-only, so RGBA sources always decode on the CPU chain.
pub fn effective_chain(requested: DecodeChain, layout: PixelLayout) -> DecodeChain {
    if layout == PixelLayout::Rgba8 {
        DecodeChain::CPU
    } else {
        requested
    }
}

/// The request every range starts from: the whole frame at `size` (stored pixels), at `fps`, on `chain`.
/// The range is filled in per decode.
pub fn decode_template(
    path: &Path,
    video: &VideoStreamInfo,
    size: (u32, u32),
    chain: DecodeChain,
    fps: Rational,
) -> DecodeRequest {
    let layout = layout_for(video);
    DecodeRequest {
        input: path.to_path_buf(),
        stream_index: video.index,
        coded_width: video.width,
        coded_height: video.height,
        bit_depth: video.bit_depth,
        has_alpha: video.has_alpha,
        codec: video.codec.clone(),
        color: video.color,
        start_time: video.start_time,
        range: None,
        crop: None,
        size: Some(size),
        fps: Some(fps),
        chain: effective_chain(chain, layout),
        layout,
    }
}

/// The source ranges the clip needs, each at least two decoded frames long so that a hold (a millisecond of source
/// time) still decodes the frame it freezes. Ranges are sorted and disjoint.
pub fn plan_ranges(clip: &Clip, fps: Rational) -> Vec<TimeRange> {
    let minimum = 2.0 * fps.frame_duration();
    required_source_ranges(clip)
        .into_iter()
        .map(|range| TimeRange::new(range.start, range.start + range.duration().max(minimum)))
        .collect()
}

/// Splits `range` into consecutive chunks of `chunk_frames` decoded frames. The last chunk may be shorter, but never
/// shorter than two frames: a shorter tail joins the chunk before it. Chunk boundaries sit on the range's own frame
/// grid (`range.start + k / fps`), so a frame of a chunk has the same source time as in the whole range, and the
/// chunks cover the range exactly.
pub fn chunk_ranges(range: TimeRange, fps: Rational, chunk_frames: u64) -> Vec<TimeRange> {
    let frame = fps.frame_duration();
    let step = chunk_frames.max(2) as f64 * frame;
    let duration = range.duration();
    if duration.is_nan() || duration <= step {
        return vec![range];
    }
    let count = (duration / step).ceil() as usize;
    let mut starts: Vec<f64> = (0..count).map(|k| range.start + k as f64 * step).collect();
    if starts.len() > 1 && range.end - starts[starts.len() - 1] < 2.0 * frame {
        starts.pop();
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &start)| TimeRange::new(start, starts.get(k + 1).copied().unwrap_or(range.end)))
        .collect()
}

/// Frame-store key of the frames one decode request produces. Every input of the request is hashed, so two requests
/// share frames only when they would decode identical frames.
pub fn source_key(req: &DecodeRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    req.input.hash(&mut hasher);
    req.stream_index.hash(&mut hasher);
    (
        req.coded_width,
        req.coded_height,
        req.bit_depth,
        req.has_alpha,
    )
        .hash(&mut hasher);
    req.codec.hash(&mut hasher);
    req.color.hash(&mut hasher);
    req.start_time.to_bits().hash(&mut hasher);
    if let Some(range) = req.range {
        (range.start.to_bits(), range.end.to_bits()).hash(&mut hasher);
    }
    if let Some(crop) = req.crop {
        (crop.x, crop.y, crop.w, crop.h).hash(&mut hasher);
    }
    req.size.hash(&mut hasher);
    req.fps.hash(&mut hasher);
    req.chain.hash(&mut hasher);
    req.layout.hash(&mut hasher);
    hasher.finish()
}

/// Index of the decoded frame that shows source time `t` for a range starting at `range_start` and decoded at `fps`,
/// before clamping to the frames that exist: `floor((t - range_start) × fps)`, never negative.
pub fn raw_index(range_start: f64, fps: Rational, t: f64) -> u64 {
    let floored = ((t - range_start) * fps.as_f64() + INDEX_EPSILON).floor();
    if !floored.is_finite() || floored <= 0.0 {
        0
    } else {
        floored as u64
    }
}

/// Index of the frame that shows `t`, clamped into the `frames` decoded frames.
pub fn frame_index(range_start: f64, fps: Rational, t: f64, frames: u64) -> u64 {
    raw_index(range_start, fps, t).min(frames.saturating_sub(1))
}

/// True when `t` lies in `range`, allowing for float error at both ends.
pub fn contains(range: TimeRange, t: f64) -> bool {
    t >= range.start - TIME_EPS && t <= range.end + TIME_EPS
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcut_core::{ColorInfo, DecodeBackend};

    fn video(
        has_alpha: bool,
        bit_depth: u8,
        matrix: YuvMatrix,
        frame_rate: Rational,
    ) -> VideoStreamInfo {
        VideoStreamInfo {
            index: 0,
            codec: "h264".to_owned(),
            profile: None,
            width: 320,
            height: 240,
            sample_aspect: Rational::integer(1),
            pix_fmt: "yuv420p".to_owned(),
            bit_depth,
            has_alpha,
            rotation: 0,
            frame_rate,
            avg_frame_rate: None,
            variable_frame_rate: false,
            color: ColorInfo {
                matrix,
                ..ColorInfo::SDR_BT709
            },
            start_time: 0.0,
            duration: 2.0,
            frame_count: None,
            interlaced: false,
            web_playable: true,
        }
    }

    #[test]
    fn index_maps_time_to_the_frame_at_or_before_it() {
        let fps = Rational::integer(30);
        // 0.6 is exactly frame 3 of a range starting at 0.5, though floating point may land it just below.
        assert_eq!(frame_index(0.5, fps, 0.6, 30), 3);
        assert_eq!(frame_index(0.5, fps, 0.5, 30), 0);
        assert_eq!(frame_index(0.5, fps, 9.0, 30), 29);
        assert_eq!(frame_index(0.5, fps, 0.0, 30), 0);
        assert_eq!(raw_index(0.5, fps, 9.0), 255);
        assert_eq!(frame_index(0.5, fps, 0.7, 0), 0);
    }

    #[test]
    fn layout_follows_alpha_matrix_and_bit_depth() {
        let fps = Rational::integer(30);
        assert_eq!(
            layout_for(&video(false, 8, YuvMatrix::Bt709, fps)),
            PixelLayout::Nv12
        );
        assert_eq!(
            layout_for(&video(false, 10, YuvMatrix::Bt2020, fps)),
            PixelLayout::P010
        );
        assert_eq!(
            layout_for(&video(true, 8, YuvMatrix::Bt709, fps)),
            PixelLayout::Rgba8
        );
        assert_eq!(
            layout_for(&video(false, 8, YuvMatrix::Rgb, fps)),
            PixelLayout::Rgba8
        );
    }

    #[test]
    fn decode_rate_is_capped_and_defaults_when_unknown() {
        let v = |rate: Rational| decode_fps(&video(false, 8, YuvMatrix::Bt709, rate));
        assert_eq!(v(Rational::new(30000, 1001)), Rational::new(30000, 1001));
        assert_eq!(v(Rational::integer(120)), Rational::integer(60));
        assert_eq!(v(Rational::new(0, 1)), Rational::integer(30));
    }

    #[test]
    fn rgba_sources_always_decode_on_the_cpu() {
        let gpu = DecodeChain {
            backend: DecodeBackend::Amf { dxgi_index: 0 },
            scaler: loopcut_core::GpuScaler::Amf,
        };
        assert_eq!(effective_chain(gpu, PixelLayout::Rgba8), DecodeChain::CPU);
        assert_eq!(effective_chain(gpu, PixelLayout::Nv12), gpu);
    }

    #[test]
    fn plan_ranges_extend_holds_to_two_frames() {
        let clip = Clip {
            id: "c".to_owned(),
            path: "x.mp4".to_owned(),
            stream_index: 0,
            segments: vec![TimeRange::new(0.0, 1.0)],
            holds: vec![loopcut_core::project::Hold {
                at: 3.0,
                duration: 0.5,
            }],
        };
        let ranges = plan_ranges(&clip, Rational::integer(10));
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], TimeRange::new(0.0, 1.0));
        assert!((ranges[1].end - 3.2).abs() < 1e-9);
    }

    #[test]
    fn containment_tolerates_float_error_at_both_ends() {
        let range = TimeRange::new(1.0, 2.0);
        assert!(contains(range, 2.0 + 1e-9));
        assert!(contains(range, 1.0 - 1e-9));
        assert!(!contains(range, 2.5));
    }

    #[test]
    fn chunks_cover_the_range_on_its_frame_grid() {
        let fps = Rational::integer(10);
        let range = TimeRange::new(0.25, 2.25);
        let chunks = chunk_ranges(range, fps, 8);
        assert_eq!(chunks.len(), 3, "20 frames in chunks of 8: 8 + 8 + 4");
        assert_eq!(chunks[0].start, range.start);
        assert_eq!(chunks[2].end, range.end);
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "chunks are contiguous");
        }
        // A frame of the second chunk keeps the source time it has in the whole range.
        let t = 1.05;
        let whole = frame_index(range.start, fps, t, 20);
        let second = &chunks[1];
        assert_eq!(
            raw_index(second.start, fps, t) + 8,
            whole,
            "frame indices agree across the chunk boundary"
        );
    }

    #[test]
    fn a_short_tail_joins_the_chunk_before_it() {
        let fps = Rational::integer(10);
        // 9 frames in chunks of 4: 4 + 4 + 1, and the single frame joins the second chunk.
        let chunks = chunk_ranges(TimeRange::new(0.0, 0.9), fps, 4);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1].end, 0.9);
        assert!(chunks[1].duration() >= 0.2 - 1e-9);
        // A range that fits is one chunk.
        assert_eq!(chunk_ranges(TimeRange::new(0.0, 0.3), fps, 8).len(), 1);
    }
}

//! Source frame provider.
//!
//! The source ranges a clip needs (see [`loopcut_core::sequencer::required_source_ranges`]) are decoded at the source's
//! own frame rate, capped at [`MAX_DECODE_FPS`], so changing the output frame rate or the speed never decodes again. The
//! Sequencer asks for frames by source time, and [`SourceFrames::frame_at`] returns the decoded frame at or before that
//! time.
//!
//! # Fallback is per range
//!
//! Every range starts on the requested decode chain. A hardware failure on a range, at start or part way through its
//! pass, restarts that pass on the CPU, and that range keeps the CPU chain for the rest of the export. Later ranges start
//! on the requested chain again, so each range that fails is reported once. Each switch is recorded as a
//! [`DecodeFallback`] for the caller to report.
//!
//! # Frames that do not fit the store
//!
//! A range is decoded into the [`FrameStore`] in one pass. The store's budget is not visible here, so a range that does
//! not fit is detected from its own frames only: one of them is dropped or evicted, or one is missing once the pass
//! ends. Frames that other users of the store keep or lose (the preview, for one) do not count, except that a loss
//! which evicts one of this range's frames while its pass runs looks the same. Such a range then either streams or
//! fails:
//!
//! * [`Access::Forward`]: the caller requests source times in non-decreasing order. The range is streamed: a stream
//!   decodes it and keeps only the current frame, so memory stays at one frame for the range. A request for an earlier
//!   time than the current frame starts a new stream from the range start.
//! * [`Access::Random`]: the export fails with [`ExportError::TooLarge`].
//!
//! Each range takes its store pass, then one stream pass for each traversal of the plan. A GIF engine that reads ahead
//! for its palette and then encodes from the first frame traverses the plan twice, so a streamed range takes two stream
//! passes. Random access takes at most two passes: the store pass, and one refill after the store lost frames. A
//! hardware restart inside a pass belongs to that pass and is not counted. [`SourceFrames::decode_passes`] reports the
//! total.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use loopcut_core::hw::{FORCE_HW_FAIL_ENV, HwStage, forced_failure_from_env};
use loopcut_core::media::{MediaInfo, VideoStreamInfo};
use loopcut_core::sequencer::required_source_ranges;
use loopcut_core::{
    Clip, ColorInfo, DecodeBackend, DecodeChain, PixelLayout, PixelRect, Rational, TimeRange,
    YuvMatrix,
};
use loopcut_media::decode::{DecodeRequest, DecodeSession, DecodedFrame};
use loopcut_media::framestore::{FrameKey, FrameStore, StoredFrame};
use loopcut_media::{CancelToken, FfmpegPaths, MediaError};

use crate::job::{ExportError, map_media};

/// Highest source rate that is decoded. Faster sources are sampled at this rate.
pub const MAX_DECODE_FPS: u32 = 60;

/// Rate used when the stream reports no usable frame rate.
const FALLBACK_DECODE_FPS: u32 = 30;

/// Added before flooring a source-time offset to a frame index. Exact multiples of the frame period can land just
/// below an integer in floating point, and this keeps them on the frame they name.
const INDEX_EPSILON: f64 = 1e-6;

/// Tolerance when matching a source time to the range that starts at or before it.
const RANGE_EPSILON: f64 = 1e-9;

/// Decode passes one random-access range may take per export: the store pass and one refill (see the module docs).
const MAX_PASSES: u32 = 2;

const MEGABYTE: f64 = 1024.0 * 1024.0;

/// Decode geometry shared by every range of one export. Crop coordinates are in stored (coded) pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeGeometry {
    /// Crop rectangle in stored pixels, or `None` for the whole frame. Even-aligned.
    pub crop: Option<PixelRect>,
    /// Size of the decoded frames after the crop, in stored orientation. Both values are even.
    pub size: (u32, u32),
}

/// A switch from a hardware decode chain to the CPU chain, for one source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeFallback {
    /// Label of the chain that failed (`amf`, `d3d11va`, ...).
    pub from: String,
    /// Label of the chain that replaced it (`cpu`).
    pub to: String,
    /// The error that made the hardware chain fail.
    pub reason: String,
}

/// How the caller will request frames. It decides what a range that does not fit the store can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Source times arrive in non-decreasing order (forward playback without a crossfade). A range that does not fit
    /// the store is streamed.
    Forward,
    /// Source times may arrive in any order. A range that does not fit the store fails with
    /// [`ExportError::TooLarge`].
    Random,
}

/// Decode settings of one [`SourceFrames`] provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// The requested chain. A range whose layout needs the CPU (RGBA sources) decodes on the CPU instead.
    pub chain: DecodeChain,
    /// Crop and size of the decoded frames.
    pub geometry: DecodeGeometry,
    /// How the frames will be requested.
    pub access: Access,
}

/// Where the frames of one range are held.
enum Held {
    /// Frames `0..frames` of the decode request with frame-store key `source` are all in the store.
    Stored { source: u64, frames: u64 },
    /// Not in the store. The stream is opened by the first request, so it is `None` until then.
    Streaming(Option<Box<Stream>>),
}

/// One requested source range and what is known about its frames.
struct RangeFrames {
    /// The source range the plan needs.
    range: TimeRange,
    /// The chain this range decodes with: the requested one, or the CPU once this range fell back.
    chain: DecodeChain,
    /// Decode passes started for this range. Restarts inside a pass are not counted.
    passes: u32,
    held: Held,
}

/// A forward decode of one range, holding the frame at the current position.
struct Stream {
    session: DecodeSession,
    /// Index and frame of the last frame read.
    current: Option<(u64, StoredFrame)>,
    /// The decoder has ended, so `current` is the last frame of the range.
    exhausted: bool,
}

impl Stream {
    /// True when the stream has read past frame `target`, so the frame `target` names is no longer held.
    fn is_past(&self, target: u64) -> bool {
        matches!(&self.current, Some((read, _)) if *read > target)
    }
}

/// Outcome of one store pass over a range.
enum Fill {
    /// Every frame of the range is in the store under `source`.
    Stored { source: u64, frames: u64 },
    /// The store could not keep the range. Its frames were removed.
    Overflow,
}

/// Decoded source frames for one export, looked up by source time.
///
/// Dropping the provider removes its frames from the store and ends its streams.
pub struct SourceFrames {
    paths: FfmpegPaths,
    store: Arc<FrameStore>,
    cancel: CancelToken,
    /// The decode request every range starts from. Its `range` and `chain` are set per pass.
    template: DecodeRequest,
    fps: Rational,
    access: Access,
    ranges: Vec<RangeFrames>,
    fallbacks: Vec<DecodeFallback>,
}

impl SourceFrames {
    /// Decodes every source range `clip` needs, at the source frame rate, into `store`.
    ///
    /// # Errors
    ///
    /// See [`SourceFrames::decode_ranges`].
    pub fn decode(
        paths: &FfmpegPaths,
        media: &MediaInfo,
        clip: &Clip,
        options: DecodeOptions,
        store: Arc<FrameStore>,
        cancel: &CancelToken,
    ) -> Result<Self, ExportError> {
        Self::decode_ranges(
            paths,
            media,
            clip,
            &required_source_ranges(clip),
            options,
            store,
            cancel,
        )
    }

    /// Decodes the given source ranges (sorted and disjoint) into `store`. Times outside them are not served.
    ///
    /// # Errors
    ///
    /// [`ExportError::InvalidRequest`] for a geometry that cannot be decoded (odd or empty size, crop outside the
    /// frame) or no ranges. [`ExportError::TooLarge`] when a range does not fit the store and `options.access` is
    /// [`Access::Random`]. [`ExportError::Cancelled`] when `cancel` fires. Otherwise the decoder's error, after the CPU
    /// restart described in the module docs.
    pub fn decode_ranges(
        paths: &FfmpegPaths,
        media: &MediaInfo,
        clip: &Clip,
        ranges: &[TimeRange],
        options: DecodeOptions,
        store: Arc<FrameStore>,
        cancel: &CancelToken,
    ) -> Result<Self, ExportError> {
        let video = &media.video;
        validate_geometry(video, options.geometry)?;
        if ranges.is_empty() {
            return Err(ExportError::InvalidRequest(
                "the clip has no source ranges to decode".to_owned(),
            ));
        }
        let layout = layout_for(video);
        let fps = decode_fps(video);
        let template = DecodeRequest {
            input: PathBuf::from(&clip.path),
            stream_index: video.index,
            coded_width: video.width,
            coded_height: video.height,
            bit_depth: video.bit_depth,
            has_alpha: video.has_alpha,
            codec: video.codec.clone(),
            color: video.color,
            start_time: video.start_time,
            range: None,
            crop: options.geometry.crop,
            size: Some(options.geometry.size),
            fps: Some(fps),
            chain: options.chain,
            layout,
        };
        let chain = effective_chain(options.chain, layout);
        let mut frames = Self {
            paths: paths.clone(),
            store,
            cancel: cancel.clone(),
            template,
            fps,
            access: options.access,
            ranges: ranges
                .iter()
                .map(|&range| RangeFrames {
                    range,
                    chain,
                    passes: 0,
                    held: Held::Streaming(None),
                })
                .collect(),
            fallbacks: Vec::new(),
        };
        for index in 0..frames.ranges.len() {
            frames.decode_range(index)?;
        }
        Ok(frames)
    }

    /// The decoded frame for source time `t`: the frame at or before `t` in the range that covers it.
    ///
    /// A frame the store no longer holds is recovered according to [`Access`]: a forward caller gets the range streamed
    /// again up to this time, and a random caller gets one refill of the range. If that still misses, the result is
    /// [`ExportError::TooLarge`]. A streamed range asked for an earlier time than it already passed is decoded again
    /// from its start.
    ///
    /// # Errors
    ///
    /// [`ExportError::FrameUnavailable`] for a time a stream cannot serve. A time before the first range is served from
    /// the first range. Decode errors otherwise.
    pub fn frame_at(&mut self, t: f64) -> Result<StoredFrame, ExportError> {
        let index =
            covering_range(&self.ranges, t).ok_or(ExportError::FrameUnavailable { time: t })?;
        if let Held::Stored { source, frames } = self.ranges[index].held {
            let key = FrameKey {
                source,
                index: frame_index(self.ranges[index].range.start, self.fps, t, frames),
            };
            if let Some(frame) = self.store.get(&key) {
                return Ok(frame);
            }
            return self.lost(index, t);
        }
        self.stream_frame(index, t)
    }

    /// Returns the fallbacks recorded since the last call, in the order they happened.
    pub fn take_fallbacks(&mut self) -> Vec<DecodeFallback> {
        std::mem::take(&mut self.fallbacks)
    }

    /// The rate the frames were decoded at.
    pub fn fps(&self) -> Rational {
        self.fps
    }

    /// Memory layout of the decoded frames.
    pub fn layout(&self) -> PixelLayout {
        self.template.layout
    }

    /// Colour description of the source, for the compositor.
    pub fn color(&self) -> ColorInfo {
        self.template.color
    }

    /// Number of source ranges held.
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Number of ranges served by a stream instead of the store.
    pub fn streaming_ranges(&self) -> usize {
        self.ranges
            .iter()
            .filter(|range| matches!(range.held, Held::Streaming(_)))
            .count()
    }

    /// Total frames held in the store across all ranges.
    pub fn decoded_frames(&self) -> u64 {
        self.ranges
            .iter()
            .map(|range| match range.held {
                Held::Stored { frames, .. } => frames,
                Held::Streaming(_) => 0,
            })
            .sum()
    }

    /// Decode passes started over all ranges: store passes, stream passes and random-access refills. A random-access
    /// range takes at most two; a streamed range takes one per traversal of the plan (see the module docs).
    pub fn decode_passes(&self) -> u64 {
        self.ranges
            .iter()
            .map(|range| u64::from(range.passes))
            .sum()
    }

    /// Stores one range's frames, or marks the range for streaming when the store cannot keep them.
    fn decode_range(&mut self, index: usize) -> Result<(), ExportError> {
        self.release(index);
        self.ranges[index].passes += 1;
        match self.fill(index)? {
            Fill::Stored { source, frames } => {
                self.ranges[index].held = Held::Stored { source, frames };
            }
            // Stays `Streaming(None)`: the stream opens, and counts as its own pass, on the first request.
            Fill::Overflow => match self.access {
                Access::Forward => {}
                Access::Random => return Err(self.too_large(index)),
            },
        }
        Ok(())
    }

    /// The store no longer holds the frame for `t` of a stored range.
    fn lost(&mut self, index: usize, t: f64) -> Result<StoredFrame, ExportError> {
        match self.access {
            Access::Forward => {
                self.release(index);
                self.stream_frame(index, t)
            }
            Access::Random => {
                if self.ranges[index].passes >= MAX_PASSES {
                    return Err(self.too_large(index));
                }
                self.decode_range(index)?;
                let Held::Stored { source, frames } = self.ranges[index].held else {
                    return Err(self.too_large(index));
                };
                let key = FrameKey {
                    source,
                    index: frame_index(self.ranges[index].range.start, self.fps, t, frames),
                };
                match self.store.get(&key) {
                    Some(frame) => Ok(frame),
                    None => Err(self.too_large(index)),
                }
            }
        }
    }

    /// Removes a stored range's frames from the store and marks the range as not stored.
    fn release(&mut self, index: usize) {
        if let Held::Stored { source, .. } = self.ranges[index].held {
            self.store.remove_source(source);
        }
        self.ranges[index].held = Held::Streaming(None);
    }

    /// Returns the frame for source time `t` from the range's stream. A stream that is not open yet starts from the range
    /// start. A stream that has read past `t` has dropped the frame `t` needs, so the range is decoded again from its
    /// start, as a new pass.
    fn stream_frame(&mut self, index: usize, t: f64) -> Result<StoredFrame, ExportError> {
        let target = frame_index(self.ranges[index].range.start, self.fps, t, u64::MAX);
        let mut stream = match self.take_stream(index) {
            Some(stream) if !stream.is_past(target) => stream,
            earlier => {
                if earlier.is_some() {
                    tracing::debug!(
                        start = self.ranges[index].range.start,
                        time = t,
                        "a streamed source range was asked for an earlier time; decoding it again from its start"
                    );
                }
                self.ranges[index].passes += 1;
                self.start_stream(index)?
            }
        };
        let advanced = self.advance(index, &mut stream, target);
        let answer = advanced.and_then(|()| self.stream_answer(index, &stream, t, target));
        // The stream goes back even when the request failed, so a later request can continue from it.
        self.ranges[index].held = Held::Streaming(Some(stream));
        answer
    }

    fn take_stream(&mut self, index: usize) -> Option<Box<Stream>> {
        if let Held::Stored { .. } = self.ranges[index].held {
            self.release(index);
        }
        match &mut self.ranges[index].held {
            Held::Streaming(slot) => slot.take(),
            Held::Stored { .. } => None,
        }
    }

    /// Reads frames until the stream is at `target` or the decoder has ended.
    fn advance(
        &mut self,
        index: usize,
        stream: &mut Box<Stream>,
        target: u64,
    ) -> Result<(), ExportError> {
        loop {
            if stream.exhausted {
                return Ok(());
            }
            if let Some((read, _)) = &stream.current
                && *read >= target
            {
                return Ok(());
            }
            match stream.session.next() {
                Some(Ok(decoded)) => {
                    stream.current = Some((decoded.index, into_stored(decoded)));
                }
                Some(Err(err)) => {
                    self.recover(index, map_media(err))?;
                    *stream = self.start_stream(index)?;
                }
                None => stream.exhausted = true,
            }
        }
    }

    /// The frame at or before `target` that a stream holds after [`Self::advance`], for source time `t`.
    fn stream_answer(
        &self,
        index: usize,
        stream: &Stream,
        t: f64,
        target: u64,
    ) -> Result<StoredFrame, ExportError> {
        match &stream.current {
            Some((read, frame)) if *read <= target => Ok(frame.clone()),
            // Indices are consecutive, so this only happens if the decoder skipped the frame the time names.
            Some(_) => Err(ExportError::FrameUnavailable { time: t }),
            None => Err(ExportError::DecodeEmpty {
                start: self.ranges[index].range.start,
            }),
        }
    }

    /// Opens a stream on the range's chain. A failure to start falls back to the CPU for this range.
    fn start_stream(&mut self, index: usize) -> Result<Box<Stream>, ExportError> {
        loop {
            let request = self.request_for(index);
            match self.start_session(&request) {
                Ok(session) => {
                    return Ok(Box::new(Stream {
                        session,
                        current: None,
                        exhausted: false,
                    }));
                }
                Err(err) => self.recover(index, err)?,
            }
        }
    }

    /// Decodes range `index` into the store, falling back to the CPU for this range if its hardware chain fails.
    fn fill(&mut self, index: usize) -> Result<Fill, ExportError> {
        loop {
            match self.fill_attempt(index) {
                Ok(fill) => return Ok(fill),
                Err(err) => self.recover(index, err)?,
            }
        }
    }

    /// One store pass with the range's current chain. On any outcome other than a complete store, nothing is left in
    /// the store under this attempt's key.
    fn fill_attempt(&mut self, index: usize) -> Result<Fill, ExportError> {
        let request = self.request_for(index);
        let source = source_key(&request);
        // Entries left under this key by an interrupted attempt are replaced.
        self.store.remove_source(source);
        let outcome = self.fill_frames(&request, source);
        if !matches!(outcome, Ok(Fill::Stored { .. })) {
            self.store.remove_source(source);
        }
        outcome
    }

    fn fill_frames(&self, request: &DecodeRequest, source: u64) -> Result<Fill, ExportError> {
        let session = self.start_session(request)?;
        let first = FrameKey { source, index: 0 };
        let mut count = 0u64;
        for item in session {
            let frame = item.map_err(map_media)?;
            let key = FrameKey {
                source,
                index: frame.index,
            };
            self.store.insert(key, into_stored(frame));
            count += 1;
            // Stop at the first sign that the range does not fit: its first frame or the frame just stored is no longer
            // held. Frames of other sources are not looked at, so a frame another user of the store loses does not count.
            if !self.store.contains(&first) || !self.store.contains(&key) {
                return Ok(Fill::Overflow);
            }
        }
        if count == 0 {
            return Err(ExportError::DecodeEmpty {
                start: request.range.map_or(0.0, |range| range.start),
            });
        }
        // A frame stored earlier can be evicted by one that followed it, so every frame of the range is checked once more.
        if (0..count).any(|index| !self.store.contains(&FrameKey { source, index })) {
            return Ok(Fill::Overflow);
        }
        Ok(Fill::Stored {
            source,
            frames: count,
        })
    }

    /// Starts one decoder. A forced hardware failure (see [`FORCE_HW_FAIL_ENV`]) is reported before the process starts,
    /// so it takes the same path as a real one.
    fn start_session(&self, request: &DecodeRequest) -> Result<DecodeSession, ExportError> {
        let backend = request.chain.backend;
        if forced_failure_from_env(HwStage::Decode, backend.name()) {
            return Err(ExportError::Media(MediaError::Unsupported(format!(
                "{FORCE_HW_FAIL_ENV} forces the {} decode chain to fail",
                backend.name()
            ))));
        }
        DecodeSession::start(&self.paths, request, self.cancel.clone()).map_err(map_media)
    }

    /// Switches range `index` to the CPU chain after a hardware failure. Any other failure is returned unchanged.
    fn recover(&mut self, index: usize, err: ExportError) -> Result<(), ExportError> {
        let chain = self.ranges[index].chain;
        if matches!(err, ExportError::Cancelled) || !chain.backend.is_hardware() {
            return Err(err);
        }
        let from = decode_label(chain);
        tracing::warn!(
            backend = from,
            error = %err,
            "hardware decode failed; this range decodes on the CPU from here on"
        );
        self.fallbacks.push(DecodeFallback {
            from: from.to_owned(),
            to: "cpu".to_owned(),
            reason: err.to_string(),
        });
        self.ranges[index].chain = DecodeChain::CPU;
        Ok(())
    }

    /// The decode request for range `index` on the range's chain. The range is at least two source frames long, so a
    /// hold (which is only a millisecond of source time) still decodes the frame it freezes.
    fn request_for(&self, index: usize) -> DecodeRequest {
        let range = self.ranges[index].range;
        let minimum = 2.0 * self.fps.frame_duration();
        let duration = range.duration().max(minimum);
        DecodeRequest {
            range: Some(TimeRange::new(range.start, range.start + duration)),
            chain: self.ranges[index].chain,
            ..self.template.clone()
        }
    }

    fn too_large(&self, index: usize) -> ExportError {
        let range = self.ranges[index].range;
        let frames = (range.duration() * self.fps.as_f64()).ceil().max(1.0);
        let megabytes = frames * self.template.frame_bytes() as f64 / MEGABYTE;
        ExportError::TooLarge(format!(
            "the source frames for {:.3}..{:.3} s need about {megabytes:.1} MB, more than the frame cache holds; \
             this plan does not play its source in order, so they cannot be streamed. Shorten the selection or \
             reduce the output size",
            range.start, range.end
        ))
    }
}

impl Drop for SourceFrames {
    fn drop(&mut self) {
        for range in &self.ranges {
            if let Held::Stored { source, .. } = range.held {
                self.store.remove_source(source);
            }
        }
    }
}

fn into_stored(frame: DecodedFrame) -> StoredFrame {
    StoredFrame {
        width: frame.width,
        height: frame.height,
        layout: frame.layout,
        time: frame.time,
        data: Arc::from(frame.data),
    }
}

/// The layout the decoder delivers for a stream: RGBA when there is an alpha plane or the source is RGB, 10-bit
/// P010 for high bit depths, and 8-bit NV12 otherwise.
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

/// Index of the decoded frame that shows source time `t` for a range that starts at `range_start` and was decoded
/// at `fps`: `floor((t - range_start) * fps + 1e-6)`, clamped into the `frames` frames the range holds.
pub fn frame_index(range_start: f64, fps: Rational, t: f64, frames: u64) -> u64 {
    let last = frames.saturating_sub(1);
    let floored = ((t - range_start) * fps.as_f64() + INDEX_EPSILON).floor();
    if !floored.is_finite() || floored <= 0.0 {
        0
    } else {
        (floored as u64).min(last)
    }
}

/// The index of the last range that starts at or before `t`, or `None` when there are no ranges. Ranges are sorted
/// and disjoint, as [`required_source_ranges`] returns them. A time before the first range maps to the first range.
fn covering_range(ranges: &[RangeFrames], t: f64) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    Some(
        ranges
            .partition_point(|range| range.range.start <= t + RANGE_EPSILON)
            .saturating_sub(1),
    )
}

/// RGBA sources are decoded on the CPU only, so they always use the CPU chain.
fn effective_chain(requested: DecodeChain, layout: PixelLayout) -> DecodeChain {
    if layout == PixelLayout::Rgba8 {
        DecodeChain::CPU
    } else {
        requested
    }
}

/// Short name of a decode chain's backend for events and logs.
fn decode_label(chain: DecodeChain) -> &'static str {
    match chain.backend {
        DecodeBackend::Cpu => "cpu",
        DecodeBackend::D3d11va { .. } => "d3d11va",
        DecodeBackend::Cuda { .. } => "cuda",
        DecodeBackend::Qsv { .. } => "qsv",
        DecodeBackend::Amf { .. } => "amf",
    }
}

/// Frame-store key for the frames one decode request produces. Every input of the request is hashed, so two
/// requests share frames only when they would decode identical frames.
fn source_key(req: &DecodeRequest) -> u64 {
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

/// Rejects decode geometry that the decoder would refuse, so that a bad request is not mistaken for a hardware failure.
fn validate_geometry(video: &VideoStreamInfo, geometry: DecodeGeometry) -> Result<(), ExportError> {
    let (width, height) = geometry.size;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(ExportError::InvalidRequest(format!(
            "the decoded frame size {width}x{height} must be non-zero and even"
        )));
    }
    if let Some(crop) = geometry.crop {
        let fits = u64::from(crop.x) + u64::from(crop.w) <= u64::from(video.width)
            && u64::from(crop.y) + u64::from(crop.h) <= u64::from(video.height);
        let even = [crop.x, crop.y, crop.w, crop.h]
            .iter()
            .all(|value| value.is_multiple_of(2));
        if crop.w == 0 || crop.h == 0 || !fits || !even {
            return Err(ExportError::InvalidRequest(format!(
                "the crop {crop:?} must be even-aligned, non-empty and inside the {}x{} frame",
                video.width, video.height
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn ranges_of(spans: &[TimeRange]) -> Vec<RangeFrames> {
        spans
            .iter()
            .map(|&range| RangeFrames {
                range,
                chain: DecodeChain::CPU,
                passes: 0,
                held: Held::Streaming(None),
            })
            .collect()
    }

    #[test]
    fn index_maps_source_time_to_the_frame_at_or_before_it() {
        let fps = Rational::integer(30);
        // Range [0.5, 1.5) decoded at 30 fps. 0.6 is exactly frame 3; floating point may land it at 2.99999.
        assert_eq!(frame_index(0.5, fps, 0.6, 30), 3);
        assert_eq!(frame_index(0.5, fps, 0.5, 30), 0);
        assert_eq!(frame_index(0.5, fps, 0.5 + 0.0999, 30), 2);
        // Past the end clamps to the last frame; before the start clamps to the first.
        assert_eq!(frame_index(0.5, fps, 9.0, 30), 29);
        assert_eq!(frame_index(0.5, fps, 0.0, 30), 0);
        // An empty range never indexes past zero.
        assert_eq!(frame_index(0.5, fps, 0.7, 0), 0);
    }

    #[test]
    fn covering_range_picks_the_last_range_that_started() {
        let ranges = ranges_of(&[TimeRange::new(0.0, 1.0), TimeRange::new(2.0, 3.0)]);
        assert_eq!(covering_range(&ranges, 0.5), Some(0));
        assert_eq!(covering_range(&ranges, 2.5), Some(1));
        // A time in the gap, or exactly at a range's end, stays with the earlier range.
        assert_eq!(covering_range(&ranges, 1.5), Some(0));
        assert_eq!(covering_range(&ranges, 3.0), Some(1));
        // Before every range, the first one is used.
        assert_eq!(covering_range(&ranges, -1.0), Some(0));
        assert_eq!(covering_range(&[], 0.0), None);
    }

    #[test]
    fn layout_follows_alpha_matrix_and_bit_depth() {
        assert_eq!(
            layout_for(&video(false, 8, YuvMatrix::Bt709, Rational::integer(30))),
            PixelLayout::Nv12
        );
        assert_eq!(
            layout_for(&video(false, 10, YuvMatrix::Bt2020, Rational::integer(30))),
            PixelLayout::P010
        );
        assert_eq!(
            layout_for(&video(true, 8, YuvMatrix::Bt709, Rational::integer(30))),
            PixelLayout::Rgba8
        );
        assert_eq!(
            layout_for(&video(false, 8, YuvMatrix::Rgb, Rational::integer(30))),
            PixelLayout::Rgba8
        );
    }

    #[test]
    fn decode_rate_is_capped_and_defaults_when_unknown() {
        assert_eq!(
            decode_fps(&video(
                false,
                8,
                YuvMatrix::Bt709,
                Rational::new(30000, 1001)
            )),
            Rational::new(30000, 1001)
        );
        assert_eq!(
            decode_fps(&video(false, 8, YuvMatrix::Bt709, Rational::integer(120))),
            Rational::integer(60)
        );
        assert_eq!(
            decode_fps(&video(false, 8, YuvMatrix::Bt709, Rational::new(0, 1))),
            Rational::integer(30)
        );
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

    fn request(range: TimeRange, chain: DecodeChain) -> DecodeRequest {
        DecodeRequest {
            input: PathBuf::from("clip.mp4"),
            stream_index: 0,
            coded_width: 320,
            coded_height: 240,
            bit_depth: 8,
            has_alpha: false,
            codec: "h264".to_owned(),
            color: ColorInfo::SDR_BT709,
            start_time: 0.0,
            range: Some(range),
            crop: None,
            size: Some((160, 120)),
            fps: Some(Rational::integer(30)),
            chain,
            layout: PixelLayout::Nv12,
        }
    }

    #[test]
    fn source_key_depends_on_every_decode_input() {
        let range = TimeRange::new(0.5, 1.5);
        let base = source_key(&request(range, DecodeChain::CPU));
        assert_eq!(base, source_key(&request(range, DecodeChain::CPU)));
        assert_ne!(
            base,
            source_key(&request(TimeRange::new(0.5, 2.0), DecodeChain::CPU))
        );
        let amf = DecodeChain {
            backend: DecodeBackend::Amf { dxgi_index: 0 },
            scaler: loopcut_core::GpuScaler::Amf,
        };
        assert_ne!(base, source_key(&request(range, amf)));
    }

    #[test]
    fn geometry_must_be_even_and_inside_the_frame() {
        let media_video = video(false, 8, YuvMatrix::Bt709, Rational::integer(30));
        let ok = DecodeGeometry {
            crop: Some(PixelRect {
                x: 0,
                y: 0,
                w: 320,
                h: 240,
            }),
            size: (160, 120),
        };
        assert!(validate_geometry(&media_video, ok).is_ok());
        let odd = DecodeGeometry {
            crop: None,
            size: (161, 120),
        };
        assert!(matches!(
            validate_geometry(&media_video, odd),
            Err(ExportError::InvalidRequest(_))
        ));
        let outside = DecodeGeometry {
            crop: Some(PixelRect {
                x: 300,
                y: 0,
                w: 40,
                h: 40,
            }),
            size: (40, 40),
        };
        assert!(matches!(
            validate_geometry(&media_video, outside),
            Err(ExportError::InvalidRequest(_))
        ));
    }

    #[test]
    fn a_clip_without_ranges_is_refused() {
        // `required_source_ranges` only returns empty for a clip with no segments, which the plan rejects first.
        let clip = Clip {
            id: "c".to_owned(),
            path: "x.mp4".to_owned(),
            stream_index: 0,
            segments: Vec::new(),
            holds: Vec::new(),
        };
        assert!(required_source_ranges(&clip).is_empty());
    }
}

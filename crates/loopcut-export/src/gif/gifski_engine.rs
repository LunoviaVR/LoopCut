//! GIF Quality engine: an adapter over [gifski](https://gif.ski) 1.34.0.
//!
//! [`GifskiEncoder::start`] spawns one writer thread that owns gifski's `Writer`. The caller
//! feeds frames from one thread with [`GifskiEncoder::add_frame`], then calls
//! [`GifskiEncoder::finish`] to complete the GIF or [`GifskiEncoder::cancel`] to stop it.
//!
//! gifski behavior this adapter works around or relies on (verified against gifski 1.34.0 source):
//!
//! - **Size.** With no size set, gifski halves each axis of any frame above roughly 0.84
//!   megapixels (1280x720 becomes 640x360). The adapter always passes the exact output size and
//!   rejects frames of any other size. Scaling belongs to the compositor.
//! - **Order.** gifski needs frame indices 0, 1, 2, ... with no gaps or repeats, and timestamps
//!   that strictly increase. [`GifskiEncoder::add_frame`] enforces both and rejects a bad frame
//!   before it reaches the writer, so the encoder stays usable.
//! - **Duplicates.** Identical consecutive frames are merged: gifski keeps only the last frame of
//!   each identical run, and that frame's delay covers the whole run, so the total duration is
//!   kept. gifski's denoiser can also drop near-identical frames on noisy content and merge their
//!   time into a neighbour. [`GifStats::frames`] can therefore be lower than the number of frames
//!   added.
//! - **Timing.** A frame's delay is the gap to the next frame's timestamp. The last frame has no
//!   next frame, so gifski gives it the previous interval. [`pts_for_exact_delays`] avoids that by
//!   putting the last delay first, so every delay is reproduced exactly.
//! - **Abort.** An encode stops for one of three reasons: the progress callback returns `false`,
//!   [`GifskiEncoder::cancel`] is called, or the encoder is dropped before `finish`. Each sets a
//!   shared flag, and the output wrapper refuses every write once the flag is set (or while the
//!   writer thread unwinds from a panic). gifski writes the trailer only after its input closes,
//!   and `cancel` and `drop` set the flag before closing the input, so an aborted encode never
//!   writes the GIF trailer. The bytes already written stay in the output, so the caller must
//!   delete that file. LoopCut writes to `<name>.partial` and removes it on abort.

use std::io::{self, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use gifski::progress::ProgressReporter;
use gifski::{Collector, Repeat, Settings, Writer};
use imgref::ImgVec;
use rgb::RGBA8;

use super::{GifError, GifProgress, GifStats};

/// Output buffer size. gifski emits many small blocks, so buffering avoids a write per block.
const OUTPUT_BUFFER_BYTES: usize = 64 * 1024;

/// Settings for the gifski engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifskiOptions {
    /// Output width. Every frame must be exactly this wide. Must be 1..=65535.
    pub width: u32,
    /// Output height. Every frame must be exactly this tall. Must be 1..=65535.
    pub height: u32,
    /// gifski quality, 1..=100. The useful range is 50..=100, and 100 is recommended.
    pub quality: u8,
    /// Lossy LZW strength, 1..=100. 100 is lossless and skips gifsicle's lossy pass.
    pub lossy_quality: u8,
    /// Motion (frame-difference) quality, 1..=100. Lower values reduce motion.
    pub motion_quality: u8,
    /// Faster encode with worse quality and a larger file (gifski's `--fast`). Independent of
    /// [`extra_effort`](Self::extra_effort).
    pub fast: bool,
    /// Slower encode for a better palette (gifski's `--extra-effort`). It costs about 1.7 times
    /// the encode time for about 2% smaller output, so it is off by default, as in gifski's CLI.
    pub extra_effort: bool,
    /// Play count, following the project model: `0` loops forever, `1` plays once (no NETSCAPE
    /// loop block), and `N` plays N times in total (written as N - 1 repeats).
    pub loop_count: u16,
}

impl Default for GifskiOptions {
    /// Quality 90, no lossy pass, full motion quality, and gifski's own CLI defaults for speed and
    /// effort. `width` and `height` default to 0, which [`GifskiEncoder::start`] rejects, so the
    /// caller must set them.
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            quality: 90,
            lossy_quality: 100,
            motion_quality: 100,
            fast: false,
            extra_effort: false,
            loop_count: 0,
        }
    }
}

impl GifskiOptions {
    fn validate(&self) -> Result<(), GifError> {
        let max_side = u32::from(u16::MAX);
        if self.width == 0 || self.height == 0 || self.width > max_side || self.height > max_side {
            return Err(GifError::InvalidOptions(format!(
                "size {}x{} must be 1..={max_side} on each side",
                self.width, self.height
            )));
        }
        for (name, value) in [
            ("quality", self.quality),
            ("lossy_quality", self.lossy_quality),
            ("motion_quality", self.motion_quality),
        ] {
            if !(1..=100).contains(&value) {
                return Err(GifError::InvalidOptions(format!(
                    "{name} {value} must be 1..=100"
                )));
            }
        }
        Ok(())
    }
}

/// Running gifski encode: frames in, one GIF file out.
///
/// Dropping the encoder without calling [`finish`](Self::finish) or [`cancel`](Self::cancel)
/// aborts the encode and waits for the writer, so no GIF is completed behind the caller's back.
pub struct GifskiEncoder {
    /// Input queue. Dropped before the writer is joined, so the writer sees the end of input.
    collector: Option<Collector>,
    worker: Option<JoinHandle<Result<GifStats, GifError>>>,
    /// Set by the progress bridge, by `cancel` and by `drop`. Once set, every output write fails.
    abort: Arc<AtomicBool>,
    width: u32,
    height: u32,
    /// Index the next frame must carry. Advanced with `compare_exchange`, so a repeated or skipped
    /// index is rejected before it reaches gifski.
    next_index: AtomicUsize,
    /// Timestamp of the last accepted frame, as `f64::to_bits`. Read only once a frame was accepted.
    last_pts_bits: AtomicU64,
}

impl GifskiEncoder {
    /// Starts an encode that writes to `out`.
    ///
    /// `on_progress` runs on the writer thread after each input frame is accounted for. Returning
    /// `false` aborts the encode: no further bytes reach `out`, and [`finish`](Self::finish)
    /// returns [`GifError::Aborted`].
    pub fn start<W: Write + Send + 'static>(
        opts: &GifskiOptions,
        out: W,
        on_progress: Box<dyn FnMut(GifProgress) -> bool + Send>,
    ) -> Result<Self, GifError> {
        opts.validate()?;

        // Exact size on both axes: gifski only resizes down to these bounds, and frames match them.
        let settings = Settings {
            width: Some(opts.width),
            height: Some(opts.height),
            quality: opts.quality,
            fast: opts.fast,
            repeat: repeat_for_loop_count(opts.loop_count),
        };
        let (collector, mut writer) = gifski::new(settings).map_err(map_gifski_error)?;
        configure_writer(&mut writer, opts);

        let abort = Arc::new(AtomicBool::new(false));
        let worker_abort = Arc::clone(&abort);
        let worker = std::thread::Builder::new()
            .name("loopcut-gifski".into())
            .spawn(move || {
                let mut progress = ProgressBridge {
                    callback: on_progress,
                    abort: Arc::clone(&worker_abort),
                    frames_written: 0,
                    bytes_written: 0,
                };
                write_gif(writer, out, &worker_abort, &mut progress)
            })?;

        Ok(Self {
            collector: Some(collector),
            worker: Some(worker),
            abort,
            width: opts.width,
            height: opts.height,
            next_index: AtomicUsize::new(0),
            last_pts_bits: AtomicU64::new(0),
        })
    }

    /// Queues one frame.
    ///
    /// `rgba` is tightly packed RGBA8 (`width * height * 4` bytes) and must match the size given
    /// to [`start`](Self::start). `index` must be the next index in sequence, starting at 0, so a
    /// repeated or skipped index is rejected. `pts_seconds` is the display time. It must be finite
    /// and non-negative, and every frame after the first must be strictly later than the frame
    /// before it.
    ///
    /// Returns [`GifError::InvalidFrame`] for a bad frame. That error changes nothing, so the
    /// caller can send a corrected frame at the same index. Blocks while the writer's input queue
    /// is full. Returns [`GifError::WriterStopped`] once the writer has ended; call
    /// [`finish`](Self::finish) to learn why. Call this from one thread: frames are ordered by
    /// index, so calls cannot usefully race.
    pub fn add_frame(
        &self,
        index: usize,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        pts_seconds: f64,
    ) -> Result<(), GifError> {
        if width != self.width || height != self.height {
            return Err(GifError::InvalidFrame(format!(
                "frame is {width}x{height} but the encoder was started for {}x{}",
                self.width, self.height
            )));
        }
        let expected_bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if expected_bytes != Some(rgba.len()) {
            return Err(GifError::InvalidFrame(format!(
                "frame {index} has {} bytes but {width}x{height} RGBA needs {}",
                rgba.len(),
                expected_bytes.map_or_else(|| "more than usize::MAX".to_owned(), |n| n.to_string())
            )));
        }
        if !pts_seconds.is_finite() || pts_seconds < 0.0 {
            return Err(GifError::InvalidFrame(format!(
                "frame {index} has invalid timestamp {pts_seconds}"
            )));
        }

        // Every check runs before the index is claimed, so a rejected frame changes nothing.
        let expected_index = self.next_index.load(Ordering::Acquire);
        if index != expected_index {
            return Err(GifError::InvalidFrame(format!(
                "frame index {index} is out of order: the next frame must be {expected_index}"
            )));
        }
        if index > 0 {
            let previous = f64::from_bits(self.last_pts_bits.load(Ordering::Acquire));
            if pts_seconds <= previous {
                return Err(GifError::InvalidFrame(format!(
                    "frame {index} has timestamp {pts_seconds} s, which is not after the \
                     previous frame's {previous} s"
                )));
            }
        }
        let Some(collector) = self.collector.as_ref() else {
            return Err(GifError::WriterStopped);
        };
        let next_index = index
            .checked_add(1)
            .ok_or_else(|| GifError::InvalidFrame("frame index overflows usize".to_owned()))?;
        self.next_index
            .compare_exchange(index, next_index, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|actual| {
                GifError::InvalidFrame(format!(
                    "frame index {index} is out of order: the next frame must be {actual}"
                ))
            })?;
        self.last_pts_bits
            .store(pts_seconds.to_bits(), Ordering::Release);

        let pixels: Vec<RGBA8> = rgb::bytemuck::cast_slice(&rgba).to_vec();
        let image = ImgVec::new(pixels, width as usize, height as usize);
        collector
            .add_frame_rgba(index, image, pts_seconds)
            .map_err(|_| GifError::WriterStopped)
    }

    /// Finishes the GIF and waits for the writer to write the trailer and release its output.
    ///
    /// Returns [`GifError::Aborted`] if the progress callback aborted the encode, and
    /// [`GifError::NoFrames`] if no frame was added. After an error the output is incomplete and
    /// should be deleted.
    pub fn finish(mut self) -> Result<GifStats, GifError> {
        // The writer ends only after its input closes, so close the input before joining.
        drop(self.collector.take());
        match self.worker.take() {
            Some(worker) => join_worker(worker),
            None => Err(GifError::Internal(
                "GIF writer was already joined".to_owned(),
            )),
        }
    }

    /// Aborts the encode and waits for the writer thread to exit, so `out` has been released when
    /// this returns.
    ///
    /// The output holds whatever was written before the abort and has no trailer. The caller
    /// should delete it.
    pub fn cancel(mut self) {
        // The writer's result is discarded: a cancelled encode has no output to report.
        let _ = self.abort_and_join();
    }

    /// Sets the abort flag, closes the input and joins the writer. A second call finds nothing to
    /// join and does nothing.
    fn abort_and_join(&mut self) -> Option<Result<GifStats, GifError>> {
        // The flag must be set before the input closes. gifski writes the trailer only after its
        // input has closed, so the output wrapper always sees the flag before that write.
        self.abort.store(true, Ordering::SeqCst);
        drop(self.collector.take());
        self.worker.take().map(join_worker)
    }
}

impl Drop for GifskiEncoder {
    /// Reached only when neither `finish` nor `cancel` ran. Aborts and joins the writer, so its
    /// output is released before the caller's `drop` returns.
    fn drop(&mut self) {
        let _ = self.abort_and_join();
    }
}

/// Presentation times that make gifski reproduce every delay exactly, including the last frame.
///
/// Frame `i` starts when the delays before it have elapsed, measured from the last delay. The
/// result is in seconds and strictly increasing. gifski treats a non-zero first-frame timestamp as
/// the delay of the last frame and shifts the whole timeline by that amount (`make_diffs` in
/// gifski 1.34.0). This function uses that rule: frame 0 starts at the last delay and every later
/// frame follows it.
///
/// Pair this with [`loopcut_core::gif_timing::delays_cs`]. For example, `[5, 5, 5]` gives
/// `[0.05, 0.10, 0.15]`, and a single frame of 5 cs gives `[0.05]`, so it keeps its 5 cs delay.
/// Every delay must be at least 2 cs. gifski only applies the offset rule above 0.01 s, and
/// [`loopcut_core::gif_timing::MIN_DELAY_CS`] guarantees that. Empty input gives an empty result.
pub fn pts_for_exact_delays(delays_cs: &[u16]) -> Vec<f64> {
    let Some(&last_delay) = delays_cs.last() else {
        return Vec::new();
    };
    let offset_cs = u64::from(last_delay);
    let mut start_cs = offset_cs;
    delays_cs
        .iter()
        .map(|&delay| {
            let pts = start_cs as f64 / 100.0;
            start_cs += u64::from(delay);
            pts
        })
        .collect()
}

/// Maps the project's loop count onto gif's repeat count.
///
/// `0` loops forever, `1` plays once, and `N` plays N times. gif writes no NETSCAPE block for
/// `Finite(0)`, which plays once. N plays need N - 1 repeats.
fn repeat_for_loop_count(loop_count: u16) -> Repeat {
    match loop_count {
        0 => Repeat::Infinite,
        1 => Repeat::Finite(0),
        plays => Repeat::Finite(plays - 1),
    }
}

/// Applies the adapter's encoder settings. The deprecated gifski setters are the only way to set
/// lossy, motion and extra-effort options in 1.34.0, so the allow is confined to this function.
#[allow(deprecated)]
fn configure_writer(writer: &mut Writer, opts: &GifskiOptions) {
    writer.set_lossy_quality(opts.lossy_quality);
    writer.set_motion_quality(opts.motion_quality);
    writer.set_extra_effort(opts.extra_effort);
}

/// Runs gifski's writer to completion and returns the byte and frame counts.
///
/// Returns [`GifError::Aborted`] whenever the abort flag is set, even if gifski reported success,
/// so an aborted encode never reports stats.
fn write_gif<W: Write>(
    writer: Writer,
    out: W,
    abort: &Arc<AtomicBool>,
    progress: &mut ProgressBridge,
) -> Result<GifStats, GifError> {
    // The counter sits outside the buffer, so it sees every byte gifski produces, including the
    // trailer that gif's encoder writes when it is dropped at the end of `write`.
    let guarded = AbortGuard {
        inner: out,
        abort: Arc::clone(abort),
    };
    let mut sink = CountingWriter::new(BufWriter::with_capacity(OUTPUT_BUFFER_BYTES, guarded));
    let written = writer.write(&mut sink, progress);
    if abort.load(Ordering::SeqCst) {
        return Err(GifError::Aborted);
    }
    written.map_err(map_gifski_error)?;
    sink.flush()?;
    // gif 0.13 writes the trailer from `Drop` and discards its error (its `raii_no_panic` feature),
    // so a failed trailer write would otherwise pass silently. The trailer is the single byte
    // after the last size gifski reported, so it must have reached the output.
    if sink.bytes != progress.bytes_written.saturating_add(1) {
        return Err(GifError::Io(io::Error::other(
            "GIF trailer could not be written",
        )));
    }
    Ok(GifStats {
        bytes: sink.bytes,
        frames: usize::try_from(progress.frames_written).unwrap_or(usize::MAX),
    })
}

fn join_worker(worker: JoinHandle<Result<GifStats, GifError>>) -> Result<GifStats, GifError> {
    worker
        .join()
        .unwrap_or_else(|_| Err(GifError::Internal("GIF writer thread panicked".to_owned())))
}

/// Maps gifski's error type onto [`GifError`].
fn map_gifski_error(err: gifski::Error) -> GifError {
    match err {
        gifski::Error::Aborted => GifError::Aborted,
        gifski::Error::NoFrames => GifError::NoFrames,
        gifski::Error::Io(io_err) => GifError::Io(io_err),
        gifski::Error::WrongSize(msg) => GifError::InvalidFrame(msg),
        gifski::Error::ThreadSend => {
            GifError::Internal("gifski pipeline stopped unexpectedly".to_owned())
        }
        other => GifError::Gifski(other.to_string()),
    }
}

/// Bridges gifski's progress callbacks to the caller's closure and to the abort flag.
struct ProgressBridge {
    callback: Box<dyn FnMut(GifProgress) -> bool + Send>,
    abort: Arc<AtomicBool>,
    frames_written: u64,
    bytes_written: u64,
}

impl ProgressReporter for ProgressBridge {
    /// Called once for every input frame gifski has accounted for, including merged duplicates.
    /// A `false` from the callback sets the abort flag, so the output stops at its next write.
    fn increase(&mut self) -> bool {
        if self.abort.load(Ordering::SeqCst) {
            return false;
        }
        let keep_going = (self.callback)(GifProgress {
            frames_written: self.frames_written,
            bytes_written: self.bytes_written,
        });
        if !keep_going {
            self.abort.store(true, Ordering::SeqCst);
        }
        keep_going
    }

    /// Called once for every GIF frame record written, before that frame's `increase` calls.
    fn written_bytes(&mut self, current_file_size_in_bytes: u64) {
        self.frames_written += 1;
        self.bytes_written = current_file_size_in_bytes;
    }

    fn error(&mut self, message: String) {
        tracing::warn!(%message, "gifski reported a usage problem");
    }
}

/// Refuses every write once the encode is aborted.
///
/// It sits below the buffer, so bytes still buffered cannot reach the output after an abort either.
/// It also refuses while the writer thread unwinds from a panic, so a panic cannot leave behind a
/// file that looks finished.
struct AbortGuard<W> {
    inner: W,
    abort: Arc<AtomicBool>,
}

impl<W> AbortGuard<W> {
    fn check(&self) -> io::Result<()> {
        if self.abort.load(Ordering::SeqCst) || std::thread::panicking() {
            Err(io::Error::other("GIF export aborted"))
        } else {
            Ok(())
        }
    }
}

impl<W: Write> Write for AbortGuard<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.inner.flush()
    }
}

/// Counts bytes that pass through. Used as the sink for gifski's output.
struct CountingWriter<W> {
    inner: W,
    bytes: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, bytes: 0 }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Upper bound for a drop or cancel in these tests. A deadlock fails the test instead of
    /// hanging the run.
    const TIMEOUT: Duration = Duration::from_secs(60);

    fn opts(width: u32, height: u32) -> GifskiOptions {
        GifskiOptions {
            width,
            height,
            ..GifskiOptions::default()
        }
    }

    /// Runs `f` on a helper thread and fails the test if it does not return within [`TIMEOUT`].
    fn within_timeout(what: &str, f: impl FnOnce() + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        rx.recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("{what} did not return within {TIMEOUT:?}"));
    }

    /// Output that records when it is dropped. The writer thread owns it, so the flag can only be
    /// set once that thread has finished and released its output.
    struct DropFlagSink {
        dropped: Arc<AtomicBool>,
    }

    impl Write for DropFlagSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for DropFlagSink {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn exact_pts_put_the_last_delay_first() {
        let pts = pts_for_exact_delays(&[7, 7, 6]);
        assert_eq!(pts.len(), 3);
        assert!((pts[0] - 0.06).abs() < 1e-12);
        assert!((pts[1] - 0.13).abs() < 1e-12);
        assert!((pts[2] - 0.20).abs() < 1e-12);
        assert!(pts_for_exact_delays(&[]).is_empty());
    }

    #[test]
    fn single_frame_pts_is_its_own_delay() {
        // The first timestamp is the last delay, so a single 5 cs frame ends at 0.05 s.
        let pts = pts_for_exact_delays(&[5]);
        assert_eq!(pts.len(), 1);
        assert!((pts[0] - 0.05).abs() < 1e-12);
    }

    #[test]
    fn loop_count_maps_onto_gif_repeats() {
        assert_eq!(repeat_for_loop_count(0), Repeat::Infinite);
        assert_eq!(repeat_for_loop_count(1), Repeat::Finite(0));
        assert_eq!(repeat_for_loop_count(2), Repeat::Finite(1));
        assert_eq!(repeat_for_loop_count(3), Repeat::Finite(2));
        assert_eq!(
            repeat_for_loop_count(u16::MAX),
            Repeat::Finite(u16::MAX - 1)
        );
    }

    #[test]
    fn default_options_match_the_recommended_settings() {
        let defaults = GifskiOptions::default();
        assert_eq!(defaults.quality, 90);
        assert_eq!(defaults.lossy_quality, 100);
        assert_eq!(defaults.motion_quality, 100);
        assert!(!defaults.fast);
        assert!(!defaults.extra_effort);
        assert_eq!(defaults.loop_count, 0);
    }

    #[test]
    fn rejects_out_of_range_options() {
        let mut bad = opts(64, 48);
        bad.quality = 0;
        assert!(matches!(
            GifskiEncoder::start(&bad, std::io::sink(), Box::new(|_| true)),
            Err(GifError::InvalidOptions(_))
        ));
        let mut bad = opts(64, 48);
        bad.lossy_quality = 101;
        assert!(matches!(
            GifskiEncoder::start(&bad, std::io::sink(), Box::new(|_| true)),
            Err(GifError::InvalidOptions(_))
        ));
        assert!(matches!(
            GifskiEncoder::start(&opts(0, 48), std::io::sink(), Box::new(|_| true)),
            Err(GifError::InvalidOptions(_))
        ));
        assert!(matches!(
            GifskiEncoder::start(&opts(65_536, 48), std::io::sink(), Box::new(|_| true)),
            Err(GifError::InvalidOptions(_))
        ));
    }

    #[test]
    fn rejects_bad_frames_without_touching_the_writer() {
        let enc =
            GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true)).expect("start");
        assert!(matches!(
            enc.add_frame(0, vec![0; 4 * 4 * 4 - 1], 4, 4, 0.0),
            Err(GifError::InvalidFrame(_))
        ));
        assert!(matches!(
            enc.add_frame(0, vec![0; 4 * 4 * 4], 8, 4, 0.0),
            Err(GifError::InvalidFrame(_))
        ));
        assert!(matches!(
            enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, f64::NAN),
            Err(GifError::InvalidFrame(_))
        ));
        assert!(matches!(
            enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, -0.5),
            Err(GifError::InvalidFrame(_))
        ));
        assert!(matches!(
            enc.add_frame(1, vec![0; 4 * 4 * 4], 4, 4, 0.0),
            Err(GifError::InvalidFrame(_))
        ));
        enc.cancel();
    }

    #[test]
    fn skipped_index_is_rejected() {
        let enc =
            GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true)).expect("start");
        enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, 0.0)
            .expect("frame 0");
        let err = enc
            .add_frame(2, vec![0; 4 * 4 * 4], 4, 4, 0.05)
            .expect_err("index 2 skips index 1");
        assert!(
            matches!(&err, GifError::InvalidFrame(msg) if msg.contains("out of order")),
            "got {err:?}"
        );
        enc.cancel();
    }

    #[test]
    fn duplicate_index_is_rejected() {
        let enc =
            GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true)).expect("start");
        enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, 0.0)
            .expect("frame 0");
        let err = enc
            .add_frame(0, vec![0; 4 * 4 * 4], 4, 4, 0.05)
            .expect_err("index 0 was already used");
        assert!(
            matches!(&err, GifError::InvalidFrame(msg) if msg.contains("out of order")),
            "got {err:?}"
        );
        enc.cancel();
    }

    #[test]
    fn decreasing_timestamp_is_rejected() {
        let enc =
            GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true)).expect("start");
        enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, 0.10)
            .expect("frame 0");
        let err = enc
            .add_frame(1, vec![0; 4 * 4 * 4], 4, 4, 0.05)
            .expect_err("0.05 s is before 0.10 s");
        assert!(
            matches!(&err, GifError::InvalidFrame(msg) if msg.contains("not after")),
            "got {err:?}"
        );
        enc.cancel();
    }

    #[test]
    fn equal_timestamp_is_rejected() {
        let enc =
            GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true)).expect("start");
        enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, 0.0)
            .expect("frame 0");
        assert!(matches!(
            enc.add_frame(1, vec![0; 4 * 4 * 4], 4, 4, 0.0),
            Err(GifError::InvalidFrame(_))
        ));
        enc.cancel();
    }

    #[test]
    fn rejected_frame_leaves_its_index_free() {
        let enc =
            GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true)).expect("start");
        enc.add_frame(0, vec![0; 4 * 4 * 4], 4, 4, 0.0)
            .expect("frame 0");
        assert!(enc.add_frame(1, vec![0; 4 * 4 * 4], 4, 4, 0.0).is_err());
        enc.add_frame(1, vec![0; 4 * 4 * 4], 4, 4, 0.05)
            .expect("the corrected frame 1 must be accepted");
        enc.cancel();
    }

    #[test]
    fn cancel_returns_promptly_with_frames_queued() {
        within_timeout("cancel()", || {
            let enc = GifskiEncoder::start(&opts(64, 48), std::io::sink(), Box::new(|_| true))
                .expect("start");
            for i in 0..3 {
                let pts = i as f64 * 0.05;
                let frame = vec![(i * 40) as u8; 64 * 48 * 4];
                enc.add_frame(i, frame, 64, 48, pts).expect("add");
            }
            enc.cancel();
        });
    }

    #[test]
    fn dropping_without_finish_joins_the_writer_and_releases_the_output() {
        let dropped = Arc::new(AtomicBool::new(false));
        let output = DropFlagSink {
            dropped: Arc::clone(&dropped),
        };
        let started = Instant::now();
        within_timeout("drop()", move || {
            let enc =
                GifskiEncoder::start(&opts(16, 16), output, Box::new(|_| true)).expect("start");
            enc.add_frame(0, vec![9; 16 * 16 * 4], 16, 16, 0.0)
                .expect("add");
            drop(enc);
        });
        assert!(started.elapsed() < TIMEOUT);
        assert!(
            dropped.load(Ordering::SeqCst),
            "drop() must join the writer, which releases the output before returning"
        );
    }
}

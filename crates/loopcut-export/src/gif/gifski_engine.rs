//! GIF Quality engine: an adapter over [gifski](https://gif.ski) 1.34.0.
//!
//! [`GifskiEncoder::start`] spawns one writer thread that owns gifski's `Writer`. The caller
//! feeds frames from its own thread with [`GifskiEncoder::add_frame`] and then calls
//! [`GifskiEncoder::finish`] or [`GifskiEncoder::cancel`].
//!
//! gifski behavior this adapter works around or relies on (verified against gifski 1.34.0 source):
//!
//! - **Size.** With no size set, gifski downscales any frame larger than about 800x600 by area.
//!   The adapter always passes the exact output size and rejects frames of any other size.
//!   Scaling belongs to the compositor.
//! - **Duplicates.** Identical consecutive frames are merged into the earlier frame, and frames
//!   that add nothing can be dropped. Total duration is kept, so [`GifStats::frames`] can be
//!   lower than the number of frames added.
//! - **Timing.** A frame's delay is the gap to the next frame's timestamp. The last frame has no
//!   next frame, so gifski uses the previous interval for its delay. [`pts_from_delays`] therefore
//!   gives the last frame the wrong delay when delays differ. [`pts_for_exact_delays`] avoids that.
//! - **Abort.** The writer thread stays alive until the input queue is closed. `finish` and
//!   `cancel` close it first, so neither can deadlock.

use std::io::{self, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Lossy LZW strength, 1..=100. 100 is lossless and does not run the lossy pass.
    pub lossy_quality: u8,
    /// Motion (frame-difference) quality, 1..=100. Lower values reduce motion.
    pub motion_quality: u8,
    /// Faster encode with a slightly larger file. When `false`, gifski's extra-effort quantizer is
    /// enabled, which is slower but gives better palettes.
    pub fast: bool,
    /// NETSCAPE loop count. `0` loops forever. Any other value is passed through as gifski's
    /// finite repeat count.
    pub loop_count: u16,
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
                return Err(GifError::InvalidOptions(format!("{name} {value} must be 1..=100")));
            }
        }
        Ok(())
    }
}

/// Running gifski encode: frames in, one GIF file out.
///
/// Dropping the encoder without calling [`finish`](Self::finish) or [`cancel`](Self::cancel)
/// aborts the writer, so no partial GIF is completed behind the caller's back.
pub struct GifskiEncoder {
    /// Input queue. Taken (and so dropped) before the writer is joined.
    collector: Option<Collector>,
    worker: Option<JoinHandle<Result<GifStats, GifError>>>,
    abort: Arc<AtomicBool>,
    width: u32,
    height: u32,
}

impl GifskiEncoder {
    /// Starts an encode that writes to `out`.
    ///
    /// `on_progress` runs on the writer thread after each input frame is accounted for. Returning
    /// `false` aborts the encode, and [`finish`](Self::finish) then returns [`GifError::Aborted`].
    pub fn start<W: Write + Send + 'static>(
        opts: &GifskiOptions,
        out: W,
        on_progress: Box<dyn FnMut(GifProgress) -> bool + Send>,
    ) -> Result<Self, GifError> {
        opts.validate()?;

        let repeat = if opts.loop_count == 0 {
            Repeat::Infinite
        } else {
            Repeat::Finite(opts.loop_count)
        };
        // Exact size on both axes: gifski only resizes down to these bounds, and frames match them.
        let settings = Settings {
            width: Some(opts.width),
            height: Some(opts.height),
            quality: opts.quality,
            fast: opts.fast,
            repeat,
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
                    abort: worker_abort,
                    frames_written: 0,
                    bytes_written: 0,
                };
                write_gif(writer, out, &mut progress)
            })?;

        Ok(Self {
            collector: Some(collector),
            worker: Some(worker),
            abort,
            width: opts.width,
            height: opts.height,
        })
    }

    /// Queues one frame.
    ///
    /// `rgba` is tightly packed RGBA8 (`width * height * 4` bytes) and must match the size given
    /// to [`start`](Self::start). Frame `index` starts at 0 and must be contiguous. `pts_seconds`
    /// is the display time, must be finite and non-negative, and must be above zero for every
    /// frame after the first. Timestamps must not decrease. gifski clamps decreasing values
    /// without reporting them.
    ///
    /// Blocks while the writer's input queue is full. Returns [`GifError::WriterStopped`] once
    /// the writer has ended. Call [`finish`](Self::finish) to find out why.
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
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4));
        if expected != Some(rgba.len()) {
            return Err(GifError::InvalidFrame(format!(
                "frame {index} has {} bytes but {width}x{height} RGBA needs {}",
                rgba.len(),
                expected.map_or_else(|| "more than usize::MAX".to_owned(), |n| n.to_string())
            )));
        }
        if !pts_seconds.is_finite() || pts_seconds < 0.0 {
            return Err(GifError::InvalidFrame(format!(
                "frame {index} has invalid timestamp {pts_seconds}"
            )));
        }
        if index > 0 && pts_seconds <= 0.0 {
            return Err(GifError::InvalidFrame(format!(
                "frame {index} must have a timestamp above zero, got {pts_seconds}"
            )));
        }

        let Some(collector) = self.collector.as_ref() else {
            return Err(GifError::WriterStopped);
        };
        let pixels: Vec<RGBA8> = rgb::bytemuck::cast_slice(&rgba).to_vec();
        let image = ImgVec::new(pixels, width as usize, height as usize);
        collector
            .add_frame_rgba(index, image, pts_seconds)
            .map_err(|_| GifError::WriterStopped)
    }

    /// Finishes the GIF and waits for the writer to write the trailer and close its output.
    ///
    /// Returns [`GifError::Aborted`] if the progress callback aborted the encode, and
    /// [`GifError::NoFrames`] if no frame was added.
    pub fn finish(mut self) -> Result<GifStats, GifError> {
        // The writer ends only after its input closes, so close the input before joining.
        drop(self.collector.take());
        let Some(worker) = self.worker.take() else {
            return Err(GifError::Internal("GIF writer was already joined".to_owned()));
        };
        join_worker(worker)
    }

    /// Aborts the encode and waits for the writer thread to exit. Output written so far is left
    /// in place, so the caller should delete it.
    pub fn cancel(mut self) {
        self.abort.store(true, Ordering::Relaxed);
        drop(self.collector.take());
        if let Some(worker) = self.worker.take() {
            // The writer's own result is always `Aborted` here, so it is discarded.
            let _ = join_worker(worker);
        }
    }
}

impl Drop for GifskiEncoder {
    fn drop(&mut self) {
        // Reached only when neither finish() nor cancel() ran: stop the writer so it does not
        // complete a truncated GIF. After a normal finish, both fields are already empty.
        self.abort.store(true, Ordering::Relaxed);
        drop(self.collector.take());
    }
}

/// Presentation times in seconds for frames shown for `delays_cs`, cumulative and starting at 0.0.
///
/// Frame `i` starts when the delays before it have elapsed. Pair this with
/// [`loopcut_core::gif_timing::delays_cs`]: `pts_from_delays(&[5, 5, 5])` is `[0.0, 0.05, 0.10]`.
///
/// Caveat: gifski gives the last frame the interval before it, not its own delay. Use
/// [`pts_for_exact_delays`] when the delays are not all equal.
pub fn pts_from_delays(delays_cs: &[u16]) -> Vec<f64> {
    let mut start_cs: u64 = 0;
    delays_cs
        .iter()
        .map(|&delay| {
            let pts = start_cs as f64 / 100.0;
            start_cs += u64::from(delay);
            pts
        })
        .collect()
}

/// Presentation times that make gifski reproduce every delay exactly, including the last frame.
///
/// gifski documents that a non-zero first-frame timestamp is used as the delay of the last frame,
/// and shifts the rest of the timeline back by that amount. This function uses that rule: frame 0
/// starts at the last delay and every later frame follows it. The result is the same as
/// [`pts_from_delays`] except for that offset.
///
/// Every delay must be at least 2 cs. gifski only applies the offset rule above 0.01 s, and
/// [`loopcut_core::gif_timing::MIN_DELAY_CS`] guarantees that. Empty input gives an empty result.
pub fn pts_for_exact_delays(delays_cs: &[u16]) -> Vec<f64> {
    let Some(&last_delay) = delays_cs.last() else {
        return Vec::new();
    };
    let offset = f64::from(last_delay) / 100.0;
    pts_from_delays(delays_cs)
        .into_iter()
        .map(|pts| pts + offset)
        .collect()
}

/// Applies the adapter's encoder settings. The deprecated gifski setters are the only way to set
/// lossy and motion quality in 1.34.0, so the allow is confined to this function.
#[allow(deprecated)]
fn configure_writer(writer: &mut Writer, opts: &GifskiOptions) {
    writer.set_lossy_quality(opts.lossy_quality);
    writer.set_motion_quality(opts.motion_quality);
    if !opts.fast {
        writer.set_extra_effort(true);
    }
}

/// Runs gifski's writer to completion and returns the byte and frame counts.
fn write_gif<W: Write>(
    writer: Writer,
    out: W,
    progress: &mut ProgressBridge,
) -> Result<GifStats, GifError> {
    // The counter sits outside the buffer, so it sees every byte gifski produces, including the
    // trailer that gif's encoder writes when it is dropped at the end of `write`.
    let mut sink = CountingWriter::new(BufWriter::with_capacity(OUTPUT_BUFFER_BYTES, out));
    writer
        .write(&mut sink, progress)
        .map_err(map_gifski_error)?;
    sink.flush()?;
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
    fn increase(&mut self) -> bool {
        if self.abort.load(Ordering::Relaxed) {
            return false;
        }
        (self.callback)(GifProgress {
            frames_written: self.frames_written,
            bytes_written: self.bytes_written,
        })
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
    use std::time::{Duration, Instant};

    fn opts(width: u32, height: u32) -> GifskiOptions {
        GifskiOptions {
            width,
            height,
            quality: 90,
            lossy_quality: 100,
            motion_quality: 90,
            fast: false,
            loop_count: 0,
        }
    }

    #[test]
    fn pts_start_at_zero_and_accumulate_centiseconds() {
        assert_eq!(pts_from_delays(&[]), Vec::<f64>::new());
        assert_eq!(pts_from_delays(&[5, 5, 5]), vec![0.0, 0.05, 0.10]);
        assert_eq!(pts_from_delays(&[7, 6]), vec![0.0, 0.07]);
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
        let enc = GifskiEncoder::start(&opts(4, 4), std::io::sink(), Box::new(|_| true))
            .expect("start");
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
    fn cancel_returns_promptly_with_frames_queued() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let enc = GifskiEncoder::start(&opts(64, 48), std::io::sink(), Box::new(|_| true))
                .expect("start");
            for i in 0..3 {
                let pts = i as f64 * 0.05;
                let frame = vec![(i * 40) as u8; 64 * 48 * 4];
                enc.add_frame(i, frame, 64, 48, pts).expect("add");
            }
            enc.cancel();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("cancel() deadlocked");
    }

    #[test]
    fn dropping_without_finish_does_not_hang() {
        let started = Instant::now();
        {
            let enc = GifskiEncoder::start(&opts(16, 16), std::io::sink(), Box::new(|_| true))
                .expect("start");
            enc.add_frame(0, vec![9; 16 * 16 * 4], 16, 16, 0.0).expect("add");
        }
        assert!(started.elapsed() < Duration::from_secs(60));
    }
}

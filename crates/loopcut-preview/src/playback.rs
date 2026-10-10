//! Playback: a render thread that walks the plan at playback quality, and a sink thread that receives the frames.
//!
//! The render thread keeps the decode window on the frame it plays, renders each frame, and sleeps until the frame is
//! due. A frame that is late when it is due, or late again once rendered, is dropped rather than shown late, and a
//! schedule that has fallen half a second behind restarts from now. Frames go to the sink through a bounded channel of
//! two, so a slow sink never blocks rendering for longer than that queue allows, and the render thread counts the
//! frames it drops.
//!
//! Pacing follows the format: a GIF plays each frame for its centisecond delay, and any other format plays each frame
//! for one frame of the plan's rate.
//!
//! A frame that is cancelled while it renders (the project changed under it, or a claim was preempted) is shown again
//! from the same index. Only the end of the plan, a stop, a sink that returns `false`, a decode error, or the engine's
//! shutdown end a playback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, SendTimeoutError, Sender, bounded};
use parking_lot::Mutex;

use crate::engine::{PreviewEngine, Shared, move_playhead, render};
use crate::error::PreviewError;
use crate::faults;
use crate::model::Want;
use crate::types::{PreviewQuality, RenderedFrame};

/// Receives each frame when it is due. Return `false` to stop playback.
pub type FrameSink = Box<dyn FnMut(&RenderedFrame) -> bool + Send>;

/// A schedule this far behind restarts from now instead of catching up frame by frame.
const RESYNC: Duration = Duration::from_millis(500);

/// How often a waiting thread checks its stop flag.
const STOP_POLL: Duration = Duration::from_millis(5);

/// Frames queued between the render thread and the sink thread.
const SINK_QUEUE: usize = 2;

/// Shortest slot a frame is shown for. Zero delays from a GIF still need time to be scheduled.
const MIN_SLOT: Duration = Duration::from_millis(10);

/// Flags and results shared by a playback's threads and its handle.
struct Control {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<PreviewError>>>,
    dropped: Arc<AtomicU64>,
}

impl Control {
    /// Records a terminal error, for the handle and for [`crate::PreviewEngine::status`], and stops. A cancelled render
    /// is a normal end, not an error.
    fn fail(&self, shared: &Shared, err: PreviewError) {
        if matches!(err, PreviewError::Cancelled) {
            return;
        }
        tracing::warn!(error = %err, "playback stopped on an error");
        shared.core.lock().last_error = Some(err.to_string());
        let mut slot = self.error.lock();
        if slot.is_none() {
            *slot = Some(err);
        }
        self.stop.store(true, Ordering::Release);
    }
}

/// A running playback. Dropping the handle stops it. [`PlaybackHandle::wait`] waits for it to end on its own.
pub struct PlaybackHandle {
    stop: Arc<AtomicBool>,
    error: Arc<Mutex<Option<PreviewError>>>,
    dropped: Arc<AtomicU64>,
    threads: Vec<JoinHandle<()>>,
}

impl PlaybackHandle {
    /// Asks the playback to stop after the frame in progress.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// True while a playback thread is still running.
    pub fn is_running(&self) -> bool {
        self.threads.iter().any(|thread| !thread.is_finished())
    }

    /// Waits for playback to end on its own (the end of a one-shot plan, or the sink returning `false`). To end it
    /// early, call [`PlaybackHandle::stop`] first.
    pub fn wait(mut self) {
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }

    /// Takes the terminal error that stopped the playback, if one did. A cancelled playback (the engine was dropped, or
    /// the plan was replaced under it) has none. The error is taken once: a second call returns `None`.
    pub fn take_error(&self) -> Option<PreviewError> {
        self.error.lock().take()
    }

    /// Frames dropped so far because they were late.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for PlaybackHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl PreviewEngine {
    /// Plays the plan from output frame `from` on a dedicated thread, calling `sink` with each frame as it is due.
    ///
    /// Playback renders at [`PreviewQuality::Reduced`]. It ends when `sink` returns `false`, when
    /// [`PlaybackHandle::stop`] or [`PreviewEngine::stop`] is called, when the engine is dropped, at the end of the plan
    /// unless `looping` is set, or on a decode error (taken with [`PlaybackHandle::take_error`]). A looping plan that
    /// shrinks under playback wraps within its new length. Starting playback stops the previous one. Editing the project
    /// during playback is allowed, and the frames follow the new plan from the frame that is due next.
    ///
    /// # Errors
    ///
    /// [`PreviewError::NoProject`] without a project, [`PreviewError::FrameOutOfRange`] when `from` is past the end of
    /// the plan, and [`PreviewError::Spawn`] when a thread cannot start.
    pub fn play(
        &self,
        from: u32,
        looping: bool,
        sink: FrameSink,
    ) -> Result<PlaybackHandle, PreviewError> {
        let frames = crate::engine::plan_frames(&self.shared)?;
        if from >= frames {
            return Err(PreviewError::FrameOutOfRange {
                frame: from,
                frames,
            });
        }
        self.stop();
        let control = Control {
            stop: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(None)),
            dropped: Arc::new(AtomicU64::new(0)),
        };
        *self.shared.playback.lock() = Some(Arc::clone(&control.stop));
        let (frame_tx, frame_rx) = bounded::<RenderedFrame>(SINK_QUEUE);
        let (recycle_tx, recycle_rx) = bounded::<RenderedFrame>(SINK_QUEUE + 2);
        let sink_stop = Arc::clone(&control.stop);
        let sink_thread = thread::Builder::new()
            .name("loopcut-preview-sink".to_owned())
            .spawn(move || run_sink(&frame_rx, &recycle_tx, sink, &sink_stop))
            .map_err(|err| {
                release_slot(&self.shared, &control.stop);
                PreviewError::Spawn(err)
            })?;
        let play_thread = {
            let shared = Arc::clone(&self.shared);
            let stop = Arc::clone(&control.stop);
            let error = Arc::clone(&control.error);
            let dropped = Arc::clone(&control.dropped);
            // The guard clears the playing flag when the thread ends, including when it panics, and when the thread
            // never starts: the closure is then dropped with the guard inside it.
            let slot = SlotGuard {
                shared: Arc::clone(&self.shared),
                stop: Arc::clone(&control.stop),
            };
            thread::Builder::new()
                .name("loopcut-preview-play".to_owned())
                .spawn(move || {
                    let _slot = slot;
                    let control = Control {
                        stop,
                        error,
                        dropped,
                    };
                    faults::playback_start(&shared);
                    run_playback(&shared, &control, from, looping, &frame_tx, &recycle_rx);
                })
                .map_err(|err| {
                    control.stop.store(true, Ordering::Release);
                    release_slot(&self.shared, &control.stop);
                    PreviewError::Spawn(err)
                })?
        };
        Ok(PlaybackHandle {
            stop: control.stop,
            error: control.error,
            dropped: control.dropped,
            threads: vec![play_thread, sink_thread],
        })
    }

    /// Stops the playback started by [`PreviewEngine::play`], if one is running. It returns without waiting.
    pub fn stop(&self) {
        if let Some(flag) = self.shared.playback.lock().take() {
            flag.store(true, Ordering::Release);
        }
    }
}

/// Clears the playback slot when the playback that owns it ends. The slot is what [`PreviewEngine::status`] reports as
/// `playing`, so it must be cleared however the thread ends.
struct SlotGuard {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        release_slot(&self.shared, &self.stop);
    }
}

/// Clears the playback slot when it still belongs to the playback that ends.
fn release_slot(shared: &Shared, stop: &Arc<AtomicBool>) {
    let mut slot = shared.playback.lock();
    if slot.as_ref().is_some_and(|flag| Arc::ptr_eq(flag, stop)) {
        *slot = None;
    }
}

/// The sink thread: hands each frame to the sink, then returns the buffer for reuse. Ends when the render thread drops
/// its sender, or when the sink returns `false`, which also stops the playback.
fn run_sink(
    frames: &Receiver<RenderedFrame>,
    recycled: &Sender<RenderedFrame>,
    mut sink: FrameSink,
    stop: &AtomicBool,
) {
    for frame in frames.iter() {
        let keep_going = sink(&frame);
        let _ = recycled.try_send(frame);
        if !keep_going {
            stop.store(true, Ordering::Release);
            break;
        }
    }
}

/// What the render thread needs to show the plan frame at `index`.
struct Slot {
    index: u32,
    duration: Duration,
}

/// The plan frame to show for `index`: wrapped into the plan when looping, or `None` at the end of a one-shot plan. The
/// slot is the frame's GIF delay, or one frame of the plan's rate for other formats.
fn playback_slot(shared: &Shared, index: u32, looping: bool) -> Result<Option<Slot>, PreviewError> {
    let core = shared.core.lock();
    if core.shutdown {
        return Err(PreviewError::Cancelled);
    }
    let active = core.active.as_ref().ok_or(PreviewError::NoProject)?;
    let len = u32::try_from(active.plan.frames.len()).unwrap_or(u32::MAX);
    if len == 0 {
        return Ok(None);
    }
    let index = if index < len {
        index
    } else if looping {
        index % len
    } else {
        return Ok(None);
    };
    let planned = active.plan.frames[index as usize];
    let duration = if active.is_gif {
        Duration::from_millis(u64::from(planned.delay_cs) * 10)
    } else {
        Duration::from_secs_f64(1.0 / active.plan.fps.as_f64())
    };
    Ok(Some(Slot {
        index,
        duration: duration.max(MIN_SLOT),
    }))
}

/// Renders plan frame `index` for playback. A test can arm a cancellation, which the frame then reports.
fn play_render(shared: &Shared, index: u32, frame: &mut RenderedFrame) -> Result<(), PreviewError> {
    if faults::play_frame_cancelled(shared) {
        return Err(PreviewError::Cancelled);
    }
    render(
        shared,
        Want::Plan {
            index,
            quality: PreviewQuality::Reduced,
        },
        true,
        frame,
    )
}

/// True once the engine has been shut down.
fn is_shut_down(shared: &Shared) -> bool {
    shared.core.lock().shutdown
}

/// The render loop. Runs on the render thread.
fn run_playback(
    shared: &Shared,
    control: &Control,
    from: u32,
    looping: bool,
    frames: &Sender<RenderedFrame>,
    recycled: &Receiver<RenderedFrame>,
) {
    let mut frame = RenderedFrame::default();
    let mut index = from;
    // When the frame at `index` is due. None until the first frame is ready: waiting for its source to decode is not
    // lateness.
    let mut due: Option<Instant> = None;
    loop {
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        let slot = match playback_slot(shared, index, looping) {
            Ok(Some(slot)) => slot,
            Ok(None) => break,
            Err(err) => {
                control.fail(shared, err);
                break;
            }
        };
        index = slot.index;
        let now = Instant::now();
        if let Some(scheduled) = due {
            if now > scheduled + RESYNC {
                due = Some(now);
            } else if now >= scheduled + slot.duration {
                // The whole slot has passed before the frame was rendered: skip it rather than show it late.
                control.dropped.fetch_add(1, Ordering::Relaxed);
                due = Some(scheduled + slot.duration);
                index += 1;
                continue;
            }
        }
        // The decode window follows the frame being played.
        move_playhead(shared, index);
        if let Err(err) = play_render(shared, index, &mut frame) {
            if matches!(err, PreviewError::Cancelled)
                && !control.stop.load(Ordering::Acquire)
                && !is_shut_down(shared)
            {
                // The frame was cancelled by a plan change or a preemption, not by the end of playback: show the
                // same index again. The schedule is kept as it is.
                continue;
            }
            control.fail(shared, err);
            break;
        }
        // Rendering takes time: check lateness again before the frame is shown.
        let now = Instant::now();
        let scheduled = due.unwrap_or(now);
        if due.is_some() && now >= scheduled + slot.duration {
            control.dropped.fetch_add(1, Ordering::Relaxed);
            due = Some(scheduled + slot.duration);
            index += 1;
            continue;
        }
        if !sleep_until(scheduled, &control.stop) {
            break;
        }
        if !send_frame(frames, recycled, &mut frame, &control.stop) {
            break;
        }
        due = Some(scheduled + slot.duration);
        index += 1;
    }
}

/// Hands `frame` to the sink thread, waiting while its queue is full and giving up when stopped. The frame is replaced
/// with a recycled buffer. Returns `false` when the playback should end.
fn send_frame(
    frames: &Sender<RenderedFrame>,
    recycled: &Receiver<RenderedFrame>,
    frame: &mut RenderedFrame,
    stop: &AtomicBool,
) -> bool {
    let mut outgoing = std::mem::take(frame);
    loop {
        match frames.send_timeout(outgoing, STOP_POLL) {
            Ok(()) => break,
            Err(SendTimeoutError::Timeout(back)) => {
                if stop.load(Ordering::Acquire) {
                    return false;
                }
                outgoing = back;
            }
            Err(SendTimeoutError::Disconnected(_)) => return false,
        }
    }
    *frame = recycled.try_recv().unwrap_or_default();
    true
}

/// Sleeps until `deadline`, checking `stop` often. Returns `false` when stopped first.
fn sleep_until(deadline: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        thread::sleep((deadline - now).min(STOP_POLL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_gives_up_when_stopped() {
        let stop = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!sleep_until(started + Duration::from_secs(10), &stop));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

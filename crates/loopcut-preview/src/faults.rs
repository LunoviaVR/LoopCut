//! Fault hooks for the engine's unit tests.
//!
//! Each hook is a function with a test body and an empty body in other builds, so production code carries no test
//! state. The armed faults live on one engine's shared state, so tests that run in parallel do not see each other's.
//! The hooks reproduce the timing and failure cases the tests need, which the real decoder cannot produce on demand: a
//! decode that stalls, a preempted claim, a playback frame that is cancelled, a playback thread that panics, and a
//! decode that fails in its middle.

use loopcut_media::CancelToken;

use crate::engine::Shared;
use crate::error::PreviewError;
use crate::model::Claim;

/// Faults armed on one engine. Everything is off by default.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Faults {
    /// The worker idles instead of claiming decodes.
    pub(crate) pause_worker: std::sync::atomic::AtomicBool,
    /// The worker sleeps inside a decode, after it has claimed the entry, until this clears.
    pub(crate) stall_decode: std::sync::atomic::AtomicBool,
    /// True while the worker sleeps in a stalled decode.
    pub(crate) stalled: std::sync::atomic::AtomicBool,
    /// The next claim a render runs (not the worker) is cancelled before it starts. Counts down.
    pub(crate) cancel_next_claim: std::sync::atomic::AtomicU32,
    /// The next frame a playback thread renders reports `Cancelled` instead. Counts down.
    pub(crate) cancel_next_play_frame: std::sync::atomic::AtomicU32,
    /// The playback thread panics when it starts.
    pub(crate) panic_playback: std::sync::atomic::AtomicBool,
    /// Decodes fail with a data error once this many frames have arrived. Zero disables it.
    pub(crate) data_error_after: std::sync::atomic::AtomicU64,
}

/// Takes one count from `counter`. True when a count was there to take.
#[cfg(test)]
fn take(counter: &std::sync::atomic::AtomicU32) -> bool {
    use std::sync::atomic::Ordering;
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
        .is_ok()
}

/// True while the worker is held idle.
#[cfg(test)]
pub(crate) fn worker_paused(shared: &Shared) -> bool {
    shared
        .faults
        .pause_worker
        .load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(not(test))]
pub(crate) fn worker_paused(_shared: &Shared) -> bool {
    false
}

/// On the worker thread, sleeps while a stall is armed (until the claim is cancelled or the stall clears).
#[cfg(test)]
pub(crate) fn stall_decode(shared: &Shared, cancel: &CancelToken) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    if std::thread::current().name() != Some(crate::worker::WORKER_THREAD) {
        return;
    }
    if !shared.faults.stall_decode.load(Ordering::Acquire) {
        return;
    }
    shared.faults.stalled.store(true, Ordering::Release);
    while shared.faults.stall_decode.load(Ordering::Acquire) && !cancel.is_cancelled() {
        std::thread::sleep(Duration::from_millis(2));
    }
    shared.faults.stalled.store(false, Ordering::Release);
}

#[cfg(not(test))]
pub(crate) fn stall_decode(_shared: &Shared, _cancel: &CancelToken) {}

/// The data error to report instead of frame number `frames` of a decode, when one is armed for that point.
#[cfg(test)]
pub(crate) fn data_error(shared: &Shared, frames: u64) -> Option<PreviewError> {
    let after = shared
        .faults
        .data_error_after
        .load(std::sync::atomic::Ordering::Acquire);
    (after > 0 && frames == after).then(|| PreviewError::Decode("injected data error".to_owned()))
}

#[cfg(not(test))]
pub(crate) fn data_error(_shared: &Shared, _frames: u64) -> Option<PreviewError> {
    None
}

/// Cancels `claim` before a render runs it, when a cancellation is armed.
#[cfg(test)]
pub(crate) fn claim_cancel(shared: &Shared, claim: &Claim) {
    if take(&shared.faults.cancel_next_claim) {
        claim.cancel.cancel();
    }
}

#[cfg(not(test))]
pub(crate) fn claim_cancel(_shared: &Shared, _claim: &Claim) {}

/// True when the playback frame about to render must report `Cancelled`.
#[cfg(test)]
pub(crate) fn play_frame_cancelled(shared: &Shared) -> bool {
    take(&shared.faults.cancel_next_play_frame)
}

#[cfg(not(test))]
pub(crate) fn play_frame_cancelled(_shared: &Shared) -> bool {
    false
}

/// Panics on the playback thread when a panic is armed.
#[cfg(test)]
pub(crate) fn playback_start(shared: &Shared) {
    if shared
        .faults
        .panic_playback
        .load(std::sync::atomic::Ordering::Acquire)
    {
        panic!("injected playback panic");
    }
}

#[cfg(not(test))]
pub(crate) fn playback_start(_shared: &Shared) {}

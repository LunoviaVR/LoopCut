//! The decode side: the worker thread that decodes the window, the claims that run a decode, and the bookkeeping that
//! records how each one ended.
//!
//! A claim is an entry marked `Decoding` together with a serial and a cancel token. Its result is recorded only when
//! the entry still holds that serial, so a claim that was released by the window or replaced by a new plan cannot
//! overwrite newer state. A claim that ends without a result it owns removes its frames, unless another entry uses the
//! same key.
//!
//! # Hardware fallback
//!
//! A hardware chain is replaced by the CPU chain only when its *session* fails: it cannot start, its set-up fails, or it
//! exits before delivering a frame. Those are the failures that say the chain does not work on this machine. A decode that
//! fails after frames have arrived is a data error. It is recorded on its entry and reported, and the source keeps its
//! chain, because the next range may decode fine.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use loopcut_core::DecodeChain;
use loopcut_core::hw::{FORCE_HW_FAIL_ENV, HwStage, forced_failure_from_env};
use loopcut_media::CancelToken;
use loopcut_media::MediaError;
use loopcut_media::decode::DecodeSession;
use loopcut_media::framestore::{FrameKey, StoredFrame};

use crate::engine::Shared;
use crate::error::PreviewError;
use crate::faults;
use crate::model::{Claim, EntryState, Running};
use crate::source::source_key;
use crate::types::DecodeFallback;

/// Name of the decode worker's thread.
pub(crate) const WORKER_THREAD: &str = "loopcut-preview-decode";

/// How often an idle worker re-checks a test pause.
const PAUSE_POLL: Duration = Duration::from_millis(2);

/// Maps decoder errors: a cancelled decode is reported as cancelled, anything else keeps its message.
fn map_media(err: MediaError) -> PreviewError {
    match err {
        MediaError::Cancelled => PreviewError::Cancelled,
        other => PreviewError::Decode(other.to_string()),
    }
}

/// A decode attempt that failed.
#[derive(Debug)]
pub(crate) struct Failure {
    pub error: PreviewError,
    /// True when the session failed before it delivered a frame: it did not start, its set-up failed, or it exited
    /// with no frame. Only such a failure may switch a hardware chain to the CPU.
    pub setup: bool,
}

impl Failure {
    /// A failure that happened `before_first_frame` or after it. A cancellation is never a setup failure.
    fn new(error: PreviewError, before_first_frame: bool) -> Self {
        let setup = before_first_frame && !matches!(error, PreviewError::Cancelled);
        Self { error, setup }
    }
}

/// The decode worker: decodes the in-window chunk nearest to the playhead, one claim at a time, until shutdown.
pub(crate) fn worker_loop(shared: &Shared) {
    loop {
        let claim = {
            let mut core = shared.core.lock();
            loop {
                if core.shutdown {
                    return;
                }
                if faults::worker_paused(shared) {
                    shared.changed.wait_for(&mut core, PAUSE_POLL);
                    continue;
                }
                if let Some(claim) = core.next_claim() {
                    break claim;
                }
                shared.changed.wait(&mut core);
            }
        };
        // A claim that ends early is recorded on its entry. The worker takes the next one.
        let _ = run_claim(shared, claim);
    }
}

/// Runs a claim: decodes its range into the store and records the outcome. A hardware chain whose session fails is
/// retried on the CPU, and the rest of the source stays on the CPU.
///
/// Returns [`PreviewError::Cancelled`] when the claim was cancelled, so that a caller waiting on it can stop. Any other
/// failure is recorded on the entry and returns `Ok`.
pub(crate) fn run_claim(shared: &Shared, claim: Claim) -> Result<(), PreviewError> {
    if claim.cancel.is_cancelled() {
        finish_claim(shared, &claim, Err(PreviewError::Cancelled));
        return Err(PreviewError::Cancelled);
    }
    let mut claim = claim;
    let mut result = decode_range(shared, &claim);
    if let Err(failure) = &result
        && failure.setup
        && claim.request.chain.backend.is_hardware()
        && let Some(retry) = cpu_retry(shared, &claim, failure.error.to_string())
    {
        result = decode_range(shared, &retry);
        claim = retry;
    }
    let outcome = result.map_err(|failure| failure.error);
    let cancelled = matches!(outcome, Err(PreviewError::Cancelled));
    finish_claim(shared, &claim, outcome);
    if cancelled {
        Err(PreviewError::Cancelled)
    } else {
        Ok(())
    }
}

/// Switches the source to the CPU chain and returns the same range decoded on it. `None` when the claim is no longer
/// the one decoding its entry.
fn cpu_retry(shared: &Shared, claim: &Claim, reason: String) -> Option<Claim> {
    let mut guard = shared.core.lock();
    let core = &mut *guard;
    let serial = core.serial();
    let active = core
        .active
        .as_mut()
        .filter(|active| active.generation == claim.generation)?;
    active.claimed_entry_mut(claim.entry, claim.serial)?;
    let backend = claim.request.chain.backend.name();
    tracing::warn!(backend, reason = %reason, "hardware decode session failed; decoding on the CPU from here on");
    core.cpu_fallback = true;
    core.fallbacks.push(DecodeFallback {
        from: backend.to_owned(),
        to: "cpu".to_owned(),
        reason,
    });
    let active = core
        .active
        .as_mut()
        .filter(|active| active.generation == claim.generation)?;
    active.template.chain = DecodeChain::CPU;
    let mut request = claim.request.clone();
    request.chain = DecodeChain::CPU;
    let key = source_key(&request);
    let cancel = CancelToken::new();
    let entry = active.claimed_entry_mut(claim.entry, claim.serial)?;
    entry.state = EntryState::Decoding { key, decoded: 0 };
    entry.running = Some(Running {
        serial,
        cancel: cancel.clone(),
    });
    shared.store.remove_source(claim.key);
    Some(Claim {
        generation: claim.generation,
        serial,
        entry: claim.entry,
        key,
        range: claim.range,
        request,
        cancel,
    })
}

/// Records how a claim ended. A result is kept only when the claim still owns its entry and the plan is the same
/// generation. A finished decode clears the last error. Frames of a claim that did not finish are removed, unless
/// another entry uses the same key.
fn finish_claim(shared: &Shared, claim: &Claim, result: Result<u64, PreviewError>) {
    let mut guard = shared.core.lock();
    let core = &mut *guard;
    let mut owned = false;
    let mut failure = None;
    if let Some(active) = core
        .active
        .as_mut()
        .filter(|active| active.generation == claim.generation)
        && let Some(entry) = active.claimed_entry_mut(claim.entry, claim.serial)
    {
        owned = true;
        entry.running = None;
        entry.state = match &result {
            Ok(frames) => EntryState::Done {
                key: claim.key,
                frames: *frames,
            },
            Err(PreviewError::Cancelled) => EntryState::Pending,
            Err(err) => {
                let message = err.to_string();
                failure = Some(message.clone());
                EntryState::Failed(message)
            }
        };
    }
    if owned && result.is_ok() {
        core.last_error = None;
    }
    if let Some(message) = failure {
        core.last_error = Some(message);
    }
    let finished = owned && result.is_ok();
    if !finished {
        let in_use = core
            .active
            .as_ref()
            .is_some_and(|active| active.uses_key(claim.key));
        if !in_use {
            shared.store.remove_source(claim.key);
        }
    }
    shared.changed.notify_all();
}

/// Decodes a claim's range into the store, one frame at a time, reporting progress as it goes. Returns the number of
/// frames decoded. Zero frames is a normal result: the range lies past the frames the stream holds.
fn decode_range(shared: &Shared, claim: &Claim) -> Result<u64, Failure> {
    if claim.cancel.is_cancelled() {
        // The claim was cancelled before it started: do not start a decoder for it.
        return Err(Failure::new(PreviewError::Cancelled, true));
    }
    faults::stall_decode(shared, &claim.cancel);
    // Frames left under this key by an interrupted attempt must not mix with the new ones.
    shared.store.remove_source(claim.key);
    let backend = claim.request.chain.backend;
    if forced_failure_from_env(HwStage::Decode, backend.name()) {
        // Reported before any process starts, so the failure takes the same path as a real one.
        return Err(Failure::new(
            PreviewError::Decode(format!(
                "{FORCE_HW_FAIL_ENV} forces the {} decode chain to fail",
                backend.name()
            )),
            true,
        ));
    }
    shared.decode_sessions.fetch_add(1, Ordering::Relaxed);
    let session = DecodeSession::start(&shared.paths, &claim.request, claim.cancel.clone())
        .map_err(|err| Failure::new(map_media(err), true))?;
    let mut count = 0u64;
    for item in session {
        let frame = match item {
            Ok(frame) => frame,
            Err(err) => return Err(Failure::new(map_media(err), count == 0)),
        };
        if let Some(error) = faults::data_error(shared, count) {
            return Err(Failure::new(error, count == 0));
        }
        shared.store.insert(
            FrameKey {
                source: claim.key,
                index: frame.index,
            },
            StoredFrame {
                width: frame.width,
                height: frame.height,
                layout: frame.layout,
                time: frame.time,
                data: Arc::from(frame.data),
            },
        );
        count += 1;
        report(shared, claim, count);
    }
    Ok(count)
}

/// Publishes the number of frames decoded so far for a claim that still owns its entry.
fn report(shared: &Shared, claim: &Claim, decoded: u64) {
    {
        let mut guard = shared.core.lock();
        if let Some(active) = guard
            .active
            .as_mut()
            .filter(|active| active.generation == claim.generation)
            && let Some(entry) = active.claimed_entry_mut(claim.entry, claim.serial)
            && let EntryState::Decoding { decoded: count, .. } = &mut entry.state
        {
            *count = decoded;
        }
    }
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_failures_before_the_first_frame_are_setup_failures() {
        assert!(Failure::new(PreviewError::Decode("start".to_owned()), true).setup);
        assert!(!Failure::new(PreviewError::Decode("data".to_owned()), false).setup);
        assert!(
            !Failure::new(PreviewError::Cancelled, true).setup,
            "a cancellation says nothing about the chain"
        );
    }
}

//! Planner state: the plan's source ranges as decode entries, the decode window that keeps the entries near the
//! playhead, and the claims that run the decodes.
//!
//! Every planned range is split into chunks (see [`crate::source::chunk_ranges`]), so a long range never has to be
//! decoded whole. Each chunk is an [`Entry`] with its own frame-store key. The window keeps the chunks nearest to the
//! playhead, up to half of the RAM budget, and releases the rest. Source-time windows for frames outside the plan are
//! `adhoc` entries, kept within a quarter of the budget.
//!
//! A range that decoded to no frames (it lies past the frames the stream holds) is an empty range: it counts as decoded,
//! and a frame inside it shows the nearest frame of a neighbouring range.
//!
//! Everything here runs under the core lock and never decodes. Calls into the frame store are allowed under that lock,
//! because the store's lock is a leaf: the store never takes the core lock. Frames that leave the plan are removed in
//! one batch per change (see [`remove_sources`]).

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use loopcut_core::geometry::{NormRect, Orientation};
use loopcut_core::project::Caption;
use loopcut_core::sequencer::{Sample, SequencePlan};
use loopcut_core::{ColorInfo, DecodeChain, Rational, TimeRange};
use loopcut_media::CancelToken;
use loopcut_media::decode::DecodeRequest;
use loopcut_media::framestore::FrameStore;
use loopcut_render::grade::GradeSetup;

use crate::error::PreviewError;
use crate::geometry::{decode_limit, decode_need, decode_size, fit_within};
use crate::source::{TIME_EPS, WINDOW_SECONDS, contains, frame_index, raw_index, source_key};
use crate::types::{DecodeFallback, PreviewQuality};

/// Most source-time windows kept at once, whatever their size.
pub(crate) const ADHOC_MAX: usize = 8;

/// The planner's identity of a source: the file, its stream, and what the caller asked of the decoder.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SourceId {
    pub path: PathBuf,
    pub stream: u32,
    pub requested: DecodeChain,
}

/// What the compositor needs besides the frames: display geometry and the render sizes for this project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Snapshot {
    pub color: ColorInfo,
    pub orientation: Orientation,
    pub crop: NormRect,
    pub background: [u8; 4],
    /// Full display size of the source after orientation (uncropped).
    pub display: (u32, u32),
    pub render_full: (u32, u32),
    pub render_reduced: (u32, u32),
}

impl Snapshot {
    /// Size of an output frame at `quality`.
    pub fn render(&self, quality: PreviewQuality) -> (u32, u32) {
        match quality {
            PreviewQuality::Full => self.render_full,
            PreviewQuality::Reduced => self.render_reduced,
        }
    }
}

/// Decode state of one entry. `Decoding` entries have a running claim; `Done` entries have frames in the store.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EntryState {
    Pending,
    /// Being decoded: frames `0..decoded` are in the store under `key`.
    Decoding {
        key: u64,
        decoded: u64,
    },
    /// Decoded: `frames` frames are in the store under `key`. Zero frames is an empty range, not a failure.
    Done {
        key: u64,
        frames: u64,
    },
    /// The decode failed. It stays failed until the source or the decode inputs change.
    Failed(String),
}

/// The claim that is decoding an entry right now, and the token that cancels it.
#[derive(Debug)]
pub(crate) struct Running {
    pub serial: u64,
    pub cancel: CancelToken,
}

/// One source range the plan may need: a chunk of a planned range, or a source-time window outside the plan.
#[derive(Debug)]
pub(crate) struct Entry {
    pub id: u64,
    /// Decoded range.
    pub range: TimeRange,
    /// Index of the planned range this chunk belongs to. Adhoc windows use `usize::MAX`.
    pub group: usize,
    /// Decode size override. `None` decodes at the plan's size.
    pub size: Option<(u32, u32)>,
    /// Bytes the entry is expected to hold, for budgeting.
    pub bytes: u64,
    /// Distance of the entry from the playhead, in output frames. Infinite when the plan does not use it, or when it
    /// lies beyond the reach of the window.
    pub dist: f64,
    /// True while the window keeps the entry.
    pub in_window: bool,
    /// Recency of an adhoc window, for eviction.
    pub last_used: u64,
    pub state: EntryState,
    pub running: Option<Running>,
}

impl Entry {
    fn new(id: u64, range: TimeRange, group: usize, size: Option<(u32, u32)>, bytes: u64) -> Self {
        Self {
            id,
            range,
            group,
            size,
            bytes,
            dist: f64::INFINITY,
            in_window: false,
            last_used: 0,
            state: EntryState::Pending,
            running: None,
        }
    }

    /// Frame-store key of the frames this entry holds, when it has any.
    pub fn key(&self) -> Option<u64> {
        match self.state {
            EntryState::Decoding { key, .. } | EntryState::Done { key, .. } => Some(key),
            _ => None,
        }
    }

    /// True for a range that decoded to no frames: it is done, and it has nothing to show.
    pub fn is_empty_range(&self) -> bool {
        matches!(self.state, EntryState::Done { frames: 0, .. })
    }
}

/// One decode the caller must run, already marked `Decoding` in the core.
#[derive(Debug)]
pub(crate) struct Claim {
    pub generation: u64,
    pub serial: u64,
    pub entry: u64,
    pub key: u64,
    pub range: TimeRange,
    pub request: DecodeRequest,
    pub cancel: CancelToken,
}

/// Where a source frame lives in the store.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameRef {
    pub entry: u64,
    pub key: u64,
    pub index: u64,
    /// Source time the frame was requested for, for error messages.
    pub time: f64,
}

/// What a plan frame needs on top of its source frames: its look (grade and captions) and the time its captions are
/// drawn at. Source-mode frames have none.
pub(crate) struct Look {
    pub time: f64,
    pub speed: f64,
    pub captions: Arc<Vec<Caption>>,
    pub grade: Option<Arc<GradeSetup>>,
}

/// Everything the compositor needs for one frame, read under one lock acquisition so that the sample, the frames and
/// the snapshot belong to the same plan generation.
pub(crate) struct Resolved {
    pub snapshot: Snapshot,
    pub refs: Vec<FrameRef>,
    /// Crossfade weight of frame B, when the sample is a blend.
    pub weight: Option<f32>,
    /// Output size of the frame.
    pub size: (u32, u32),
    /// Crop applied to the stored frame (the project's crop for plan frames, the full frame for source frames).
    pub crop: NormRect,
    pub look: Option<Look>,
}

/// What a caller wants resolved.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Want {
    /// One output frame of the plan.
    Plan { index: u32, quality: PreviewQuality },
    /// The uncropped source at `time`, fitted into `max` (display pixels).
    Source { time: f64, max: (u32, u32) },
}

/// The outcome of looking at the plan for a `Want`.
pub(crate) enum Next {
    Ready(Resolved),
    /// The caller must decode this claim.
    Run(Claim),
    /// Another thread is decoding `entry`, which has `decoded` frames so far.
    Wait {
        entry: u64,
        decoded: u64,
        time: f64,
    },
}

/// The plan and decode state for the current project.
pub(crate) struct Active {
    pub generation: u64,
    pub source: SourceId,
    /// The request every range starts from. Its `chain` may have moved to the CPU after a fallback.
    pub template: DecodeRequest,
    pub decode_fps: Rational,
    pub decode_size: (u32, u32),
    /// Stored size of the source, and the widest decode the effects renderer accepts.
    pub coded: (u32, u32),
    pub cap: u32,
    pub snapshot: Snapshot,
    /// Source duration, used to keep source-time windows inside the source.
    pub duration: f64,
    pub plan: Arc<SequencePlan>,
    pub is_gif: bool,
    pub speed: f64,
    pub captions: Arc<Vec<Caption>>,
    /// The project's grade, prepared once per project change.
    pub grade: Option<Arc<GradeSetup>>,
    /// Chunks of the planned ranges, sorted by start and disjoint.
    pub plan_entries: Vec<Entry>,
    pub adhoc: Vec<Entry>,
    pub playhead: u32,
    /// Budget for the plan's window: half the RAM budget.
    pub window_budget: u64,
    /// Budget for source-time windows outside the plan: a quarter of the RAM budget.
    pub adhoc_budget: u64,
    pub tick: u64,
}

/// Everything the planner and the decoders share, behind the engine's core mutex.
#[derive(Default)]
pub(crate) struct Core {
    pub active: Option<Active>,
    /// Last generation handed out. A new generation starts whenever the decode inputs change.
    pub generation: u64,
    /// Bumped by every update that changes the plan or the decode. A request that waits on the plan ends when a later
    /// update lands under a different revision. Grade, caption and crop edits leave it alone.
    pub project_revision: u64,
    /// Next entry id to hand out.
    pub next_id: u64,
    /// Next claim serial to hand out.
    pub next_serial: u64,
    pub shutdown: bool,
    /// Source of the last `set_project`, used to reset the CPU fallback when the source changes.
    pub last_source: Option<SourceId>,
    /// Set once a hardware chain failed to start for the current source; decodes then stay on the CPU.
    pub cpu_fallback: bool,
    pub fallbacks: Vec<DecodeFallback>,
    pub last_error: Option<String>,
}

/// How a `Want` reaches a frame: ready, held by another thread, or to be decoded by the caller.
enum Action {
    Ready(FrameRef),
    Wait {
        entry: u64,
        decoded: u64,
    },
    Pending,
    /// The range decoded to no frames.
    Empty,
    Fail(String),
}

/// What a single entry lookup produced: the same outcomes as [`Action`], with a claim for `Pending`.
enum Outcome {
    Ready(FrameRef),
    Wait { entry: u64, decoded: u64 },
    Run(Claim),
}

/// Reads an entry's state for source time `t`.
fn action(entry: &Entry, t: f64, fps: Rational, playback: bool) -> Action {
    match &entry.state {
        EntryState::Done { key, frames } => {
            if *frames == 0 {
                return Action::Empty;
            }
            Action::Ready(FrameRef {
                entry: entry.id,
                key: *key,
                index: frame_index(entry.range.start, fps, t, *frames),
                time: t,
            })
        }
        EntryState::Decoding { key, decoded } => {
            let raw = raw_index(entry.range.start, fps, t);
            if raw < *decoded {
                Action::Ready(FrameRef {
                    entry: entry.id,
                    key: *key,
                    index: raw,
                    time: t,
                })
            } else {
                Action::Wait {
                    entry: entry.id,
                    decoded: *decoded,
                }
            }
        }
        // A playback waits for the worker when the window holds the entry, so that it never decodes on its own thread
        // while the worker can deliver the frame.
        EntryState::Pending if playback && entry.in_window => Action::Wait {
            entry: entry.id,
            decoded: 0,
        },
        EntryState::Pending => Action::Pending,
        EntryState::Failed(message) => Action::Fail(message.clone()),
    }
}

/// Index of the planned entry that holds source time `t`. Entries are sorted by start and disjoint, so this is the
/// last entry starting at or before `t`. Times in a gap map to the entry before the gap, which clamps to its last
/// frame (float error at a range end lands here).
pub(crate) fn locate(entries: &[Entry], t: f64) -> Option<usize> {
    entries
        .partition_point(|entry| entry.range.start <= t + TIME_EPS)
        .checked_sub(1)
}

/// Cancels an entry's claim and resets its state. Returns the frame-store key whose frames must go; the caller removes
/// the keys of a change together, with [`remove_sources`].
fn detach(entry: &mut Entry) -> Option<u64> {
    if let Some(running) = entry.running.take() {
        running.cancel.cancel();
    }
    let key = entry.key();
    entry.state = EntryState::Pending;
    key
}

/// Like [`detach`], but a failed entry stays failed until the decode inputs change.
pub(crate) fn release(entry: &mut Entry) -> Option<u64> {
    if matches!(entry.state, EntryState::Failed(_)) {
        None
    } else {
        detach(entry)
    }
}

/// Removes the frames of every key in `keys`, each key once. The store removes by source, so one call per distinct key
/// is all a change costs, however many entries it releases.
pub(crate) fn remove_sources(store: &FrameStore, mut keys: Vec<u64>) {
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        store.remove_source(key);
    }
}

pub(crate) fn take_id(next: &mut u64) -> u64 {
    let id = *next;
    *next += 1;
    id
}

fn take_serial(next: &mut u64) -> u64 {
    take_id(next)
}

/// The range bounds of an entry as a hashable key. Bit patterns are exact: a chunk's bounds come from the same
/// arithmetic as the entry it matches.
fn range_bits(range: TimeRange) -> (u64, u64) {
    (range.start.to_bits(), range.end.to_bits())
}

impl Core {
    /// Decides what `want` needs right now, marking a claim when the caller must decode.
    pub(crate) fn next(
        &mut self,
        store: &FrameStore,
        want: Want,
        playback: bool,
    ) -> Result<Next, PreviewError> {
        let Core {
            active,
            next_id,
            next_serial,
            ..
        } = self;
        let active = active.as_mut().ok_or(PreviewError::NoProject)?;
        match want {
            Want::Plan { index, quality } => {
                active.plan_next(index, quality, playback, next_serial)
            }
            Want::Source { time, max } => {
                active.source_next(store, time, max, next_id, next_serial)
            }
        }
    }

    /// Hands out the next claim serial.
    pub(crate) fn serial(&mut self) -> u64 {
        take_serial(&mut self.next_serial)
    }

    /// Cancels the decodes of plan entries that are farther from the playhead than the awaited entry, so that the worker
    /// takes up the awaited one next. A cancelled decode ends as pending, and its frames are removed.
    pub(crate) fn preempt_farther(&mut self, awaited: u64) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(dist) = active
            .plan_entries
            .iter()
            .find(|entry| entry.id == awaited)
            .map(|entry| entry.dist)
        else {
            return;
        };
        for entry in active.plan_entries.iter_mut() {
            if entry.id != awaited
                && entry.dist > dist
                && let Some(running) = entry.running.as_ref()
            {
                running.cancel.cancel();
            }
        }
    }

    /// Next claim the worker should run: the pending entry in the window nearest to the playhead.
    pub(crate) fn next_claim(&mut self) -> Option<Claim> {
        let Core {
            active,
            next_serial,
            ..
        } = self;
        let active = active.as_mut()?;
        let index = active
            .plan_entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.in_window && matches!(entry.state, EntryState::Pending))
            .min_by(|(_, a), (_, b)| a.dist.total_cmp(&b.dist))
            .map(|(index, _)| index)?;
        Some(active.claim(false, index, next_serial))
    }
}

impl Active {
    /// Clamps a source time for a window request into the source.
    fn clamp_time(&self, t: f64) -> f64 {
        if self.duration > 0.0 {
            t.clamp(0.0, self.duration)
        } else {
            t.max(0.0)
        }
    }

    /// The entry lookup shared by plan and source requests.
    fn outcome(
        &mut self,
        adhoc: bool,
        index: usize,
        t: f64,
        playback: bool,
        serials: &mut u64,
    ) -> Result<Outcome, PreviewError> {
        let entries = if adhoc {
            &self.adhoc
        } else {
            &self.plan_entries
        };
        match action(&entries[index], t, self.decode_fps, playback) {
            Action::Ready(frame) => Ok(Outcome::Ready(frame)),
            Action::Wait { entry, decoded } => Ok(Outcome::Wait { entry, decoded }),
            Action::Pending => Ok(Outcome::Run(self.claim(adhoc, index, serials))),
            Action::Empty => Err(PreviewError::FrameUnavailable { time: t }),
            Action::Fail(message) => Err(PreviewError::Decode(message)),
        }
    }

    /// The range a time inside the empty range `index` takes its frame from: the nearest range before or after it that
    /// holds frames or may still produce them. Failed and empty ranges are skipped. Returns the range and the source
    /// time to read it at: the end of a range before, the start of one after. `None` when there is no such range.
    fn neighbour(&self, index: usize, t: f64) -> Option<(usize, f64)> {
        let usable = |entry: &Entry| {
            !entry.is_empty_range() && !matches!(entry.state, EntryState::Failed(_))
        };
        // Each candidate carries its time gap from `t`, so the nearer of the two wins.
        let before = (0..index)
            .rev()
            .find(|&j| usable(&self.plan_entries[j]))
            .map(|j| {
                let end = self.plan_entries[j].range.end;
                (j, end, (t - end).max(0.0))
            });
        let after = (index + 1..self.plan_entries.len())
            .find(|&j| usable(&self.plan_entries[j]))
            .map(|j| {
                let start = self.plan_entries[j].range.start;
                (j, start, (start - t).max(0.0))
            });
        match (before, after) {
            (Some(b), Some(a)) => Some(if a.2 < b.2 { (a.0, a.1) } else { (b.0, b.1) }),
            (Some(b), None) => Some((b.0, b.1)),
            (None, Some(a)) => Some((a.0, a.1)),
            (None, None) => None,
        }
    }

    /// Resolves the frames of output frame `index`, reading the sample under the same lock as the frames.
    fn plan_next(
        &mut self,
        index: u32,
        quality: PreviewQuality,
        playback: bool,
        serials: &mut u64,
    ) -> Result<Next, PreviewError> {
        let frames = u32::try_from(self.plan.frames.len()).unwrap_or(u32::MAX);
        let planned =
            self.plan
                .frames
                .get(index as usize)
                .copied()
                .ok_or(PreviewError::FrameOutOfRange {
                    frame: index,
                    frames,
                })?;
        let (first, second, weight) = match planned.sample {
            Sample::At { t } => (t, None, None),
            Sample::Blend { a, b, weight_b } => (a, Some(b), Some(weight_b)),
        };
        let mut refs = Vec::with_capacity(2);
        for t in std::iter::once(first).chain(second) {
            let Some(i) = locate(&self.plan_entries, t) else {
                return Err(PreviewError::Decode(format!(
                    "no planned range covers {t:.3} s"
                )));
            };
            // A time inside a range that decoded to nothing shows the nearest frame of a neighbouring range. Only when no
            // neighbour can supply one is the frame unavailable.
            let (j, at) = if self.plan_entries[i].is_empty_range() {
                self.neighbour(i, t)
                    .ok_or(PreviewError::FrameUnavailable { time: t })?
            } else {
                (i, t)
            };
            match self.outcome(false, j, at, playback, serials)? {
                Outcome::Ready(frame) => refs.push(frame),
                Outcome::Wait { entry, decoded } => {
                    return Ok(Next::Wait {
                        entry,
                        decoded,
                        time: at,
                    });
                }
                Outcome::Run(claim) => return Ok(Next::Run(claim)),
            }
        }
        Ok(Next::Ready(Resolved {
            snapshot: self.snapshot,
            refs,
            weight,
            size: self.snapshot.render(quality),
            crop: self.snapshot.crop,
            look: Some(Look {
                time: first,
                speed: self.speed,
                captions: Arc::clone(&self.captions),
                grade: self.grade.clone(),
            }),
        }))
    }

    /// Resolves the uncropped source at `time`, fitted into `max`. The plan's decode serves it when the decode is at
    /// least as wide as the box needs; otherwise a source-time window is decoded at the box's size.
    fn source_next(
        &mut self,
        store: &FrameStore,
        time: f64,
        max: (u32, u32),
        next_id: &mut u64,
        serials: &mut u64,
    ) -> Result<Next, PreviewError> {
        let t = self.clamp_time(time);
        let display = self.snapshot.display;
        let target = fit_within(display.0, display.1, max.0, max.1);
        let need = decode_need(target, NormRect::FULL).ceil();
        let limit = decode_limit(self.coded.0, self.cap);
        let plan_covers = f64::from(self.decode_size.0) >= need.min(f64::from(limit));
        let plan_index = if plan_covers {
            locate(&self.plan_entries, t).filter(|&i| contains(self.plan_entries[i].range, t))
        } else {
            None
        };
        let (adhoc, index) = match plan_index {
            Some(i) => (false, i),
            None => {
                let size = decode_size(self.coded, NormRect::FULL, target, self.cap, None);
                let index = match self
                    .adhoc
                    .iter()
                    .position(|entry| entry.size == Some(size) && contains(entry.range, t))
                {
                    Some(index) => index,
                    None => self.push_adhoc(store, t, size, next_id),
                };
                self.tick += 1;
                self.adhoc[index].last_used = self.tick;
                (true, index)
            }
        };
        match self.outcome(adhoc, index, t, false, serials)? {
            Outcome::Ready(frame) => Ok(Next::Ready(Resolved {
                snapshot: self.snapshot,
                refs: vec![frame],
                weight: None,
                size: target,
                crop: NormRect::FULL,
                look: None,
            })),
            Outcome::Wait { entry, decoded } => Ok(Next::Wait {
                entry,
                decoded,
                time: t,
            }),
            Outcome::Run(claim) => Ok(Next::Run(claim)),
        }
    }

    /// Adds a source-time window around `t` at `size`, evicting the least recently used windows first when the window
    /// count or the adhoc budget would be exceeded. Windows are aligned so that repeated requests in the same cell
    /// share a key. Returns the index of the new entry.
    fn push_adhoc(
        &mut self,
        store: &FrameStore,
        t: f64,
        size: (u32, u32),
        next_id: &mut u64,
    ) -> usize {
        let frame_bytes = self.template.layout.frame_bytes(size.0, size.1).max(1) as u64;
        let fps = self.decode_fps;
        let budget_frames = (self.adhoc_budget / frame_bytes).max(2);
        let len = (budget_frames as f64 / fps.as_f64())
            .min(WINDOW_SECONDS)
            .max(2.0 * fps.frame_duration());
        let mut start = (t / len).floor() * len;
        if self.duration > 0.0 {
            start = start.min((self.duration - len).max(0.0));
        }
        let start = start.max(0.0);
        let frames = ((len * fps.as_f64()).ceil() as u64).max(2);
        let bytes = frames * frame_bytes;
        let mut stale = Vec::new();
        while !self.adhoc.is_empty()
            && (self.adhoc.len() >= ADHOC_MAX || self.adhoc_bytes() + bytes > self.adhoc_budget)
        {
            let Some(lru) = self
                .adhoc
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
            else {
                break;
            };
            let mut victim = self.adhoc.swap_remove(lru);
            stale.extend(detach(&mut victim));
        }
        remove_sources(store, stale);
        let id = take_id(next_id);
        self.adhoc.push(Entry::new(
            id,
            TimeRange::new(start, start + len),
            usize::MAX,
            Some(size),
            bytes,
        ));
        self.adhoc.len() - 1
    }

    /// Bytes held by the adhoc windows.
    fn adhoc_bytes(&self) -> u64 {
        self.adhoc.iter().map(|entry| entry.bytes).sum()
    }

    /// Marks an entry as decoding and returns the claim that decodes it, with its own cancel token.
    pub(crate) fn claim(&mut self, adhoc: bool, index: usize, serials: &mut u64) -> Claim {
        let template = &self.template;
        let entries = if adhoc {
            &mut self.adhoc
        } else {
            &mut self.plan_entries
        };
        let entry = &mut entries[index];
        let mut request = template.clone();
        request.range = Some(entry.range);
        request.size = entry.size.or(template.size);
        let key = source_key(&request);
        let serial = take_serial(serials);
        let cancel = CancelToken::new();
        entry.state = EntryState::Decoding { key, decoded: 0 };
        entry.running = Some(Running {
            serial,
            cancel: cancel.clone(),
        });
        Claim {
            generation: self.generation,
            serial,
            entry: entry.id,
            key,
            range: entry.range,
            request,
            cancel,
        }
    }

    /// The entry with `id`, plan or adhoc.
    pub(crate) fn entry_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.plan_entries
            .iter_mut()
            .chain(self.adhoc.iter_mut())
            .find(|entry| entry.id == id)
    }

    /// The entry with `id`, when the claim `serial` is still the one decoding it.
    pub(crate) fn claimed_entry_mut(&mut self, id: u64, serial: u64) -> Option<&mut Entry> {
        self.entry_mut(id).filter(|entry| {
            entry
                .running
                .as_ref()
                .is_some_and(|running| running.serial == serial)
        })
    }

    /// True when some entry holds or is decoding frames under `key`.
    pub(crate) fn uses_key(&self, key: u64) -> bool {
        self.plan_entries
            .iter()
            .chain(self.adhoc.iter())
            .any(|entry| entry.key() == Some(key))
    }

    /// Moves the decode window to `playhead`. The window takes the plan's chunks nearest to the playhead (distance in
    /// output frames, going round a loop) while they fit in the window budget, and always takes the nearest one. Chunks
    /// that leave the window are released: their claims are cancelled and their frames removed.
    ///
    /// Only the frames within the window's reach are looked at. The scan walks outward from the playhead one distance at
    /// a time, and stops as soon as the window is full, so a playhead move costs the frames near it, not the whole plan.
    pub(crate) fn retarget(&mut self, store: &FrameStore, playhead: u32) {
        let len = self.plan.frames.len();
        if len == 0 {
            self.playhead = 0;
            return;
        }
        let playhead = (playhead as usize).min(len - 1);
        self.playhead = playhead as u32;
        let count = self.plan_entries.len();
        let min_bytes = self
            .plan_entries
            .iter()
            .map(|entry| entry.bytes)
            .min()
            .unwrap_or(0);
        let mut dist = vec![f64::INFINITY; count];
        let mut keep = vec![false; count];
        let mut unreached = count;
        let mut used = 0u64;
        for radius in 0..=len / 2 {
            if unreached == 0 {
                break;
            }
            let forward = (playhead + radius) % len;
            let backward = (playhead + len - radius) % len;
            let other = (backward != forward).then_some(backward);
            // Entries first reached at this distance, in index order: the same order a sort by (distance, index) gives.
            let mut touched = Vec::new();
            for frame in std::iter::once(forward).chain(other) {
                for t in self.plan.frames[frame].sample.source_times() {
                    if let Some(e) = locate(&self.plan_entries, t)
                        && dist[e].is_infinite()
                    {
                        dist[e] = radius as f64;
                        unreached -= 1;
                        touched.push(e);
                    }
                }
            }
            touched.sort_unstable();
            for e in touched {
                let bytes = self.plan_entries[e].bytes;
                if used == 0 || used.saturating_add(bytes) <= self.window_budget {
                    keep[e] = true;
                    used = used.saturating_add(bytes);
                }
            }
            // Once even the smallest entry cannot fit, no farther entry can enter the window.
            if used > 0 && used.saturating_add(min_bytes) > self.window_budget {
                break;
            }
        }
        let mut stale = Vec::new();
        for (e, entry) in self.plan_entries.iter_mut().enumerate() {
            entry.dist = dist[e];
            entry.in_window = keep[e];
            if !keep[e] {
                stale.extend(release(entry));
            }
        }
        remove_sources(store, stale);
    }

    /// Decoded fraction of the planned source time, leaving out ranges that failed, and the number of ranges that
    /// failed. Empty ranges count as decoded.
    pub(crate) fn progress(&self) -> (f32, u32) {
        let failed: BTreeSet<usize> = self
            .plan_entries
            .iter()
            .filter(|entry| matches!(entry.state, EntryState::Failed(_)))
            .map(|entry| entry.group)
            .collect();
        let fps = self.decode_fps.as_f64();
        let (mut total, mut done) = (0.0_f64, 0.0_f64);
        for entry in self
            .plan_entries
            .iter()
            .filter(|entry| !failed.contains(&entry.group))
        {
            let duration = entry.range.duration();
            total += duration;
            done += match entry.state {
                EntryState::Done { .. } => duration,
                EntryState::Decoding { decoded, .. } => (decoded as f64 / fps).min(duration),
                _ => 0.0,
            };
        }
        let fraction = if total <= 0.0 {
            1.0
        } else {
            (done / total).clamp(0.0, 1.0)
        };
        (
            fraction as f32,
            u32::try_from(failed.len()).unwrap_or(u32::MAX),
        )
    }

    /// Cancels every claim and removes every frame of this plan. Used when the engine shuts down or the plan is
    /// replaced.
    pub(crate) fn drop_all(&mut self, store: &FrameStore) {
        let stale: Vec<u64> = self
            .plan_entries
            .iter_mut()
            .chain(self.adhoc.iter_mut())
            .filter_map(detach)
            .collect();
        remove_sources(store, stale);
    }
}

/// Builds the chunk entries of a plan. Entries of `old` whose range is unchanged keep their state and their claim; the
/// rest of `old` is detached and its frames removed. `chunks` pairs each chunk with its planned range's index.
///
/// Old entries are indexed by their range, so each chunk finds its entry in one lookup.
pub(crate) fn carry_entries(
    old: Vec<Entry>,
    chunks: &[(usize, TimeRange)],
    fps: Rational,
    frame_bytes: u64,
    next_id: &mut u64,
    store: &FrameStore,
) -> Vec<Entry> {
    let mut by_range: HashMap<(u64, u64), Entry> = HashMap::with_capacity(old.len());
    let mut left_over = Vec::new();
    for entry in old {
        if let Some(previous) = by_range.insert(range_bits(entry.range), entry) {
            left_over.push(previous);
        }
    }
    let mut entries = Vec::with_capacity(chunks.len());
    for &(group, range) in chunks {
        match by_range.remove(&range_bits(range)) {
            Some(mut entry) => {
                entry.group = group;
                entries.push(entry);
            }
            None => {
                let frames = ((range.duration() * fps.as_f64()).ceil() as u64).max(2);
                entries.push(Entry::new(
                    take_id(next_id),
                    range,
                    group,
                    None,
                    frames * frame_bytes,
                ));
            }
        }
    }
    left_over.extend(by_range.into_values());
    let stale: Vec<u64> = left_over.iter_mut().filter_map(detach).collect();
    remove_sources(store, stale);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use loopcut_core::PixelLayout;
    use loopcut_media::framestore::{FrameKey, FrameStoreConfig, StoredFrame};

    fn store() -> (FrameStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FrameStore::new(FrameStoreConfig {
            ram_budget_bytes: 1 << 20,
            disk_budget_bytes: 1 << 20,
            spill_dir: dir.path().join("spill"),
        })
        .expect("frame store");
        (store, dir)
    }

    fn one_frame() -> StoredFrame {
        StoredFrame {
            width: 2,
            height: 2,
            layout: PixelLayout::Nv12,
            time: 0.0,
            data: Arc::from(vec![0u8; 6]),
        }
    }

    #[test]
    fn take_id_counts_up() {
        let mut next = 0;
        assert_eq!(take_id(&mut next), 0);
        assert_eq!(take_id(&mut next), 1);
        assert_eq!(next, 2);
    }

    #[test]
    fn carried_ranges_keep_their_frames_and_the_rest_leave_the_store() {
        let (store, _dir) = store();
        let fps = Rational::integer(10);
        // Three decoded ranges of one second each. The plan keeps the last two and adds a fourth.
        let mut old = Vec::new();
        for i in 0..3u64 {
            let start = i as f64;
            let key = 10 + i;
            store.insert(
                FrameKey {
                    source: key,
                    index: 0,
                },
                one_frame(),
            );
            let mut entry = Entry::new(i, TimeRange::new(start, start + 1.0), 0, None, 1);
            entry.state = EntryState::Done { key, frames: 10 };
            old.push(entry);
        }
        let chunks: Vec<(usize, TimeRange)> = [1.0, 2.0, 3.0]
            .iter()
            .enumerate()
            .map(|(group, &start)| (group, TimeRange::new(start, start + 1.0)))
            .collect();
        let mut next_id = 100;
        let entries = carry_entries(old, &chunks, fps, 1, &mut next_id, &store);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, 1, "the kept range keeps its entry");
        assert!(matches!(entries[0].state, EntryState::Done { key: 11, .. }));
        assert_eq!(entries[1].id, 2);
        assert_eq!(entries[2].id, 100, "the new range gets a fresh id");
        assert!(matches!(entries[2].state, EntryState::Pending));
        assert_eq!(next_id, 101);
        assert!(store.contains(&FrameKey {
            source: 11,
            index: 0
        }));
        assert!(store.contains(&FrameKey {
            source: 12,
            index: 0
        }));
        assert!(
            !store.contains(&FrameKey {
                source: 10,
                index: 0
            }),
            "the range that left the plan releases its frames"
        );
    }
}

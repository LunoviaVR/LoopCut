//! Filmstrip thumbnails held in RAM: one set per height per media file, under one global byte budget.
//!
//! A set is RGBA8 pixels for `count` frames at one display height. Each media file keeps only the newest set for each
//! height. [`ThumbBudget`] tracks every set across all files in least-recently-used order and reports which sets must
//! go when the total exceeds the budget. Eviction is whole-set: a partly evicted filmstrip would be useless.

use std::collections::HashMap;

use parking_lot::Mutex;

/// Default RAM budget for all filmstrip thumbnails together.
pub const DEFAULT_THUMB_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// The height the thumbnail generator actually produces for a requested height: rounded down to an even number and
/// never below 2. Requests and protocol URLs are normalized the same way, so an odd request still finds its set.
pub fn normalized_height(requested: u32) -> u32 {
    (requested & !1).max(2)
}

/// Thumbnails generated for one `(count, height)` request. Pixels are RGBA8, upright, `width * height * 4` bytes each.
#[derive(Debug)]
pub struct ThumbSet {
    /// Number of samples requested.
    pub count: u32,
    /// The real output height in pixels (`thumbs[0].height`), always even.
    pub height: u32,
    /// The real output width in pixels, always even.
    pub width: u32,
    /// Source time of each sample in seconds, in index order.
    pub times: Vec<f64>,
    /// One RGBA8 frame per sample.
    pub frames: Vec<Vec<u8>>,
}

impl ThumbSet {
    /// RAM held by the pixels of this set.
    pub fn bytes(&self) -> u64 {
        self.frames.iter().map(|frame| frame.len() as u64).sum()
    }
}

/// Identity of one cached set: the media token and the real height.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub token: String,
    pub height: u32,
}

#[derive(Debug, Default)]
struct Lru {
    clock: u64,
    bytes: u64,
    sets: HashMap<ThumbKey, Slot>,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    bytes: u64,
    tick: u64,
}

/// Global LRU bookkeeping for thumbnail sets. Holds no pixels itself; the media entries own the sets and act on the
/// keys returned by [`Self::insert`].
#[derive(Debug)]
pub struct ThumbBudget {
    limit: u64,
    inner: Mutex<Lru>,
}

impl ThumbBudget {
    /// A budget of `limit` bytes for all sets together.
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            inner: Mutex::new(Lru::default()),
        }
    }

    /// The configured limit in bytes.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Bytes currently accounted for.
    pub fn total_bytes(&self) -> u64 {
        self.inner.lock().bytes
    }

    /// Number of sets currently accounted for.
    pub fn set_count(&self) -> usize {
        self.inner.lock().sets.len()
    }

    /// Marks a set as used now (a cache hit or a protocol read).
    pub fn touch(&self, key: &ThumbKey) {
        let mut lru = self.inner.lock();
        lru.clock += 1;
        let tick = lru.clock;
        if let Some(slot) = lru.sets.get_mut(key) {
            slot.tick = tick;
        }
    }

    /// Records a newly stored set of `bytes` (replacing any set with the same key) and returns the least recently used
    /// keys that must be evicted to get back under the limit. The new set is never returned, so one set larger than
    /// the whole budget is still kept until another set replaces it.
    pub fn insert(&self, key: ThumbKey, bytes: u64) -> Vec<ThumbKey> {
        let mut lru = self.inner.lock();
        lru.clock += 1;
        let tick = lru.clock;
        if let Some(old) = lru.sets.insert(key.clone(), Slot { bytes, tick }) {
            lru.bytes -= old.bytes;
        }
        lru.bytes += bytes;

        let mut victims = Vec::new();
        while lru.bytes > self.limit {
            let oldest = lru
                .sets
                .iter()
                .filter(|(candidate, _)| **candidate != key)
                .min_by_key(|(_, slot)| slot.tick)
                .map(|(candidate, slot)| (candidate.clone(), slot.bytes));
            let Some((victim, victim_bytes)) = oldest else {
                break;
            };
            lru.sets.remove(&victim);
            lru.bytes -= victim_bytes;
            victims.push(victim);
        }
        victims
    }

    /// Forgets every set of `token`, after the media is released or probed again.
    pub fn forget_token(&self, token: &str) {
        let mut lru = self.inner.lock();
        let mut freed = 0u64;
        lru.sets.retain(|key, slot| {
            if key.token == token {
                freed += slot.bytes;
                false
            } else {
                true
            }
        });
        lru.bytes -= freed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(token: &str, height: u32) -> ThumbKey {
        ThumbKey {
            token: token.to_owned(),
            height,
        }
    }

    #[test]
    fn requested_heights_round_down_to_the_generator_height() {
        assert_eq!(normalized_height(91), 90);
        assert_eq!(normalized_height(90), 90);
        assert_eq!(normalized_height(1), 2);
        assert_eq!(normalized_height(0), 2);
    }

    #[test]
    fn sets_are_evicted_least_recently_used_first_and_whole() {
        let budget = ThumbBudget::new(100);
        assert!(budget.insert(key("a", 90), 40).is_empty());
        assert!(budget.insert(key("b", 90), 40).is_empty());
        // Using "a" makes "b" the oldest.
        budget.touch(&key("a", 90));
        let victims = budget.insert(key("c", 90), 40);
        assert_eq!(victims, vec![key("b", 90)]);
        assert_eq!(budget.total_bytes(), 80);
        assert_eq!(budget.set_count(), 2);
    }

    #[test]
    fn the_new_set_is_kept_even_when_it_alone_exceeds_the_budget() {
        let budget = ThumbBudget::new(10);
        assert!(budget.insert(key("a", 90), 4).is_empty());
        let victims = budget.insert(key("big", 90), 50);
        assert_eq!(victims, vec![key("a", 90)]);
        assert_eq!(budget.total_bytes(), 50);
        assert_eq!(budget.set_count(), 1);
    }

    #[test]
    fn replacing_a_key_updates_the_bytes_instead_of_adding() {
        let budget = ThumbBudget::new(100);
        budget.insert(key("a", 90), 60);
        assert!(budget.insert(key("a", 90), 30).is_empty());
        assert_eq!(budget.total_bytes(), 30);
        assert_eq!(budget.set_count(), 1);
    }

    #[test]
    fn forgetting_a_token_frees_all_of_its_sets() {
        let budget = ThumbBudget::new(1000);
        budget.insert(key("a", 90), 10);
        budget.insert(key("a", 180), 20);
        budget.insert(key("b", 90), 30);
        budget.forget_token("a");
        assert_eq!(budget.total_bytes(), 30);
        assert_eq!(budget.set_count(), 1);
    }

    #[test]
    fn set_bytes_count_every_frame() {
        let set = ThumbSet {
            count: 2,
            height: 2,
            width: 2,
            times: vec![0.0, 1.0],
            frames: vec![vec![0; 16], vec![0; 16]],
        };
        assert_eq!(set.bytes(), 32);
    }
}

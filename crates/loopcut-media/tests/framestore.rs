//! FrameStore behaviour against the real filesystem: RAM to disk spill, budgets, source removal, concurrency, failures
//! and the cleanup rules for spill folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use loopcut_core::PixelLayout;
use loopcut_media::framestore::{
    FrameKey, FrameStore, FrameStoreConfig, StoredFrame, default_ram_budget,
};

/// A frame whose payload is `len` bytes of `fill`, so every byte can be checked after a round trip.
fn frame(len: usize, fill: u8) -> StoredFrame {
    StoredFrame {
        width: 4,
        height: 4,
        layout: PixelLayout::Rgba8,
        time: f64::from(fill),
        data: Arc::from(vec![fill; len]),
    }
}

fn key(source: u64, index: u64) -> FrameKey {
    FrameKey { source, index }
}

/// A store whose spill root is `dir/spill`.
fn store(dir: &Path, ram: u64, disk: u64) -> FrameStore {
    FrameStore::new(FrameStoreConfig {
        ram_budget_bytes: ram,
        disk_budget_bytes: disk,
        spill_dir: dir.join("spill"),
    })
    .expect("store")
}

/// Names of the spill files in the store's own folder, sorted.
fn spill_files(store: &FrameStore) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(store.spill_folder())
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".lcframe"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn frames_beyond_the_ram_budget_spill_and_come_back_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Room for two 1 KiB frames in RAM; the third insert pushes the oldest one to disk.
    let store = store(dir.path(), 2048, 1 << 20);
    for index in 0..3u64 {
        store.insert(key(1, index), frame(1024, index as u8 + 1));
    }

    let stats = store.stats();
    assert_eq!(stats.ram_frames, 2);
    assert_eq!(stats.disk_frames, 1);
    assert_eq!(stats.disk_bytes, 1024);
    assert_eq!(spill_files(&store).len(), 1);

    // Frame 0 was the least recently used, so it is the spilled one.
    let spilled = store
        .get(&key(1, 0))
        .expect("spilled frame is still served");
    assert_eq!(spilled.data.len(), 1024);
    assert!(spilled.data.iter().all(|byte| *byte == 1));
    assert_eq!(spilled.time, 1.0);
    assert_eq!((spilled.width, spilled.height), (4, 4));
    assert_eq!(spilled.layout, PixelLayout::Rgba8);

    let back_in_ram = store.get(&key(1, 2)).expect("RAM frame");
    assert!(back_in_ram.data.iter().all(|byte| *byte == 3));
}

#[test]
fn spilled_frame_is_promoted_back_to_ram_when_it_fits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path(), 1024, 1 << 20);
    store.insert(key(7, 0), frame(1024, 9));
    store.insert(key(7, 1), frame(1024, 8)); // evicts key(7, 0) to disk
    assert_eq!(store.stats().disk_frames, 1);

    store.remove_source(7);
    store.insert(key(7, 2), frame(1024, 5));
    store.insert(key(8, 0), frame(512, 4)); // evicts key(7, 2) to disk, leaving 512 bytes in RAM
    assert!(store.contains(&key(7, 2)));
    assert_eq!(store.stats().disk_frames, 1);

    // While the RAM budget is still occupied, a read is served from disk and stays there.
    assert!(store.get(&key(7, 2)).is_some());
    assert_eq!(
        store.stats().disk_frames,
        1,
        "no room in RAM, so no promotion"
    );

    // With RAM free, the same read moves the frame back into RAM and deletes its spill file.
    store.remove_source(8);
    let got = store.get(&key(7, 2)).expect("frame");
    assert!(got.data.iter().all(|byte| *byte == 5));
    let stats = store.stats();
    assert_eq!((stats.ram_frames, stats.disk_frames), (1, 0));
    assert_eq!(stats.ram_bytes, 1024);
    assert!(spill_files(&store).is_empty());
}

#[test]
fn disk_budget_evicts_the_oldest_spilled_frames() {
    let dir = tempfile::tempdir().expect("tempdir");
    // RAM holds nothing, so every frame is spilled; the disk budget holds three 1 KiB frames.
    let store = store(dir.path(), 0, 3 * 1024);
    for index in 0..5u64 {
        store.insert(key(2, index), frame(1024, index as u8));
    }

    let stats = store.stats();
    assert_eq!(stats.disk_frames, 3);
    assert_eq!(stats.disk_bytes, 3 * 1024);
    assert_eq!(stats.dropped_frames, 0, "LRU evictions are not drops");
    assert!(!store.contains(&key(2, 0)), "oldest frame was evicted");
    assert!(
        !store.contains(&key(2, 1)),
        "second-oldest frame was evicted"
    );
    assert!(store.contains(&key(2, 2)));
    assert!(store.contains(&key(2, 4)));
    assert_eq!(spill_files(&store).len(), 3);
    assert!(store.get(&key(2, 0)).is_none());
    assert!(store.get(&key(2, 4)).is_some());
}

#[test]
fn oversized_frames_are_dropped_and_counted_instead_of_thrashing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path(), 512, 512);
    store.insert(key(3, 0), frame(1024, 1));
    assert!(!store.contains(&key(3, 0)));
    let stats = store.stats();
    assert_eq!(stats.disk_bytes, 0);
    assert_eq!(stats.dropped_frames, 1);
    assert!(spill_files(&store).is_empty());
}

#[test]
fn a_failed_spill_write_is_counted_and_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path(), 1024, 1 << 20);
    store.insert(key(4, 0), frame(1024, 1));
    // Removing the folder makes the next spill write fail, as a disk error would.
    fs::remove_dir_all(store.spill_folder()).expect("remove folder");
    store.insert(key(4, 1), frame(1024, 2)); // evicts key(4, 0), whose write fails

    let stats = store.stats();
    assert_eq!(stats.dropped_frames, 1);
    assert!(!store.contains(&key(4, 0)));
    assert!(store.contains(&key(4, 1)));
    let message = store.last_error().expect("the failure is described");
    assert!(message.contains("could not spill frame"), "{message}");
}

#[test]
fn an_unreadable_spill_file_is_a_miss_and_is_forgotten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path(), 0, 1 << 20);
    store.insert(key(9, 0), frame(256, 1));
    let file = store.spill_folder().join(spill_files(&store).remove(0));
    fs::write(&file, b"corrupted").expect("corrupt the spill file");

    assert!(store.get(&key(9, 0)).is_none());
    let stats = store.stats();
    assert_eq!((stats.hits, stats.misses), (0, 1));
    assert!(!store.contains(&key(9, 0)));
    assert!(
        store
            .last_error()
            .is_some_and(|msg| msg.contains("could not read spill file"))
    );
}

#[test]
fn remove_source_clears_only_that_source_in_both_tiers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path(), 2048, 1 << 20);
    for index in 0..4u64 {
        store.insert(key(10, index), frame(1024, 1));
        store.insert(key(20, index), frame(1024, 2));
    }
    assert!(store.stats().disk_frames > 0 && store.stats().ram_frames > 0);

    store.remove_source(10);

    for index in 0..4u64 {
        assert!(!store.contains(&key(10, index)));
        assert!(store.contains(&key(20, index)));
    }
    let stats = store.stats();
    assert_eq!(stats.ram_frames + stats.disk_frames, 4);
    assert_eq!(stats.ram_bytes + stats.disk_bytes, 4 * 1024);
    assert_eq!(spill_files(&store).len(), stats.disk_frames);
}

#[test]
fn concurrent_insert_and_get_from_four_threads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(store(dir.path(), 16 * 1024, 1 << 22));
    let workers: Vec<_> = (0..4u64)
        .map(|worker| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                for index in 0..64u64 {
                    let fill = (worker * 64 + index) as u8;
                    store.insert(key(100 + worker, index), frame(1024, fill));
                    let back = store
                        .get(&key(100 + worker, index))
                        .expect("a frame just inserted by this thread is still stored");
                    assert!(back.data.iter().all(|byte| *byte == fill));
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker finished without panicking");
    }
    let stats = store.stats();
    assert_eq!(stats.hits, 4 * 64);
    assert_eq!(stats.dropped_frames, 0);
    assert!(stats.ram_bytes <= 16 * 1024);
    assert!(stats.disk_bytes <= 1 << 22);
}

/// Creates a folder that looks like one a crashed process left behind: its owner lock exists but nothing holds it.
fn abandoned_folder(root: &Path, name: &str) -> PathBuf {
    let folder = root.join(name);
    fs::create_dir_all(&folder).expect("mkdir");
    fs::write(folder.join("owner.lock"), b"").expect("owner");
    fs::write(folder.join("left-behind.lcframe"), vec![0u8; 64]).expect("stale");
    folder
}

#[test]
fn new_sweeps_only_folders_whose_owner_has_exited() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("spill");
    fs::create_dir_all(&root).expect("mkdir");
    let dead = abandoned_folder(&root, "lc-0000000000000001");
    // A folder without an owner lock is not ours to judge, so it stays.
    let unowned = root.join("lc-0000000000000002");
    fs::create_dir_all(&unowned).expect("mkdir");
    fs::write(unowned.join("maybe-live.lcframe"), b"keep").expect("file");
    fs::write(root.join("notes.txt"), b"keep").expect("other");

    let live = store(dir.path(), 1024, 1 << 20);
    let live_folder = live.spill_folder().to_path_buf();
    // A second store in the same process must not sweep the first: its lock is held.
    let _second = store(dir.path(), 1024, 1 << 20);

    assert!(!dead.exists(), "abandoned folder is removed");
    assert!(unowned.join("maybe-live.lcframe").exists());
    assert!(live_folder.is_dir(), "a live store keeps its folder");
    assert!(root.join("notes.txt").exists());
}

#[test]
fn drop_deletes_the_stores_own_spill_files_and_folder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("spill");
    let other = store(dir.path(), 0, 1 << 20);
    other.insert(key(50, 0), frame(256, 9));
    let folder = {
        let store = store(dir.path(), 0, 1 << 20);
        store.insert(key(5, 0), frame(256, 1));
        store.insert(key(5, 1), frame(256, 2));
        assert_eq!(spill_files(&store).len(), 2);
        store.spill_folder().to_path_buf()
    };
    assert!(!folder.exists(), "the dropped store's folder is removed");
    assert!(root.is_dir(), "the shared root stays");
    assert_eq!(spill_files(&other).len(), 1, "other stores are untouched");
}

#[test]
fn clear_removes_frames_but_keeps_counters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path(), 1024, 1 << 20);
    store.insert(key(6, 0), frame(1024, 1));
    store.insert(key(6, 1), frame(1024, 2));
    assert!(store.get(&key(6, 1)).is_some());
    assert!(store.get(&key(6, 9)).is_none());

    store.clear();

    let stats = store.stats();
    assert_eq!((stats.ram_frames, stats.disk_frames), (0, 0));
    assert_eq!((stats.ram_bytes, stats.disk_bytes), (0, 0));
    assert_eq!((stats.hits, stats.misses), (1, 1));
    assert!(spill_files(&store).is_empty());
}

#[test]
fn default_budget_follows_the_documented_rule() {
    assert_eq!(
        default_ram_budget(16 * 1024 * 1024 * 1024),
        2 * 1024 * 1024 * 1024
    );
    assert_eq!(
        default_ram_budget(2 * 1024 * 1024 * 1024),
        512 * 1024 * 1024
    );
}

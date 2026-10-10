//! FrameStore: decoded-frame cache with a RAM budget, LRU eviction and disk spill.
//!
//! Preview and export share one store. Frames are immutable and reference-counted, so a lookup hands out a cheap
//! clone of the pixel buffer. When RAM use exceeds the budget, the least-recently used frames move to one file each in
//! the store's spill folder. When the disk tier exceeds its budget, the least-recently used spilled files are deleted.
//!
//! # Spill folders
//!
//! Each store writes into its own folder `lc-<nonce>` under [`FrameStoreConfig::spill_dir`]. The folder holds an
//! `owner.lock` file that the store keeps exclusively locked for its whole life. [`FrameStore::new`] removes a folder
//! only when it can take that lock, which means its owner has exited, and it never touches other files or folders.
//! Dropping the store deletes its own folder.
//!
//! # Locking and failures
//!
//! The store's mutex is never held during file IO: victims are chosen under the lock, written outside it, and committed
//! under the lock again. A frame being written stays readable from memory until its file is committed. Cache failures
//! never fail a caller: a frame that cannot be kept is counted in [`FrameStoreStats::dropped_frames`] and described by
//! [`FrameStore::last_error`], and a spill file that cannot be read counts as a miss.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use loopcut_core::PixelLayout;
use parking_lot::Mutex;

use crate::error::MediaResult;

/// File extension of spilled frames. Only files with this extension are ever deleted by the store.
const SPILL_EXTENSION: &str = "lcframe";

/// Name prefix of the per-store spill folders.
const FOLDER_PREFIX: &str = "lc-";

/// Name of the lock file that proves a spill folder's owner is still running.
const OWNER_FILE: &str = "owner.lock";

/// Magic bytes at the start of every spill file.
const SPILL_MAGIC: [u8; 4] = *b"LCFR";

/// Spill file format version.
const SPILL_VERSION: u8 = 1;

/// Size of the spill file header: magic, version, layout, reserved, width, height, time, payload length.
const HEADER_LEN: usize = 32;

const GIB: u64 = 1024 * 1024 * 1024;

/// Identity of one cached frame: the decode request it came from, and its frame index within that request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameKey {
    /// Hash of the decode request identity (source file, stream, crop, scale, color path).
    pub source: u64,
    /// Frame index within the decode request.
    pub index: u64,
}

/// A decoded frame ready for the compositor.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Memory layout of `data`.
    pub layout: PixelLayout,
    /// Source time of the frame in seconds.
    pub time: f64,
    /// Packed pixel data, `layout.frame_bytes(width, height)` bytes for well-formed frames.
    pub data: Arc<[u8]>,
}

/// Budgets and location of a [`FrameStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameStoreConfig {
    /// Maximum bytes of frame data kept in RAM.
    pub ram_budget_bytes: u64,
    /// Maximum bytes of frame data kept in spill files.
    pub disk_budget_bytes: u64,
    /// Root folder for spill files. Created if missing. The store works in its own subfolder of it, so the root can be
    /// shared with other stores and with other files.
    pub spill_dir: PathBuf,
}

/// A snapshot of store usage and effectiveness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStoreStats {
    /// Frame bytes currently held in RAM.
    pub ram_bytes: u64,
    /// Frame bytes currently held in spill files.
    pub disk_bytes: u64,
    /// Number of frames held in RAM.
    pub ram_frames: usize,
    /// Number of frames held in spill files.
    pub disk_frames: usize,
    /// Lookups that found a frame (RAM, memory during a spill, or disk).
    pub hits: u64,
    /// Lookups that found nothing, or a spill file that could not be read.
    pub misses: u64,
    /// Frames that could not be kept: larger than the disk budget, or the spill write failed. Ordinary LRU evictions
    /// are not counted.
    pub dropped_frames: u64,
}

/// RAM budget for a machine with `total_ram_bytes` of memory: a quarter of RAM, capped at 2 GiB.
pub fn default_ram_budget(total_ram_bytes: u64) -> u64 {
    (total_ram_bytes / 4).min(2 * GIB)
}

/// Thread-safe frame cache. Every method takes `&self`, so one store can be shared through an `Arc`.
pub struct FrameStore {
    inner: Mutex<Inner>,
    ram_budget: u64,
    disk_budget: u64,
    /// This store's spill folder, deleted on drop.
    folder: PathBuf,
    /// Exclusive lock on `folder/owner.lock`. Released before the folder is removed.
    owner: Option<File>,
}

/// Mutable state, protected by the store's mutex.
#[derive(Default)]
struct Inner {
    ram: HashMap<FrameKey, RamEntry>,
    disk: HashMap<FrameKey, DiskEntry>,
    /// Frames whose spill write is in progress. They stay readable from memory until the file is committed.
    spilling: HashMap<FrameKey, Spilling>,
    /// Recency index for RAM entries: tick to key. The smallest tick is the least recently used.
    ram_lru: BTreeMap<u64, FrameKey>,
    /// Recency index for disk entries.
    disk_lru: BTreeMap<u64, FrameKey>,
    ram_bytes: u64,
    disk_bytes: u64,
    clock: u64,
    /// Source of unique spill file names and write tickets.
    tickets: u64,
    hits: u64,
    misses: u64,
    dropped: u64,
    last_error: Option<String>,
}

struct RamEntry {
    frame: StoredFrame,
    tick: u64,
}

struct DiskEntry {
    path: PathBuf,
    bytes: u64,
    tick: u64,
}

struct Spilling {
    frame: StoredFrame,
    ticket: u64,
}

/// A frame chosen for spilling, to be written outside the lock.
struct Staged {
    key: FrameKey,
    ticket: u64,
    frame: StoredFrame,
}

/// The outcome of writing one [`Staged`] frame.
struct Written {
    key: FrameKey,
    ticket: u64,
    path: PathBuf,
    bytes: u64,
    result: io::Result<()>,
}

impl FrameStore {
    /// Creates a store: makes the root folder, removes spill folders whose owners have exited, and creates this store's
    /// own locked folder.
    ///
    /// # Errors
    ///
    /// [`crate::MediaError::Io`] if the root folder cannot be created or this store's folder cannot be created or locked.
    pub fn new(config: FrameStoreConfig) -> MediaResult<Self> {
        fs::create_dir_all(&config.spill_dir)?;
        sweep_dead_folders(&config.spill_dir);
        let (folder, owner) = create_own_folder(&config.spill_dir)?;
        Ok(Self {
            inner: Mutex::new(Inner::default()),
            ram_budget: config.ram_budget_bytes,
            disk_budget: config.disk_budget_bytes,
            folder,
            owner: Some(owner),
        })
    }

    /// This store's spill folder. Files in it belong to the store and are removed when it is dropped.
    pub fn spill_folder(&self) -> &Path {
        &self.folder
    }

    /// The RAM budget the store was created with, in bytes. Frames beyond it spill to disk.
    pub fn ram_budget(&self) -> u64 {
        self.ram_budget
    }

    /// The disk budget the store was created with, in bytes. Spill files beyond it are deleted, oldest first.
    pub fn disk_budget(&self) -> u64 {
        self.disk_budget
    }

    /// The message of the most recent failure (a spill that could not be written, or a file that could not be read).
    pub fn last_error(&self) -> Option<String> {
        self.inner.lock().last_error.clone()
    }

    /// Stores `frame` under `key`, replacing any earlier frame with the same key, then evicts to stay within budget.
    ///
    /// A frame larger than the RAM budget goes straight to disk. A frame larger than the disk budget is dropped and
    /// counted in [`FrameStoreStats::dropped_frames`].
    pub fn insert(&self, key: FrameKey, frame: StoredFrame) {
        let bytes = frame.data.len() as u64;
        let mut stale = Vec::new();
        let mut staged = Vec::new();
        {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            remove_locked(inner, &key, &mut stale);
            if bytes <= self.ram_budget {
                let tick = inner.next_tick();
                inner.add_ram(key, frame, tick);
                self.evict_ram_locked(inner, &mut staged);
            } else {
                self.stage_locked(inner, key, frame, &mut staged);
            }
        }
        self.flush(staged, &mut stale);
        remove_files(&stale);
    }

    /// Returns the frame stored under `key`.
    ///
    /// A RAM hit refreshes recency. A disk hit is read back without the lock and promoted to RAM when it fits in the RAM
    /// budget without evicting anything; otherwise the frame stays on disk.
    pub fn get(&self, key: &FrameKey) -> Option<StoredFrame> {
        let (path, bytes) = {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            if let Some(entry) = inner.ram.remove(key) {
                inner.ram_lru.remove(&entry.tick);
                inner.ram_bytes -= entry.frame.data.len() as u64;
                let frame = entry.frame.clone();
                let tick = inner.next_tick();
                inner.add_ram(*key, entry.frame, tick);
                inner.hits += 1;
                return Some(frame);
            }
            if let Some(pending) = inner.spilling.get(key) {
                inner.hits += 1;
                return Some(pending.frame.clone());
            }
            match inner.disk.get(key) {
                Some(entry) => (entry.path.clone(), entry.bytes),
                None => {
                    inner.misses += 1;
                    return None;
                }
            }
        };

        // The file is read without the lock. Its entry may have moved on meanwhile, so the bookkeeping below only acts
        // when the entry still names this file.
        let read = read_spill_file(&path);
        let mut stale = Vec::new();
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let current = inner.disk.get(key).is_some_and(|entry| entry.path == path);
        let outcome = match read {
            Ok(frame) => {
                inner.hits += 1;
                if current {
                    if inner.ram_bytes + bytes <= self.ram_budget {
                        take_disk_entry(inner, key, &mut stale);
                        let tick = inner.next_tick();
                        inner.add_ram(*key, frame.clone(), tick);
                    } else {
                        let tick = inner.next_tick();
                        if let Some(entry) = inner.disk.get_mut(key) {
                            inner.disk_lru.remove(&entry.tick);
                            entry.tick = tick;
                            inner.disk_lru.insert(tick, *key);
                        }
                    }
                }
                Some(frame)
            }
            Err(err) => {
                inner.misses += 1;
                let message = format!("could not read spill file {}: {err}", path.display());
                tracing::warn!(%message, "dropping unreadable spill file");
                inner.last_error = Some(message);
                if current {
                    take_disk_entry(inner, key, &mut stale);
                }
                None
            }
        };
        drop(guard);
        remove_files(&stale);
        outcome
    }

    /// True when `key` is held in RAM, in a spill in progress, or on disk. Does not change recency or the counters.
    pub fn contains(&self, key: &FrameKey) -> bool {
        let inner = self.inner.lock();
        inner.ram.contains_key(key)
            || inner.spilling.contains_key(key)
            || inner.disk.contains_key(key)
    }

    /// Removes every frame that belongs to `source`, in all tiers, and deletes their spill files.
    pub fn remove_source(&self, source: u64) {
        let mut stale = Vec::new();
        {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            let keys: Vec<FrameKey> = inner
                .ram
                .keys()
                .chain(inner.disk.keys())
                .chain(inner.spilling.keys())
                .filter(|key| key.source == source)
                .copied()
                .collect();
            for key in keys {
                remove_locked(inner, &key, &mut stale);
            }
        }
        remove_files(&stale);
    }

    /// Removes every frame and deletes every spill file this store wrote. Hit, miss and drop counters are kept.
    pub fn clear(&self) {
        let mut stale = Vec::new();
        {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            let keys: Vec<FrameKey> = inner
                .ram
                .keys()
                .chain(inner.disk.keys())
                .chain(inner.spilling.keys())
                .copied()
                .collect();
            for key in keys {
                remove_locked(inner, &key, &mut stale);
            }
        }
        remove_files(&stale);
    }

    /// Current usage and counters.
    pub fn stats(&self) -> FrameStoreStats {
        let inner = self.inner.lock();
        FrameStoreStats {
            ram_bytes: inner.ram_bytes,
            disk_bytes: inner.disk_bytes,
            ram_frames: inner.ram.len(),
            disk_frames: inner.disk.len(),
            hits: inner.hits,
            misses: inner.misses,
            dropped_frames: inner.dropped,
        }
    }

    /// Spills least-recently used RAM frames until RAM is back within budget. Caller holds the lock.
    fn evict_ram_locked(&self, inner: &mut Inner, staged: &mut Vec<Staged>) {
        while inner.ram_bytes > self.ram_budget {
            let Some((_, key)) = inner.ram_lru.pop_first() else {
                break;
            };
            let Some(entry) = inner.ram.remove(&key) else {
                continue;
            };
            inner.ram_bytes -= entry.frame.data.len() as u64;
            self.stage_locked(inner, key, entry.frame, staged);
        }
    }

    /// Marks `frame` as being spilled and queues its write, or drops it when it cannot fit the disk budget at all.
    /// Caller holds the lock.
    fn stage_locked(
        &self,
        inner: &mut Inner,
        key: FrameKey,
        frame: StoredFrame,
        staged: &mut Vec<Staged>,
    ) {
        if frame.data.len() as u64 > self.disk_budget {
            inner.dropped += 1;
            return;
        }
        let ticket = inner.next_ticket();
        inner.spilling.insert(
            key,
            Spilling {
                frame: frame.clone(),
                ticket,
            },
        );
        staged.push(Staged { key, ticket, frame });
    }

    /// Writes the staged frames without the lock, then commits them and enforces the disk budget under the lock.
    fn flush(&self, staged: Vec<Staged>, stale: &mut Vec<PathBuf>) {
        if staged.is_empty() {
            return;
        }
        let written: Vec<Written> = staged
            .into_iter()
            .map(|item| self.write_staged(item))
            .collect();
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        for item in written {
            commit_locked(inner, item, stale);
        }
        enforce_disk_budget(inner, self.disk_budget, stale);
    }

    /// Writes one staged frame to a fresh file name. Runs without the lock.
    fn write_staged(&self, item: Staged) -> Written {
        let path = self.folder.join(spill_file_name(item.key, item.ticket));
        let result = write_spill_file(&path, &item.frame);
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        Written {
            key: item.key,
            ticket: item.ticket,
            path,
            bytes: item.frame.data.len() as u64,
            result,
        }
    }
}

impl Inner {
    fn next_tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn next_ticket(&mut self) -> u64 {
        self.tickets += 1;
        self.tickets
    }

    fn add_ram(&mut self, key: FrameKey, frame: StoredFrame, tick: u64) {
        self.ram_bytes += frame.data.len() as u64;
        self.ram_lru.insert(tick, key);
        self.ram.insert(key, RamEntry { frame, tick });
    }

    fn add_disk(&mut self, key: FrameKey, path: PathBuf, bytes: u64, tick: u64) {
        self.disk_bytes += bytes;
        self.disk_lru.insert(tick, key);
        self.disk.insert(key, DiskEntry { path, bytes, tick });
    }
}

/// Removes `key` from every tier. Spill files go to `stale`, to be deleted after the lock is released.
fn remove_locked(inner: &mut Inner, key: &FrameKey, stale: &mut Vec<PathBuf>) {
    if let Some(entry) = inner.ram.remove(key) {
        inner.ram_lru.remove(&entry.tick);
        inner.ram_bytes -= entry.frame.data.len() as u64;
    }
    take_disk_entry(inner, key, stale);
    inner.spilling.remove(key);
}

/// Removes the disk entry for `key`, if any, and queues its file for deletion.
fn take_disk_entry(inner: &mut Inner, key: &FrameKey, stale: &mut Vec<PathBuf>) {
    if let Some(entry) = inner.disk.remove(key) {
        inner.disk_lru.remove(&entry.tick);
        inner.disk_bytes -= entry.bytes;
        stale.push(entry.path);
    }
}

/// Commits one written frame. A frame that was replaced or removed while it was being written is not kept.
fn commit_locked(inner: &mut Inner, item: Written, stale: &mut Vec<PathBuf>) {
    let current = inner
        .spilling
        .get(&item.key)
        .is_some_and(|pending| pending.ticket == item.ticket);
    if !current {
        stale.push(item.path);
        return;
    }
    inner.spilling.remove(&item.key);
    match item.result {
        Ok(()) => {
            let tick = inner.next_tick();
            inner.add_disk(item.key, item.path, item.bytes, tick);
        }
        Err(err) => {
            inner.dropped += 1;
            let message = format!("could not spill frame to {}: {err}", item.path.display());
            tracing::warn!(%message, "dropping frame");
            inner.last_error = Some(message);
        }
    }
}

/// Deletes least-recently used spill files until the disk tier is back within `budget`.
fn enforce_disk_budget(inner: &mut Inner, budget: u64, stale: &mut Vec<PathBuf>) {
    while inner.disk_bytes > budget {
        let Some((_, key)) = inner.disk_lru.pop_first() else {
            break;
        };
        if let Some(entry) = inner.disk.remove(&key) {
            inner.disk_bytes -= entry.bytes;
            stale.push(entry.path);
        }
    }
}

impl Drop for FrameStore {
    fn drop(&mut self) {
        let inner = self.inner.get_mut();
        // Writes are synchronous within `insert`, so no spill is in progress once `&mut self` exists.
        let files: Vec<PathBuf> = inner
            .disk
            .values()
            .map(|entry| entry.path.clone())
            .collect();
        inner.disk.clear();
        inner.disk_lru.clear();
        inner.spilling.clear();
        remove_files(&files);
        // Release the lock first: Windows cannot delete a locked file.
        drop(self.owner.take());
        let _ = fs::remove_file(self.folder.join(OWNER_FILE));
        // Succeeds only when the folder is empty, so anything else placed in it is left alone.
        if let Err(err) = fs::remove_dir(&self.folder)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.folder.display(), %err, "could not remove spill folder");
        }
    }
}

/// Deletes spill files that no longer have an owner. A folder is stale when its `owner.lock` can be locked, which
/// only happens after the process that held it has exited. Folders whose owner is alive, folders without a lock file,
/// and everything else in `root` are left alone.
fn sweep_dead_folders(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        let named = entry
            .file_name()
            .to_string_lossy()
            .starts_with(FOLDER_PREFIX);
        if !named || !folder.is_dir() {
            continue;
        }
        let owner_path = folder.join(OWNER_FILE);
        let Ok(owner) = OpenOptions::new().read(true).write(true).open(&owner_path) else {
            continue;
        };
        match owner.try_lock() {
            Ok(()) => {
                remove_spill_files(&folder);
                drop(owner);
                let _ = fs::remove_file(&owner_path);
                let _ = fs::remove_dir(&folder);
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(err)) => {
                tracing::debug!(path = %owner_path.display(), %err, "could not check spill folder owner");
            }
        }
    }
}

/// Deletes the spill files in `folder`, leaving every other file alone.
fn remove_spill_files(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == SPILL_EXTENSION)
        {
            remove_files(std::slice::from_ref(&path));
        }
    }
}

/// Creates this store's folder under `root` with its owner lock already held.
fn create_own_folder(root: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..16 {
        let folder = root.join(format!("{FOLDER_PREFIX}{:016x}", nonce()));
        match fs::create_dir(&folder) {
            Ok(()) => {
                let owner = OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(folder.join(OWNER_FILE))?;
                match owner.try_lock() {
                    Ok(()) => return Ok((folder, owner)),
                    Err(TryLockError::WouldBlock) => {
                        return Err(io::Error::other("new spill folder lock is already held"));
                    }
                    Err(TryLockError::Error(err)) => return Err(err),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique spill folder",
    ))
}

/// A value that differs between stores and between runs of the same process: process id, time and the per-process
/// random hasher state.
fn nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    hasher.finish()
}

/// Spill file name for one frame write. The ticket keeps names unique even for the same key.
fn spill_file_name(key: FrameKey, ticket: u64) -> String {
    format!(
        "{:016x}-{:016x}-{ticket:x}.{SPILL_EXTENSION}",
        key.source, key.index
    )
}

/// Deletes each path, ignoring files that are already gone.
fn remove_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(err) = fs::remove_file(path)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %err, "could not delete spill file");
        }
    }
}

fn layout_tag(layout: PixelLayout) -> u8 {
    match layout {
        PixelLayout::Nv12 => 0,
        PixelLayout::P010 => 1,
        PixelLayout::Rgba8 => 2,
    }
}

fn layout_from_tag(tag: u8) -> Option<PixelLayout> {
    match tag {
        0 => Some(PixelLayout::Nv12),
        1 => Some(PixelLayout::P010),
        2 => Some(PixelLayout::Rgba8),
        _ => None,
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Writes one frame as header plus raw payload.
fn write_spill_file(path: &Path, frame: &StoredFrame) -> io::Result<()> {
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&SPILL_MAGIC);
    header[4] = SPILL_VERSION;
    header[5] = layout_tag(frame.layout);
    header[8..12].copy_from_slice(&frame.width.to_le_bytes());
    header[12..16].copy_from_slice(&frame.height.to_le_bytes());
    header[16..24].copy_from_slice(&frame.time.to_bits().to_le_bytes());
    header[24..32].copy_from_slice(&(frame.data.len() as u64).to_le_bytes());
    let mut file = File::create(path)?;
    file.write_all(&header)?;
    file.write_all(&frame.data)?;
    file.flush()
}

/// Reads a frame written by [`write_spill_file`], rejecting files whose header does not match their size.
///
/// The expected length is computed with checked arithmetic, so a corrupt header cannot wrap around and pass the check.
/// The payload buffer is only allocated once the file length has been confirmed to match.
fn read_spill_file(path: &Path) -> io::Result<StoredFrame> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header)?;
    if header[0..4] != SPILL_MAGIC || header[4] != SPILL_VERSION {
        return Err(invalid_data("not a LoopCut spill file"));
    }
    let layout = layout_from_tag(header[5]).ok_or_else(|| invalid_data("unknown pixel layout"))?;
    let width = u32::from_le_bytes(header_array(&header[8..12])?);
    let height = u32::from_le_bytes(header_array(&header[12..16])?);
    let time = f64::from_bits(u64::from_le_bytes(header_array(&header[16..24])?));
    let payload_len = u64::from_le_bytes(header_array(&header[24..32])?);
    let expected_len = (HEADER_LEN as u64)
        .checked_add(payload_len)
        .ok_or_else(|| invalid_data("spill payload length overflows"))?;
    if file_len != expected_len {
        return Err(invalid_data("spill file length does not match its header"));
    }
    let payload_len =
        usize::try_from(payload_len).map_err(|_| invalid_data("spill payload too large"))?;
    let mut data = vec![0u8; payload_len];
    file.read_exact(&mut data)?;
    Ok(StoredFrame {
        width,
        height,
        layout,
        time,
        data: Arc::from(data),
    })
}

/// Copies a fixed-size header field out of a slice.
fn header_array<const N: usize>(bytes: &[u8]) -> io::Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| invalid_data("truncated spill header"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spill_file_round_trips_header_and_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("frame.lcframe");
        let original = StoredFrame {
            width: 640,
            height: 360,
            layout: PixelLayout::P010,
            time: 12.5,
            data: Arc::from(vec![7u8; 16]),
        };
        write_spill_file(&path, &original).expect("write");
        let back = read_spill_file(&path).expect("read");
        assert_eq!(back, original);
    }

    #[test]
    fn corrupt_spill_file_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.lcframe");
        fs::write(&path, b"short").expect("write");
        assert!(read_spill_file(&path).is_err());
    }

    #[test]
    fn a_header_claiming_a_huge_payload_is_rejected_without_allocating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.lcframe");
        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(&SPILL_MAGIC);
        header[4] = SPILL_VERSION;
        header[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, header).expect("write");
        let err = read_spill_file(&path).expect_err("length overflows");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn budgets_are_reported_as_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FrameStore::new(FrameStoreConfig {
            ram_budget_bytes: 1234,
            disk_budget_bytes: 5678,
            spill_dir: dir.path().join("spill"),
        })
        .expect("frame store");
        assert_eq!(store.ram_budget(), 1234);
        assert_eq!(store.disk_budget(), 5678);
    }

    #[test]
    fn default_budget_is_a_quarter_capped_at_two_gib() {
        assert_eq!(default_ram_budget(8 * GIB), 2 * GIB);
        assert_eq!(default_ram_budget(4 * GIB), GIB);
        assert_eq!(default_ram_budget(0), 0);
    }
}

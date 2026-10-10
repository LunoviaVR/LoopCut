//! Application state shared by every command and protocol handler.
//!
//! One `Arc<AppState>` is managed by Tauri at startup. Blocking work clones the `Arc` into `spawn_blocking`; no
//! lock is held across FFmpeg or file I/O.
//!
//! Lock order, when two are needed: `caps` before `settings`; `gpu` before `preview`; `media` (the map) before a
//! `MediaEntry`'s `state`, the token table and the thumbnail budget. The thumbnail budget lock is never held while
//! another lock is taken. Never take `media` while holding `gpu` or `preview`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use loopcut_core::hw::DecodeChain;
use loopcut_core::media::{MediaInfo, VideoStreamInfo};
use loopcut_hw::Capabilities;
use loopcut_media::FfmpegPaths;
use loopcut_media::framestore::FrameStore;
use loopcut_media::paths::FFMPEG_DIR_ENV;
use loopcut_media::runner::{CancelToken, is_unc_path};
use loopcut_render::caption_layers::CaptionRenderer;
use parking_lot::{Mutex, RwLock};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

use crate::dto::AppSettings;
use crate::error::{AppError, code};
use crate::netpath::{has_accepted_prefix, is_network_path};
use crate::render::{GpuSlot, PreviewSession};
use crate::thumbcache::{DEFAULT_THUMB_BUDGET_BYTES, ThumbBudget, ThumbKey, ThumbSet};
use crate::tokens::{TokenTable, is_token};

/// Version of the bundled FFmpeg, from `scripts/ffmpeg.lock.json` (set by `build.rs`).
pub const FFMPEG_VERSION: &str = env!("LOOPCUT_FFMPEG_VERSION");

/// Bytes in a mebibyte.
pub const MIB: u64 = 1024 * 1024;
/// Bytes in a gibibyte.
pub const GIB: u64 = 1024 * MIB;

/// Most runtime warnings kept for diagnostics.
const MAX_RUNTIME_WARNINGS: usize = 50;

/// The refusal every new piece of background or GPU work gets while the registry is closed for an update.
pub const UPDATE_REFUSAL: &str =
    "LoopCut is stopping background work to install an update. Try again after the update.";

/// Every directory LoopCut reads or writes.
#[derive(Debug, Clone)]
pub struct AppDirs {
    /// `settings.json` lives here.
    pub config: PathBuf,
    /// `recent.json`, the `recovery` folder and `hw-capabilities.json` live here.
    pub data: PathBuf,
    /// Disposable data: proxies, frame spill files, partial exports.
    pub cache: PathBuf,
    /// `loopcut.log` and crash reports live here.
    pub logs: PathBuf,
    pub recovery: PathBuf,
    pub proxies: PathBuf,
    /// Root of the frame store's spill folders. Each store owns one folder under it, with an `owner.lock` file.
    pub frames: PathBuf,
}

impl AppDirs {
    /// Resolves the standard Tauri directories and derives the subfolders.
    pub fn resolve(app: &AppHandle) -> tauri::Result<Self> {
        let config = app.path().app_config_dir()?;
        let data = app.path().app_data_dir()?;
        let cache = app.path().app_cache_dir()?;
        let logs = app.path().app_log_dir()?;
        Ok(Self::from_roots(config, data, cache, logs))
    }

    /// Every folder under one root. Used when the standard folders cannot be resolved at all.
    pub fn under_root(root: &Path) -> Self {
        Self::from_roots(
            root.join("config"),
            root.join("data"),
            root.join("cache"),
            root.join("logs"),
        )
    }

    fn from_roots(config: PathBuf, data: PathBuf, cache: PathBuf, logs: PathBuf) -> Self {
        Self {
            recovery: data.join("recovery"),
            proxies: cache.join("proxies"),
            frames: cache.join("frames"),
            config,
            data,
            cache,
            logs,
        }
    }

    /// Creates every folder. Returns the first failure; the caller decides whether that is fatal.
    pub fn create_all(&self) -> std::io::Result<()> {
        for dir in [
            &self.config,
            &self.data,
            &self.cache,
            &self.logs,
            &self.recovery,
            &self.proxies,
            &self.frames,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Path of `settings.json`.
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// Path of `recent.json`.
    pub fn recent_file(&self) -> PathBuf {
        self.data.join("recent.json")
    }

    /// Path of the cached hardware capability record.
    pub fn hw_cache_file(&self) -> PathBuf {
        self.data.join("hw-capabilities.json")
    }
}

/// Folder used when the standard app folders or the frame cache folder are unusable.
pub fn temp_root() -> PathBuf {
    std::env::temp_dir().join("LoopCut")
}

/// One media file's cached facts, derived from a successful probe. An entry exists only for media that was probed,
/// so a token can never refer to a file LoopCut has not read.
#[derive(Debug)]
pub struct MediaEntry {
    /// Canonical local path.
    pub path: PathBuf,
    /// Probe result.
    pub info: Arc<MediaInfo>,
    pub state: Mutex<MediaState>,
}

/// Mutable facts about one media file.
#[derive(Debug, Default)]
pub struct MediaState {
    /// Finished proxy file, once one exists for this file.
    pub proxy: Option<PathBuf>,
    /// Job id of the proxy encode this entry started, recorded when the job registers. Release cancels this id, so it
    /// does not depend on the file's metadata at the time of the release.
    pub proxy_job: Option<String>,
    /// The newest thumbnail set for each height.
    pub thumbs: Vec<Arc<ThumbSet>>,
}

impl MediaEntry {
    pub fn new(path: PathBuf, info: Arc<MediaInfo>, proxy: Option<PathBuf>) -> Self {
        Self {
            path,
            info,
            state: Mutex::new(MediaState {
                proxy,
                proxy_job: None,
                thumbs: Vec::new(),
            }),
        }
    }

    /// Stores a thumbnail set, replacing the previous set with the same real height (newest wins).
    pub fn store_thumbs(&self, set: Arc<ThumbSet>) {
        let mut state = self.state.lock();
        state.thumbs.retain(|kept| kept.height != set.height);
        state.thumbs.push(set);
    }

    /// Drops the set at `height`, when the global budget evicts it.
    pub fn drop_thumbs(&self, height: u32) {
        self.state
            .lock()
            .thumbs
            .retain(|kept| kept.height != height);
    }
}

/// Cancellation handles of running jobs, keyed by job id. A `JobGuard` removes its entry when dropped.
///
/// Job ids are plain strings: `media:<token>:<kind>` for work on one media file, `probe:<path>` and `keyframes:<path>`
/// for scans, `proxy:<output path>` for a timeline proxy (keyed by its output file, so two tokens for one file cannot
/// encode it twice), `hw-probe` for the capability probe, `ffmpeg-version` for the startup version read, `export:<id>`
/// for exports, `estimate:<clip>` for size estimates and `preview:<call>` for the preview commands that do work.
///
/// Once [`JobRegistry::close`] is called (by `app_prepare_update`), no job can start or register until the registry is
/// reopened or the app restarts.
#[derive(Debug, Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, CancelToken>>,
    closed: AtomicBool,
}

/// Keeps one job registered. Dropping it unregisters the job.
#[derive(Debug)]
pub struct JobGuard {
    registry: Arc<JobRegistry>,
    id: String,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.registry.jobs.lock().remove(&self.id);
    }
}

impl JobRegistry {
    /// Registers `token` under `id`. Returns `None` when a job with that id is already running or the registry is
    /// closed.
    pub fn register(self: &Arc<Self>, id: &str, token: CancelToken) -> Option<JobGuard> {
        let mut jobs = self.jobs.lock();
        if self.is_closed() || jobs.contains_key(id) {
            return None;
        }
        jobs.insert(id.to_owned(), token);
        Some(JobGuard {
            registry: Arc::clone(self),
            id: id.to_owned(),
        })
    }

    /// Starts a job and registers it, so it can always be cancelled. The job is registered under `id` when that is
    /// free, otherwise under `id#<n>` for the first free `n` from 2. Cancelling by family ([`Self::cancel_family`])
    /// reaches every one of them. Returns `None` while the registry is closed.
    pub fn start(self: &Arc<Self>, id: &str) -> Option<(CancelToken, JobGuard)> {
        let mut jobs = self.jobs.lock();
        if self.is_closed() {
            return None;
        }
        let token = CancelToken::new();
        let key = if jobs.contains_key(id) {
            let mut n = 2u32;
            loop {
                let candidate = format!("{id}#{n}");
                if !jobs.contains_key(&candidate) {
                    break candidate;
                }
                n = n.wrapping_add(1);
            }
        } else {
            id.to_owned()
        };
        jobs.insert(key.clone(), token.clone());
        let guard = JobGuard {
            registry: Arc::clone(self),
            id: key,
        };
        Some((token, guard))
    }

    /// Refuses every new job until [`Self::reopen`]. Jobs already running are not touched.
    pub fn close(&self) {
        let _jobs = self.jobs.lock();
        self.closed.store(true, Ordering::Release);
    }

    /// Allows new jobs again after [`Self::close`].
    pub fn reopen(&self) {
        let _jobs = self.jobs.lock();
        self.closed.store(false, Ordering::Release);
    }

    /// True while new jobs are refused.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Requests cancellation of every job started under `id`, including the `id#<n>` duplicates. Returns how many were
    /// signalled.
    pub fn cancel_family(&self, id: &str) -> usize {
        let duplicate_prefix = format!("{id}#");
        let jobs = self.jobs.lock();
        let mut signalled = 0;
        for (key, token) in jobs.iter() {
            if key == id || key.starts_with(&duplicate_prefix) {
                token.cancel();
                signalled += 1;
            }
        }
        signalled
    }

    /// Requests cancellation of every job whose id starts with `prefix`. Returns how many were signalled.
    pub fn cancel_prefix(&self, prefix: &str) -> usize {
        let jobs = self.jobs.lock();
        let mut signalled = 0;
        for (id, token) in jobs.iter() {
            if id.starts_with(prefix) {
                token.cancel();
                signalled += 1;
            }
        }
        signalled
    }

    /// Requests cancellation of every running job and returns how many there were.
    pub fn cancel_all(&self) -> usize {
        let jobs = self.jobs.lock();
        for token in jobs.values() {
            token.cancel();
        }
        jobs.len()
    }

    /// Number of running jobs.
    pub fn len(&self) -> usize {
        self.jobs.lock().len()
    }

    /// True when no job is running.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Job id of work on one media token: `media:<token>:<kind>`.
pub fn media_job_id(token: &str, kind: &str) -> String {
    format!("media:{token}:{kind}")
}

/// Prefix shared by every job of one media token.
pub fn media_job_prefix(token: &str) -> String {
    format!("media:{token}:")
}

/// Job id of the timeline proxy that encodes `output`.
pub fn proxy_job_id(output: &Path) -> String {
    format!("proxy:{}", output.display())
}

/// Job id of the metadata probe of one local file.
pub fn probe_job_id(path: &Path) -> String {
    format!("probe:{}", path.display())
}

/// Job id of the keyframe scan of one local file.
pub fn keyframe_job_id(path: &Path) -> String {
    format!("keyframes:{}", path.display())
}

/// The caption rasterizer shared between the font list and the exports.
pub type SharedCaptions = Arc<std::sync::Mutex<CaptionRenderer>>;

/// The newest size estimate of one clip: its generation and the token that stops it.
#[derive(Debug, Clone)]
pub struct EstimateSlot {
    pub generation: u64,
    pub cancel: CancelToken,
}

/// Everything shared across commands.
pub struct AppState {
    /// The bundled FFmpeg, or `None` when it could not be located (the reason is in `ffmpeg_problem`).
    pub ffmpeg: Option<FfmpegPaths>,
    pub ffmpeg_problem: Option<String>,
    /// First line of `ffmpeg -version`, filled in the background at startup.
    pub ffmpeg_version: OnceLock<String>,
    pub dirs: AppDirs,
    pub settings: RwLock<AppSettings>,
    pub tokens: TokenTable,
    pub media: RwLock<HashMap<String, Arc<MediaEntry>>>,
    /// RAM budget for filmstrip thumbnails across all media files.
    pub thumb_budget: ThumbBudget,
    /// The frame store, or `None` when neither the cache folder nor the temp folder could host one.
    pub framestore: Option<Arc<FrameStore>>,
    pub jobs: Arc<JobRegistry>,
    /// Hardware capabilities: `None` until the startup probe (or the cache) provides them.
    pub caps: RwLock<Option<Capabilities>>,
    /// Problems found while starting (unusable folders, a corrupt settings file). Shown by diagnostics.
    pub startup_warnings: Vec<String>,
    /// Problems found while running (a GPU fallback, a lost device). Shown by diagnostics.
    pub runtime_warnings: Mutex<Vec<String>>,
    /// The shared GPU context. See `crate::render`.
    pub gpu: Mutex<GpuSlot>,
    /// The preview engine and its session. See `crate::render`.
    pub preview: Mutex<PreviewSession>,
    /// Held while an update is prepared. A call that finds it held is refused with `busy`, so only one call closes the
    /// registry and, after a failure, reopens it.
    pub update_gate: Mutex<()>,
    /// A RAM-only frame store for this session, used when the cache folder could not host one. Created on first use.
    pub session_frames: Mutex<Option<Arc<FrameStore>>>,
    /// The caption font rasterizer, created on first use by `fonts_list` or by an export of a project with captions. It is
    /// shared, so the installed fonts are scanned once. See [`AppState::caption_renderer`].
    pub captions: Mutex<Option<SharedCaptions>>,
    /// The newest estimate per clip. Claiming a clip's estimate cancels the one it supersedes.
    pub estimates: Mutex<HashMap<String, EstimateSlot>>,
    /// Source of estimate generations.
    pub estimate_seq: AtomicU64,
    /// A file passed on the command line at first launch.
    pub open_path_arg: Option<String>,
    /// The project file the session last opened or saved. Cleared by "new project".
    pub current_project: Mutex<Option<PathBuf>>,
}

impl AppState {
    /// Assembles the state. `ffmpeg` is `None` when FFmpeg could not be located; `ffmpeg_problem` then says why.
    pub fn new(
        dirs: AppDirs,
        settings: AppSettings,
        ffmpeg: Result<FfmpegPaths, String>,
        framestore: Option<Arc<FrameStore>>,
        startup_warnings: Vec<String>,
        open_path_arg: Option<String>,
    ) -> Self {
        let (ffmpeg, ffmpeg_problem) = match ffmpeg {
            Ok(paths) => (Some(paths), None),
            Err(problem) => (None, Some(problem)),
        };
        Self {
            ffmpeg,
            ffmpeg_problem,
            ffmpeg_version: OnceLock::new(),
            dirs,
            settings: RwLock::new(settings),
            tokens: TokenTable::default(),
            media: RwLock::new(HashMap::new()),
            thumb_budget: ThumbBudget::new(DEFAULT_THUMB_BUDGET_BYTES),
            framestore,
            jobs: Arc::new(JobRegistry::default()),
            caps: RwLock::new(None),
            startup_warnings,
            runtime_warnings: Mutex::new(Vec::new()),
            gpu: Mutex::new(GpuSlot::default()),
            preview: Mutex::new(PreviewSession::default()),
            update_gate: Mutex::new(()),
            session_frames: Mutex::new(None),
            captions: Mutex::new(None),
            estimates: Mutex::new(HashMap::new()),
            estimate_seq: AtomicU64::new(0),
            open_path_arg,
            current_project: Mutex::new(None),
        }
    }

    /// The shared caption rasterizer, created on the first call. Its font scan is the slow part, so it is made once.
    pub fn caption_renderer(&self) -> SharedCaptions {
        let mut slot = self.captions.lock();
        Arc::clone(
            slot.get_or_insert_with(|| Arc::new(std::sync::Mutex::new(CaptionRenderer::new()))),
        )
    }

    /// The FFmpeg paths, or an `ffmpeg_missing` error that says why.
    pub fn ffmpeg(&self) -> Result<&FfmpegPaths, AppError> {
        self.ffmpeg.as_ref().ok_or_else(|| {
            AppError::ffmpeg_missing(
                self.ffmpeg_problem
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
            )
        })
    }

    /// The entry for a live token.
    pub fn media_entry(&self, token: &str) -> Option<Arc<MediaEntry>> {
        self.media.read().get(token).cloned()
    }

    /// The canonical path behind a token whose media was probed. `None` for unknown, released or malformed tokens.
    /// The map is checked under the same lock that mints and releases tokens, so a token is never half live.
    pub fn source_path(&self, token: &str) -> Option<PathBuf> {
        let media = self.media.read();
        if !media.contains_key(token) {
            return None;
        }
        self.tokens.resolve(token)
    }

    /// Makes a probed file available under a token and returns the token. Any earlier entry for the same file is
    /// replaced, and its thumbnails are forgotten. The replaced entry's proxy job moves to the new entry, so a release
    /// still cancels an encode that started before the file was imported again. The token and the entry are published
    /// under one lock.
    pub fn publish_media(
        &self,
        path: PathBuf,
        info: Arc<MediaInfo>,
        proxy: Option<PathBuf>,
    ) -> String {
        let mut media = self.media.write();
        let token = self.tokens.mint(&path);
        self.thumb_budget.forget_token(&token);
        // Map before entry state, as the lock order requires.
        let proxy_job = media
            .get(&token)
            .and_then(|replaced| replaced.state.lock().proxy_job.clone());
        let entry = Arc::new(MediaEntry::new(path, info, proxy));
        entry.state.lock().proxy_job = proxy_job;
        media.insert(token.clone(), entry);
        token
    }

    /// Caches a thumbnail set for `entry` and evicts, from every file, the least recently used sets the budget
    /// requires. The set is stored only while `entry` is still the live entry of `token`: a set made for a released or
    /// replaced entry is dropped.
    pub fn remember_thumbs(&self, token: &str, entry: &Arc<MediaEntry>, set: Arc<ThumbSet>) {
        let key = ThumbKey {
            token: token.to_owned(),
            height: set.height,
        };
        let bytes = set.bytes();
        let victims = {
            let media = self.media.read();
            match media.get(token) {
                Some(live) if Arc::ptr_eq(live, entry) => {
                    entry.store_thumbs(set);
                    self.thumb_budget.insert(key, bytes)
                }
                _ => return,
            }
        };
        for victim in victims {
            if let Some(other) = self.media_entry(&victim.token) {
                other.drop_thumbs(victim.height);
            }
        }
    }

    /// Forgets a token: cancels its jobs (thumbnails, probes, scans and the proxy it encodes), drops its media entry,
    /// thumbnails and path. Later requests for it get 404. The proxy encode is cancelled by the job id its entry
    /// recorded when it started, so no file metadata is read here.
    pub fn release_media(&self, token: &str) {
        if !is_token(token) {
            return;
        }
        let removed = {
            let mut media = self.media.write();
            let removed = media.remove(token);
            self.tokens.release(token);
            self.thumb_budget.forget_token(token);
            removed
        };
        self.jobs.cancel_prefix(&media_job_prefix(token));
        if let Some(entry) = removed {
            self.jobs.cancel_family(&probe_job_id(&entry.path));
            self.jobs.cancel_family(&keyframe_job_id(&entry.path));
            let proxy_job = entry.state.lock().proxy_job.clone();
            if let Some(job) = proxy_job {
                self.jobs.cancel_family(&job);
            }
        }
    }

    /// Refuses with `busy` while the registry is closed for an update. Preview and GPU work check this first, so
    /// nothing starts an engine, a device or a decode while the installer needs the files.
    pub fn refuse_if_closed(&self) -> Result<(), AppError> {
        if self.jobs.is_closed() {
            Err(AppError::new(code::BUSY, UPDATE_REFUSAL))
        } else {
            Ok(())
        }
    }

    /// Starts a job, or refuses with `busy` while the registry is closed for an update or the id is taken.
    pub fn start_job(&self, id: &str) -> Result<(CancelToken, JobGuard), AppError> {
        self.jobs
            .start(id)
            .ok_or_else(|| self.jobs_refusal("That work is already running."))
    }

    /// Registers an existing token as a job, or refuses with `busy` (`busy_message` when the id is taken).
    pub fn register_job(
        &self,
        id: &str,
        cancel: CancelToken,
        busy_message: &str,
    ) -> Result<JobGuard, AppError> {
        self.jobs
            .register(id, cancel)
            .ok_or_else(|| self.jobs_refusal(busy_message))
    }

    /// The error for a refused job: the update message while the registry is closed, otherwise `busy_message`.
    fn jobs_refusal(&self, busy_message: &str) -> AppError {
        if self.jobs.is_closed() {
            AppError::new(code::BUSY, UPDATE_REFUSAL)
        } else {
            AppError::new(code::BUSY, busy_message)
        }
    }

    /// Records a problem found while running. The list is bounded; the oldest entries go first.
    pub fn push_runtime_warning(&self, text: impl Into<String>) {
        let mut warnings = self.runtime_warnings.lock();
        if warnings.len() >= MAX_RUNTIME_WARNINGS {
            warnings.remove(0);
        }
        warnings.push(text.into());
    }

    /// The FFmpeg version line, or a note while it is still being read.
    pub fn ffmpeg_version_line(&self) -> String {
        match self.ffmpeg_version.get() {
            Some(line) => line.clone(),
            None => match &self.ffmpeg_problem {
                Some(problem) => format!("unavailable: {problem}"),
                None => "unavailable".to_owned(),
            },
        }
    }

    /// The decode chain for one source stream, from the probed capabilities and the user's settings. CPU while the
    /// probe has not finished or found nothing usable.
    pub fn decode_chain_for(&self, info: &VideoStreamInfo) -> DecodeChain {
        let caps = self.caps.read();
        let settings = self.settings.read();
        match caps.as_ref() {
            Some(caps) => loopcut_hw::select::choose_decode_chain(
                caps,
                &settings.hw,
                &info.codec,
                info.bit_depth,
                info.has_alpha,
            ),
            None => DecodeChain::CPU,
        }
    }
}

/// Locates the bundled FFmpeg. Order: `LOOPCUT_FFMPEG_DIR`, then the Tauri resource folder for the pinned version,
/// then (debug builds only) the repository's staged build. Returns the reason on failure.
pub fn resolve_ffmpeg(app: &AppHandle) -> Result<FfmpegPaths, String> {
    let mut problems = Vec::new();

    if let Some(dir) = std::env::var_os(FFMPEG_DIR_ENV) {
        match FfmpegPaths::from_dir(Path::new(&dir)) {
            Ok(paths) => return Ok(paths),
            Err(err) => problems.push(format!("{FFMPEG_DIR_ENV}: {err}")),
        }
    }

    match app
        .path()
        .resolve(format!("ffmpeg/{FFMPEG_VERSION}"), BaseDirectory::Resource)
    {
        Ok(dir) => match FfmpegPaths::from_dir(&dir) {
            Ok(paths) => return Ok(paths),
            Err(err) => problems.push(format!("{}: {err}", dir.display())),
        },
        Err(err) => problems.push(format!("resource folder: {err}")),
    }

    if cfg!(debug_assertions) {
        if let Some(paths) = FfmpegPaths::discover_dev() {
            return Ok(paths);
        }
        problems.push("no staged development build under src-tauri/resources/ffmpeg".to_owned());
    }
    Err(problems.join("; "))
}

/// The first command-line argument (after the program name) that names an existing local file. Relative names are
/// joined to `cwd`, and the returned path is trimmed and absolute. UNC and mapped network paths are never returned:
/// opening them contacts another computer, which needs the user's confirmation first. Device paths (`\\.\`, `\\?\`)
/// are never returned either.
pub fn first_openable_arg_in<I>(args: I, cwd: &Path) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    args.into_iter().skip(1).find_map(|arg| {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            return None;
        }
        let given = Path::new(trimmed);
        if is_unc_path(given) || is_network_path(given) {
            return None;
        }
        let candidate = if given.is_absolute() {
            given.to_path_buf()
        } else {
            cwd.join(given)
        };
        let text = candidate.to_string_lossy().into_owned();
        if !has_accepted_prefix(&text) || is_network_path(&candidate) {
            return None;
        }
        candidate.is_file().then_some(text)
    })
}

/// [`first_openable_arg_in`] for the first launch, relative to the process's working directory.
pub fn first_openable_arg(args: impl IntoIterator<Item = String>) -> Option<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    first_openable_arg_in(args, &cwd)
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Builds a complete state rooted in a temporary folder, without FFmpeg.

    use super::*;
    use loopcut_media::framestore::FrameStoreConfig;

    /// A state whose folders live under `root`, with a RAM-only frame store and no FFmpeg.
    pub fn state_under(root: &Path) -> Arc<AppState> {
        let dirs = AppDirs::under_root(root);
        dirs.create_all().expect("test folders");
        let framestore = FrameStore::new(FrameStoreConfig {
            ram_budget_bytes: MIB,
            disk_budget_bytes: 0,
            spill_dir: dirs.frames.clone(),
        })
        .expect("test frame store");
        Arc::new(AppState::new(
            dirs,
            AppSettings::default(),
            Err("tests have no FFmpeg".to_owned()),
            Some(Arc::new(framestore)),
            Vec::new(),
            None,
        ))
    }

    /// Media info parsed from a recorded ffprobe fixture, so tests need no FFmpeg.
    pub fn sample_info() -> Arc<MediaInfo> {
        let json =
            include_str!("../../crates/loopcut-media/tests/fixtures/ffprobe/h264_320x240.json");
        Arc::new(loopcut_media::parse_ffprobe_json(json, 1024).expect("fixture parses"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_guard_unregisters_on_drop() {
        let registry = Arc::new(JobRegistry::default());
        let guard = registry
            .register("job-1", CancelToken::new())
            .expect("first registration");
        assert!(
            registry.register("job-1", CancelToken::new()).is_none(),
            "duplicate ids are refused"
        );
        assert_eq!(registry.len(), 1);
        drop(guard);
        assert!(registry.is_empty());
    }

    #[test]
    fn cancel_all_signals_every_token() {
        let registry = Arc::new(JobRegistry::default());
        let a = CancelToken::new();
        let b = CancelToken::new();
        let _ga = registry.register("a", a.clone()).expect("register a");
        let _gb = registry.register("b", b.clone()).expect("register b");
        assert_eq!(registry.cancel_all(), 2);
        assert!(a.is_cancelled() && b.is_cancelled());
        assert_eq!(registry.cancel_family("missing"), 0);
    }

    #[test]
    fn cancel_prefix_only_reaches_matching_jobs() {
        let registry = Arc::new(JobRegistry::default());
        let mine = CancelToken::new();
        let theirs = CancelToken::new();
        let _g1 = registry
            .register(
                &media_job_id("0123456789abcdef0123456789abcdef", "proxy"),
                mine.clone(),
            )
            .expect("register");
        let _g2 = registry
            .register(
                &media_job_id("ffffffffffffffffffffffffffffffff", "proxy"),
                theirs.clone(),
            )
            .expect("register");
        let prefix = media_job_prefix("0123456789abcdef0123456789abcdef");
        assert_eq!(registry.cancel_prefix(&prefix), 1);
        assert!(mine.is_cancelled());
        assert!(!theirs.is_cancelled());
    }

    #[test]
    fn duplicate_starts_are_registered_and_cancelled_as_a_family() {
        let registry = Arc::new(JobRegistry::default());
        let (first, _first_guard) = registry.start("probe:x").expect("start");
        let (second, second_guard) = registry.start("probe:x").expect("duplicate start");
        let (unrelated, _unrelated_guard) = registry.start("probe:xy").expect("start");
        assert_eq!(registry.len(), 3, "the duplicate is registered too");
        assert!(has_suffixed_duplicate(&registry));
        assert_eq!(registry.cancel_family("probe:x"), 2);
        assert!(first.is_cancelled() && second.is_cancelled());
        assert!(!unrelated.is_cancelled(), "a longer id is a different job");
        drop(second_guard);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn a_closed_registry_refuses_new_jobs_until_reopened() {
        let registry = Arc::new(JobRegistry::default());
        let (running, _guard) = registry.start("job-a").expect("start before closing");
        registry.close();
        assert!(registry.is_closed());
        assert!(
            registry.start("job-b").is_none(),
            "start refused while closed"
        );
        assert!(
            registry.register("job-c", CancelToken::new()).is_none(),
            "register refused while closed"
        );
        assert_eq!(registry.len(), 1, "running jobs are left alone");
        assert!(!running.is_cancelled(), "closing does not cancel by itself");
        registry.reopen();
        assert!(
            registry.start("job-b").is_some(),
            "reopened registry starts jobs"
        );
    }

    /// True when a `probe:x#<n>` entry exists alongside `probe:x`.
    fn has_suffixed_duplicate(registry: &Arc<JobRegistry>) -> bool {
        registry
            .jobs
            .lock()
            .keys()
            .any(|key| key.starts_with("probe:x#"))
    }

    #[test]
    fn thumbnail_sets_keep_the_newest_per_height() {
        let entry = MediaEntry::new(
            PathBuf::from("C:\\a.mp4"),
            test_support::sample_info(),
            None,
        );
        let set = |count: u32, height: u32| {
            Arc::new(ThumbSet {
                count,
                height,
                width: 16,
                times: vec![],
                frames: vec![],
            })
        };
        entry.store_thumbs(set(4, 90));
        entry.store_thumbs(set(8, 90));
        entry.store_thumbs(set(3, 180));
        let state = entry.state.lock();
        assert_eq!(state.thumbs.len(), 2, "one set per height");
        let at_90 = state
            .thumbs
            .iter()
            .find(|s| s.height == 90)
            .expect("90 kept");
        assert_eq!(at_90.count, 8, "the newest set for a height wins");
    }

    #[test]
    fn thumbnails_are_stored_only_for_the_live_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        let path = dir.path().join("clip.mp4");
        let token = state.publish_media(path.clone(), test_support::sample_info(), None);
        let stale = state.media_entry(&token).expect("first entry");
        // Publishing the same file again replaces the entry and keeps the token.
        let again = state.publish_media(path, test_support::sample_info(), None);
        assert_eq!(again, token, "one token per file");
        let set = || {
            Arc::new(ThumbSet {
                count: 1,
                height: 90,
                width: 16,
                times: vec![0.0],
                frames: vec![vec![0; 16 * 90 * 4]],
            })
        };
        state.remember_thumbs(&token, &stale, set());
        assert_eq!(
            state.thumb_budget.set_count(),
            0,
            "a replaced entry is not stored"
        );
        assert!(stale.state.lock().thumbs.is_empty());
        let live = state.media_entry(&token).expect("live entry");
        state.remember_thumbs(&token, &live, set());
        assert_eq!(state.thumb_budget.set_count(), 1);
        assert_eq!(live.state.lock().thumbs.len(), 1);
    }

    #[test]
    fn first_openable_arg_skips_the_program_and_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("clip.loopcut");
        std::fs::write(&file, b"{}").expect("seed");
        let file_str = file.to_string_lossy().into_owned();
        let args = vec![
            "loopcut.exe".to_owned(),
            "--flag".to_owned(),
            dir.path()
                .join("missing.mp4")
                .to_string_lossy()
                .into_owned(),
            file_str.clone(),
        ];
        assert_eq!(
            first_openable_arg_in(args, dir.path()),
            Some(file_str.clone())
        );
        assert_eq!(
            first_openable_arg_in(vec!["loopcut.exe".to_owned(), "   ".to_owned()], dir.path()),
            None
        );
        // The program name itself is never treated as the file to open.
        let only_program = vec![file_str];
        assert_eq!(first_openable_arg_in(only_program, dir.path()), None);
    }

    #[test]
    fn device_paths_are_never_forwarded_from_the_command_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let args = vec![
            "loopcut.exe".to_owned(),
            r"\\.\C:\clip.mp4".to_owned(),
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\clip.mp4".to_owned(),
        ];
        assert_eq!(first_openable_arg_in(args, dir.path()), None);
    }

    #[test]
    fn relative_arguments_are_joined_to_the_working_directory_and_trimmed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("clip.loopcut"), b"{}").expect("seed");
        let args = vec!["loopcut.exe".to_owned(), "  clip.loopcut  ".to_owned()];
        let found = first_openable_arg_in(args, dir.path()).expect("found relative to cwd");
        assert_eq!(
            PathBuf::from(&found),
            dir.path().join("clip.loopcut"),
            "joined to the second instance's folder"
        );
        assert!(!found.starts_with(' ') && !found.ends_with(' '));
    }

    #[test]
    fn released_media_leaves_no_jobs_or_thumbnails_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        let path = dir.path().join("clip.mp4");
        let token = state.publish_media(path.clone(), test_support::sample_info(), None);
        let (cancel, _guard) = state
            .start_job(&media_job_id(&token, "proxy"))
            .expect("job starts");
        state.release_media(&token);
        assert!(cancel.is_cancelled());
        assert!(state.media_entry(&token).is_none());
        assert!(state.source_path(&token).is_none());
        assert_eq!(state.thumb_budget.set_count(), 0);
    }

    #[test]
    fn releasing_a_token_cancels_the_proxy_it_encodes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        let path = dir.path().join("clip.mp4");
        let token = state.publish_media(path.clone(), test_support::sample_info(), None);
        let output = crate::commands::media::proxy_path(&state.dirs.proxies, &path, 0);
        let cancel = CancelToken::new();
        let job = proxy_job_id(&output);
        let _guard = state
            .register_job(&job, cancel.clone(), "busy")
            .expect("proxy registers");
        // The proxy encode records its job id on the entry when it registers, as proxy_start does.
        state
            .media_entry(&token)
            .expect("live")
            .state
            .lock()
            .proxy_job = Some(job);
        state.release_media(&token);
        assert!(cancel.is_cancelled(), "the recorded proxy job is cancelled");
    }

    #[test]
    fn release_cancels_the_recorded_proxy_job_not_a_recomputed_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        let token = state.publish_media(
            dir.path().join("clip.mp4"),
            test_support::sample_info(),
            None,
        );
        let recorded = CancelToken::new();
        let unrelated = CancelToken::new();
        let _recorded_guard = state
            .register_job("proxy:recorded", recorded.clone(), "busy")
            .expect("registers");
        let _other_guard = state
            .register_job("proxy:other", unrelated.clone(), "busy")
            .expect("registers");
        state
            .media_entry(&token)
            .expect("live")
            .state
            .lock()
            .proxy_job = Some("proxy:recorded".to_owned());
        state.release_media(&token);
        assert!(recorded.is_cancelled());
        assert!(
            !unrelated.is_cancelled(),
            "only the recorded id is cancelled"
        );
    }

    #[test]
    fn importing_a_file_again_keeps_the_proxy_job_that_release_cancels() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        let path = dir.path().join("clip.mp4");
        let token = state.publish_media(path.clone(), test_support::sample_info(), None);
        let job = proxy_job_id(&dir.path().join("out.mp4"));
        let cancel = CancelToken::new();
        let _guard = state
            .register_job(&job, cancel.clone(), "busy")
            .expect("proxy registers");
        state
            .media_entry(&token)
            .expect("live")
            .state
            .lock()
            .proxy_job = Some(job.clone());
        // The same file is imported while its proxy encodes. The token is kept, and so is the recorded job.
        let again = state.publish_media(path, test_support::sample_info(), None);
        assert_eq!(again, token, "one token per file");
        let replaced = state.media_entry(&token).expect("live");
        assert_eq!(
            replaced.state.lock().proxy_job.as_deref(),
            Some(job.as_str()),
            "the new entry records the running proxy"
        );
        state.release_media(&token);
        assert!(cancel.is_cancelled(), "release cancels the running proxy");
    }

    #[test]
    fn a_closed_registry_says_so_for_gpu_and_preview_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        assert!(state.refuse_if_closed().is_ok());
        state.jobs.close();
        let err = state.refuse_if_closed().expect_err("closed");
        assert_eq!(err.code, "busy");
        assert_eq!(err.message, UPDATE_REFUSAL);
        state.jobs.reopen();
        assert!(state.refuse_if_closed().is_ok());
    }

    #[test]
    fn refused_jobs_say_why() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_support::state_under(dir.path());
        state.jobs.close();
        let err = state.start_job("probe:x").expect_err("refused");
        assert_eq!(err.code, "busy");
        assert!(err.message.contains("install an update"));
        state.jobs.reopen();
        let (_cancel, _guard) = state.start_job("probe:x").expect("allowed again");
        let err = state
            .register_job("probe:x", CancelToken::new(), "already running")
            .expect_err("duplicate");
        assert_eq!(err.message, "already running");
    }
}

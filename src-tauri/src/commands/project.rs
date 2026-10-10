//! Project commands: new, open, save, autosave and crash recovery, recent projects and the sequence summary.
//!
//! Project files are untrusted input. They are read with a size cap, validated and migrated by `loopcut_core::migrate`,
//! and written atomically with media paths stored relative to the project file. Opening never touches a network
//! location: such clips are reported and left alone until the user confirms them through `media_import`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use loopcut_core::gif_timing;
use loopcut_core::media::MediaInfo;
use loopcut_core::migrate::{self, MAX_PROJECT_BYTES};
use loopcut_core::project::{Clip, Project, SCHEMA_VERSION};
use loopcut_core::sequencer::{self, SequenceError};
use loopcut_core::time::TimeRange;
use loopcut_media::runner::is_url_like;
use serde_json::Value;
use tauri::State;

use crate::commands::blocking;
use crate::commands::media::{LocalOpen, canonical_path, open_local, source_duration};
use crate::dto::{OpenedProject, RecentProject, RecoverySnapshot, SequencePlanSummary};
use crate::error::{AppError, code, io_error};
use crate::netpath::{has_accepted_prefix, is_network_path};
use crate::recent;
use crate::recovery;
use crate::state::AppState;
use crate::storage;
use crate::tokens::random_hex_suffix;

/// Length of the clip a new project starts with, in seconds of source time.
pub const DEFAULT_CLIP_SECONDS: f64 = 6.0;

/// Serializes read-modify-write of `recent.json` across concurrent commands.
static RECENT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// A new project with one clip: the first six seconds of the media. Resets the session's project path, because a new
/// project has not been saved anywhere yet.
#[tauri::command]
pub async fn project_new(
    state: State<'_, Arc<AppState>>,
    token: String,
) -> Result<Project, AppError> {
    let st = Arc::clone(&state);
    blocking(move || new_project(&st, &token)).await
}

/// Opens a `.loopcut` file: loads and migrates it, then probes each local clip and mints tokens for the ones that
/// probed successfully. Clips that are missing, unreadable or on a network share are listed in `missingMedia`
/// (network ones also in `networkMedia`) and get no token.
#[tauri::command]
pub async fn project_open(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<OpenedProject, AppError> {
    let st = Arc::clone(&state);
    blocking(move || open_project(&st, &path)).await
}

/// Saves a project atomically (temporary file, then rename). Media paths are stored relative to the project file.
#[tauri::command]
pub async fn project_save(
    state: State<'_, Arc<AppState>>,
    path: String,
    project: Project,
) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || save_project(&st, &path, &project)).await
}

/// Writes the crash-recovery slot. The UI debounces calls (about one second).
#[tauri::command]
pub async fn project_autosave(
    state: State<'_, Arc<AppState>>,
    project: Project,
) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let original = st
            .current_project
            .lock()
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        recovery::write(
            &st.dirs.recovery,
            &project,
            original.as_deref(),
            &storage::now_iso8601(),
        )
    })
    .await
}

/// The autosave slot, if one survived a crash. Damaged slots read as absent.
#[tauri::command]
pub async fn project_recovery(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<RecoverySnapshot>, AppError> {
    let st = Arc::clone(&state);
    blocking(move || Ok(recovery::read(&st.dirs.recovery))).await
}

/// Deletes the autosave slot (after the user saved, discarded or restored it).
#[tauri::command]
pub async fn project_recovery_clear(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || recovery::clear(&st.dirs.recovery)).await
}

/// Recently opened or saved projects, most recent first.
#[tauri::command]
pub async fn recent_projects(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<RecentProject>, AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        let limit = st.settings.read().general.recent_limit as usize;
        Ok(recent::load(&st.dirs.recent_file(), limit))
    })
    .await
}

/// Frame count, duration, effective frame rate and per-frame GIF delays for a project.
#[tauri::command]
pub async fn sequence_plan(project: Project) -> Result<SequencePlanSummary, AppError> {
    blocking(move || sequence_summary(&project)).await
}

fn new_project(st: &AppState, token: &str) -> Result<Project, AppError> {
    let entry = st
        .media_entry(token)
        .ok_or_else(|| AppError::not_found("This media"))?;
    let project = project_for_media(&entry.path, &entry.info)?;
    *st.current_project.lock() = None;
    Ok(project)
}

/// The starting project for one probed file: one clip of up to [`DEFAULT_CLIP_SECONDS`], named after the file.
pub(crate) fn project_for_media(path: &Path, info: &MediaInfo) -> Result<Project, AppError> {
    let duration = source_duration(info);
    if !(duration.is_finite() && duration > 0.0) {
        return Err(AppError::new(
            code::UNSUPPORTED_MEDIA,
            "The media has no usable duration.",
        ));
    }
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_owned());
    let clip = Clip {
        id: format!("clip-{}", random_hex_suffix()),
        path: path.to_string_lossy().into_owned(),
        stream_index: info.video.index,
        segments: vec![TimeRange::new(0.0, duration.min(DEFAULT_CLIP_SECONDS))],
        holds: Vec::new(),
    };
    Ok(Project::new(name, clip))
}

/// Where a clip's media lives, judged from its stored path before the file system is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipSource {
    /// A UNC path or a mapped network drive. Never opened by `project_open`.
    Network(String),
    /// A local path, to be canonicalized and probed.
    Local(PathBuf),
    /// Not a drive-letter or UNC path (a device path, a URL-like string, or a relative path). Never touched; the clip is
    /// reported as missing.
    Refused(String),
}

/// Classifies one clip path (already resolved against the project folder). The check runs on the stored text, before
/// the file system is touched.
pub fn classify_clip(stored: &str) -> ClipSource {
    if !has_accepted_prefix(stored) {
        return ClipSource::Refused(stored.to_owned());
    }
    let path = PathBuf::from(stored);
    if is_network_path(&path) {
        ClipSource::Network(stored.to_owned())
    } else {
        ClipSource::Local(path)
    }
}

/// Loads a project from the bytes of a `.loopcut` file: UTF-8, then schema validation and migration.
pub(crate) fn decode_project(bytes: Vec<u8>) -> Result<(Project, bool), AppError> {
    let text = String::from_utf8(bytes)
        .map_err(|_| AppError::new(code::INVALID_PROJECT, "The project file is not text."))?;
    Ok(migrate::load(&text)?)
}

/// The JSON written for `project`, with media paths made relative to `dir`.
pub(crate) fn project_json(project: &Project, dir: &Path) -> Result<String, AppError> {
    let mut out = project.clone();
    migrate::relativize_paths(&mut out, dir);
    Ok(migrate::save(&out)?)
}

fn open_project(st: &AppState, raw: &str) -> Result<OpenedProject, AppError> {
    let requested = project_path(raw)?;
    let canonical = canonical_path(&requested)?;
    let bytes = storage::read_capped(&canonical, MAX_PROJECT_BYTES as u64)
        .map_err(|err| io_error(&err, "The project file could not be read"))?;
    let (mut project, migrated) = decode_project(bytes)?;
    if let Some(dir) = canonical.parent() {
        migrate::resolve_paths(&mut project, dir);
    }

    let mut tokens = BTreeMap::new();
    let mut missing_media = Vec::new();
    let mut network_media = Vec::new();
    for clip in &project.clips {
        match classify_clip(&clip.path) {
            ClipSource::Network(stored) => {
                missing_media.push(stored.clone());
                network_media.push(stored);
            }
            ClipSource::Refused(stored) => missing_media.push(stored),
            ClipSource::Local(path) => match open_local(st, &path)? {
                LocalOpen::Token(token) => {
                    tokens.insert(clip.id.clone(), token);
                }
                LocalOpen::Missing => missing_media.push(clip.path.clone()),
                LocalOpen::Network => {
                    missing_media.push(clip.path.clone());
                    network_media.push(clip.path.clone());
                }
            },
        }
    }

    let name = project.name.clone();
    *st.current_project.lock() = Some(canonical.clone());
    remember(st, &canonical, &name);
    Ok(OpenedProject {
        project,
        path: canonical.to_string_lossy().into_owned(),
        tokens,
        migrated,
        missing_media,
        network_media,
    })
}

fn save_project(st: &AppState, raw: &str, project: &Project) -> Result<(), AppError> {
    let requested = project_path(raw)?;
    let target = std::path::absolute(&requested)
        .map_err(|err| io_error(&err, "The project location is not valid"))?;
    let dir = target
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::invalid_argument("The project needs to be saved in a folder."))?;

    let existing = storage::read_capped(&target, MAX_PROJECT_BYTES as u64);
    match overwrite_decision(existing)? {
        Overwrite::Replace => {}
        Overwrite::BackupThenReplace => backup_once(&target)?,
    }

    let json = project_json(project, &dir)?;
    storage::atomic_write(&target, json.as_bytes())
        .map_err(|err| io_error(&err, "The project could not be saved"))?;

    *st.current_project.lock() = Some(target.clone());
    remember(st, &target, &project.name);
    Ok(())
}

/// What a save may do to the file already at its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    /// Nothing there, or a current-schema project, or bytes that are not a project: replace it.
    Replace,
    /// An older schema: keep a `.bak` copy first (the first copy is never replaced), then replace.
    BackupThenReplace,
}

/// Decides how a save may replace the file that exists at its target, from the result of reading it (capped).
///
/// * A missing file is replaced.
/// * Any other read error, and a file over the size cap, refuse the save: the bytes cannot be checked, so they are kept.
/// * A file written by a newer schema refuses the save.
/// * A file written by an older schema is backed up first.
/// * Bytes that do not parse as JSON are replaced, because they are not a project anyone can lose.
///
/// # Errors
///
/// `invalid_project` for an oversized file, `newer_project` for a newer schema, `io` for other read errors.
pub fn overwrite_decision(existing: io::Result<Vec<u8>>) -> Result<Overwrite, AppError> {
    let bytes = match existing {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Overwrite::Replace),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            return Err(AppError::new(
                code::INVALID_PROJECT,
                "The file already at this location is too large to be a project, so it was not overwritten.",
            )
            .with_detail(err.to_string()));
        }
        Err(err) => {
            return Err(io_error(
                &err,
                "The file already at this location could not be read, so it was not overwritten",
            ));
        }
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(Overwrite::Replace);
    };
    let version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if version > u64::from(SCHEMA_VERSION) {
        return Err(AppError::new(
            code::NEWER_PROJECT,
            "This file was saved by a newer version of LoopCut. Save under a different name to keep it.",
        ));
    }
    if version < u64::from(SCHEMA_VERSION) {
        return Ok(Overwrite::BackupThenReplace);
    }
    Ok(Overwrite::Replace)
}

/// Copies `path` to its `.bak` name unless a backup already exists.
fn backup_once(path: &Path) -> Result<(), AppError> {
    let backup = backup_path(path);
    if backup.exists() {
        return Ok(());
    }
    fs::copy(path, &backup)
        .map(|_| ())
        .map_err(|err| io_error(&err, "The original project could not be backed up"))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".bak");
    path.with_file_name(name)
}

/// Adds a project to the recent list. Failure is logged, never shown: the list is a convenience.
fn remember(st: &AppState, path: &Path, name: &str) {
    let _guard = RECENT_LOCK.lock();
    let limit = st.settings.read().general.recent_limit as usize;
    let file = st.dirs.recent_file();
    let mut list = recent::load(&file, limit);
    recent::record(
        &mut list,
        RecentProject {
            path: path.to_string_lossy().into_owned(),
            name: name.to_owned(),
            opened_at: storage::now_iso8601(),
        },
        limit,
    );
    if let Err(err) = recent::store(&file, &list) {
        tracing::warn!(error = %err.message, "the recent projects list could not be updated");
    }
}

fn sequence_summary(project: &Project) -> Result<SequencePlanSummary, AppError> {
    migrate::validate(project)?;
    let clip = project
        .clip()
        .ok_or_else(|| AppError::new(code::INVALID_PROJECT, "The project has no clip."))?;
    let fps = project.output.fps;
    let plan = sequencer::plan(clip, &project.timing, fps).map_err(sequence_error)?;
    Ok(SequencePlanSummary {
        frames: u32::try_from(plan.len()).unwrap_or(u32::MAX),
        duration_seconds: plan.duration,
        effective_fps: gif_timing::effective_fps(fps),
        delays_cs: plan.frames.iter().map(|frame| frame.delay_cs).collect(),
    })
}

fn sequence_error(err: SequenceError) -> AppError {
    match err {
        SequenceError::TooLong(_) | SequenceError::InvalidFps => {
            AppError::invalid_argument(err.to_string())
        }
        _ => AppError::new(code::INVALID_PROJECT, err.to_string()),
    }
}

/// A user-supplied project path: non-empty and local (URLs are refused).
fn project_path(raw: &str) -> Result<PathBuf, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument("No project path was given."));
    }
    if is_url_like(trimmed) {
        return Err(AppError::invalid_argument(
            "Project files must be local paths.",
        ));
    }
    if !has_accepted_prefix(trimmed) {
        return Err(AppError::invalid_argument(
            "Project files must be on a drive letter or a UNC share.",
        ));
    }
    Ok(PathBuf::from(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::{sample_info, state_under};

    fn clip(path: &str) -> Clip {
        Clip {
            id: "c1".into(),
            path: path.into(),
            stream_index: 0,
            segments: vec![TimeRange::new(0.0, 2.0)],
            holds: vec![],
        }
    }

    #[test]
    fn backup_name_appends_bak() {
        assert_eq!(
            backup_path(Path::new("C:\\p\\work.loopcut")),
            PathBuf::from("C:\\p\\work.loopcut.bak")
        );
    }

    #[test]
    fn project_paths_must_be_local() {
        assert!(project_path("  ").is_err());
        assert!(project_path("https://example.com/p.loopcut").is_err());
        assert!(project_path("file:///C:/p.loopcut").is_err());
        assert_eq!(
            project_path(" C:\\p\\a.loopcut ").ok(),
            Some(PathBuf::from("C:\\p\\a.loopcut"))
        );
    }

    #[test]
    fn sequence_summary_reports_frames_and_delays() {
        let project = Project::new("p", clip("C:\\v.mp4"));
        let summary = sequence_summary(&project).expect("summary");
        // Default output is 20 fps: one second is 20 frames of 5 cs each.
        let one_second = {
            let mut p = project.clone();
            p.clips[0].segments = vec![TimeRange::new(0.0, 1.0)];
            p
        };
        let summary_one = sequence_summary(&one_second).expect("one second");
        assert_eq!(summary_one.frames, 20);
        assert!((summary_one.duration_seconds - 1.0).abs() < 1e-9);
        assert!(summary_one.delays_cs.iter().all(|&d| d == 5));
        assert!((summary_one.effective_fps - 20.0).abs() < 1e-9);
        assert!(summary.frames > 0);
    }

    #[test]
    fn sequence_summary_rejects_projects_without_a_clip() {
        let mut project = Project::new("p", clip("C:\\v.mp4"));
        project.clips.clear();
        assert!(sequence_summary(&project).is_err());
    }

    #[test]
    fn new_projects_start_with_six_seconds_named_after_the_file() {
        let info = sample_info();
        let project =
            project_for_media(Path::new("C:\\videos\\Holiday.mov"), &info).expect("project");
        assert_eq!(project.name, "Holiday");
        let clip = project.clip().expect("one clip");
        assert_eq!(clip.path, "C:\\videos\\Holiday.mov");
        assert_eq!(clip.segments.len(), 1);
        assert!(clip.segments[0].end <= DEFAULT_CLIP_SECONDS + 1e-9);
    }

    #[test]
    fn clips_on_network_shares_are_classified_without_touching_them() {
        assert_eq!(
            classify_clip(r"\\nas\media\clip.mp4"),
            ClipSource::Network(r"\\nas\media\clip.mp4".to_owned())
        );
        assert_eq!(
            classify_clip(r"C:\videos\clip.mp4"),
            ClipSource::Local(PathBuf::from(r"C:\videos\clip.mp4"))
        );
    }

    #[test]
    fn clips_on_device_paths_or_relative_paths_are_refused_untouched() {
        for stored in [
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\clip.mp4",
            r"\\.\C:\clip.mp4",
            r"clips\clip.mp4",
        ] {
            assert_eq!(
                classify_clip(stored),
                ClipSource::Refused(stored.to_owned()),
                "{stored}"
            );
        }
    }

    #[test]
    fn a_project_with_a_device_clip_opens_with_that_clip_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let project_file = dir.path().join("work.loopcut");
        let mut project = Project::new("Work", clip("C:\\placeholder.mp4"));
        project.clips[0].path = r"\\?\GLOBALROOT\Device\HarddiskVolume1\clip.mp4".to_owned();
        fs::write(
            &project_file,
            serde_json::to_string(&project).expect("json"),
        )
        .expect("write");
        let opened = open_project(&state, &project_file.to_string_lossy()).expect("opens");
        assert!(opened.tokens.is_empty());
        assert_eq!(opened.missing_media.len(), 1);
        assert!(
            opened.network_media.is_empty(),
            "a device path is not a network clip"
        );
    }

    #[test]
    fn project_bytes_must_be_utf8_text() {
        let err = decode_project(vec![0xff, 0xfe, 0x00]).expect_err("binary is not a project");
        assert_eq!(err.code, "invalid_project");
    }

    #[test]
    fn save_then_open_round_trips_with_relative_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let media_dir = dir.path().join("media");
        fs::create_dir_all(&media_dir).expect("mkdir");
        let media = media_dir.join("clip.mp4");
        fs::write(&media, b"not really a video").expect("seed");
        let project_dir = dir.path().join("projects");
        fs::create_dir_all(&project_dir).expect("mkdir");
        let project_file = project_dir.join("work.loopcut");

        let project = Project::new("Work", clip(&media.to_string_lossy()));
        let json = project_json(&project, &project_dir).expect("json");
        storage::atomic_write(&project_file, json.as_bytes()).expect("write");

        let on_disk = fs::read(&project_file).expect("read");
        let (stored, migrated) = decode_project(on_disk).expect("load");
        assert!(!migrated);
        // Stored relative to the project folder: it climbs out of `projects` and into `media`.
        assert!(
            PathBuf::from(&stored.clips[0].path).starts_with(".."),
            "{}",
            stored.clips[0].path
        );

        let mut loaded = stored;
        migrate::resolve_paths(&mut loaded, &project_dir);
        assert_eq!(
            PathBuf::from(&loaded.clips[0].path),
            media_dir.join("clip.mp4")
        );
        assert_eq!(loaded.clips[0].segments, project.clips[0].segments);
    }

    #[test]
    fn overwrite_policy_follows_the_existing_file() {
        let newer = format!("{{\"schemaVersion\": {}}}", SCHEMA_VERSION + 1);
        let err = overwrite_decision(Ok(newer.into_bytes())).expect_err("newer refused");
        assert_eq!(err.code, "newer_project");

        assert_eq!(
            overwrite_decision(Ok(b"{\"schemaVersion\": 0}".to_vec())).expect("older"),
            Overwrite::BackupThenReplace
        );
        assert_eq!(
            overwrite_decision(Ok(b"not json at all".to_vec())).expect("garbage"),
            Overwrite::Replace,
            "unparseable bytes are not a project anyone can lose"
        );
        assert_eq!(
            overwrite_decision(Err(io::Error::from(io::ErrorKind::NotFound))).expect("missing"),
            Overwrite::Replace
        );
    }

    #[test]
    fn unreadable_or_oversized_targets_are_never_overwritten() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let err = overwrite_decision(Err(denied)).expect_err("read error refuses");
        assert_eq!(err.code, "io");
        let oversized = io::Error::new(io::ErrorKind::InvalidData, "file is too large");
        let err = overwrite_decision(Err(oversized)).expect_err("oversized refuses");
        assert_eq!(err.code, "invalid_project");
    }

    #[test]
    fn older_projects_are_backed_up_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("old.loopcut");
        fs::write(&file, b"{\"schemaVersion\": 0}").expect("seed");
        backup_once(&file).expect("backup");
        let backup = backup_path(&file);
        assert_eq!(
            fs::read(&backup).expect("backup exists"),
            b"{\"schemaVersion\": 0}"
        );
        fs::write(&file, b"{\"schemaVersion\": 0, \"edited\": true}").expect("edit");
        backup_once(&file).expect("backup again");
        assert_eq!(
            fs::read(&backup).expect("backup kept"),
            b"{\"schemaVersion\": 0}"
        );
    }

    #[test]
    fn opening_a_missing_clip_reports_it_and_mints_no_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let project_file = dir.path().join("work.loopcut");
        let project = Project::new("Work", clip(&dir.path().join("gone.mp4").to_string_lossy()));
        fs::write(
            &project_file,
            project_json(&project, dir.path()).expect("json"),
        )
        .expect("write");

        let opened = open_project(&state, &project_file.to_string_lossy()).expect("opens");
        assert!(
            opened.tokens.is_empty(),
            "nothing probed, so nothing minted"
        );
        assert_eq!(opened.missing_media.len(), 1);
        assert!(
            opened.network_media.is_empty(),
            "a local miss is not a network clip"
        );
    }

    #[test]
    fn opening_a_network_clip_never_touches_it_and_says_why() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let project_file = dir.path().join("work.loopcut");
        let mut project = Project::new("Work", clip("C:\\placeholder.mp4"));
        project.clips[0].path = r"\\nas\media\far.mp4".to_owned();
        // Written directly: the save path validates clip paths, and a UNC path is a valid stored path.
        fs::write(
            &project_file,
            serde_json::to_string(&project).expect("json"),
        )
        .expect("write");

        let opened = open_project(&state, &project_file.to_string_lossy()).expect("opens");
        assert!(opened.tokens.is_empty());
        assert_eq!(
            opened.network_media,
            vec![r"\\nas\media\far.mp4".to_owned()]
        );
        assert_eq!(
            opened.missing_media, opened.network_media,
            "network clips are also missing"
        );
    }

    #[test]
    fn saving_refuses_to_replace_a_newer_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let target = dir.path().join("future.loopcut");
        fs::write(
            &target,
            format!("{{\"schemaVersion\": {}}}", SCHEMA_VERSION + 1),
        )
        .expect("seed");
        let project = Project::new("p", clip("C:\\v.mp4"));
        let err = save_project(&state, &target.to_string_lossy(), &project).expect_err("refused");
        assert_eq!(err.code, "newer_project");
        assert!(
            fs::read_to_string(&target)
                .expect("kept")
                .contains("schemaVersion")
        );
    }
}

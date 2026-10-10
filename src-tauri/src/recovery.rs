//! The crash-recovery slot: one autosaved project in the app data folder.
//!
//! The UI autosaves (debounced) through `project_autosave`. After a crash the next launch offers the slot. A slot that
//! fails validation is ignored rather than offered, so a damaged file cannot wedge the launch.
//!
//! The slot's `originalPath` is advisory. It names the project file the work came from, so the UI can show it, but
//! restoring a recovered project must behave as Save-As: nothing is written to `originalPath` until the user chooses it.

use std::path::Path;

use loopcut_core::migrate;
use loopcut_core::project::Project;
use serde::{Deserialize, Serialize};

use crate::dto::RecoverySnapshot;
use crate::error::AppError;
use crate::storage;

/// Name of the slot file inside the recovery folder.
pub const SLOT_FILE: &str = "autosave.json";
/// Largest slot accepted (a project plus a little metadata).
const MAX_SLOT_BYTES: u64 = (migrate::MAX_PROJECT_BYTES as u64) + 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slot {
    saved_at: String,
    original_path: Option<String>,
    project: Project,
}

/// Replaces the slot with `project`. `original_path` is the project file the work came from, if any.
pub fn write(
    dir: &Path,
    project: &Project,
    original_path: Option<&str>,
    saved_at: &str,
) -> Result<(), AppError> {
    migrate::validate(project)?;
    let slot = Slot {
        saved_at: saved_at.to_owned(),
        original_path: original_path.map(str::to_owned),
        project: project.clone(),
    };
    let json = serde_json::to_vec(&slot).map_err(|err| {
        AppError::new(
            crate::error::code::IO,
            format!("recovery data could not be encoded: {err}"),
        )
    })?;
    storage::atomic_write(&dir.join(SLOT_FILE), &json)
        .map_err(|err| crate::error::io_error(&err, "recovery data could not be saved"))
}

/// Reads the slot. Missing, unreadable, invalid or newer-than-supported slots all read as `None`.
pub fn read(dir: &Path) -> Option<RecoverySnapshot> {
    let bytes = storage::read_capped(&dir.join(SLOT_FILE), MAX_SLOT_BYTES).ok()?;
    let slot: Slot = serde_json::from_slice(&bytes).ok()?;
    migrate::validate(&slot.project).ok()?;
    Some(RecoverySnapshot {
        project: slot.project,
        saved_at: slot.saved_at,
        original_path: slot.original_path,
    })
}

/// Deletes the slot. A missing slot is not an error.
pub fn clear(dir: &Path) -> Result<(), AppError> {
    match std::fs::remove_file(dir.join(SLOT_FILE)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(crate::error::io_error(
            &err,
            "recovery data could not be cleared",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcut_core::project::Clip;
    use loopcut_core::time::TimeRange;

    fn project() -> Project {
        Project::new(
            "Recovered",
            Clip {
                id: "c1".into(),
                path: "C:\\videos\\clip.mp4".into(),
                stream_index: 0,
                segments: vec![TimeRange::new(0.0, 2.0)],
                holds: vec![],
            },
        )
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            &project(),
            Some("C:\\p\\work.loopcut"),
            "2026-10-10T12:00:00Z",
        )
        .expect("write");
        let snapshot = read(dir.path()).expect("slot present");
        assert_eq!(snapshot.project, project());
        assert_eq!(snapshot.saved_at, "2026-10-10T12:00:00Z");
        assert_eq!(
            snapshot.original_path.as_deref(),
            Some("C:\\p\\work.loopcut")
        );
    }

    #[test]
    fn clear_removes_the_slot_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &project(), None, "2026-10-10T12:00:00Z").expect("write");
        clear(dir.path()).expect("clear");
        assert!(read(dir.path()).is_none());
        clear(dir.path()).expect("clearing twice is fine");
    }

    #[test]
    fn damaged_slots_are_not_offered() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read(dir.path()).is_none());
        std::fs::write(dir.path().join(SLOT_FILE), b"{\"savedAt\":").expect("seed");
        assert!(read(dir.path()).is_none());
    }

    #[test]
    fn invalid_projects_are_refused_on_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut bad = project();
        bad.clips[0].path = "https://example.com/clip.mp4".into();
        assert!(write(dir.path(), &bad, None, "2026-10-10T12:00:00Z").is_err());
        assert!(read(dir.path()).is_none());
    }
}

//! Loading, validating, migrating and saving `.loopcut` documents.
//!
//! Project files are untrusted input: size-capped, URL-free, validated, and refused when written by a
//! newer LoopCut than this one. Unknown fields survive a load/save round trip.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

use crate::project::{Project, SCHEMA_VERSION};

/// Largest project file accepted (projects are small JSON documents).
pub const MAX_PROJECT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("project file is too large ({0} bytes)")]
    TooLarge(usize),
    #[error("project file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("project file has no schemaVersion")]
    MissingVersion,
    #[error(
        "this project was saved by a newer LoopCut (schema {found}, this version supports {supported}); please update LoopCut"
    )]
    NewerVersion { found: u32, supported: u32 },
    #[error("invalid project: {0}")]
    Invalid(String),
}

type Migration = fn(Value) -> Result<Value, ProjectError>;

/// `MIGRATIONS[n]` upgrades schema `n + 1` to `n + 2`. Empty while the schema is v1.
const MIGRATIONS: &[Migration] = &[];

/// Parses and validates a project file, applying migrations. Returns the project and whether it was
/// migrated (callers keep a backup of the original before overwriting a migrated file).
pub fn load(json: &str) -> Result<(Project, bool), ProjectError> {
    if json.len() > MAX_PROJECT_BYTES {
        return Err(ProjectError::TooLarge(json.len()));
    }
    let mut value: Value = serde_json::from_str(json)?;
    let version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or(ProjectError::MissingVersion)? as u32;
    if version > SCHEMA_VERSION {
        return Err(ProjectError::NewerVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version == 0 {
        return Err(ProjectError::Invalid("schemaVersion must be >= 1".into()));
    }
    let migrated = version < SCHEMA_VERSION;
    for migration in &MIGRATIONS[(version as usize - 1)..(SCHEMA_VERSION as usize - 1)] {
        value = migration(value)?;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("schemaVersion".into(), Value::from(SCHEMA_VERSION));
    }
    let project: Project = serde_json::from_value(value)?;
    validate(&project)?;
    Ok((project, migrated))
}

/// Serializes a project as pretty JSON.
pub fn save(project: &Project) -> Result<String, ProjectError> {
    validate(project)?;
    Ok(serde_json::to_string_pretty(project)?)
}

fn looks_like_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    match lower.split_once(':') {
        // Windows drive letters ("C:\...") are one character before the colon.
        Some((scheme, _)) if scheme.len() > 1 => scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)),
        _ => false,
    }
}

fn check_path(field: &str, path: &str) -> Result<(), ProjectError> {
    if path.is_empty() {
        return Err(ProjectError::Invalid(format!("{field} is empty")));
    }
    if looks_like_url(path) || path.contains("://") {
        return Err(ProjectError::Invalid(format!(
            "{field} must be a local file path, not a URL"
        )));
    }
    if path.len() > 32 * 1024 {
        return Err(ProjectError::Invalid(format!("{field} is too long")));
    }
    Ok(())
}

/// Structural validation shared by load and save.
pub fn validate(p: &Project) -> Result<(), ProjectError> {
    let bad = |m: &str| Err(ProjectError::Invalid(m.into()));
    if p.clips.is_empty() {
        return bad("a project needs at least one clip");
    }
    if p.clips.len() > 1 {
        return bad("this version of LoopCut supports one clip per project");
    }
    if p.name.len() > 1024 {
        return bad("project name is too long");
    }
    for clip in &p.clips {
        check_path("clip path", &clip.path)?;
        if clip.segments.is_empty() || clip.segments.len() > 10_000 {
            return bad("a clip needs between 1 and 10000 segments");
        }
        if clip.segments.iter().any(|s| !s.is_valid()) {
            return bad("a clip segment has an invalid time range");
        }
        if clip.holds.len() > 10_000
            || clip.holds.iter().any(|h| {
                !(h.at.is_finite()
                    && h.duration.is_finite()
                    && h.duration >= 0.0
                    && h.duration <= 3600.0)
            })
        {
            return bad("a clip has an invalid hold");
        }
    }
    if !p.timing.speed.is_finite() || p.timing.speed <= 0.0 {
        return bad("speed must be positive");
    }
    if !p.timing.crossfade.is_finite() || p.timing.crossfade < 0.0 {
        return bad("crossfade must be >= 0");
    }
    if !p.output.fps.is_valid() {
        return bad("output frame rate is invalid");
    }
    if ![0u16, 90, 180, 270].contains(&p.transform.rotation) {
        return bad("rotation must be 0, 90, 180 or 270");
    }
    let c = p.transform.crop;
    if ![c.x, c.y, c.w, c.h].iter().all(|v| v.is_finite()) || c.w <= 0.0 || c.h <= 0.0 {
        return bad("crop rectangle is invalid");
    }
    if let Some(lut) = &p.grade.lut_path {
        check_path("LUT path", lut)?;
    }
    if p.captions.len() > 1000 {
        return bad("too many captions");
    }
    if p.captions.iter().any(|cap| cap.text.len() > 10_000) {
        return bad("caption text is too long");
    }
    Ok(())
}

/// Rewrites absolute media paths as paths relative to `project_dir` where possible (portable projects).
pub fn relativize_paths(project: &mut Project, project_dir: &Path) {
    let rel = |p: &str| -> String {
        let path = Path::new(p);
        relative_to(path, project_dir)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.to_string())
    };
    for clip in &mut project.clips {
        clip.path = rel(&clip.path);
    }
    if let Some(l) = project.grade.lut_path.as_mut() {
        *l = rel(l);
    }
}

/// Resolves relative media paths against `project_dir` (call after [`load`]).
pub fn resolve_paths(project: &mut Project, project_dir: &Path) {
    let abs = |p: &str| -> String {
        let path = Path::new(p);
        if path.is_absolute() {
            p.to_string()
        } else {
            normalize(&project_dir.join(path))
                .to_string_lossy()
                .into_owned()
        }
    };
    for clip in &mut project.clips {
        clip.path = abs(&clip.path);
    }
    if let Some(l) = project.grade.lut_path.as_mut() {
        *l = abs(l);
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !base.is_absolute() {
        return None;
    }
    let path = normalize(path);
    let base = normalize(base);
    let pc: Vec<_> = path.components().collect();
    let bc: Vec<_> = base.components().collect();
    // Different drives / roots cannot be made relative.
    if pc.first() != bc.first() {
        return None;
    }
    let common = pc.iter().zip(&bc).take_while(|(a, b)| a == b).count();
    let mut rel = PathBuf::new();
    for _ in common..bc.len() {
        rel.push("..");
    }
    for c in &pc[common..] {
        rel.push(c.as_os_str());
    }
    Some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Clip;
    use crate::time::TimeRange;

    fn sample() -> Project {
        Project::new(
            "Test",
            Clip {
                id: "c1".into(),
                path: "C:\\videos\\clip.mp4".into(),
                stream_index: 0,
                segments: vec![TimeRange::new(1.0, 3.0)],
                holds: vec![],
            },
        )
    }

    #[test]
    fn roundtrip_preserves_unknown_fields() {
        let mut v: Value = serde_json::from_str(&save(&sample()).unwrap()).unwrap();
        v.as_object_mut()
            .unwrap()
            .insert("futureFeature".into(), Value::from(42));
        let (p, migrated) = load(&v.to_string()).unwrap();
        assert!(!migrated);
        let again: Value = serde_json::from_str(&save(&p).unwrap()).unwrap();
        assert_eq!(again["futureFeature"], Value::from(42));
    }

    #[test]
    fn refuses_newer_schema() {
        let mut v: Value = serde_json::from_str(&save(&sample()).unwrap()).unwrap();
        v["schemaVersion"] = Value::from(SCHEMA_VERSION + 1);
        assert!(matches!(
            load(&v.to_string()),
            Err(ProjectError::NewerVersion { .. })
        ));
    }

    #[test]
    fn rejects_urls_and_garbage() {
        let mut p = sample();
        p.clips[0].path = "https://example.com/a.m3u8".into();
        assert!(save(&p).is_err());
        p.clips[0].path = "file:///etc/passwd".into();
        assert!(save(&p).is_err());
        p.clips[0].path = "D:\\ok\\video.mov".into();
        assert!(save(&p).is_ok());
        assert!(load("{").is_err());
        assert!(matches!(load("{}"), Err(ProjectError::MissingVersion)));
    }

    #[test]
    fn missing_sections_use_defaults() {
        let json = r#"{"schemaVersion":1,"name":"x","clips":[{"id":"a","path":"a.mp4","streamIndex":0,"segments":[{"start":0,"end":1}]}]}"#;
        let (p, _) = load(json).unwrap();
        assert_eq!(p.output.loop_count, 0);
        assert!((p.timing.speed - 1.0).abs() < 1e-12);
    }

    #[cfg(windows)]
    #[test]
    fn relative_paths_roundtrip() {
        let mut p = sample();
        relativize_paths(&mut p, Path::new("C:\\videos\\projects"));
        assert_eq!(p.clips[0].path, "..\\clip.mp4");
        resolve_paths(&mut p, Path::new("C:\\videos\\projects"));
        assert_eq!(p.clips[0].path, "C:\\videos\\clip.mp4");
        let mut q = sample();
        relativize_paths(&mut q, Path::new("D:\\elsewhere"));
        assert_eq!(q.clips[0].path, "C:\\videos\\clip.mp4");
    }
}

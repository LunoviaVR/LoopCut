//! Data transfer objects that cross the IPC boundary. Every type here is exported to TypeScript by ts-rs (run
//! `cargo test -p loopcut`); field names follow docs/ipc.md in camelCase.

use std::collections::BTreeMap;

use loopcut_core::hw::HwSettings;
use loopcut_core::media::MediaInfo;
use loopcut_core::project::Project;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::AppError;

/// Result of `media_import`: the token the webview uses from now on, plus what the editor needs to know.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportedMedia {
    pub token: String,
    /// Canonical local path (for display; the webview never uses it to read the file).
    pub path: String,
    pub file_name: String,
    pub info: MediaInfo,
    /// Set when WebView2 can play the original and no proxy is required.
    pub source_url: Option<String>,
    pub needs_proxy: bool,
    /// Set when a proxy for this file already exists in the cache.
    pub proxy_url: Option<String>,
    /// Longest group of pictures in seconds. `None` (unknown: the packet scan failed) makes `needsProxy` true.
    pub keyframe_interval: Option<f64>,
}

/// Filmstrip thumbnails. Pixels are fetched per index from the `loopcut` protocol.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailSet {
    pub width: u32,
    /// Real output height in pixels (even). It can be one less than the requested height; the protocol reports the same value.
    pub height: u32,
    /// Source time of each thumbnail in seconds, in index order.
    pub times: Vec<f64>,
}

/// Progress for `media_proxy_start`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ProxyEvent {
    /// Fraction of the source encoded so far, 0 to 1.
    Progress { value: f64 },
    /// The proxy is ready at `url` (a `loopcut-media` protocol URL).
    Done { url: String },
    /// The proxy failed or was cancelled.
    Error { error: AppError },
}

/// Result of `project_open`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OpenedProject {
    pub project: Project,
    /// Canonical path of the project file.
    pub path: String,
    /// Clip id to media token, for every clip that exists locally and was probed successfully. Clips listed in
    /// `missingMedia` have no token.
    pub tokens: BTreeMap<String, String>,
    /// True when the file was written by an older schema; a `.bak` is kept before the first save.
    pub migrated: bool,
    /// Clip paths with no token: the file is missing, could not be probed, or lives on a network share (those are
    /// also listed in `networkMedia`). Network clips are never touched by `project_open`.
    pub missing_media: Vec<String>,
    /// The subset of `missingMedia` that names a network location (UNC path or mapped drive). Opening one needs the
    /// user's confirmation through `media_import` with `confirmNetwork`.
    pub network_media: Vec<String>,
}

/// The autosave slot, offered after a crash.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecoverySnapshot {
    pub project: Project,
    /// UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
    pub saved_at: String,
    /// Project file the work came from, if it had been saved before. Advisory only: it is a hint for where the work
    /// was, never a write target. The UI must treat restoring a recovered project as Save-As, so the original file is
    /// not overwritten without the user choosing it.
    pub original_path: Option<String>,
}

/// One entry of the recent-projects list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    /// UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
    pub opened_at: String,
}

/// Summary of the frame sequence for the current project.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SequencePlanSummary {
    pub frames: u32,
    pub duration_seconds: f64,
    pub effective_fps: f64,
    pub delays_cs: Vec<u16>,
}

/// Updater preferences. The updater itself is wired in a later milestone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct UpdateSettings {
    pub check_on_startup: bool,
    pub skipped_version: Option<String>,
    pub last_check: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            skipped_version: None,
            last_check: None,
        }
    }
}

/// Cache limits. Changes take effect the next time LoopCut starts (the frame store is sized once).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct CacheSettings {
    /// Disk spill cap for decoded frames, in GiB.
    pub max_disk_gb: f64,
    /// RAM cap for decoded frames, in MiB.
    pub ram_budget_mb: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            max_disk_gb: 10.0,
            ram_budget_mb: 2048,
        }
    }
}

/// General editor preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct GeneralSettings {
    pub default_export_dir: Option<String>,
    pub recent_limit: u32,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            default_export_dir: None,
            recent_limit: 10,
        }
    }
}

/// Everything persisted in `settings.json`. Missing or unknown fields fall back to defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AppSettings {
    pub hw: HwSettings,
    pub updates: UpdateSettings,
    pub cache: CacheSettings,
    pub general: GeneralSettings,
}

/// Which hardware path failed in an export. Mirrors the `stage` of `loopcut_export`'s fallback events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FallbackStage {
    /// A hardware decode chain failed on one source range; that range restarted on the CPU.
    Decode,
    /// A hardware video encoder failed; the whole encode restarted on the CPU encoder.
    Encode,
}

impl FallbackStage {
    /// Parses the stage name the exporter sends. Unknown names give `None`, so they are never shown as a real stage.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "decode" => Some(Self::Decode),
            "encode" => Some(Self::Encode),
            _ => None,
        }
    }
}

/// One event on the `export_start` channel, per docs/ipc.md.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ExportEvent {
    /// Output frames done so far and the bytes written to the partial file.
    Progress {
        frames_done: u64,
        frames_total: u64,
        bytes: u64,
    },
    /// A hardware path failed and was replaced by its CPU path.
    Fallback {
        stage: FallbackStage,
        from: String,
        to: String,
        reason: String,
    },
    /// A size-target search pass finished: `settings` names what it tried and `bytes` is the size it reached.
    TargetPass {
        pass: u32,
        settings: String,
        bytes: u64,
    },
    /// The output file is complete. `frames` counts the frames in the file, which GIF merging can make fewer than the
    /// plan's.
    Done {
        path: String,
        bytes: u64,
        seconds: f64,
        frames: u64,
    },
    /// The export failed or was cancelled. The partial file has been removed.
    Error { error: AppError },
}

/// Whether a LUT is one-dimensional or three-dimensional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum LutKind {
    #[serde(rename = "1d")]
    OneD,
    #[serde(rename = "3d")]
    ThreeD,
}

/// Result of `lut_inspect`: what a `.cube` file holds, checked before a project refers to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LutInfo {
    /// The file that was read.
    pub path: String,
    /// The `TITLE` line of the file, if it has one.
    pub title: Option<String>,
    /// Grid size per axis.
    pub size: u32,
    pub kind: LutKind,
}

/// Result of `cache_usage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CacheUsage {
    pub bytes: u64,
    pub files: u64,
    pub path: String,
}

/// Result of `app_info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppInfo {
    pub version: String,
    pub ffmpeg_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub webview2_version: Option<String>,
    pub log_dir: String,
    pub data_dir: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_with_camel_case_keys() {
        let json = serde_json::to_value(AppSettings::default()).expect("serialize");
        assert_eq!(json["cache"]["maxDiskGb"], 10.0);
        assert_eq!(json["general"]["recentLimit"], 10);
        assert_eq!(json["updates"]["checkOnStartup"], true);
        assert_eq!(json["hw"]["enabled"], true);
        let back: AppSettings = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, AppSettings::default());
    }

    #[test]
    fn partial_settings_fill_in_defaults() {
        let back: AppSettings =
            serde_json::from_str(r#"{"cache":{"maxDiskGb":4}}"#).expect("deserialize");
        assert_eq!(back.cache.max_disk_gb, 4.0);
        assert_eq!(back.cache.ram_budget_mb, 2048);
        assert_eq!(back.general.recent_limit, 10);
    }

    #[test]
    fn proxy_events_are_tagged() {
        let json = serde_json::to_value(ProxyEvent::Progress { value: 0.5 }).expect("serialize");
        assert_eq!(json, serde_json::json!({"kind": "progress", "value": 0.5}));
        let done = serde_json::to_value(ProxyEvent::Done { url: "u".into() }).expect("serialize");
        assert_eq!(done, serde_json::json!({"kind": "done", "url": "u"}));
    }

    #[test]
    fn app_info_omits_missing_webview_version() {
        let info = AppInfo {
            version: "0.0.1".into(),
            ffmpeg_version: "n9".into(),
            webview2_version: None,
            log_dir: "l".into(),
            data_dir: "d".into(),
        };
        let json = serde_json::to_value(info).expect("serialize");
        assert!(json.get("webview2Version").is_none());
        assert_eq!(json["ffmpegVersion"], "n9");
    }
}

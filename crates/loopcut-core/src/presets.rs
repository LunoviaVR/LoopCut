//! Platform export presets, shipped as versioned data.
//!
//! Platform limits change often and several are only documented second-hand, so every preset records when it was
//! last checked and whether a primary source confirmed it. The UI shows "unverified" for presets without one, and
//! limits that could not be confirmed are deliberately conservative.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::project::ExportFormat;

/// Bump when any preset value changes, so the UI can tell users the preset list was updated.
pub const PRESETS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlatformPreset {
    pub id: String,
    pub name: String,
    /// Format the platform handles best (several platforms convert GIFs to MP4 on upload).
    pub format: ExportFormat,
    /// Upload size limit in bytes (decimal megabytes, as platforms state them).
    pub max_bytes: Option<u64>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_frames: Option<u32>,
    pub max_fps: Option<u32>,
    /// Suggested output width for a good quality/size balance.
    pub recommended_width: u32,
    pub recommended_fps: u32,
    /// True only when a primary source (the platform's own documentation) confirmed the limits.
    pub verified: bool,
    /// ISO date the values were last checked.
    pub checked_on: String,
    pub source: Option<String>,
    /// Short user-facing note (plan differences, conversions).
    pub note: String,
}

const CHECKED_ON: &str = "2026-10-10";
const MB: u64 = 1_000_000;

#[allow(clippy::too_many_arguments)]
fn preset(
    id: &str,
    name: &str,
    format: ExportFormat,
    max_bytes: Option<u64>,
    max_size: Option<(u32, u32)>,
    max_frames: Option<u32>,
    max_fps: Option<u32>,
    recommended: (u32, u32),
    verified: bool,
    source: Option<&str>,
    note: &str,
) -> PlatformPreset {
    PlatformPreset {
        id: id.into(),
        name: name.into(),
        format,
        max_bytes,
        max_width: max_size.map(|s| s.0),
        max_height: max_size.map(|s| s.1),
        max_frames,
        max_fps,
        recommended_width: recommended.0,
        recommended_fps: recommended.1,
        verified,
        checked_on: CHECKED_ON.into(),
        source: source.map(Into::into),
        note: note.into(),
    }
}

/// The built-in platform presets.
pub fn platform_presets() -> Vec<PlatformPreset> {
    use ExportFormat::*;
    vec![
        preset(
            "discord",
            "Discord",
            Gif,
            Some(10 * MB),
            None,
            None,
            Some(50),
            (480, 20),
            false,
            None,
            "Conservative limit for free accounts; Nitro plans allow larger uploads.",
        ),
        preset(
            "discord-nitro",
            "Discord Nitro Basic",
            Gif,
            Some(50 * MB),
            None,
            None,
            Some(50),
            (640, 25),
            false,
            None,
            "Nitro Basic upload limit as reported by press; check your plan.",
        ),
        preset(
            "x",
            "X (Twitter)",
            Gif,
            Some(15 * MB),
            Some((1280, 1080)),
            Some(350),
            Some(50),
            (640, 20),
            true,
            Some(
                "https://developer.x.com/en/docs/x-api/v1/media/upload-media/uploading-media/media-best-practices",
            ),
            "X converts GIFs to MP4 on upload; at most 350 frames.",
        ),
        preset(
            "telegram",
            "Telegram",
            Mp4H264,
            Some(50 * MB),
            None,
            None,
            Some(60),
            (720, 30),
            false,
            Some("https://core.telegram.org/bots/api#sendanimation"),
            "Telegram turns GIFs into silent MP4 loops; MP4 keeps more quality.",
        ),
        preset(
            "whatsapp",
            "WhatsApp",
            Mp4H264,
            Some(16 * MB),
            None,
            None,
            Some(30),
            (640, 30),
            false,
            Some("https://developers.facebook.com/docs/whatsapp/cloud-api/reference/media"),
            "WhatsApp plays short H.264 MP4 loops as GIFs.",
        ),
        preset(
            "github",
            "GitHub (README, issues)",
            Gif,
            Some(10 * MB),
            None,
            None,
            Some(50),
            (800, 15),
            false,
            Some(
                "https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files",
            ),
            "Images and GIFs up to 10 MB in issues, pull requests and READMEs.",
        ),
        preset(
            "slack",
            "Slack",
            Gif,
            None,
            None,
            None,
            Some(50),
            (480, 20),
            false,
            None,
            "No practical size limit; smaller GIFs load faster in channels.",
        ),
        preset(
            "reddit",
            "Reddit",
            Gif,
            Some(8 * MB),
            None,
            None,
            Some(50),
            (640, 20),
            false,
            None,
            "Conservative limit; Reddit may convert large GIFs to video.",
        ),
        preset(
            "giphy",
            "GIPHY",
            Gif,
            Some(8 * MB),
            None,
            None,
            Some(50),
            (480, 20),
            false,
            None,
            "Conservative limit; 480 px is GIPHY's common rendition width.",
        ),
        preset(
            "email",
            "Email",
            Gif,
            Some(5 * MB),
            None,
            None,
            Some(25),
            (480, 15),
            false,
            None,
            "Keeps messages small enough for most mail servers.",
        ),
    ]
}

/// Looks up a preset by id.
pub fn find_preset(id: &str) -> Option<PlatformPreset> {
    platform_presets().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_values_sane() {
        let presets = platform_presets();
        let mut ids: Vec<_> = presets.iter().map(|p| p.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
        for p in &presets {
            assert!(
                p.recommended_width >= 160 && p.recommended_width % 2 == 0,
                "{}",
                p.id
            );
            assert!(p.recommended_fps > 0 && p.recommended_fps <= 60, "{}", p.id);
            if let Some(max_fps) = p.max_fps {
                assert!(p.recommended_fps <= max_fps, "{}", p.id);
            }
            assert!(
                !p.verified || p.source.is_some(),
                "verified presets need a source: {}",
                p.id
            );
        }
    }

    #[test]
    fn lookup() {
        assert_eq!(find_preset("x").map(|p| p.max_frames), Some(Some(350)));
        assert!(find_preset("nope").is_none());
    }
}

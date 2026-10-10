//! Network-location detection that does not touch the location.
//!
//! Opening a file on a network share contacts another computer, so LoopCut decides whether a path is remote before it
//! canonicalizes it (canonicalization itself reads the file system). Two signals count: UNC syntax (`\\server\share`,
//! including the verbatim `\\?\UNC\` form) and a drive letter whose drive type is `DRIVE_REMOTE` (a mapped network
//! drive). The check runs on the raw path and again on the canonical path, because a local path can resolve to a share.

use std::path::{Component, Path, Prefix};

use loopcut_media::runner::is_unc_path;

use crate::error::{AppError, code};

/// `DRIVE_REMOTE` from `winbase.h`: a network drive. Defined here so no extra windows feature is needed for the value.
#[cfg(windows)]
const DRIVE_REMOTE: u32 = 4;

/// True when `path` names a network location: UNC syntax, or a drive letter mapped to a network share.
///
/// Does not read the file or contact the network: UNC detection is a string check and the drive type comes from the
/// operating system's drive table.
pub fn is_network_path(path: &Path) -> bool {
    is_unc_path(path) || mapped_drive_is_remote(path)
}

/// True for the only path forms LoopCut accepts from projects, dialogs and the command line: an absolute drive-letter
/// path (`C:\clips\a.mp4`, or with forward slashes) or a UNC path (`\\server\share\a.mp4`).
///
/// Device namespaces (`\\.\`, `\\?\`, including the verbatim `\\?\UNC\` and `\\?\C:\` forms and `\\?\GLOBALROOT`) are
/// refused, and so is anything relative. Project files are untrusted input: a clip path that fails this check is
/// reported as missing and never touched.
pub fn has_accepted_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    let is_sep = |b: u8| b == b'\\' || b == b'/';
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && is_sep(bytes[2]) {
        return true;
    }
    if bytes.len() >= 3 && is_sep(bytes[0]) && is_sep(bytes[1]) {
        // The server name must follow directly; `.` and `?` start the device namespaces.
        return !matches!(bytes[2], b'.' | b'?') && !is_sep(bytes[2]);
    }
    false
}

/// The root of a drive-letter path (`Z:\` for `Z:\clips\a.mp4`), or `None` for anything else.
fn drive_root(path: &Path) -> Option<String> {
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(format!("{}:\\", char::from(letter)))
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(windows)]
fn mapped_drive_is_remote(path: &Path) -> bool {
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows::core::PCWSTR;

    let Some(root) = drive_root(path) else {
        return false;
    };
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call, and GetDriveTypeW only reads it.
    let kind = unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) };
    kind == DRIVE_REMOTE
}

#[cfg(not(windows))]
fn mapped_drive_is_remote(_path: &Path) -> bool {
    false
}

/// Refuses a network location unless the user has confirmed it. Every given path is checked; any one on a network
/// share needs the confirmation.
///
/// # Errors
///
/// `network_path` when a path is remote and `confirmed` is false.
pub fn require_confirmation(confirmed: bool, locations: &[&Path]) -> Result<(), AppError> {
    if confirmed || !locations.iter().any(|path| is_network_path(path)) {
        return Ok(());
    }
    Err(AppError::new(
        code::NETWORK_PATH,
        "This file is on a network share. Confirm to open it: reading it contacts another computer.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_paths_are_network_locations() {
        assert!(is_network_path(Path::new(r"\\nas\media\clip.mp4")));
        assert!(is_network_path(Path::new(r"\\?\UNC\nas\media\clip.mp4")));
        assert!(is_network_path(Path::new("//nas/media/clip.mp4")));
        assert!(!is_network_path(Path::new(r"\\.\C:\device")));
    }

    #[test]
    fn drive_roots_come_from_drive_letters_only() {
        assert_eq!(
            drive_root(Path::new(r"Z:\clips\a.mp4")).as_deref(),
            Some(r"Z:\")
        );
        assert_eq!(
            drive_root(Path::new(r"\\?\Z:\clips\a.mp4")).as_deref(),
            Some(r"Z:\")
        );
        if cfg!(windows) {
            assert_eq!(drive_root(Path::new("relative\\a.mp4")), None);
        }
    }

    #[test]
    fn confirmation_is_required_for_any_network_location() {
        let unc = Path::new(r"\\nas\media\clip.mp4");
        let local = Path::new(r"C:\videos\clip.mp4");
        let error = require_confirmation(false, &[local, unc]).expect_err("refused");
        assert_eq!(error.code, "network_path");
        assert!(require_confirmation(true, &[local, unc]).is_ok());
        assert!(require_confirmation(false, &[local]).is_ok());
    }

    #[test]
    fn only_drive_letter_and_unc_prefixes_are_accepted() {
        for ok in [
            r"C:\clips\a.mp4",
            "d:/clips/a.mp4",
            r"\\nas\media\a.mp4",
            "//nas/media/a.mp4",
        ] {
            assert!(has_accepted_prefix(ok), "{ok}");
        }
        for refused in [
            r"\\.\C:\device",
            r"\\?\C:\verbatim.mp4",
            r"\\?\UNC\nas\share\a.mp4",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\a.mp4",
            r"\\\nas\share",
            r"clips\a.mp4",
            r"C:clips\a.mp4",
            "C:",
            "",
            "/etc/passwd",
        ] {
            assert!(!has_accepted_prefix(refused), "{refused}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_fixed_local_drive_is_not_remote() {
        // The system drive is always local on the machines LoopCut targets.
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
        let root = format!("{system}\\clip.mp4");
        assert!(!mapped_drive_is_remote(Path::new(&root)));
    }
}

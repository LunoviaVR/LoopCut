//! Colour commands: inspecting a `.cube` LUT before a project refers to it.

use loopcut_render::lut::load_cube;

use crate::commands::{blocking, existing_local_file};
use crate::dto::{LutInfo, LutKind};
use crate::error::{AppError, io_error};

/// Largest `.cube` file read. Real 3D LUTs are a few megabytes; anything bigger is not a LUT anyone wrote.
pub const MAX_LUT_BYTES: u64 = 32 * 1024 * 1024;

/// Parses a `.cube` file and reports what it holds (title, grid size, 1D or 3D), so the UI can show it before the
/// project's grade refers to it. Network locations are refused with `network_path`.
///
/// # Errors
///
/// `invalid_argument` with the parse problem for a malformed file, `not_found` or `io` for a file that cannot be read.
#[tauri::command]
pub async fn lut_inspect(path: String) -> Result<LutInfo, AppError> {
    blocking(move || inspect(&path)).await
}

fn inspect(raw: &str) -> Result<LutInfo, AppError> {
    let path = existing_local_file(raw, "LUT file")?;
    let bytes = std::fs::metadata(&path)
        .map(|meta| meta.len())
        .map_err(|err| io_error(&err, "The LUT could not be read"))?;
    check_size(bytes)?;
    let lut = load_cube(&path).map_err(|err| AppError::invalid_argument(err.to_string()))?;
    Ok(LutInfo {
        path: path.to_string_lossy().into_owned(),
        title: lut.title().map(str::to_owned),
        size: lut.size(),
        kind: if lut.is_3d() {
            LutKind::ThreeD
        } else {
            LutKind::OneD
        },
    })
}

/// Refuses a LUT file over [`MAX_LUT_BYTES`] before it is read.
fn check_size(bytes: u64) -> Result<(), AppError> {
    if bytes > MAX_LUT_BYTES {
        return Err(AppError::invalid_argument(format!(
            "The LUT file is {bytes} bytes; the limit is {MAX_LUT_BYTES}."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_luts_are_refused_before_parsing() {
        assert!(check_size(MAX_LUT_BYTES).is_ok());
        assert_eq!(
            check_size(MAX_LUT_BYTES + 1).err().map(|e| e.code),
            Some("invalid_argument".to_owned())
        );
    }

    #[test]
    fn missing_luts_report_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone.cube");
        let err = inspect(&missing.to_string_lossy()).expect_err("missing");
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn device_and_url_lut_paths_are_refused() {
        for raw in [r"\\?\GLOBALROOT\x.cube", "https://example.com/a.cube", ""] {
            assert_eq!(
                inspect(raw).err().map(|e| e.code),
                Some("invalid_argument".to_owned()),
                "{raw:?}"
            );
        }
    }
}

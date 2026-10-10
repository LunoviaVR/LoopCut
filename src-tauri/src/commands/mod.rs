//! Tauri commands, grouped by area. Every command is `async` so that no work runs on the main thread, and every piece
//! of blocking work (probing, file I/O, FFmpeg) runs through [`blocking`].

pub mod app;
pub mod captions;
pub mod color;
pub mod export;
pub mod hw;
pub mod media;
pub mod preview;
pub mod project;
pub mod settings;

use std::path::PathBuf;

use crate::error::{AppError, code, io_error};
use crate::netpath::{has_accepted_prefix, require_confirmation};

/// Runs `work` on Tauri's blocking pool and returns its result. A panic or cancelled task becomes an `io` error.
pub async fn blocking<T, F>(work: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result,
        Err(err) => Err(AppError::new(
            code::IO,
            format!("a background task failed: {err}"),
        )),
    }
}

/// A local file named by the webview (a LUT, a subtitle file). The path must be a drive-letter or UNC path, must not
/// be a URL, and must name an existing file. A network location is refused with `network_path`: reading it contacts
/// another computer, and these commands take no confirmation.
pub(crate) fn existing_local_file(raw: &str, what: &str) -> Result<PathBuf, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument(format!("No {what} was given.")));
    }
    if loopcut_media::runner::is_url_like(trimmed) || !has_accepted_prefix(trimmed) {
        return Err(AppError::invalid_argument(format!(
            "The {what} must be a local path with a drive letter or a UNC share."
        )));
    }
    let path = PathBuf::from(trimmed);
    require_confirmation(false, &[path.as_path()])?;
    let meta =
        std::fs::metadata(&path).map_err(|err| io_error(&err, "The file could not be opened"))?;
    if !meta.is_file() {
        return Err(AppError::invalid_argument(format!(
            "That path is a folder, not a {what}."
        )));
    }
    Ok(path)
}

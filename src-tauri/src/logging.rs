//! Backend logging: every `tracing` event from every crate is written to `<logs>/loopcut.log`.
//!
//! The file is truncated at startup once it exceeds [`MAX_LOG_BYTES`], which bounds the disk use of a long-lived
//! install without rotating files. Frontend messages keep going through `tauri-plugin-log`. The subscriber is installed
//! with `set_global_default` rather than `try_init`, because `try_init` would also try to claim the `log` crate, which
//! `tauri-plugin-log` has already done.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tracing_subscriber::EnvFilter;

/// Name of the log file inside the log folder.
pub const LOG_FILE: &str = "loopcut.log";
/// A log larger than this is truncated when LoopCut starts.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// Environment variable that overrides the default filter (for example `LOOPCUT_LOG=debug`).
pub const FILTER_ENV: &str = "LOOPCUT_LOG";

/// A file shared by every writer of the subscriber.
#[derive(Clone)]
struct SharedFile(Arc<Mutex<File>>);

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().flush()
    }
}

/// Opens the log file for appending, or truncates it first when it is over the size limit.
fn open_log(path: &Path) -> io::Result<File> {
    let oversized = fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES);
    if oversized {
        File::create(path)
    } else {
        OpenOptions::new().create(true).append(true).open(path)
    }
}

/// Installs the global subscriber, writing to `<logs>/loopcut.log`. Returns the log file path.
///
/// # Errors
///
/// A message when the folder or the file cannot be used, or when a global subscriber is already installed. The app
/// keeps running without a log file in that case and reports the message as a startup warning.
pub fn init(logs: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(logs).map_err(|err| format!("log folder {}: {err}", logs.display()))?;
    let path = logs.join(LOG_FILE);
    let file = open_log(&path).map_err(|err| format!("log file {}: {err}", path.display()))?;
    let writer = SharedFile(Arc::new(Mutex::new(file)));

    let filter = EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .with_env_var(FILTER_ENV)
        .from_env_lossy();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|err| format!("logging was already set up: {err}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_logs_are_truncated_and_small_ones_are_appended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(LOG_FILE);

        fs::write(&path, b"small").expect("seed");
        let mut file = open_log(&path).expect("open small");
        file.write_all(b"+more").expect("append");
        drop(file);
        assert_eq!(fs::read(&path).expect("read"), b"small+more");

        let big = vec![b'x'; (MAX_LOG_BYTES + 1) as usize];
        fs::write(&path, &big).expect("seed big");
        let file = open_log(&path).expect("open big");
        drop(file);
        assert_eq!(fs::metadata(&path).expect("meta").len(), 0);
    }
}

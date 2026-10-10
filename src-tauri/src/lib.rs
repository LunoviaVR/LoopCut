//! LoopCut desktop app (Tauri shell): plugins, app state, commands, custom protocols and startup.
//!
//! Startup order matters: the single-instance plugin is registered first so that a second launch (for example a
//! double-clicked `.loopcut` file) is forwarded to the running window instead of starting another process.
//!
//! Startup never aborts on a bad folder. Unusable app folders, a corrupt settings file or a frame cache that cannot be
//! created are recorded as startup warnings (shown by "Copy diagnostics"), and the app falls back to temporary
//! folders or RAM-only caching.

mod commands;
mod dto;
mod error;
mod logging;
mod netpath;
mod protocol;
mod recent;
mod recovery;
mod render;
mod scan;
mod settings;
mod spike;
mod state;
mod storage;
mod thumbcache;
mod tokens;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use http::{Request, Response, StatusCode};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

use crate::dto::AppSettings;
use crate::state::{
    AppDirs, AppState, GIB, MIB, first_openable_arg, first_openable_arg_in, resolve_ffmpeg,
    temp_root,
};

/// Event sent to the webview when a second instance forwards a file path.
const OPEN_PATH_EVENT: &str = "app://open-path";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first so a second launch is forwarded to the running instance.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            forward_open_path(app, argv, &cwd)
        }))
        .plugin(frontend_log())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol("loopcut", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let response = match managed_state(&app) {
                    Some(state) => protocol::handle_loopcut(&state, &request),
                    None => starting(&request),
                };
                responder.respond(response);
            });
        })
        .register_asynchronous_uri_scheme_protocol("loopcut-media", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let response = match managed_state(&app) {
                    Some(state) => protocol::handle_media(&state, &request),
                    None => starting(&request),
                };
                responder.respond(response);
            });
        })
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            commands::media::media_import,
            commands::media::media_thumbnails,
            commands::media::media_proxy_start,
            commands::media::media_release,
            commands::project::project_new,
            commands::project::project_open,
            commands::project::project_save,
            commands::project::project_autosave,
            commands::project::project_recovery,
            commands::project::project_recovery_clear,
            commands::project::recent_projects,
            commands::project::sequence_plan,
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::cache_usage,
            commands::settings::cache_clear,
            commands::app::app_info,
            commands::app::app_diagnostics,
            commands::app::app_prepare_update,
            commands::app::app_resume_background,
            commands::app::app_open_path_arg,
            commands::hw::hw_capabilities,
            commands::hw::hw_probe,
            commands::preview::preview_set_project,
            commands::preview::preview_frame,
            commands::preview::preview_play,
            commands::preview::preview_stop,
            commands::preview::preview_status,
            commands::export::export_presets,
            commands::export::export_estimate,
            commands::export::export_start,
            commands::export::export_cancel,
            commands::color::lut_inspect,
            commands::captions::captions_import_srt,
            commands::captions::fonts_list,
            spike::spike_flags,
            spike::spike_report,
            #[cfg(debug_assertions)]
            spike::bench::bench_channel,
            #[cfg(debug_assertions)]
            spike::bench::bench_invoke_frame,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LoopCut");
}

/// Frontend and `log`-crate messages. The file is `frontend.log`, not the product name: Windows file names are
/// case-insensitive, and the product name (`LoopCut.log`) would be the same file as the backend's `loopcut.log`.
fn frontend_log() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_log::log::LevelFilter;

    let builder = tauri_plugin_log::Builder::new()
        .clear_targets()
        .target(Target::new(TargetKind::Stdout))
        .target(Target::new(TargetKind::LogDir {
            file_name: Some("frontend".to_owned()),
        }));
    // Echoing backend records into the webview console is a development aid only.
    #[cfg(debug_assertions)]
    let builder = builder.target(Target::new(TargetKind::Webview));
    builder
        // The plugin defaults to TRACE, and wgpu logs every GPU call through the `log` crate at TRACE/DEBUG: that
        // floods the file (tens of MB per minute), the console and the IPC channel. Keep GPU internals at WARN.
        .level(LevelFilter::Info)
        .level_for("wgpu", LevelFilter::Warn)
        .level_for("wgpu_core", LevelFilter::Warn)
        .level_for("wgpu_hal", LevelFilter::Warn)
        .level_for("naga", LevelFilter::Warn)
        // The frontend log rotates at 10 MB and keeps the three most recent rotated files.
        .max_file_size(FRONTEND_LOG_MAX_BYTES)
        .rotation_strategy(RotationStrategy::KeepSome(FRONTEND_LOG_KEPT))
        .build()
}

/// Size at which `frontend.log` rotates.
const FRONTEND_LOG_MAX_BYTES: u128 = 10 * 1024 * 1024;
/// Rotated `frontend.log` files kept besides the current one.
const FRONTEND_LOG_KEPT: usize = 3;

/// The shared state, once setup has run. Protocol requests that arrive earlier are answered with 503.
fn managed_state(app: &AppHandle) -> Option<Arc<AppState>> {
    app.try_state::<Arc<AppState>>()
        .map(|state| Arc::clone(state.inner()))
}

fn starting<B>(request: &Request<B>) -> Response<Vec<u8>> {
    protocol::respond_bytes(
        request,
        StatusCode::SERVICE_UNAVAILABLE,
        "text/plain",
        b"starting".to_vec(),
    )
}

/// Creates the folders, starts logging, loads settings, opens the frame store and locates FFmpeg, then registers the
/// shared state. Problems are collected as startup warnings instead of stopping the app.
fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let mut warnings = Vec::new();

    let dirs = match AppDirs::resolve(&handle) {
        Ok(dirs) => dirs,
        Err(err) => {
            warnings.push(format!(
                "the app folders could not be resolved ({err}); using a temporary folder"
            ));
            AppDirs::under_root(&temp_root())
        }
    };
    if let Err(err) = dirs.create_all() {
        warnings.push(format!("an app folder could not be created: {err}"));
    }
    if let Err(problem) = logging::init(&dirs.logs) {
        warnings.push(format!("logging to file is off: {problem}"));
    }
    install_panic_hook(dirs.logs.clone());
    for warning in &warnings {
        tracing::warn!("{warning}");
    }

    let swept = storage::sweep_partials(&dirs.cache);
    if swept > 0 {
        tracing::info!(swept, "removed stale partial files from the cache");
    }

    let defaults = settings::defaults_for_ram(settings::total_ram_bytes());
    let (loaded, warning) = settings::load(&dirs.settings_file(), defaults);
    if let Some(note) = warning {
        tracing::warn!("{note}");
        warnings.push(note);
    }

    let framestore = open_framestore(&loaded, &dirs, &mut warnings);
    let ffmpeg = resolve_ffmpeg(&handle);
    if let Err(problem) = &ffmpeg {
        tracing::error!(%problem, "FFmpeg could not be located; media commands will report it until it is restored");
    }

    let state = Arc::new(AppState::new(
        dirs,
        loaded,
        ffmpeg,
        framestore,
        warnings,
        // Lossy conversion keeps a launch with an odd argument working; the path is then checked like any other.
        first_openable_arg(std::env::args_os().map(|arg| arg.to_string_lossy().into_owned())),
    ));
    if let Err(err) = commands::hw::spawn_startup(Arc::clone(&state)) {
        tracing::warn!(error = %err, "the hardware startup thread could not start; hardware acceleration stays off");
    }
    app.manage(state);
    Ok(())
}

/// Opens the frame store in the configured cache folder. If that fails, it uses the temp folder with RAM only (no disk
/// spill). If even that fails, there is no frame store and the warning says so.
fn open_framestore(
    settings: &AppSettings,
    dirs: &AppDirs,
    warnings: &mut Vec<String>,
) -> Option<Arc<FrameStore>> {
    let ram_budget_bytes = settings.cache.ram_budget_mb * MIB;
    let disk_budget_bytes = (settings.cache.max_disk_gb * GIB as f64) as u64;
    let configured = FrameStoreConfig {
        ram_budget_bytes,
        disk_budget_bytes,
        spill_dir: dirs.frames.clone(),
    };
    match FrameStore::new(configured) {
        Ok(store) => return Some(Arc::new(store)),
        Err(err) => warnings.push(format!(
            "the frame cache folder could not be used ({err}); frames stay in RAM under the temp folder"
        )),
    }
    let fallback = FrameStoreConfig {
        ram_budget_bytes,
        disk_budget_bytes: 0,
        spill_dir: temp_root().join("frames"),
    };
    match FrameStore::new(fallback) {
        Ok(store) => Some(Arc::new(store)),
        Err(err) => {
            warnings.push(format!(
                "no frame store could be created ({err}); frame caching is off"
            ));
            None
        }
    }
}

/// Writes a crash report (message, location, backtrace) to the log folder, then runs the previous hook.
fn install_panic_hook(logs: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "LoopCut {} crashed at {}\n{info}\n\n{backtrace}\n",
            env!("CARGO_PKG_VERSION"),
            storage::now_iso8601()
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = std::fs::create_dir_all(&logs);
        let _ = std::fs::write(logs.join(format!("crash-{stamp}.txt")), report);
        previous(info);
    }));
}

/// A second instance was launched with arguments: focus the window and forward any openable file path. Relative
/// paths are resolved against the second instance's working directory.
fn forward_open_path(app: &AppHandle, argv: Vec<String>, cwd: &str) {
    if let Some(path) = first_openable_arg_in(argv, Path::new(cwd))
        && let Err(err) = app.emit(OPEN_PATH_EVENT, path)
    {
        tracing::warn!(error = %err, "could not forward the file path to the running window");
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

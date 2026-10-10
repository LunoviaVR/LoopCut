//! LoopCut desktop app (Tauri shell): plugins, app state, commands and custom protocols.

mod protocol;
mod spike;

use http::StatusCode;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first so a second launch (e.g. double-clicking a .loopcut file) is
        // forwarded to the running instance.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .register_asynchronous_uri_scheme_protocol("loopcut", |_ctx, request, responder| {
            std::thread::spawn(move || {
                let path = protocol::decoded_path(&request);
                let response = match spike::handle_bench_request(&path) {
                    Some(bytes) => protocol::respond_bytes(&request, StatusCode::OK, "application/octet-stream", bytes),
                    None => protocol::not_found(&request),
                };
                responder.respond(response);
            });
        })
        .invoke_handler(tauri::generate_handler![
            spike::spike_flags,
            spike::bench_channel,
            spike::bench_invoke_frame,
            spike::spike_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LoopCut");
}

//! Custom URI scheme protocols.
//!
//! On Windows the webview reaches these as `http://<scheme>.localhost/...` (cross-origin to the app
//! page), so every response carries CORS headers for the app's own origins only.

use http::{Request, Response, StatusCode, header};

/// Origins allowed to read protocol responses: the bundled app and the Vite dev server.
const ALLOWED_ORIGINS: &[&str] = &["http://tauri.localhost", "https://tauri.localhost", "http://localhost:1420", "tauri://localhost"];

pub fn cors_origin<B>(request: &Request<B>) -> &'static str {
    let origin = request.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    ALLOWED_ORIGINS.iter().copied().find(|o| *o == origin).unwrap_or(ALLOWED_ORIGINS[0])
}

pub fn respond_bytes<B>(request: &Request<B>, status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, cors_origin(request))
        .header(header::VARY, "Origin")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

pub fn not_found<B>(request: &Request<B>) -> Response<Vec<u8>> {
    respond_bytes(request, StatusCode::NOT_FOUND, "text/plain", b"not found".to_vec())
}

/// Percent-decodes the request path (convertFileSrc encodes `/` as `%2F`).
pub fn decoded_path<B>(request: &Request<B>) -> String {
    percent_encoding::percent_decode_str(request.uri().path()).decode_utf8_lossy().into_owned()
}

//! Custom URI scheme protocols.
//!
//! * `loopcut-media`: `src/<token>` serves the original file and `proxy/<token>` the timeline proxy, both with exact
//!   HTTP Range support. Each response carries at most [`MAX_SLICE_BYTES`], so memory stays bounded and `<video>`
//!   simply asks for the next slice.
//! * `loopcut`: `thumb/<token>/<height>/<index>` returns one filmstrip frame as raw RGBA with `x-width`/`x-height`.
//!   `x-height` is the real height of the set, which can be one pixel less than requested (odd heights round down).
//!
//! On Windows the webview reaches these as `http://<scheme>.localhost/…` (cross-origin to the app page), so every
//! response carries CORS headers for the app's own origins only. Tokens are the only identifiers accepted; a
//! filesystem path in a URL is never read. A token serves media only while it is live, and tokens are minted only for
//! media that was probed successfully.
//!
//! Status codes: 404 for an unknown, released or malformed token, an unknown route, or a missing proxy or thumbnail;
//! 405 (with `Allow`) for methods other than GET, HEAD and OPTIONS; 416 for a Range that is unsatisfiable or malformed.
//! The malformed case is a deliberate choice: RFC 9110 lets a server ignore a Range it does not understand and send the
//! whole body, but a source can be many gigabytes, so LoopCut refuses instead. Error bodies are the status reason text.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use http::{Method, Request, Response, StatusCode, header};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::state::{AppState, MediaState};
use crate::thumbcache::{ThumbKey, normalized_height};
use crate::tokens::is_token;

/// Largest body served for one request. A Range asking for more gets a shorter 206 that says exactly what was sent.
pub const MAX_SLICE_BYTES: u64 = 4 * 1024 * 1024;

/// Origins allowed to read protocol responses: the bundled app. The first entry is the default for other origins.
const APP_ORIGINS: &[&str] = &[
    "http://tauri.localhost",
    "https://tauri.localhost",
    "tauri://localhost",
];

/// The Vite dev server origin. Only debug builds accept it; a release build never trusts a local dev server.
#[cfg(debug_assertions)]
const DEV_ORIGINS: &[&str] = &["http://localhost:1420"];
/// Release builds accept no development origin.
#[cfg(not(debug_assertions))]
const DEV_ORIGINS: &[&str] = &[];

/// Every origin whose requests get a CORS echo: the app's origins, plus the dev server in debug builds.
fn allowed_origins() -> impl Iterator<Item = &'static str> {
    APP_ORIGINS.iter().chain(DEV_ORIGINS.iter()).copied()
}

/// Characters `encodeURIComponent` leaves alone. Tauri's `convertFileSrc` uses it, so URLs built here must match.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// Builds the URL the webview uses for a custom scheme, mirroring Tauri's `convertFileSrc` (Windows uses the
/// `http://<scheme>.localhost` form). `path` is encoded as a whole, so `src/<token>` becomes `src%2F<token>`.
pub fn protocol_url(scheme: &str, path: &str) -> String {
    let encoded = utf8_percent_encode(path, URI_COMPONENT).to_string();
    if cfg!(windows) {
        format!("http://{scheme}.localhost/{encoded}")
    } else {
        format!("{scheme}://localhost/{encoded}")
    }
}

pub fn cors_origin<B>(request: &Request<B>) -> &'static str {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    allowed_origins()
        .find(|o| *o == origin)
        .unwrap_or(APP_ORIGINS[0])
}

pub fn respond_bytes<B>(
    request: &Request<B>,
    status: StatusCode,
    content_type: &str,
    body: Vec<u8>,
) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, cors_origin(request))
        .header(header::VARY, "Origin")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// Percent-decodes the request path. `convertFileSrc` encodes `/` as `%2F`, so decoding happens before routing.
pub fn decoded_path<B>(request: &Request<B>) -> String {
    percent_decode_str(request.uri().path())
        .decode_utf8_lossy()
        .into_owned()
}

/// Outcome of interpreting a `Range` header against a body of `len` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Send the whole body (200). Only chosen when the body fits in one slice.
    Full,
    /// Send bytes `start..=end` (206). Both bounds are inclusive and inside the body.
    Partial { start: u64, end: u64 },
    /// The range starts at or past the end of the body, or the body is empty (416).
    Unsatisfiable,
    /// The header is not a single valid `bytes=` range (416; a server cannot ignore it without sending a huge body).
    Invalid,
}

/// Parses one `Range` header (`bytes=a-b`, `bytes=a-`, `bytes=-n`) for a body of `len` bytes.
///
/// Only single ranges are supported; multi-range requests are `Invalid`. Ends past the body are clamped. Any range is
/// shortened to at most `max_slice` bytes. With no header, a body that fits in one slice is `Full`, and a larger
/// body is answered with its first slice.
pub fn parse_range(header_value: Option<&str>, len: u64, max_slice: u64) -> RangeOutcome {
    let max_slice = max_slice.max(1);
    let Some(raw) = header_value else {
        return if len <= max_slice {
            RangeOutcome::Full
        } else {
            RangeOutcome::Partial {
                start: 0,
                end: max_slice - 1,
            }
        };
    };
    let spec = raw.trim();
    let Some(ranges) = spec
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("bytes="))
        .map(|_| &spec[6..])
    else {
        return RangeOutcome::Invalid;
    };
    if ranges.contains(',') {
        return RangeOutcome::Invalid;
    }
    let Some((first, last)) = ranges.split_once('-') else {
        return RangeOutcome::Invalid;
    };
    let (first, last) = (first.trim(), last.trim());
    let (start, end) = match (first.is_empty(), last.is_empty()) {
        (true, true) => return RangeOutcome::Invalid,
        (true, false) => {
            // Suffix form: the last `n` bytes.
            let Some(n) = parse_digits(last) else {
                return RangeOutcome::Invalid;
            };
            if n == 0 || len == 0 {
                return RangeOutcome::Unsatisfiable;
            }
            (len.saturating_sub(n), len - 1)
        }
        (false, true) => {
            let Some(start) = parse_digits(first) else {
                return RangeOutcome::Invalid;
            };
            if len == 0 || start >= len {
                return RangeOutcome::Unsatisfiable;
            }
            (start, len - 1)
        }
        (false, false) => {
            let (Some(start), Some(end)) = (parse_digits(first), parse_digits(last)) else {
                return RangeOutcome::Invalid;
            };
            if end < start {
                return RangeOutcome::Invalid;
            }
            if len == 0 || start >= len {
                return RangeOutcome::Unsatisfiable;
            }
            (start, end.min(len - 1))
        }
    };
    let end = end.min(start.saturating_add(max_slice - 1));
    RangeOutcome::Partial { start, end }
}

/// Parses a non-empty run of ASCII digits, saturating on overflow (anything that large is past the end of any file).
fn parse_digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(text.bytes().fold(0u64, |acc, b| {
        acc.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    }))
}

/// Content type for a source file, from its extension. Unknown types are served as opaque bytes.
pub fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "gif" => "image/gif",
        "m4a" => "audio/mp4",
        _ => "application/octet-stream",
    }
}

/// Serves `path` honoring the request's `Range` header. HEAD gets the same headers with an empty body.
fn serve_file(
    method: &Method,
    range: Option<&str>,
    origin: &'static str,
    path: &Path,
    content_type: &str,
) -> Response<Vec<u8>> {
    let head_only = *method == Method::HEAD;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return plain(StatusCode::NOT_FOUND, origin);
        }
        Err(_) => return plain(StatusCode::INTERNAL_SERVER_ERROR, origin),
    };
    let len = match file.metadata() {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return plain(StatusCode::NOT_FOUND, origin),
    };

    let builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Accept-Ranges, Content-Length, Content-Range",
        )
        .header(header::VARY, "Origin")
        .header(header::CACHE_CONTROL, "no-store");

    match parse_range(range, len, MAX_SLICE_BYTES) {
        RangeOutcome::Full => {
            let body = if head_only {
                Ok(Vec::new())
            } else {
                read_slice(file, 0, len)
            };
            match body {
                Ok(body) => builder
                    .status(StatusCode::OK)
                    .header(header::CONTENT_LENGTH, len.to_string())
                    .body(body)
                    .unwrap_or_else(|_| Response::new(Vec::new())),
                Err(_) => plain(StatusCode::INTERNAL_SERVER_ERROR, origin),
            }
        }
        RangeOutcome::Partial { start, end } => {
            let count = end - start + 1;
            let body = if head_only {
                Ok(Vec::new())
            } else {
                read_slice(file, start, count)
            };
            match body {
                Ok(body) => builder
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
                    .header(header::CONTENT_LENGTH, count.to_string())
                    .body(body)
                    .unwrap_or_else(|_| Response::new(Vec::new())),
                Err(_) => plain(StatusCode::INTERNAL_SERVER_ERROR, origin),
            }
        }
        RangeOutcome::Unsatisfiable | RangeOutcome::Invalid => builder
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .header(header::CONTENT_LENGTH, "0")
            .body(Vec::new())
            .unwrap_or_else(|_| Response::new(Vec::new())),
    }
}

fn read_slice(mut file: File, start: u64, count: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut buf =
        vec![0u8; usize::try_from(count).map_err(|_| std::io::Error::other("slice too large"))?];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// A short plain-text response with CORS headers. The body is the status's reason text (`Not Found`, ...).
fn plain(status: StatusCode, origin: &'static str) -> Response<Vec<u8>> {
    let reason = status.canonical_reason().unwrap_or("Error");
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
        .header(header::VARY, "Origin")
        .header(header::CACHE_CONTROL, "no-store")
        .body(reason.as_bytes().to_vec())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 405 for a method these protocols do not implement, with the methods they do.
fn method_not_allowed(origin: &'static str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(header::ALLOW, "GET, HEAD, OPTIONS")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
        .header(header::VARY, "Origin")
        .body(Vec::new())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// Answers a CORS preflight for the media protocols.
fn preflight(origin: &'static str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Range")
        .header(header::ACCESS_CONTROL_MAX_AGE, "600")
        .header(header::VARY, "Origin")
        .body(Vec::new())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// What a decoded request path asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route<'a> {
    /// `/src/<token>`
    Source(&'a str),
    /// `/proxy/<token>`
    Proxy(&'a str),
    /// `/thumb/<token>/<height>/<index>`
    Thumb {
        token: &'a str,
        height: u32,
        index: usize,
    },
    /// Anything else, including malformed numbers and extra segments.
    Unknown,
}

/// Routes a percent-decoded path such as `/src/<token>`. Tokens are returned unchecked; callers validate them.
pub fn route(path: &str) -> Route<'_> {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    match segments.as_slice() {
        ["src", token] => Route::Source(token),
        ["proxy", token] => Route::Proxy(token),
        ["thumb", token, height, index] => match (height.parse::<u32>(), index.parse::<usize>()) {
            (Ok(height), Ok(index)) => Route::Thumb {
                token,
                height,
                index,
            },
            _ => Route::Unknown,
        },
        _ => Route::Unknown,
    }
}

/// Answers a request on the `loopcut-media` protocol.
pub fn handle_media(state: &AppState, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let origin = cors_origin(request);
    if *request.method() == Method::OPTIONS {
        return preflight(origin);
    }
    if *request.method() != Method::GET && *request.method() != Method::HEAD {
        return method_not_allowed(origin);
    }
    let decoded = decoded_path(request);
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    match route(&decoded) {
        Route::Source(token) if is_token(token) => match state.source_path(token) {
            Some(path) => serve_file(
                request.method(),
                range,
                origin,
                &path,
                content_type_for(&path),
            ),
            None => plain(StatusCode::NOT_FOUND, origin),
        },
        Route::Proxy(token) if is_token(token) => {
            let proxy = state.media_entry(token).and_then(|entry| {
                let guard = entry.state.lock();
                guard.proxy.clone()
            });
            match proxy {
                Some(path) => serve_file(request.method(), range, origin, &path, "video/mp4"),
                None => plain(StatusCode::NOT_FOUND, origin),
            }
        }
        _ => plain(StatusCode::NOT_FOUND, origin),
    }
}

/// Answers a request on the `loopcut` protocol.
pub fn handle_loopcut(state: &AppState, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let origin = cors_origin(request);
    if *request.method() == Method::OPTIONS {
        return preflight(origin);
    }
    if *request.method() != Method::GET && *request.method() != Method::HEAD {
        return method_not_allowed(origin);
    }
    let decoded = decoded_path(request);
    if let Some(response) = bench_response(request, &decoded) {
        return response;
    }
    match route(&decoded) {
        Route::Thumb {
            token,
            height,
            index,
        } if is_token(token) => {
            let Some(entry) = state.media_entry(token) else {
                return plain(StatusCode::NOT_FOUND, origin);
            };
            let found = {
                let guard = entry.state.lock();
                thumb_frame(&guard, height, index)
            };
            match found {
                Some((width, real_height, rgba)) => {
                    state.thumb_budget.touch(&ThumbKey {
                        token: token.to_owned(),
                        height: real_height,
                    });
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/octet-stream")
                        .header("x-width", width.to_string())
                        .header("x-height", real_height.to_string())
                        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, "x-width, x-height")
                        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
                        .header(header::VARY, "Origin")
                        .header(header::CACHE_CONTROL, "no-store")
                        .body(rgba)
                        .unwrap_or_else(|_| Response::new(Vec::new()))
                }
                None => plain(StatusCode::NOT_FOUND, origin),
            }
        }
        _ => plain(StatusCode::NOT_FOUND, origin),
    }
}

/// Debug builds keep the M0 benchmark endpoint (`bench/<w>/<h>/<i>`); release builds do not serve it.
#[cfg(debug_assertions)]
fn bench_response(request: &Request<Vec<u8>>, decoded: &str) -> Option<Response<Vec<u8>>> {
    crate::spike::bench::handle_bench_request(decoded)
        .map(|bytes| respond_bytes(request, StatusCode::OK, "application/octet-stream", bytes))
}

#[cfg(not(debug_assertions))]
fn bench_response(_request: &Request<Vec<u8>>, _decoded: &str) -> Option<Response<Vec<u8>>> {
    None
}

/// The frame at `index` of the thumbnail set for `height`: `(width, real_height, rgba)`.
///
/// The request height is normalized the way the generator rounds it, so an odd height finds the set made for its even
/// neighbour. The returned height is the set's real height, which the response reports as `x-height`.
pub fn thumb_frame(state: &MediaState, height: u32, index: usize) -> Option<(u32, u32, Vec<u8>)> {
    let wanted = normalized_height(height);
    state
        .thumbs
        .iter()
        .find(|set| set.height == wanted && index < set.frames.len())
        .map(|set| (set.width, set.height, set.frames[index].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::{sample_info, state_under};
    use crate::thumbcache::ThumbSet;
    use std::sync::Arc;

    const MAX: u64 = 4 * 1024 * 1024;

    fn partial(start: u64, end: u64) -> RangeOutcome {
        RangeOutcome::Partial { start, end }
    }

    #[test]
    fn no_header_serves_small_bodies_whole() {
        assert_eq!(parse_range(None, 0, MAX), RangeOutcome::Full);
        assert_eq!(parse_range(None, 1, MAX), RangeOutcome::Full);
        assert_eq!(parse_range(None, MAX, MAX), RangeOutcome::Full);
    }

    #[test]
    fn no_header_bounds_large_bodies() {
        assert_eq!(parse_range(None, MAX + 1, MAX), partial(0, MAX - 1));
        assert_eq!(parse_range(None, 10 * MAX, MAX), partial(0, MAX - 1));
    }

    #[test]
    fn closed_ranges() {
        assert_eq!(parse_range(Some("bytes=0-0"), 10, MAX), partial(0, 0));
        assert_eq!(parse_range(Some("bytes=2-5"), 10, MAX), partial(2, 5));
        assert_eq!(parse_range(Some("bytes=0-9"), 10, MAX), partial(0, 9));
    }

    #[test]
    fn closed_range_end_is_clamped_to_the_body() {
        assert_eq!(parse_range(Some("bytes=5-100"), 10, MAX), partial(5, 9));
        assert_eq!(
            parse_range(Some("bytes=0-18446744073709551615"), 10, MAX),
            partial(0, 9)
        );
    }

    #[test]
    fn open_ended_ranges_run_to_the_end() {
        assert_eq!(parse_range(Some("bytes=0-"), 10, MAX), partial(0, 9));
        assert_eq!(parse_range(Some("bytes=7-"), 10, MAX), partial(7, 9));
        assert_eq!(parse_range(Some("bytes=9-"), 10, MAX), partial(9, 9));
    }

    #[test]
    fn suffix_ranges_take_the_last_bytes() {
        assert_eq!(parse_range(Some("bytes=-1"), 10, MAX), partial(9, 9));
        assert_eq!(parse_range(Some("bytes=-4"), 10, MAX), partial(6, 9));
        assert_eq!(parse_range(Some("bytes=-10"), 10, MAX), partial(0, 9));
        assert_eq!(parse_range(Some("bytes=-1000"), 10, MAX), partial(0, 9));
    }

    #[test]
    fn suffix_of_zero_is_unsatisfiable() {
        assert_eq!(
            parse_range(Some("bytes=-0"), 10, MAX),
            RangeOutcome::Unsatisfiable
        );
    }

    #[test]
    fn starts_past_the_end_are_unsatisfiable() {
        assert_eq!(
            parse_range(Some("bytes=10-"), 10, MAX),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=10-20"), 10, MAX),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=99999999999999999999999-"), 10, MAX),
            RangeOutcome::Unsatisfiable
        );
    }

    #[test]
    fn empty_bodies_are_unsatisfiable_for_any_range() {
        assert_eq!(
            parse_range(Some("bytes=0-"), 0, MAX),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=0-0"), 0, MAX),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some("bytes=-5"), 0, MAX),
            RangeOutcome::Unsatisfiable
        );
    }

    #[test]
    fn reversed_ranges_are_invalid() {
        assert_eq!(
            parse_range(Some("bytes=5-3"), 10, MAX),
            RangeOutcome::Invalid
        );
    }

    #[test]
    fn multi_range_requests_are_invalid() {
        assert_eq!(
            parse_range(Some("bytes=0-1,3-4"), 10, MAX),
            RangeOutcome::Invalid
        );
        assert_eq!(
            parse_range(Some("bytes=0-1, 3-4"), 10, MAX),
            RangeOutcome::Invalid
        );
    }

    #[test]
    fn malformed_headers_are_invalid() {
        for raw in [
            "",
            "bytes",
            "bytes=",
            "bytes=-",
            "bytes=a-b",
            "bytes=1-2-3",
            "bytes=+1-2",
            "bytes=-+1",
            "bytes=1 2-3",
            "items=0-1",
            "0-1",
            "bytes==0-1",
            "bytes=1.5-2",
            "bytes=0x10-",
            "bytes=-1x",
        ] {
            assert_eq!(
                parse_range(Some(raw), 10, MAX),
                RangeOutcome::Invalid,
                "header {raw:?}"
            );
        }
    }

    #[test]
    fn unit_name_is_case_insensitive_and_whitespace_tolerant() {
        assert_eq!(parse_range(Some("BYTES=0-3"), 10, MAX), partial(0, 3));
        assert_eq!(
            parse_range(Some("  bytes= 0 - 3  "), 10, MAX),
            partial(0, 3)
        );
        assert_eq!(parse_range(Some("bytes=0 -"), 10, MAX), partial(0, 9));
    }

    #[test]
    fn long_ranges_are_shortened_to_one_slice() {
        assert_eq!(
            parse_range(Some("bytes=0-"), 10 * MAX, MAX),
            partial(0, MAX - 1)
        );
        assert_eq!(
            parse_range(Some("bytes=100-"), 10 * MAX, MAX),
            partial(100, 100 + MAX - 1)
        );
        assert_eq!(
            parse_range(Some("bytes=0-99"), 10 * MAX, MAX),
            partial(0, 99)
        );
        let suffix = format!("bytes=-{}", 2 * MAX);
        assert_eq!(
            parse_range(Some(&suffix), 10 * MAX, MAX),
            partial(8 * MAX, 9 * MAX - 1)
        );
        assert_eq!(
            parse_range(Some("bytes=-10000000000"), 10 * MAX, MAX),
            partial(0, MAX - 1)
        );
    }

    #[test]
    fn slices_never_exceed_the_limit_for_any_start() {
        let len = 3 * MAX + 17;
        for start in [0u64, 1, MAX - 1, MAX, 2 * MAX + 5, len - 1] {
            let header = format!("bytes={start}-");
            match parse_range(Some(&header), len, MAX) {
                RangeOutcome::Partial { start: s, end } => {
                    assert_eq!(s, start);
                    assert!(end - s < MAX && end < len);
                }
                other => panic!("unexpected {other:?} for {header}"),
            }
        }
    }

    #[test]
    fn routes_are_recognized() {
        assert_eq!(route("/src/abc"), Route::Source("abc"));
        assert_eq!(route("/proxy/abc"), Route::Proxy("abc"));
        assert_eq!(
            route("/thumb/abc/90/12"),
            Route::Thumb {
                token: "abc",
                height: 90,
                index: 12
            }
        );
        assert_eq!(route("src/abc"), Route::Source("abc"));
        assert_eq!(route("/src/abc/"), Route::Unknown);
        assert_eq!(route("/src/abc/extra"), Route::Unknown);
        assert_eq!(route("/thumb/abc/x/1"), Route::Unknown);
        assert_eq!(route("/thumb/abc/90/-1"), Route::Unknown);
        assert_eq!(route("/"), Route::Unknown);
        assert_eq!(route(""), Route::Unknown);
        assert_eq!(route("/src/../../secret"), Route::Unknown);
    }

    #[test]
    fn protocol_urls_match_convert_file_src() {
        let url = protocol_url("loopcut-media", "src/0123456789abcdef0123456789abcdef");
        if cfg!(windows) {
            assert_eq!(
                url,
                "http://loopcut-media.localhost/src%2F0123456789abcdef0123456789abcdef"
            );
        } else {
            assert_eq!(
                url,
                "loopcut-media://localhost/src%2F0123456789abcdef0123456789abcdef"
            );
        }
        assert_eq!(
            protocol_url("loopcut", "a b(c)!*"),
            if cfg!(windows) {
                "http://loopcut.localhost/a%20b(c)!*"
            } else {
                "loopcut://localhost/a%20b(c)!*"
            }
        );
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type_for(Path::new("a.MP4")), "video/mp4");
        assert_eq!(content_type_for(Path::new("a.mov")), "video/quicktime");
        assert_eq!(content_type_for(Path::new("a.mkv")), "video/x-matroska");
        assert_eq!(content_type_for(Path::new("a.webm")), "video/webm");
        assert_eq!(
            content_type_for(Path::new("noext")),
            "application/octet-stream"
        );
    }

    fn fixture(len: usize) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("clip.mp4");
        let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).expect("seed");
        (dir, path, bytes)
    }

    fn header_str(res: &Response<Vec<u8>>, name: header::HeaderName) -> String {
        res.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    }

    #[test]
    fn full_response_for_small_files() {
        let (_dir, path, bytes) = fixture(100);
        let res = serve_file(
            &Method::GET,
            None,
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header_str(&res, header::CONTENT_LENGTH), "100");
        assert_eq!(header_str(&res, header::ACCEPT_RANGES), "bytes");
        assert_eq!(res.body(), &bytes);
    }

    #[test]
    fn partial_response_describes_exactly_the_bytes_sent() {
        let (_dir, path, bytes) = fixture(100);
        let res = serve_file(
            &Method::GET,
            Some("bytes=10-19"),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header_str(&res, header::CONTENT_RANGE), "bytes 10-19/100");
        assert_eq!(header_str(&res, header::CONTENT_LENGTH), "10");
        assert_eq!(res.body(), &bytes[10..20].to_vec());
    }

    #[test]
    fn open_range_is_a_bounded_slice() {
        let (_dir, path, bytes) = fixture(10);
        let res = serve_file(
            &Method::GET,
            Some("bytes=3-"),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header_str(&res, header::CONTENT_RANGE), "bytes 3-9/10");
        assert_eq!(res.body(), &bytes[3..].to_vec());
    }

    #[test]
    fn unsatisfiable_range_is_416_with_total_length() {
        let (_dir, path, _) = fixture(10);
        let res = serve_file(
            &Method::GET,
            Some("bytes=50-"),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header_str(&res, header::CONTENT_RANGE), "bytes */10");
        assert!(res.body().is_empty());
    }

    #[test]
    fn invalid_range_is_416_too() {
        let (_dir, path, _) = fixture(10);
        let res = serve_file(
            &Method::GET,
            Some("bytes=9-1"),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }

    #[test]
    fn large_files_without_range_get_a_bounded_206() {
        let (_dir, path, _) = fixture(MAX as usize + 100);
        let res = serve_file(
            &Method::GET,
            None,
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().len() as u64, MAX);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes 0-{}/{}", MAX - 1, MAX + 100)
        );
    }

    /// Offsets past 4 GiB (the M0 range-seek spike): a 5 GiB sparse file, so the test writes a few bytes, not gigabytes.
    /// Skipped when the file system cannot mark the file sparse, because a plain file would allocate the whole size.
    #[cfg(windows)]
    #[test]
    fn ranges_past_four_gibibytes_are_served_exactly() {
        use std::io::Write;
        const LEN: u64 = 5 * 1024 * 1024 * 1024;
        const MARK_AT: u64 = 4 * 1024 * 1024 * 1024 + 512 * 1024 * 1024;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.mp4");
        File::create(&path).expect("create");
        let sparse = std::process::Command::new("fsutil")
            .args(["sparse", "setflag"])
            .arg(&path)
            .output()
            .is_ok_and(|out| out.status.success());
        if !sparse {
            eprintln!("skipped: the temp folder cannot hold sparse files");
            return;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open");
        file.set_len(LEN).expect("set_len");
        file.seek(SeekFrom::Start(MARK_AT)).expect("seek");
        file.write_all(b"LOOPCUT").expect("mark");
        file.seek(SeekFrom::Start(LEN - 4)).expect("seek end");
        file.write_all(b"TAIL").expect("tail");
        drop(file);

        let res = serve_file(
            &Method::GET,
            Some(&format!("bytes={MARK_AT}-")),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes {MARK_AT}-{}/{LEN}", MARK_AT + MAX - 1)
        );
        assert_eq!(header_str(&res, header::CONTENT_LENGTH), MAX.to_string());
        assert_eq!(&res.body()[..7], b"LOOPCUT");

        let tail = serve_file(
            &Method::GET,
            Some("bytes=-4"),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(tail.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            header_str(&tail, header::CONTENT_RANGE),
            format!("bytes {}-{}/{LEN}", LEN - 4, LEN - 1)
        );
        assert_eq!(tail.body(), b"TAIL");
    }

    #[test]
    fn head_has_headers_and_no_body() {
        let (_dir, path, _) = fixture(100);
        let res = serve_file(
            &Method::HEAD,
            Some("bytes=0-9"),
            "http://tauri.localhost",
            &path,
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header_str(&res, header::CONTENT_LENGTH), "10");
        assert!(res.body().is_empty());
    }

    #[test]
    fn missing_files_are_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let res = serve_file(
            &Method::GET,
            None,
            "http://tauri.localhost",
            &dir.path().join("gone.mp4"),
            "video/mp4",
        );
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn cors_echoes_only_known_origins() {
        let known = Request::builder()
            .header(header::ORIGIN, "http://tauri.localhost")
            .body(Vec::<u8>::new())
            .expect("request");
        assert_eq!(cors_origin(&known), "http://tauri.localhost");
        let foreign = Request::builder()
            .header(header::ORIGIN, "https://evil.example")
            .body(Vec::<u8>::new())
            .expect("request");
        assert_eq!(cors_origin(&foreign), APP_ORIGINS[0]);
    }

    #[test]
    fn the_dev_server_origin_is_trusted_only_in_debug_builds() {
        let dev = Request::builder()
            .header(header::ORIGIN, "http://localhost:1420")
            .body(Vec::<u8>::new())
            .expect("request");
        let expected = if cfg!(debug_assertions) {
            "http://localhost:1420"
        } else {
            APP_ORIGINS[0]
        };
        assert_eq!(cors_origin(&dev), expected);
    }

    #[test]
    fn thumbnails_are_found_by_height_and_index() {
        let set = |count: u32, height: u32, frames: usize| ThumbSet {
            count,
            height,
            width: 16,
            times: vec![0.0; frames],
            frames: (0..frames).map(|i| vec![i as u8; 4]).collect(),
        };
        let mut state = MediaState::default();
        state.thumbs.push(Arc::new(set(3, 90, 3)));
        state.thumbs.push(Arc::new(set(8, 180, 8)));
        assert_eq!(thumb_frame(&state, 90, 2), Some((16, 90, vec![2, 2, 2, 2])));
        assert_eq!(thumb_frame(&state, 90, 3), None, "index past the set");
        assert_eq!(
            thumb_frame(&state, 180, 7).map(|f| f.2),
            Some(vec![7, 7, 7, 7])
        );
        assert_eq!(thumb_frame(&state, 45, 0), None, "unknown height");
    }

    #[test]
    fn thumbnails_report_the_real_height_for_odd_requests() {
        let set = ThumbSet {
            count: 1,
            height: 90,
            width: 16,
            times: vec![0.0],
            frames: vec![vec![5; 16 * 90 * 4]],
        };
        let mut state = MediaState::default();
        state.thumbs.push(Arc::new(set));
        // The generator rounds 91 down to 90; the lookup and the reported height both use the real one.
        let (width, height, rgba) = thumb_frame(&state, 91, 0).expect("odd request finds its set");
        assert_eq!((width, height), (16, 90));
        assert_eq!(rgba.len(), 16 * 90 * 4);
    }

    fn request(method: Method, uri: &str) -> Request<Vec<u8>> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Vec::new())
            .expect("request")
    }

    fn header_value(res: &Response<Vec<u8>>, name: &str) -> String {
        res.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    }

    #[test]
    fn source_tokens_serve_only_while_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let media = dir.path().join("clip.mp4");
        std::fs::write(&media, b"0123456789").expect("seed");
        let token = state.publish_media(media, sample_info(), None);
        let uri = format!("http://loopcut-media.localhost/src%2F{token}");

        let mut ranged = request(Method::GET, &uri);
        ranged
            .headers_mut()
            .insert(header::RANGE, "bytes=2-4".parse().expect("header"));
        let res = handle_media(&state, &ranged);
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body(), b"234");

        let unknown = format!("http://loopcut-media.localhost/src%2F{}", "0".repeat(32));
        assert_eq!(
            handle_media(&state, &request(Method::GET, &unknown)).status(),
            StatusCode::NOT_FOUND,
            "a token that was never minted"
        );
        assert_eq!(
            handle_media(
                &state,
                &request(
                    Method::GET,
                    "http://loopcut-media.localhost/src%2F..%2Fsecret"
                )
            )
            .status(),
            StatusCode::NOT_FOUND,
            "anything that is not a token"
        );

        state.release_media(&token);
        assert_eq!(
            handle_media(&state, &request(Method::GET, &uri)).status(),
            StatusCode::NOT_FOUND,
            "a released token"
        );
    }

    #[test]
    fn thumbnail_responses_report_the_real_height_and_unknown_ones_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let media = dir.path().join("clip.mp4");
        std::fs::write(&media, b"x").expect("seed");
        let token = state.publish_media(media, sample_info(), None);
        let entry = state.media_entry(&token).expect("entry");
        state.remember_thumbs(
            &token,
            &entry,
            Arc::new(ThumbSet {
                count: 2,
                height: 90,
                width: 16,
                times: vec![0.0, 1.0],
                frames: vec![vec![1; 16 * 90 * 4], vec![2; 16 * 90 * 4]],
            }),
        );

        let odd = handle_loopcut(
            &state,
            &request(
                Method::GET,
                &format!("http://loopcut.localhost/thumb/{token}/91/1"),
            ),
        );
        assert_eq!(odd.status(), StatusCode::OK);
        assert_eq!(header_value(&odd, "x-height"), "90");
        assert_eq!(header_value(&odd, "x-width"), "16");
        assert_eq!(odd.body().len(), 16 * 90 * 4);
        assert_eq!(odd.body()[0], 2, "index 1 is the second frame");

        let past_end = handle_loopcut(
            &state,
            &request(
                Method::GET,
                &format!("http://loopcut.localhost/thumb/{token}/90/2"),
            ),
        );
        assert_eq!(past_end.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            past_end.body(),
            b"Not Found",
            "errors carry the reason text"
        );

        state.release_media(&token);
        let released = handle_loopcut(
            &state,
            &request(
                Method::GET,
                &format!("http://loopcut.localhost/thumb/{token}/90/0"),
            ),
        );
        assert_eq!(
            released.status(),
            StatusCode::NOT_FOUND,
            "released thumbnails are gone"
        );
    }

    #[test]
    fn unsupported_methods_get_405_and_preflights_get_204() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let post = handle_loopcut(
            &state,
            &request(Method::POST, "http://loopcut.localhost/thumb/abc/90/0"),
        );
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(header_value(&post, "allow"), "GET, HEAD, OPTIONS");

        let media_post = handle_media(
            &state,
            &request(Method::DELETE, "http://loopcut-media.localhost/src%2Fabc"),
        );
        assert_eq!(media_post.status(), StatusCode::METHOD_NOT_ALLOWED);

        let preflight = handle_loopcut(
            &state,
            &request(Method::OPTIONS, "http://loopcut.localhost/thumb/abc/90/0"),
        );
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    }
}

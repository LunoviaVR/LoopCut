//! Media commands: import, filmstrip thumbnails, timeline proxies and release.
//!
//! Imports mint an opaque token for the canonical path, and only after the file probed successfully. Nothing else in
//! the webview ever refers to a file path. Every FFmpeg or ffprobe call goes through `loopcut-media` (contained,
//! protocol-whitelisted, no console window) or through the cancellable scans in `crate::scan`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use loopcut_core::hw::{EncoderBackend, HwStage, VideoCodec, forced_failure_from_env};
use loopcut_core::media::MediaInfo;
use loopcut_hw::select::choose_encoder;
use loopcut_media::MediaError;
use loopcut_media::keyframes::max_gop_seconds;
use loopcut_media::proxy::{DEFAULT_PROXY_MAX_HEIGHT, ProxyRequest, make_proxy};
use loopcut_media::runner::{CancelToken, is_url_like};
use loopcut_media::thumbnails::{MAX_THUMBNAILS, ThumbnailRequest, generate_thumbnails};
use sha2::{Digest, Sha256};
use tauri::State;
use tauri::ipc::Channel;

use crate::commands::blocking;
use crate::dto::{ImportedMedia, ProxyEvent, ThumbnailSet};
use crate::error::{AppError, code, io_error};
use crate::netpath::{has_accepted_prefix, is_network_path, require_confirmation};
use crate::protocol::protocol_url;
use crate::scan;
use crate::state::{
    AppState, MediaEntry, keyframe_job_id, media_job_id, probe_job_id, proxy_job_id,
};
use crate::storage::strip_verbatim;
use crate::thumbcache::{ThumbKey, ThumbSet, normalized_height};

/// What `project_open` learned about one local clip path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalOpen {
    /// The file probed successfully and now has this token.
    Token(String),
    /// The file is missing, unreadable, not video, or FFmpeg is unavailable.
    Missing,
    /// The path resolves to a network location (a mapped drive or a UNC share). Nothing was probed.
    Network,
}

/// Longest keyframe gap, in seconds, that the webview can seek through directly.
pub const MAX_DIRECT_GOP_SECONDS: f64 = 2.0;
/// Tallest coded source the webview plays directly; taller sources get a proxy.
pub const MAX_DIRECT_HEIGHT: u32 = 1080;
/// Thumbnail heights accepted from the UI, in pixels.
const THUMB_HEIGHT_RANGE: std::ops::RangeInclusive<u32> = 8..=720;
/// Version of the proxy recipe. Bump it when the encode settings change so stale proxies are not reused.
const PROXY_RECIPE_VERSION: u32 = 1;

/// Imports a media file: probes it, mints its token and reports how the editor should play it.
///
/// Network locations (UNC paths and mapped network drives, checked before the file is touched) are refused with
/// `network_path` unless `confirmNetwork` is true, because opening one contacts another computer.
#[tauri::command]
pub async fn media_import(
    state: State<'_, Arc<AppState>>,
    path: String,
    confirm_network: Option<bool>,
) -> Result<ImportedMedia, AppError> {
    let st = Arc::clone(&state);
    blocking(move || import(&st, &path, confirm_network.unwrap_or(false))).await
}

/// Filmstrip thumbnails for a token: `count` frames spread over the source, `height` pixels tall. Pixels come from the
/// `loopcut` protocol (`thumb/<token>/<height>/<index>`). One set is kept per height; the returned `height` is the real
/// output height, which is the requested height rounded down to an even number.
#[tauri::command]
pub async fn media_thumbnails(
    state: State<'_, Arc<AppState>>,
    token: String,
    count: u32,
    height: u32,
) -> Result<ThumbnailSet, AppError> {
    let st = Arc::clone(&state);
    blocking(move || thumbnails(&st, &token, count, height)).await
}

/// Makes the timeline proxy for a token if it does not exist yet. Progress arrives as `ProxyEvent::Progress`, and the
/// final `Done` or `Error` event arrives on the same channel. The channel is authoritative: a failed encode sends
/// `Error` and the command still resolves `Ok`. Only requests that cannot start (unknown token, a proxy already being
/// made, FFmpeg missing) reject.
///
/// A hardware encode that fails is retried once on the CPU, with a progress reset to 0 first.
#[tauri::command]
pub async fn media_proxy_start(
    state: State<'_, Arc<AppState>>,
    token: String,
    on_progress: Channel<ProxyEvent>,
) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || proxy_start(&st, &token, &on_progress)).await
}

/// Forgets a token: cancels its jobs (thumbnails, proxy, and any probe or keyframe scan of its file), then drops its
/// caches. Later requests for it get 404. Runs on the blocking pool, because cancelling and dropping a token's entry
/// takes the media locks and may drop a thumbnail set.
#[tauri::command]
pub async fn media_release(state: State<'_, Arc<AppState>>, token: String) -> Result<(), AppError> {
    let st = Arc::clone(&state);
    blocking(move || {
        st.release_media(&token);
        Ok(())
    })
    .await
}

fn import(st: &AppState, raw: &str, confirm_network: bool) -> Result<ImportedMedia, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument("No file was given."));
    }
    if is_url_like(trimmed) {
        return Err(AppError::new(
            code::UNSUPPORTED_MEDIA,
            "Only local files can be opened.",
        ));
    }
    if !has_accepted_prefix(trimmed) {
        return Err(AppError::invalid_argument(
            "Only drive-letter and UNC paths can be opened.",
        ));
    }
    let requested = PathBuf::from(trimmed);
    // Judge the raw path before canonicalizing, which reads the file system, and the canonical path after it.
    require_confirmation(confirm_network, &[&requested])?;
    let canonical = canonical_path(&requested)?;
    require_confirmation(confirm_network, &[&canonical])?;
    if !has_accepted_prefix(&canonical.to_string_lossy()) {
        return Err(AppError::invalid_argument(
            "This path cannot be opened: it names a device, not a file.",
        ));
    }

    let paths = st.ffmpeg()?;
    let info = {
        let (cancel, _job) = st.start_job(&probe_job_id(&canonical))?;
        scan::probe_info(paths, &canonical, &cancel).map_err(probe_error)?
    };
    let keyframe_interval = {
        let (cancel, _job) = st.start_job(&keyframe_job_id(&canonical))?;
        match scan::keyframe_times(
            paths,
            &canonical,
            info.video.index,
            info.video.start_time,
            &cancel,
        ) {
            Ok(keys) => Some(max_gop_seconds(&keys, info.video.duration)),
            Err(MediaError::Cancelled) => return Err(AppError::from(MediaError::Cancelled)),
            Err(err) => {
                // Unknown GOP: the proxy decision then treats the source as needing a proxy.
                tracing::warn!(error = %err, "keyframe scan failed; the source will be proxied");
                None
            }
        }
    };

    let direct = direct_playback(
        info.video.web_playable,
        info.video.height,
        keyframe_interval,
    );
    let file_name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path_text = canonical.to_string_lossy().into_owned();
    let proxy = existing_proxy(&st.dirs.proxies, &canonical, info.video.index);
    let proxy_ready = proxy.is_some();
    let token = st.publish_media(canonical, Arc::new(info.clone()), proxy);

    Ok(ImportedMedia {
        file_name,
        path: path_text,
        source_url: direct.then(|| protocol_url("loopcut-media", &format!("src/{token}"))),
        needs_proxy: !direct,
        proxy_url: proxy_ready.then(|| protocol_url("loopcut-media", &format!("proxy/{token}"))),
        keyframe_interval,
        info,
        token,
    })
}

/// Whether the webview plays the original file directly: WebView2 can decode it, it is no taller than
/// [`MAX_DIRECT_HEIGHT`], and its longest GOP is known and short. An unknown GOP (`None`) means a proxy.
pub fn direct_playback(web_playable: bool, height: u32, keyframe_interval: Option<f64>) -> bool {
    web_playable
        && height <= MAX_DIRECT_HEIGHT
        && keyframe_interval.is_some_and(|gop| gop <= MAX_DIRECT_GOP_SECONDS)
}

/// Probes a local file for `project_open` and publishes it under a token. The result is [`LocalOpen::Missing`] when the
/// file is missing, cannot be read as video, or FFmpeg is unavailable (the project still opens), and
/// [`LocalOpen::Network`] when the path resolves to a network location, which is never probed.
pub(crate) fn open_local(st: &AppState, requested: &Path) -> Result<LocalOpen, AppError> {
    let Ok(canonical) = canonical_path(requested) else {
        return Ok(LocalOpen::Missing);
    };
    if is_network_path(&canonical) {
        return Ok(LocalOpen::Network);
    }
    if !has_accepted_prefix(&canonical.to_string_lossy()) {
        return Ok(LocalOpen::Missing);
    }
    let Ok(paths) = st.ffmpeg() else {
        return Ok(LocalOpen::Missing);
    };
    let (cancel, _job) = st.start_job(&probe_job_id(&canonical))?;
    match scan::probe_info(paths, &canonical, &cancel) {
        Ok(info) => Ok(LocalOpen::Token(publish_probed(st, canonical, info))),
        Err(MediaError::Cancelled) => Err(AppError::from(MediaError::Cancelled)),
        Err(_) => Ok(LocalOpen::Missing),
    }
}

/// The probed media entry for a clip path from a project or a webview request. The path must be a drive-letter or UNC
/// path; it matches the entry whose canonical file it names (case-insensitively on Windows). `not_found` when no token
/// was minted for that file, which is also the answer for a clip that was never opened.
pub(crate) fn entry_for_clip(st: &AppState, clip_path: &str) -> Result<Arc<MediaEntry>, AppError> {
    let trimmed = clip_path.trim();
    if !has_accepted_prefix(trimmed) {
        return Err(AppError::invalid_argument(
            "The clip path must be a drive-letter or UNC path.",
        ));
    }
    let wanted = Path::new(trimmed);
    st.media
        .read()
        .values()
        .find(|entry| same_file(&entry.path, wanted))
        .cloned()
        .ok_or_else(|| AppError::not_found("The media for this clip"))
}

/// Path equality that matches Windows file names: case-insensitive.
fn same_file(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

/// Publishes an already-probed file under a token, attaching any proxy that exists in the cache.
fn publish_probed(st: &AppState, canonical: PathBuf, info: MediaInfo) -> String {
    let proxy = existing_proxy(&st.dirs.proxies, &canonical, info.video.index);
    st.publish_media(canonical, Arc::new(info), proxy)
}

fn cached_set(entry: &MediaEntry, count: u32, height: u32) -> Option<Arc<ThumbSet>> {
    entry
        .state
        .lock()
        .thumbs
        .iter()
        .find(|set| set.count == count && set.height == height)
        .cloned()
}

fn thumbnails(
    st: &AppState,
    token: &str,
    count: u32,
    height: u32,
) -> Result<ThumbnailSet, AppError> {
    if !(1..=MAX_THUMBNAILS).contains(&count) {
        return Err(AppError::invalid_argument(format!(
            "thumbnail count must be between 1 and {MAX_THUMBNAILS}"
        )));
    }
    if !THUMB_HEIGHT_RANGE.contains(&height) {
        return Err(AppError::invalid_argument(
            "thumbnail height is out of range",
        ));
    }
    let entry = st
        .media_entry(token)
        .ok_or_else(|| AppError::not_found("This media"))?;
    let wanted = normalized_height(height);

    if let Some(set) = cached_set(&entry, count, wanted) {
        st.thumb_budget.touch(&ThumbKey {
            token: token.to_owned(),
            height: wanted,
        });
        return Ok(to_dto(&set));
    }

    let paths = st.ffmpeg()?;
    let info = &entry.info;
    let (display_w, display_h) = info.video.display_size();
    let request = ThumbnailRequest {
        input: entry.path.clone(),
        stream_index: info.video.index,
        start_time: info.video.start_time,
        duration: source_duration(info),
        count,
        height: wanted,
        rotation_cw: info.video.rotation,
        display_aspect: f64::from(display_w) / f64::from(display_h.max(1)),
        color: info.video.color,
    };
    let (cancel, _job) = st.start_job(&media_job_id(token, &format!("thumbs:{count}:{wanted}")))?;
    // A release that ran before the registration cancelled nothing, because the job was not registered yet. The check
    // after the registration catches it. A release after the registration cancels this job instead. A token is never
    // minted again once released, so a missing entry means a release; a re-import keeps the token and the file.
    if st.media_entry(token).is_none() {
        return Err(AppError::not_found("This media"));
    }
    let thumbs = generate_thumbnails(paths, &request, cancel).map_err(AppError::from)?;

    // The generator rounds odd heights down; the set and its reported height use the real output height.
    let real_height = thumbs.first().map_or(wanted, |t| t.height);
    let set = Arc::new(ThumbSet {
        count,
        height: real_height,
        width: thumbs.first().map_or(0, |t| t.width),
        times: thumbs.iter().map(|t| t.time).collect(),
        frames: thumbs.into_iter().map(|t| t.rgba).collect(),
    });
    if !set.frames.is_empty() {
        st.remember_thumbs(token, &entry, Arc::clone(&set));
    }
    Ok(to_dto(&set))
}

/// What a failed proxy encode leads to. See [`proxy_next_after_failure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyNext {
    /// Restart the whole encode on this encoder (always the CPU). Progress goes back to zero first.
    Retry(EncoderBackend),
    /// Report the failure on the channel and finish.
    Report,
}

/// The retry rule of the proxy encode: a hardware encoder that fails, other than by cancellation, is retried once on
/// the CPU. Anything else (a CPU failure, or a cancelled encode) is reported. An encode counts as cancelled when its
/// token is set, even if FFmpeg exited with its own failure first: the update terminates children after cancelling them.
pub fn proxy_next_after_failure(
    encoder: EncoderBackend,
    err: &MediaError,
    cancel: &CancelToken,
) -> ProxyNext {
    let cancelled = cancel.is_cancelled() || matches!(err, MediaError::Cancelled);
    if encoder.is_hardware() && !cancelled {
        ProxyNext::Retry(EncoderBackend::Cpu)
    } else {
        ProxyNext::Report
    }
}

/// True while `entry` is still the live entry of `token`. A released token, or one whose file was imported again, has a
/// different (or no) entry.
fn is_live_entry(st: &AppState, token: &str, entry: &Arc<MediaEntry>) -> bool {
    matches!(st.media_entry(token), Some(live) if Arc::ptr_eq(&live, entry))
}

/// Lands a finished proxy on the live entry of `token`. Returns false when the token was released or its entry was
/// replaced while the encode ran: the file stays in the cache, but this entry must not be changed.
fn land_proxy(st: &AppState, token: &str, entry: &Arc<MediaEntry>, output: PathBuf) -> bool {
    if !is_live_entry(st, token, entry) {
        return false;
    }
    entry.state.lock().proxy = Some(output);
    true
}

fn proxy_start(st: &AppState, token: &str, progress: &Channel<ProxyEvent>) -> Result<(), AppError> {
    let entry = st
        .media_entry(token)
        .ok_or_else(|| AppError::not_found("This media"))?;
    let info = Arc::clone(&entry.info);
    let output = proxy_path(&st.dirs.proxies, &entry.path, info.video.index);
    let done_url = protocol_url("loopcut-media", &format!("proxy/{token}"));

    if output.is_file() {
        if land_proxy(st, token, &entry, output) {
            send(progress, ProxyEvent::Done { url: done_url });
        } else {
            send(progress, released_error());
        }
        return Ok(());
    }

    let paths = st.ffmpeg()?.clone();
    let cancel = CancelToken::new();
    // Keyed by the output file, so two tokens for one file never encode it twice. The entry records the job id, so a
    // release cancels this encode by that id, whatever the source file looks like by then.
    let job = proxy_job_id(&output);
    let _job = st.register_job(
        &job,
        cancel.clone(),
        "A proxy for this file is already being made.",
    )?;
    entry.state.lock().proxy_job = Some(job);
    // The token may have been released, or its file imported again, while the job registered. Nothing is encoded then.
    if !is_live_entry(st, token, &entry) {
        send(progress, released_error());
        return Ok(());
    }
    fs::create_dir_all(&st.dirs.proxies)
        .map_err(|err| io_error(&err, "The cache folder could not be created"))?;

    let (display_w, display_h) = info.video.display_size();
    let (width, height) = proxy_output_size(display_w, display_h, DEFAULT_PROXY_MAX_HEIGHT);
    let mut encoder = choose_proxy_encoder(st, width, height);
    loop {
        // A cancel that landed after the last attempt (an update, a release) starts no further encode. make_proxy first
        // runs an ffprobe that does not look at the token, so the check has to come before the call.
        if cancel.is_cancelled() {
            send(
                progress,
                failure_event(st, token, &entry, MediaError::Cancelled),
            );
            return Ok(());
        }
        let request = ProxyRequest {
            input: entry.path.clone(),
            stream_index: info.video.index,
            output: output.clone(),
            max_height: DEFAULT_PROXY_MAX_HEIGHT,
            fps_cap: None,
            encoder,
            color: info.video.color,
            rotation_cw: info.video.rotation,
        };
        let sink = progress.clone();
        let on_progress = move |fraction: f64| {
            let _ = sink.send(ProxyEvent::Progress {
                value: fraction.clamp(0.0, 1.0),
            });
        };
        let outcome = if forced_failure_from_env(HwStage::Encode, encoder.name()) {
            Err(MediaError::Failed {
                code: None,
                stderr_tail: "hardware encode failure forced by LOOPCUT_FORCE_HW_FAIL".to_owned(),
            })
        } else {
            make_proxy(&paths, &request, on_progress, cancel.clone())
        };
        match outcome {
            Ok(_) => {
                if land_proxy(st, token, &entry, output) {
                    send(progress, ProxyEvent::Done { url: done_url });
                } else {
                    send(progress, released_error());
                }
                return Ok(());
            }
            Err(err) => match proxy_next_after_failure(encoder, &err, &cancel) {
                ProxyNext::Retry(next) => {
                    tracing::warn!(
                        encoder = encoder.name(),
                        error = %err,
                        "hardware proxy encode failed; retrying once on the CPU"
                    );
                    send(progress, ProxyEvent::Progress { value: 0.0 });
                    encoder = next;
                }
                ProxyNext::Report => {
                    send(progress, failure_event(st, token, &entry, err));
                    return Ok(());
                }
            },
        }
    }
}

/// The channel event for an encode that stopped with `err`. A token released while the encode ran reports `not_found`,
/// whatever stopped the encode (its release cancels the encode, and the cancellation is not the news); a live token
/// reports the encode's own error.
fn failure_event(
    st: &AppState,
    token: &str,
    entry: &Arc<MediaEntry>,
    err: MediaError,
) -> ProxyEvent {
    if is_live_entry(st, token, entry) {
        ProxyEvent::Error {
            error: AppError::from(err),
        }
    } else {
        released_error()
    }
}

/// The channel error for a proxy whose token was released while the encode ran.
fn released_error() -> ProxyEvent {
    ProxyEvent::Error {
        error: AppError::not_found("This media"),
    }
}

/// Upright size the proxy encoder receives: the display size scaled down to `max_height`, with an even height and a
/// width that follows the aspect ratio (also even). Matches the scale filter in `loopcut_media::proxy`.
pub fn proxy_output_size(display_w: u32, display_h: u32, max_height: u32) -> (u32, u32) {
    let display_w = display_w.max(2);
    let display_h = display_h.max(2);
    let height = (2 * (display_h.min(max_height) / 2)).max(2);
    let width_exact = f64::from(display_w) * f64::from(height) / f64::from(display_h);
    let width = ((width_exact / 2.0).round() as u32 * 2).max(2);
    (width, height)
}

/// Encoder for the timeline proxy: the probed hardware encoder when settings and probes allow it at this size, else
/// the CPU. Without a probe result the answer is always the CPU.
fn choose_proxy_encoder(st: &AppState, width: u32, height: u32) -> EncoderBackend {
    let caps = st.caps.read();
    let settings = st.settings.read();
    match caps.as_ref() {
        Some(caps) => choose_encoder(caps, &settings.hw, VideoCodec::H264, width, height, true),
        None => EncoderBackend::Cpu,
    }
}

fn send(channel: &Channel<ProxyEvent>, event: ProxyEvent) {
    let _ = channel.send(event);
}

fn to_dto(set: &ThumbSet) -> ThumbnailSet {
    ThumbnailSet {
        width: set.width,
        height: set.height,
        times: set.times.clone(),
    }
}

/// Source duration used for thumbnails: the video stream's, falling back to the container's.
pub(crate) fn source_duration(info: &MediaInfo) -> f64 {
    if info.video.duration.is_finite() && info.video.duration > 0.0 {
        info.video.duration
    } else {
        info.duration
    }
}

/// A probe that fails because ffprobe rejected the file is an unsupported media file, not an FFmpeg fault.
fn probe_error(err: MediaError) -> AppError {
    if let MediaError::Failed { stderr_tail, .. } = &err {
        return AppError::new(
            code::UNSUPPORTED_MEDIA,
            "This file could not be read as video.",
        )
        .with_detail(stderr_tail.clone());
    }
    AppError::from(err)
}

/// Canonical form of a user-supplied path: resolved, with the Windows verbatim prefix removed. Fails for missing files
/// and folders. Callers check [`is_network_path`] on the raw and the canonical path before relying on it.
pub fn canonical_path(requested: &Path) -> Result<PathBuf, AppError> {
    let resolved = fs::canonicalize(requested)
        .map_err(|err| io_error(&err, "The file could not be opened"))?;
    let path = strip_verbatim(&resolved);
    if !path.is_file() {
        return Err(AppError::invalid_argument(
            "That path is a folder, not a file.",
        ));
    }
    Ok(path)
}

/// Where the proxy for a file lives. The name is a hash of the file's identity (path, size, modification time, video
/// stream and recipe version), so a proxy is reused across launches and regenerated when the source changes.
pub fn proxy_path(dir: &Path, source: &Path, stream_index: u32) -> PathBuf {
    let meta = fs::metadata(source).ok();
    let size = meta.as_ref().map_or(0, |m| m.len());
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let identity = source.to_string_lossy().to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    hasher.update(size.to_le_bytes());
    hasher.update(modified.to_le_bytes());
    hasher.update(stream_index.to_le_bytes());
    hasher.update(PROXY_RECIPE_VERSION.to_le_bytes());
    let digest = hasher.finalize();
    let hex: String = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    dir.join(format!("{hex}.mp4"))
}

/// The proxy for `source` if one has already been made.
fn existing_proxy(dir: &Path, source: &Path, stream_index: u32) -> Option<PathBuf> {
    let path = proxy_path(dir, source, stream_index);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::{sample_info, state_under};

    #[test]
    fn direct_playback_needs_a_known_short_gop_and_a_web_codec() {
        assert!(direct_playback(true, 720, Some(1.0)));
        assert!(
            !direct_playback(true, 720, None),
            "an unknown GOP means a proxy"
        );
        assert!(!direct_playback(true, 720, Some(4.0)));
        assert!(!direct_playback(true, 2160, Some(1.0)));
        assert!(!direct_playback(false, 720, Some(1.0)));
    }

    #[test]
    fn proxy_sizes_are_even_and_capped() {
        assert_eq!(proxy_output_size(1920, 1080, 720), (1280, 720));
        assert_eq!(
            proxy_output_size(640, 360, 720),
            (640, 360),
            "small sources keep their size"
        );
        let (w, h) = proxy_output_size(1080, 1920, 720);
        assert_eq!(h, 720);
        assert_eq!(w % 2, 0);
        assert!(w > 0 && w < 720);
    }

    #[test]
    fn network_refusals_come_before_any_scan() {
        let unc = Path::new(r"\\nas\media\clip.mp4");
        assert_eq!(
            require_confirmation(false, &[unc])
                .expect_err("refused")
                .code,
            "network_path"
        );
        assert!(require_confirmation(true, &[unc]).is_ok());
    }

    #[test]
    fn released_tokens_have_no_thumbnails_to_serve() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let token = state.publish_media(dir.path().join("clip.mp4"), sample_info(), None);
        let err = thumbnails(&state, &token, 4, 90).expect_err("no ffmpeg in tests");
        // Validation passed and the entry exists, so the failure is FFmpeg being absent, not an unknown token.
        assert_eq!(err.code, "ffmpeg_missing");
        state.release_media(&token);
        let unknown = thumbnails(&state, &token, 4, 90).expect_err("released");
        assert_eq!(unknown.code, "not_found");
    }

    #[test]
    fn thumbnail_arguments_are_validated_before_lookup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        assert_eq!(
            thumbnails(&state, "0".repeat(32).as_str(), 0, 90)
                .expect_err("count")
                .code,
            "invalid_argument"
        );
        assert_eq!(
            thumbnails(&state, "0".repeat(32).as_str(), 4, 4)
                .expect_err("height")
                .code,
            "invalid_argument"
        );
    }

    /// The error of an encode that exited on its own with a failure.
    fn exit_failure() -> MediaError {
        MediaError::Failed {
            code: Some(1),
            stderr_tail: "encoder failed".to_owned(),
        }
    }

    #[test]
    fn a_failed_hardware_proxy_retries_once_on_the_cpu() {
        for hardware in [
            EncoderBackend::Nvenc { cuda_index: 0 },
            EncoderBackend::Amf { dxgi_index: 0 },
            EncoderBackend::Qsv { dxgi_index: 0 },
        ] {
            assert_eq!(
                proxy_next_after_failure(hardware, &exit_failure(), &CancelToken::new()),
                ProxyNext::Retry(EncoderBackend::Cpu)
            );
        }
    }

    #[test]
    fn cpu_failures_and_cancellations_are_reported_not_retried() {
        assert_eq!(
            proxy_next_after_failure(EncoderBackend::Cpu, &exit_failure(), &CancelToken::new()),
            ProxyNext::Report,
            "the CPU has nothing to fall back to"
        );
        assert_eq!(
            proxy_next_after_failure(
                EncoderBackend::Amf { dxgi_index: 0 },
                &MediaError::Cancelled,
                &CancelToken::new()
            ),
            ProxyNext::Report,
            "a cancelled hardware encode is not retried"
        );
    }

    #[test]
    fn a_cancelled_encode_that_exits_with_a_failure_is_not_retried() {
        // The update cancels the encode and then terminates FFmpeg, so the runner reports a failed exit, not Cancelled.
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            proxy_next_after_failure(
                EncoderBackend::Amf { dxgi_index: 0 },
                &exit_failure(),
                &cancel
            ),
            ProxyNext::Report,
            "a set token wins over the exit status"
        );
    }

    #[test]
    fn a_retried_proxy_resets_its_progress_and_then_reports_the_second_failure() {
        // The state machine as proxy_start walks it: hardware fails, the CPU retry fails, the CPU failure is reported.
        let mut encoder = EncoderBackend::Nvenc { cuda_index: 0 };
        let mut resets = 0;
        let mut attempts = 0;
        let reported = loop {
            attempts += 1;
            match proxy_next_after_failure(encoder, &exit_failure(), &CancelToken::new()) {
                ProxyNext::Retry(next) => {
                    resets += 1;
                    encoder = next;
                }
                ProxyNext::Report => break attempts,
            }
        };
        assert_eq!((resets, reported), (1, 2));
    }

    #[test]
    fn a_replaced_or_released_entry_is_not_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let path = dir.path().join("clip.mp4");
        let token = state.publish_media(path.clone(), sample_info(), None);
        let first = state.media_entry(&token).expect("entry");
        assert!(is_live_entry(&state, &token, &first));
        state.publish_media(path, sample_info(), None);
        assert!(!is_live_entry(&state, &token, &first), "replaced");
        let second = state.media_entry(&token).expect("entry");
        state.release_media(&token);
        assert!(!is_live_entry(&state, &token, &second), "released");
    }

    #[test]
    fn a_released_token_reports_not_found_for_a_cancelled_encode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let token = state.publish_media(dir.path().join("clip.mp4"), sample_info(), None);
        let entry = state.media_entry(&token).expect("entry");
        let ProxyEvent::Error { error } =
            failure_event(&state, &token, &entry, MediaError::Cancelled)
        else {
            panic!("a failed encode is an error event");
        };
        assert_eq!(
            error.code, "cancelled",
            "a live token keeps the encode's error"
        );
        state.release_media(&token);
        let ProxyEvent::Error { error } =
            failure_event(&state, &token, &entry, MediaError::Cancelled)
        else {
            panic!("a failed encode is an error event");
        };
        assert_eq!(error.code, "not_found", "a released token is not found");
    }

    #[test]
    fn clips_are_matched_to_their_probed_media_case_insensitively() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        let token = state.publish_media(PathBuf::from(r"C:\Videos\Clip.mp4"), sample_info(), None);
        let found = entry_for_clip(&state, r"c:\videos\clip.MP4").expect("same file");
        assert_eq!(found.path, PathBuf::from(r"C:\Videos\Clip.mp4"));
        let missing = entry_for_clip(&state, r"C:\Videos\Other.mp4").expect_err("no token");
        assert_eq!(missing.code, "not_found");
        state.release_media(&token);
        assert_eq!(
            entry_for_clip(&state, r"C:\Videos\Clip.mp4")
                .err()
                .map(|e| e.code),
            Some("not_found".to_owned()),
            "a released clip is not found"
        );
    }

    #[test]
    fn clip_paths_must_be_drive_or_unc_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_under(dir.path());
        for device in [r"\\.\C:\clip.mp4", r"\\?\GLOBALROOT\x", "relative.mp4", ""] {
            assert_eq!(
                entry_for_clip(&state, device).err().map(|e| e.code),
                Some("invalid_argument".to_owned()),
                "{device}"
            );
        }
    }

    #[test]
    fn proxy_paths_depend_on_the_source_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("clip.mp4");
        std::fs::write(&source, b"abc").expect("seed");
        let first = proxy_path(dir.path(), &source, 0);
        assert_eq!(first, proxy_path(dir.path(), &source, 0));
        assert_ne!(
            first,
            proxy_path(dir.path(), &source, 1),
            "the video stream matters"
        );
    }
}

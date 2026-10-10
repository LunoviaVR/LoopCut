//! Helpers shared by the FFmpeg-backed integration tests.
//!
//! Every test that needs FFmpeg calls [`paths`] first and returns early when no staged build exists, so `cargo test`
//! stays green on machines without the bundle. Media is generated with lavfi sources into a temporary directory, so
//! no binary fixtures are committed for these tests.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these helpers"
)]

use std::path::{Path, PathBuf};

use loopcut_media::{FfmpegCommand, FfmpegPaths, InputOptions, MediaError, SpawnIo};

/// The staged FFmpeg build, or `None` (after printing a notice) when it is not available.
pub fn paths() -> Option<FfmpegPaths> {
    let found = FfmpegPaths::discover_dev();
    if found.is_none() {
        eprintln!(
            "skipping: no staged FFmpeg build (set LOOPCUT_FFMPEG_DIR or stage src-tauri/resources/ffmpeg)"
        );
    }
    found
}

/// Options for a lavfi virtual source input.
pub fn lavfi() -> InputOptions {
    InputOptions {
        lavfi: true,
        ..InputOptions::default()
    }
}

/// Runs a fully built command to completion and returns its stderr tail on success.
pub fn run(cmd: FfmpegCommand) -> Result<String, MediaError> {
    cmd.spawn(SpawnIo::default())?
        .wait()
        .map(|info| info.stderr_tail)
}

/// Generates a clip from a lavfi graph into `out`, encoded with `output_args`.
pub fn generate(
    paths: &FfmpegPaths,
    graph: &str,
    output_args: &[&str],
    out: &Path,
) -> Result<(), MediaError> {
    let cmd = FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input(graph, &lavfi())?
        .output_args(output_args.iter().copied())
        .expect("valid FFmpeg argument")
        .output(out)?;
    run(cmd).map(|_| ())
}

/// The recipe for the 320x240, 10 fps, 1 second H.264 clip used across the probe and decode tests.
pub fn make_h264_clip(paths: &FfmpegPaths, dir: &Path) -> PathBuf {
    let out = dir.join("plain.mp4");
    generate(
        paths,
        "testsrc2=size=320x240:rate=10:duration=1",
        &["-c:v", "libx264", "-pix_fmt", "yuv420p"],
        &out,
    )
    .expect("generate H.264 clip");
    out
}

/// Returns a copy of `source` with a display matrix set to `display_rotation` degrees (counter-clockwise), like a
/// phone recording in portrait. Only metadata changes; the coded frames are untouched.
pub fn with_display_rotation(
    paths: &FfmpegPaths,
    source: &Path,
    display_rotation: i32,
    out: &Path,
) -> Result<(), MediaError> {
    let rotation = display_rotation.to_string();
    let cmd = FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")
        .expect("valid FFmpeg argument")
        .input(
            source,
            &InputOptions {
                extra: vec!["-display_rotation:v:0".to_owned(), rotation],
                ..InputOptions::default()
            },
        )?
        .output_args(["-c", "copy"])
        .expect("valid FFmpeg argument")
        .output(out)?;
    run(cmd).map(|_| ())
}

/// Decodes the first frame of `path` to packed RGB24 with FFmpeg's default autorotation applied, as a reference for
/// what an upright picture looks like. `width` x `height` are the displayed dimensions (the caller takes them from
/// `VideoStreamInfo::display_size`). Uses a raw command because the builder always disables autorotation.
pub fn autorotated_first_frame_rgb(
    paths: &FfmpegPaths,
    path: &Path,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let output = std::process::Command::new(&paths.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .output()
        .expect("run ffmpeg for reference frame");
    assert!(output.status.success(), "reference decode failed");
    assert_eq!(
        output.stdout.len(),
        (width * height * 3) as usize,
        "autorotated frame has unexpected size"
    );
    output.stdout
}

/// True if a process with `pid` is still running. Uses `tasklist` on Windows; other platforms report false.
pub fn pid_alive(pid: u32) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .expect("run tasklist");
    String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

/// Polls until the process with `pid` is gone, or `limit` passes. Returns whether it disappeared in time.
pub fn wait_for_exit(pid: u32, limit: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < limit {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    !pid_alive(pid)
}

/// Quadrant of the centroid of pure-red pixels, as `(top, left)`. Used to locate a marker in a decoded frame.
pub fn red_marker_quadrant(width: u32, height: u32, rgb: &[u8]) -> (bool, bool) {
    let (mut sum_x, mut sum_y, mut count) = (0u64, 0u64, 0u64);
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 3) as usize;
            if rgb[i] > 200 && rgb[i + 1] < 60 && rgb[i + 2] < 60 {
                sum_x += u64::from(x);
                sum_y += u64::from(y);
                count += 1;
            }
        }
    }
    assert!(count > 0, "no red marker found in decoded frame");
    let cx = sum_x as f64 / count as f64;
    let cy = sum_y as f64 / count as f64;
    (cy < f64::from(height) / 2.0, cx < f64::from(width) / 2.0)
}

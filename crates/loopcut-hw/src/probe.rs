//! Capability probing: decode, scale and encode tests per adapter, run with the command builders the app uses.
//!
//! Each test builds its command with [`loopcut_media::decode::build_decode_command`] or
//! [`loopcut_media::encode::build_encode_command`], so a passing probe means the app's own command line works on this
//! adapter. Every child process gets the probe timeout and honors cancellation. Failures are recorded in the returned
//! [`Capabilities`] as statuses; only cancellation returns an error.
//!
//! # Rules the probe follows
//!
//! * **AMF runs on one adapter only.** AMF cannot select an adapter, so its decode, scale and encode run on the default
//!   AMD adapter. The probe assumes that default is the first non-software AMD adapter in DXGI order (see
//!   [`crate::devices::amf_default_adapter`]). Other AMD adapters get `unsupported` AMF entries that say so, and they
//!   use D3D11VA decode and CPU encode.
//! * **A GPU scaler is probed per codec.** After a decode of a codec passes on a backend with a native scaler, the
//!   scaler is run on the same fixture. The result records the codec, and selection uses it only for that codec.
//! * **An encode passes only when the encoder counted every frame sent.** A non-empty output file is not enough,
//!   because a truncated encode can still leave one. The check reads the final `frame=` value from FFmpeg's
//!   `-progress` output.
//! * **Decode tests stop after `DECODE_FRAMES` (10) frames.** `build_decode_command` sets no frame limit. The probe adds
//!   `-frames:v` to the built command as a post-processing step, after the builder's own output options. FFmpeg then
//!   exits, and the probe checks the byte count against the frame size.
//! * **Benchmarks are like with like.** Each hardware encoder is compared with the CPU encoder of the same codec
//!   (libx264, libx265 or libsvtav1), with the same 300 frames at 1280x720. Frames are generated before the clock starts.
//!
//! [`loopcut_media::encode::EncodeSession::finish`] waits without a timeout, so encodes are started from the same
//! [`FfmpegCommand`] that `build_encode_command` returns and waited on with [`loopcut_media::runner::FfmpegProcess::wait_cancellable`].
//! [`loopcut_media::decode::DecodeSession`] has no timeout either, so decodes use `build_decode_command` the same way.
//!
//! The probe is blocking. Run it on a worker thread.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loopcut_core::hw::{DecodeBackend, EncoderBackend, GpuScaler, GpuVendor, VideoCodec};
use loopcut_core::{ColorInfo, DecodeChain, PixelLayout, Rational};
use loopcut_media::decode::{DecodeRequest, build_decode_command};
use loopcut_media::encode::{Container, EncodeRequest, build_encode_command};
use loopcut_media::runner::{CancelToken, FfmpegCommand, InputOptions, Progress, SpawnIo};
use loopcut_media::{FfmpegPaths, MediaError};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::HwError;
use crate::caps::{self, CapStatus, Capabilities, DecodeCap, DeviceCaps, EncodeCap, ScalerCap};
use crate::devices::{self, GpuDevice};

/// Version of the probe commands, sizes and pass rules. Bump it whenever a change would make a cached result wrong:
/// a different command, size list, frame count, pass rule or benchmark. It is part of the cache key.
pub const PROBE_LOGIC_VERSION: u32 = 2;

/// Frame rate of the fixtures and of every synthetic encode.
const FPS: u32 = 30;
/// Frames decoded per decode test.
const DECODE_FRAMES: u32 = 10;
/// Frames encoded per size attempt.
const ENCODE_FRAMES: u32 = 15;
/// Frames per benchmark run: ten seconds of video at [`FPS`].
const BENCH_FRAMES: u32 = 300;
/// Size used for benchmarks.
const BENCH_SIZE: (u32, u32) = (1280, 720);
/// The shortest timeout a benchmark child gets. 300 frames of libx265 at 720p can take longer than the 20 s default.
const BENCH_MIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Output size of decode and scale tests.
const DECODE_SIZE: (u32, u32) = (640, 360);
/// Size of the quick CPU encoder check.
const CPU_SIZE: (u32, u32) = (320, 180);
/// Quality passed to every encode (1 to 100).
const QUALITY: u8 = 80;
/// Size candidates for the hardware encoder minimum search, smallest first. All dimensions are even.
const SIZE_CANDIDATES: [(u32, u32); 7] = [
    (256, 144),
    (320, 180),
    (426, 240),
    (480, 270),
    (640, 360),
    (854, 480),
    (1280, 720),
];
/// Distinct synthetic frames that an encode cycles through, so content changes between frames.
const SYNTHETIC_FRAMES: u32 = 8;
/// Codecs with a hardware encoder path.
const HW_CODECS: [VideoCodec; 3] = [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1];
/// Codecs with a CPU encoder.
const CPU_CODECS: [VideoCodec; 4] = [
    VideoCodec::H264,
    VideoCodec::Hevc,
    VideoCodec::Av1,
    VideoCodec::Vp9,
];
/// CPU encoders that are benchmarked as the speed reference for their codec: libx264, libx265 and libsvtav1. VP9 has no
/// hardware encoder in LoopCut, so it needs no reference.
const BENCH_CPU_CODECS: [VideoCodec; 3] = [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1];

// The benchmark must cover exactly ten seconds, so that the speed figure is in video seconds per wall second.
const _: () = assert!(BENCH_FRAMES == 10 * FPS);

/// Options for one probe run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Limit for every FFmpeg child, including each encode attempt. Benchmark runs use at least
    /// 120 seconds.
    pub timeout: Duration,
    /// Also measure encode speed at 1280x720 with 300 frames for each working hardware encoder, and for the CPU
    /// references libx264, libx265 and libsvtav1.
    pub benchmark: bool,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            benchmark: false,
        }
    }
}

/// Progress reported while probing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ProbeEvent {
    /// A test is starting. `done` is the number of tests finished so far and `total` the number planned.
    Step {
        label: String,
        done: u32,
        total: u32,
    },
    /// A test finished. `ok` is true when its status is `ok`.
    Result { label: String, ok: bool },
}

/// The sample clips the decode tests read. Each is one second of 1280x720 test pattern at 30 fps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fixture {
    H264,
    HevcTenBit,
    Av1,
}

impl Fixture {
    const ALL: [Self; 3] = [Self::H264, Self::HevcTenBit, Self::Av1];

    fn index(self) -> usize {
        match self {
            Self::H264 => 0,
            Self::HevcTenBit => 1,
            Self::Av1 => 2,
        }
    }

    fn codec(self) -> VideoCodec {
        match self {
            Self::H264 => VideoCodec::H264,
            Self::HevcTenBit => VideoCodec::Hevc,
            Self::Av1 => VideoCodec::Av1,
        }
    }

    fn codec_name(self) -> &'static str {
        caps::codec_name(self.codec())
    }

    fn ten_bit(self) -> bool {
        matches!(self, Self::HevcTenBit)
    }

    fn layout(self) -> PixelLayout {
        if self.ten_bit() {
            PixelLayout::P010
        } else {
            PixelLayout::Nv12
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::H264 => "h264 8-bit",
            Self::HevcTenBit => "hevc 10-bit",
            Self::Av1 => "av1 8-bit",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::H264 => "fixture-h264-8bit.mp4",
            Self::HevcTenBit => "fixture-hevc-10bit.mp4",
            Self::Av1 => "fixture-av1-8bit.mp4",
        }
    }

    /// Encoder options that make the fixture, written after the `-i` of a test pattern.
    fn encode_args(self) -> &'static [&'static str] {
        match self {
            Self::H264 => &[
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
                "-g", "30",
            ],
            Self::HevcTenBit => &[
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-crf",
                "28",
                "-pix_fmt",
                "yuv420p10le",
                "-g",
                "30",
            ],
            Self::Av1 => &[
                "-c:v",
                "libsvtav1",
                "-preset",
                "10",
                "-crf",
                "40",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "30",
            ],
        }
    }
}

/// One planned test. Planning happens before any process starts, so progress totals are exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    Fixture(Fixture),
    CpuEncode(VideoCodec),
    /// Decodes one fixture on one backend. When that passes and the backend has a native GPU scaler, the scaler is
    /// probed on the same fixture as part of this job.
    Decode {
        device: usize,
        backend: DecodeBackend,
        fixture: Fixture,
    },
    Encode {
        device: usize,
        backend: EncoderBackend,
        codec: VideoCodec,
    },
}

/// Why a probe step stopped. Failures become statuses; cancellation aborts the whole probe.
#[derive(Debug)]
enum Stop {
    Cancelled,
    Failed(String),
}

impl From<MediaError> for Stop {
    fn from(err: MediaError) -> Self {
        match err {
            MediaError::Cancelled => Self::Cancelled,
            other => Self::Failed(describe_media_error(&other)),
        }
    }
}

impl From<io::Error> for Stop {
    fn from(err: io::Error) -> Self {
        Self::Failed(err.to_string())
    }
}

/// Short, single-line description of a failed FFmpeg run: the exit code and the last two stderr lines.
fn describe_media_error(err: &MediaError) -> String {
    let text = match err {
        MediaError::Failed { code, stderr_tail } => {
            let lines: Vec<&str> = stderr_tail
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            let tail = lines
                .iter()
                .rev()
                .take(2)
                .rev()
                .copied()
                .collect::<Vec<_>>()
                .join(" | ");
            if tail.is_empty() {
                format!("exit code {code:?}")
            } else {
                format!("exit code {code:?}: {tail}")
            }
        }
        other => other.to_string(),
    };
    truncate(text, 300)
}

/// Shortens `text` to at most `max` bytes on a character boundary, marking the cut.
fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

/// Runs `command`, reads its stdout on a helper thread, and returns the bytes when the process exits successfully.
/// The helper keeps a large output from stalling the child. `cancel` and `timeout` stop the child as usual.
fn capture_stdout(
    command: FfmpegCommand,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<Vec<u8>, Stop> {
    let mut process = command.spawn(SpawnIo {
        stdin: false,
        stdout: true,
    })?;
    let mut stdout = process
        .take_stdout()
        .ok_or_else(|| Stop::Failed("stdout pipe is missing".to_owned()))?;
    let reader = thread::Builder::new()
        .name("hw-probe-stdout".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })?;
    let outcome = process.wait_cancellable(cancel, Some(timeout));
    let read = reader.join();
    outcome?;
    match read {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(err)) => Err(err.into()),
        Err(_) => Err(Stop::Failed("stdout reader panicked".to_owned())),
    }
}

/// What FFmpeg reports as available, from `-version`, `-encoders`, `-filters` and `-hwaccels`. `None` means the listing
/// could not be read, and the check is then skipped (the test itself will report any problem).
#[derive(Debug, Default)]
struct Tables {
    encoders: Option<HashSet<String>>,
    filters: Option<HashSet<String>>,
    hwaccels: Option<HashSet<String>>,
}

impl Tables {
    /// Reads the version and the listings. The version is `"unknown"` when `-version` fails.
    ///
    /// # Errors
    ///
    /// [`HwError::Cancelled`] when `cancel` is set while a listing is running.
    fn load(
        paths: &FfmpegPaths,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<(String, Self), HwError> {
        let version = optional_listing(paths, "-version", timeout, cancel)?
            .map_or_else(|| "unknown".to_owned(), |text| parse_version(&text));
        let tables = Self {
            encoders: optional_listing(paths, "-encoders", timeout, cancel)?
                .map(|text| parse_named_list(&text)),
            filters: optional_listing(paths, "-filters", timeout, cancel)?
                .map(|text| parse_named_list(&text)),
            hwaccels: optional_listing(paths, "-hwaccels", timeout, cancel)?
                .map(|text| parse_hwaccels(&text)),
        };
        Ok((version, tables))
    }

    /// True when `set` lists `name`, or when the listing is unknown.
    fn has(set: &Option<HashSet<String>>, name: &str) -> bool {
        set.as_ref().is_none_or(|names| names.contains(name))
    }
}

/// Reads one informational FFmpeg option. `None` when FFmpeg cannot provide it; cancellation is an error.
fn optional_listing(
    paths: &FfmpegPaths,
    flag: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<Option<String>, HwError> {
    match list(paths, flag, timeout, cancel) {
        Ok(text) => Ok(Some(text)),
        Err(Stop::Cancelled) => Err(HwError::Cancelled),
        Err(Stop::Failed(_)) => Ok(None),
    }
}

/// Runs one informational FFmpeg option and returns its stdout.
fn list(
    paths: &FfmpegPaths,
    flag: &str,
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<String, Stop> {
    let command = FfmpegCommand::new_ffmpeg(paths).global_arg(flag)?;
    let bytes = capture_stdout(command, cancel, timeout)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The version token from the first line (`ffmpeg version n9.0.2-25-g67b60c310b-20261010 Copyright ...`).
fn parse_version(text: &str) -> String {
    text.lines()
        .next()
        .and_then(|line| {
            let mut words = line.split_whitespace();
            if words.any(|word| word == "version") {
                words.next()
            } else {
                None
            }
        })
        .map_or_else(|| "unknown".to_owned(), str::to_owned)
}

/// Names from a `-encoders`, `-decoders` or `-filters` listing: a flag column followed by the name.
fn parse_named_list(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let flags = words.next()?;
            let name = words.next()?;
            let flags_ok =
                flags.len() <= 6 && flags.chars().all(|c| c == '.' || c.is_ascii_uppercase());
            flags_ok.then(|| name.to_owned())
        })
        .collect()
}

/// Names from a `-hwaccels` listing: one per line after the "Hardware acceleration methods" header.
fn parse_hwaccels(text: &str) -> HashSet<String> {
    let mut in_list = false;
    let mut names = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Hardware acceleration methods") {
            in_list = true;
        } else if in_list && !trimmed.is_empty() {
            names.insert(trimmed.to_owned());
        }
    }
    names
}

/// A private folder for fixtures and encoder output, removed when the probe ends.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn create() -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("loopcut-hw-probe-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Container used for a codec's probe output.
fn container_for(codec: VideoCodec) -> Container {
    match codec {
        VideoCodec::Vp9 => Container::Webm,
        VideoCodec::H264 | VideoCodec::Hevc | VideoCodec::Av1 => Container::Mp4,
    }
}

/// Deterministic RGBA test pattern: a colour gradient that shifts between frames.
fn synthetic_frames(width: u32, height: u32, count: u32) -> Vec<Vec<u8>> {
    (0..count)
        .map(|frame| {
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height {
                for x in 0..width {
                    let r = x * 255 / width.max(1);
                    let g = y * 255 / height.max(1);
                    let b = (x + y + frame * 17) & 0xff;
                    pixels.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
                }
            }
            pixels
        })
        .collect()
}

/// Feeds `count` frames (cycling through `frames`) to an encoder, then closes its input so it can finish. Returns the
/// number of frames written.
fn write_frames(mut stdin: ChildStdin, frames: &[Vec<u8>], count: u32) -> io::Result<u64> {
    if frames.is_empty() {
        return Ok(0);
    }
    for index in 0..count as usize {
        stdin.write_all(&frames[index % frames.len()])?;
    }
    stdin.flush()?;
    Ok(u64::from(count))
}

/// Checks that the encoder counted every frame that was sent. `reported` is the last `frame=` value from its progress
/// output.
fn check_frame_count(sent: u64, reported: Option<u64>) -> Result<(), Stop> {
    match reported {
        Some(count) if count == sent => Ok(()),
        Some(count) => Err(Stop::Failed(format!(
            "the encoder reported {count} of {sent} frames"
        ))),
        None => Err(Stop::Failed(
            "the encoder reported no frame count".to_owned(),
        )),
    }
}

/// Decode request for one fixture through `chain`, scaled to `size`.
fn decode_request(
    input: &Path,
    fixture: Fixture,
    chain: DecodeChain,
    size: (u32, u32),
) -> DecodeRequest {
    DecodeRequest {
        input: input.to_path_buf(),
        stream_index: 0,
        coded_width: 1280,
        coded_height: 720,
        bit_depth: if fixture.ten_bit() { 10 } else { 8 },
        has_alpha: false,
        codec: fixture.codec_name().to_owned(),
        color: ColorInfo::SDR_BT709,
        start_time: 0.0,
        range: None,
        crop: None,
        size: Some(size),
        fps: Some(Rational::integer(FPS)),
        chain,
        layout: fixture.layout(),
    }
}

/// Mutable state of one probe run.
struct Prober<'a> {
    paths: &'a FfmpegPaths,
    opts: ProbeOptions,
    cancel: &'a CancelToken,
    scratch: PathBuf,
    tables: Tables,
    fixtures: [Option<PathBuf>; 3],
    warnings: Vec<String>,
    next_file: u32,
}

impl Prober<'_> {
    /// A fresh output name in the scratch folder. Encoder outputs must contain `.partial` (the encode builder refuses
    /// to overwrite anything else), so every name carries that marker.
    fn scratch_file(&mut self, stem: &str, extension: &str) -> PathBuf {
        self.next_file += 1;
        self.scratch
            .join(format!("{stem}-{}.partial.{extension}", self.next_file))
    }

    /// Runs `command` to completion with the probe timeout and cancellation, discarding its output.
    fn run_to_completion(&self, command: FfmpegCommand) -> Result<(), Stop> {
        let process = command.spawn(SpawnIo::default())?;
        process.wait_cancellable(self.cancel, Some(self.opts.timeout))?;
        Ok(())
    }

    /// Runs `command` and returns its stdout, with the probe timeout and cancellation.
    fn capture(&self, command: FfmpegCommand) -> Result<Vec<u8>, Stop> {
        capture_stdout(command, self.cancel, self.opts.timeout)
    }

    /// Path of a fixture, or a failure when it was not generated.
    fn fixture_path(&self, fixture: Fixture) -> Result<PathBuf, Stop> {
        self.fixtures[fixture.index()].clone().ok_or_else(|| {
            Stop::Failed(format!("the {} fixture was not generated", fixture.label()))
        })
    }

    /// Generates one fixture with a CPU encoder (the only way to produce a known-good stream of each codec).
    fn run_fixture(&self, fixture: Fixture) -> Result<PathBuf, Stop> {
        let output = self.scratch.join(fixture.file_name());
        let input = InputOptions {
            lavfi: true,
            duration: Some(1.0),
            ..InputOptions::default()
        };
        let command = FfmpegCommand::new_ffmpeg(self.paths)
            .global_arg("-y")?
            .input(format!("testsrc2=size=1280x720:rate={FPS}"), &input)?
            .output_args(fixture.encode_args().iter().copied())?
            .output(&output)?;
        self.run_to_completion(command)?;
        Ok(output)
    }

    /// Runs one decode request and checks that exactly [`DECODE_FRAMES`] frames came out.
    ///
    /// `build_decode_command` sets no frame limit, so this is a post-processing step: `-frames:v` is appended to the
    /// built command as an output option, after the builder's own output options and before the `pipe:1` target.
    /// FFmpeg then stops after that many frames, so each decode test does a fixed amount of work.
    fn decode_probe(&self, request: &DecodeRequest) -> Result<(), Stop> {
        let command = build_decode_command(self.paths, request)?
            .output_args(["-frames:v".to_owned(), DECODE_FRAMES.to_string()])?;
        let bytes = self.capture(command)?;
        let frame_bytes = request.frame_bytes().max(1);
        let expected = frame_bytes * DECODE_FRAMES as usize;
        if bytes.len() == expected {
            Ok(())
        } else {
            Err(Stop::Failed(format!(
                "decoded {} of {DECODE_FRAMES} frames",
                bytes.len() / frame_bytes
            )))
        }
    }

    /// Decodes every fixture in `fixtures` through `chain` at `size`. Stops at the first failure.
    fn decode_chain_probe(
        &self,
        chain: DecodeChain,
        fixtures: &[Fixture],
        size: (u32, u32),
    ) -> Result<(), Stop> {
        for &fixture in fixtures {
            let input = self.fixture_path(fixture)?;
            let request = decode_request(&input, fixture, chain, size);
            self.decode_probe(&request).map_err(|stop| match stop {
                Stop::Failed(message) => Stop::Failed(format!("{}: {message}", fixture.label())),
                cancelled @ Stop::Cancelled => cancelled,
            })?;
        }
        Ok(())
    }

    /// The reason a hardware backend cannot run in this FFmpeg build, if it cannot.
    fn missing_hwaccel(&self, backend_name: &str) -> Option<String> {
        (!Tables::has(&self.tables.hwaccels, backend_name))
            .then(|| format!("this FFmpeg build has no {backend_name} hwaccel"))
    }

    /// The reason a GPU scaler cannot run in this FFmpeg build, if it cannot.
    fn missing_filter(&self, scaler: GpuScaler) -> Option<String> {
        let filter = caps::scaler_filter(scaler)?;
        (!Tables::has(&self.tables.filters, filter))
            .then(|| format!("this FFmpeg build has no {filter} filter"))
    }

    fn run_decode(&self, backend: DecodeBackend, fixture: Fixture) -> Result<DecodeCap, HwError> {
        let backend_name = caps::decode_backend_name(backend);
        let mut cap = DecodeCap {
            codec: fixture.codec(),
            ten_bit: fixture.ten_bit(),
            backend: backend_name.to_owned(),
            status: CapStatus::Ok,
            speed_x: None,
            error: None,
        };
        if let Some(reason) = self.missing_hwaccel(backend_name) {
            cap.status = CapStatus::Unsupported;
            cap.error = Some(reason);
            return Ok(cap);
        }
        let chain = DecodeChain {
            backend,
            scaler: GpuScaler::None,
        };
        match self.decode_chain_probe(chain, &[fixture], DECODE_SIZE) {
            Ok(()) => Ok(cap),
            Err(Stop::Cancelled) => Err(HwError::Cancelled),
            Err(Stop::Failed(message)) => {
                cap.status = CapStatus::Failed;
                cap.error = Some(message);
                Ok(cap)
            }
        }
    }

    /// Tests a GPU scaler after the decode of one fixture on `backend`. Only called when that decode passed.
    fn run_scale(
        &self,
        backend: DecodeBackend,
        scaler: GpuScaler,
        fixture: Fixture,
    ) -> Result<ScalerCap, HwError> {
        let mut cap = ScalerCap {
            codec: fixture.codec(),
            scaler,
            backend: caps::decode_backend_name(backend).to_owned(),
            status: CapStatus::Ok,
            error: None,
        };
        if let Some(reason) = self.missing_filter(scaler) {
            cap.status = CapStatus::Unsupported;
            cap.error = Some(reason);
            return Ok(cap);
        }
        let chain = DecodeChain { backend, scaler };
        match self.decode_chain_probe(chain, &[fixture], DECODE_SIZE) {
            Ok(()) => Ok(cap),
            Err(Stop::Cancelled) => Err(HwError::Cancelled),
            Err(Stop::Failed(message)) => {
                cap.status = CapStatus::Failed;
                cap.error = Some(message);
                Ok(cap)
            }
        }
    }

    /// Encodes `frames` synthetic frames at `size` with the app's encode command and returns the wall time of the run.
    ///
    /// The run passes only when the encoder reports the same number of frames as were sent (see [`check_frame_count`]).
    /// Frames are generated before the timer starts, so the time covers the encoder and the pipe only.
    fn encode_once(
        &mut self,
        backend: EncoderBackend,
        codec: VideoCodec,
        size: (u32, u32),
        frames: u32,
        timeout: Duration,
    ) -> Result<Duration, Stop> {
        let container = container_for(codec);
        let extension = match container {
            Container::Mp4 => "mp4",
            Container::Webm => "webm",
        };
        let output = self.scratch_file("encode", extension);
        let (width, height) = size;
        let request = EncodeRequest {
            output: output.clone(),
            width,
            height,
            fps: Rational::integer(FPS),
            codec,
            backend,
            quality: QUALITY,
            container,
            keyint: None,
        };

        let frame_set = Arc::new(synthetic_frames(width, height, SYNTHETIC_FRAMES));
        let reported: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&reported);
        let command =
            build_encode_command(self.paths, &request)?.on_progress(move |progress: Progress| {
                if let Some(frame) = progress.frame
                    && let Ok(mut slot) = sink.lock()
                {
                    *slot = Some(frame);
                }
            });

        let started = Instant::now();
        let mut process = command.spawn(SpawnIo {
            stdin: true,
            stdout: false,
        })?;
        let Some(stdin) = process.take_stdin() else {
            return Err(Stop::Failed("encoder stdin pipe is missing".to_owned()));
        };
        let writer = thread::Builder::new()
            .name("hw-probe-stdin".to_owned())
            .spawn(move || write_frames(stdin, &frame_set, frames))?;
        // The writer ends when the encoder exits, is killed, or has all its frames, so this wait cannot hang on it.
        let outcome = process.wait_cancellable(self.cancel, Some(timeout));
        let written = writer.join();
        outcome?;
        let elapsed = started.elapsed();

        let sent = match written {
            Ok(Ok(count)) => count,
            Ok(Err(err)) => {
                return Err(Stop::Failed(format!(
                    "could not write every frame to the encoder: {err}"
                )));
            }
            Err(_) => return Err(Stop::Failed("frame writer panicked".to_owned())),
        };
        let reported = reported.lock().ok().and_then(|slot| *slot);
        check_frame_count(sent, reported)?;

        let bytes = fs::metadata(&output)?.len();
        let _ = fs::remove_file(&output);
        if bytes == 0 {
            return Err(Stop::Failed("the encoder wrote an empty file".to_owned()));
        }
        Ok(elapsed)
    }

    /// Measures encode speed at 1280x720 with [`BENCH_FRAMES`] frames and returns video seconds per wall second. A
    /// failure is a warning, not an error, because the encoder itself already passed.
    fn benchmark(
        &mut self,
        label: &str,
        backend: EncoderBackend,
        codec: VideoCodec,
    ) -> Result<Option<f32>, HwError> {
        let timeout = self.opts.timeout.max(BENCH_MIN_TIMEOUT);
        match self.encode_once(backend, codec, BENCH_SIZE, BENCH_FRAMES, timeout) {
            Ok(elapsed) => {
                let seconds = elapsed.as_secs_f64().max(1e-3);
                let video_seconds = f64::from(BENCH_FRAMES) / f64::from(FPS);
                Ok(Some((video_seconds / seconds) as f32))
            }
            Err(Stop::Cancelled) => Err(HwError::Cancelled),
            Err(Stop::Failed(message)) => {
                self.warnings
                    .push(format!("{label} benchmark failed: {message}"));
                Ok(None)
            }
        }
    }

    /// Finds the smallest working size for a hardware encoder and optionally benchmarks it.
    fn run_encode(
        &mut self,
        backend: EncoderBackend,
        codec: VideoCodec,
    ) -> Result<EncodeCap, HwError> {
        let name = caps::encoder_name(backend);
        let mut cap = EncodeCap {
            codec,
            backend: name.to_owned(),
            status: CapStatus::Failed,
            min_width: None,
            min_height: None,
            speed_x: None,
            error: None,
        };
        let Some(encoder) = backend.ffmpeg_encoder(codec) else {
            cap.status = CapStatus::Unsupported;
            cap.error = Some("the backend has no encoder for this codec".to_owned());
            return Ok(cap);
        };
        if !Tables::has(&self.tables.encoders, encoder) {
            cap.status = CapStatus::Unsupported;
            cap.error = Some(format!("this FFmpeg build has no {encoder}"));
            return Ok(cap);
        }

        let timeout = self.opts.timeout;
        let mut last_error = "no size was tried".to_owned();
        for (width, height) in SIZE_CANDIDATES {
            match self.encode_once(backend, codec, (width, height), ENCODE_FRAMES, timeout) {
                Ok(_) => {
                    cap.status = CapStatus::Ok;
                    cap.min_width = Some(width);
                    cap.min_height = Some(height);
                    break;
                }
                Err(Stop::Cancelled) => return Err(HwError::Cancelled),
                Err(Stop::Failed(message)) => last_error = format!("{width}x{height}: {message}"),
            }
        }

        if cap.status == CapStatus::Ok {
            if self.opts.benchmark {
                cap.speed_x = self.benchmark(encoder, backend, codec)?;
            }
        } else {
            cap.error = Some(last_error);
        }
        Ok(cap)
    }

    /// Quick check of a CPU encoder with the app's command. Benchmarks the codec's reference encoder when requested.
    fn run_cpu_encode(&mut self, codec: VideoCodec) -> Result<EncodeCap, HwError> {
        let encoder = caps::cpu_encoder_name(codec);
        let backend = EncoderBackend::Cpu;
        let mut cap = EncodeCap {
            codec,
            backend: encoder.to_owned(),
            status: CapStatus::Failed,
            min_width: None,
            min_height: None,
            speed_x: None,
            error: None,
        };
        if !Tables::has(&self.tables.encoders, encoder) {
            cap.status = CapStatus::Unsupported;
            cap.error = Some(format!("this FFmpeg build has no {encoder}"));
            return Ok(cap);
        }
        let timeout = self.opts.timeout;
        match self.encode_once(backend, codec, CPU_SIZE, ENCODE_FRAMES, timeout) {
            Ok(_) => cap.status = CapStatus::Ok,
            Err(Stop::Cancelled) => return Err(HwError::Cancelled),
            Err(Stop::Failed(message)) => cap.error = Some(message),
        }
        if cap.status == CapStatus::Ok && self.opts.benchmark && BENCH_CPU_CODECS.contains(&codec) {
            cap.speed_x = self.benchmark(encoder, backend, codec)?;
        }
        Ok(cap)
    }
}

/// Why AMF cannot run on `device`, when that applies. AMF runs only on the default AMD adapter, so every other AMD
/// adapter gets a reason. Non-AMD and software adapters get none.
fn amf_block_reason(device: &GpuDevice, default: Option<&GpuDevice>) -> Option<String> {
    if device.is_software || device.vendor != GpuVendor::Amd {
        return None;
    }
    let owner = default?;
    (owner.dxgi_index != device.dxgi_index).then(|| {
        format!(
            "AMF runs only on the default AMD adapter, {} (DXGI #{}); this adapter is DXGI #{} and uses D3D11VA",
            owner.key.name, owner.dxgi_index, device.dxgi_index
        )
    })
}

/// Records every decode of `backend` as unsupported, one entry per fixture.
fn unsupported_decodes(out: &mut DeviceCaps, backend: &str, reason: &str) {
    for fixture in Fixture::ALL {
        out.decode.push(DecodeCap {
            codec: fixture.codec(),
            ten_bit: fixture.ten_bit(),
            backend: backend.to_owned(),
            status: CapStatus::Unsupported,
            speed_x: None,
            error: Some(reason.to_owned()),
        });
    }
}

/// Records one encoder of one codec as unsupported.
fn unsupported_encode(codec: VideoCodec, backend: &str, reason: String) -> EncodeCap {
    EncodeCap {
        codec,
        backend: backend.to_owned(),
        status: CapStatus::Unsupported,
        min_width: None,
        min_height: None,
        speed_x: None,
        error: Some(reason),
    }
}

/// Adds the decode and encode tests that apply to one adapter. `amf_block` is the reason AMF cannot run here, if it
/// cannot (see [`amf_block_reason`]). Backends the adapter cannot have are recorded as unsupported immediately, without
/// a job.
fn plan_device(
    index: usize,
    device: &GpuDevice,
    amf_block: Option<&str>,
    out: &mut DeviceCaps,
    jobs: &mut Vec<Job>,
) {
    let decode_names: Vec<&str> = match caps::native_decode_name(device.vendor) {
        Some(native) => vec![native, "d3d11va"],
        None => vec!["d3d11va"],
    };
    for name in decode_names {
        if let Some(reason) = amf_block.filter(|_| name == "amf") {
            unsupported_decodes(out, name, reason);
            continue;
        }
        match caps::decode_backend_for(name, device) {
            Some(backend) => {
                for fixture in Fixture::ALL {
                    jobs.push(Job::Decode {
                        device: index,
                        backend,
                        fixture,
                    });
                }
            }
            None => {
                let reason = format!("{name} needs a device index this adapter does not have");
                unsupported_decodes(out, name, &reason);
            }
        }
    }

    let Some(encoder_name) = caps::native_encode_name(device.vendor) else {
        return;
    };
    for codec in HW_CODECS {
        if let Some(reason) = amf_block.filter(|_| encoder_name == "amf") {
            out.encode
                .push(unsupported_encode(codec, encoder_name, reason.to_owned()));
            continue;
        }
        match caps::encode_backend_for(encoder_name, device) {
            Some(backend) => jobs.push(Job::Encode {
                device: index,
                backend,
                codec,
            }),
            None => out.encode.push(unsupported_encode(
                codec,
                encoder_name,
                format!("{encoder_name} needs a device index this adapter does not have"),
            )),
        }
    }
}

/// Human-readable label for a planned job, used in progress events.
fn job_label(job: &Job, devices: &[GpuDevice]) -> String {
    let adapter = |index: usize| {
        devices
            .get(index)
            .map_or("adapter", |device| device.key.name.as_str())
            .to_owned()
    };
    match *job {
        Job::Fixture(fixture) => format!("generate {} fixture", fixture.label()),
        Job::CpuEncode(codec) => format!(
            "CPU encoder {} ({})",
            caps::cpu_encoder_name(codec),
            caps::codec_name(codec)
        ),
        Job::Decode {
            device,
            backend,
            fixture,
        } => {
            let scaled = caps::native_scaler_for(caps::decode_backend_name(backend)).is_some();
            format!(
                "{}: decode {} via {}{}",
                adapter(device),
                fixture.label(),
                caps::decode_backend_name(backend),
                if scaled { ", then GPU scale" } else { "" }
            )
        }
        Job::Encode {
            device,
            backend,
            codec,
        } => format!(
            "{}: encode {} with {}",
            adapter(device),
            caps::codec_name(codec),
            caps::encoder_name(backend)
        ),
    }
}

/// Probes FFmpeg and the given adapters, returning what works and the smallest working encoder sizes.
///
/// `on_event` receives a [`ProbeEvent::Step`] before and a [`ProbeEvent::Result`] after each test. Software adapters
/// are listed in the result but get no tests. Each FFmpeg child is limited to `opts.timeout` and stops when `cancel` is
/// set. Benchmark runs use at least 120 seconds.
///
/// # Errors
///
/// [`HwError::Cancelled`] when `cancel` is set during the probe, including while the FFmpeg listings are read, or
/// [`HwError::Io`] when the scratch folder cannot be created. Test failures are recorded in the result instead.
pub fn probe_capabilities(
    paths: &FfmpegPaths,
    gpus: &[GpuDevice],
    opts: &ProbeOptions,
    on_event: &mut dyn FnMut(ProbeEvent),
    cancel: &CancelToken,
) -> Result<Capabilities, HwError> {
    if cancel.is_cancelled() {
        return Err(HwError::Cancelled);
    }
    let scratch = ScratchDir::create()?;
    let (ffmpeg_version, tables) = Tables::load(paths, opts.timeout, cancel)?;
    let mut prober = Prober {
        paths,
        opts: *opts,
        cancel,
        scratch: scratch.0.clone(),
        tables,
        fixtures: [None, None, None],
        warnings: Vec::new(),
        next_file: 0,
    };

    let mut devices: Vec<DeviceCaps> = gpus
        .iter()
        .map(|device| DeviceCaps {
            device: device.clone(),
            decode: Vec::new(),
            scale: Vec::new(),
            encode: Vec::new(),
        })
        .collect();
    let mut jobs: Vec<Job> = Fixture::ALL.into_iter().map(Job::Fixture).collect();
    jobs.extend(CPU_CODECS.into_iter().map(Job::CpuEncode));
    let amf_default = devices::amf_default_adapter(gpus);
    for (index, device) in gpus.iter().enumerate() {
        if !device.is_software {
            let amf_block = amf_block_reason(device, amf_default);
            plan_device(
                index,
                device,
                amf_block.as_deref(),
                &mut devices[index],
                &mut jobs,
            );
        }
    }

    let mut cpu_encode = Vec::new();
    let total = u32::try_from(jobs.len()).unwrap_or(u32::MAX);
    for (done, job) in jobs.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(HwError::Cancelled);
        }
        let label = job_label(job, gpus);
        on_event(ProbeEvent::Step {
            label: label.clone(),
            done: u32::try_from(done).unwrap_or(u32::MAX),
            total,
        });
        let ok = match *job {
            Job::Fixture(fixture) => match prober.run_fixture(fixture) {
                Ok(path) => {
                    prober.fixtures[fixture.index()] = Some(path);
                    true
                }
                Err(Stop::Cancelled) => return Err(HwError::Cancelled),
                Err(Stop::Failed(message)) => {
                    prober.warnings.push(format!(
                        "{} fixture was not generated: {message}",
                        fixture.label()
                    ));
                    false
                }
            },
            Job::CpuEncode(codec) => {
                let cap = prober.run_cpu_encode(codec)?;
                let ok = cap.status == CapStatus::Ok;
                cpu_encode.push(cap);
                ok
            }
            Job::Decode {
                device,
                backend,
                fixture,
            } => {
                let cap = prober.run_decode(backend, fixture)?;
                let ok = cap.status == CapStatus::Ok;
                devices[device].decode.push(cap);
                if ok
                    && let Some(scaler) =
                        caps::native_scaler_for(caps::decode_backend_name(backend))
                {
                    let scale = prober.run_scale(backend, scaler, fixture)?;
                    devices[device].scale.push(scale);
                }
                ok
            }
            Job::Encode {
                device,
                backend,
                codec,
            } => {
                let cap = prober.run_encode(backend, codec)?;
                let ok = cap.status == CapStatus::Ok;
                devices[device].encode.push(cap);
                ok
            }
        };
        on_event(ProbeEvent::Result { label, ok });
    }

    let mut warnings = std::mem::take(&mut prober.warnings);
    summarize(&devices, amf_default, &mut warnings);
    let probed_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    Ok(Capabilities {
        schema: caps::SCHEMA_VERSION,
        ffmpeg_version,
        probed_at_unix,
        devices,
        cpu_encode,
        warnings,
    })
}

/// Adds warnings for results that deserve a look in diagnostics: failed hardware tests, missing CUDA indices, and the
/// AMF adapter rule when more than one AMD adapter is present.
fn summarize(devices: &[DeviceCaps], amf_default: Option<&GpuDevice>, warnings: &mut Vec<String>) {
    let adapters: Vec<&DeviceCaps> = devices
        .iter()
        .filter(|device| !device.device.is_software)
        .collect();
    if adapters.is_empty() {
        warnings.push("no hardware adapter found; only CPU encoders are available".to_owned());
    }
    let amd_count = adapters
        .iter()
        .filter(|device| device.device.vendor == GpuVendor::Amd)
        .count();
    if amd_count > 1
        && let Some(owner) = amf_default
    {
        warnings.push(format!(
            "AMF runs only on the default AMD adapter, {} (DXGI #{}); the other AMD adapters use D3D11VA decode and CPU encode",
            owner.key.name, owner.dxgi_index
        ));
    }
    for device in adapters {
        let name = device.device.key.name.as_str();
        if device.device.vendor == GpuVendor::Nvidia && device.device.cuda_index.is_none() {
            warnings.push(format!(
                "{name}: no CUDA device matches this adapter, so NVDEC and NVENC are skipped"
            ));
        }
        for cap in device
            .decode
            .iter()
            .filter(|cap| cap.status == CapStatus::Failed)
        {
            warnings.push(format!(
                "{name}: {} {}-bit decode via {} failed: {}",
                caps::codec_name(cap.codec),
                if cap.ten_bit { 10 } else { 8 },
                cap.backend,
                cap.error.as_deref().unwrap_or("unknown error")
            ));
        }
        for cap in device
            .scale
            .iter()
            .filter(|cap| cap.status == CapStatus::Failed)
        {
            warnings.push(format!(
                "{name}: {} GPU scale {} after {} failed: {}",
                caps::codec_name(cap.codec),
                caps::scaler_filter(cap.scaler).unwrap_or("none"),
                cap.backend,
                cap.error.as_deref().unwrap_or("unknown error")
            ));
        }
        for cap in device
            .encode
            .iter()
            .filter(|cap| cap.status == CapStatus::Failed)
        {
            warnings.push(format!(
                "{name}: {} encode with {} failed: {}",
                caps::codec_name(cap.codec),
                cap.backend,
                cap.error.as_deref().unwrap_or("unknown error")
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use loopcut_core::GpuKey;

    use super::*;

    #[test]
    fn version_is_the_second_word_after_version() {
        let text =
            "ffmpeg version n9.0.2-25-g67b60c310b-20261010 Copyright (c) 2000-2026\nbuilt with gcc";
        assert_eq!(parse_version(text), "n9.0.2-25-g67b60c310b-20261010");
        assert_eq!(parse_version("nonsense"), "unknown");
    }

    #[test]
    fn encoder_and_filter_listings_parse_names() {
        let text = "Encoders:\n V..... = Video\n ------\n V....D libx264              libx264 H.264\n V....D h264_amf             AMD AMF H.264 Encoder\n A....D aac                  AAC\n";
        let names = parse_named_list(text);
        assert!(names.contains("libx264"));
        assert!(names.contains("h264_amf"));
        assert!(names.contains("aac"));
        assert!(!names.contains("Encoders:"));

        let filters = parse_named_list(
            " ... vpp_amf            V->V       AMF video scaling\n .. scale_d3d11       V->V       Scale video using Direct3D11\n",
        );
        assert!(filters.contains("vpp_amf"));
        assert!(filters.contains("scale_d3d11"));
    }

    #[test]
    fn hwaccel_listing_parses_names_after_the_header() {
        let text = "ffmpeg noise\nHardware acceleration methods:\ncuda\nd3d11va\namf\n";
        let names = parse_hwaccels(text);
        assert_eq!(names.len(), 3);
        assert!(names.contains("d3d11va"));
        assert!(!names.contains("ffmpeg noise"));
    }

    #[test]
    fn synthetic_frames_have_the_right_size_and_vary() {
        let frames = synthetic_frames(16, 8, 3);
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|frame| frame.len() == 16 * 8 * 4));
        assert_ne!(frames[0], frames[1]);
    }

    #[test]
    fn long_errors_are_truncated_on_a_character_boundary() {
        let text = "é".repeat(200);
        let cut = truncate(text, 301);
        assert!(cut.len() <= 304);
        assert!(cut.ends_with("..."));
    }

    #[test]
    fn media_failures_keep_the_last_two_stderr_lines() {
        let err = MediaError::Failed {
            code: Some(1),
            stderr_tail: "first\nsecond\n\nthird line\n".to_owned(),
        };
        let text = describe_media_error(&err);
        assert!(text.contains("second | third line"), "{text}");
        assert!(!text.contains("first"), "{text}");
    }

    #[test]
    fn fixtures_have_distinct_outputs() {
        let names: HashSet<&str> = Fixture::ALL.iter().map(|f| f.file_name()).collect();
        assert_eq!(names.len(), 3);
        assert!(Fixture::HevcTenBit.ten_bit());
        assert_eq!(Fixture::HevcTenBit.layout(), PixelLayout::P010);
        assert_eq!(Fixture::H264.layout(), PixelLayout::Nv12);
    }

    #[test]
    fn an_encode_passes_only_when_the_encoder_counted_every_sent_frame() {
        assert!(check_frame_count(15, Some(15)).is_ok());
        assert!(
            matches!(check_frame_count(15, Some(14)), Err(Stop::Failed(message)) if message.contains("14 of 15")),
            "a short encode must fail"
        );
        assert!(
            matches!(check_frame_count(15, None), Err(Stop::Failed(_))),
            "a missing frame count must fail"
        );
    }

    fn gpu(name: &str, vendor_id: u32, dxgi_index: u32) -> GpuDevice {
        GpuDevice {
            key: GpuKey {
                vendor_id,
                device_id: 0x7550,
                ordinal: 0,
                name: name.to_owned(),
            },
            vendor: GpuVendor::from_pci_id(vendor_id),
            dxgi_index,
            luid: u64::from(dxgi_index) + 1,
            dedicated_vram_mb: 16_368,
            driver_version: None,
            is_software: false,
            cuda_index: None,
        }
    }

    /// Layout: an APU (DXGI 0, the default AMD adapter for AMF) and a discrete card (DXGI 1).
    fn apu_and_dgpu() -> (GpuDevice, GpuDevice) {
        (
            gpu("AMD Radeon 890M", 0x1002, 0),
            gpu("AMD Radeon RX 9060 XT", 0x1002, 1),
        )
    }

    fn jobs_for(device: &GpuDevice, amf_block: Option<&str>) -> (DeviceCaps, Vec<Job>) {
        let mut out = DeviceCaps {
            device: device.clone(),
            decode: Vec::new(),
            scale: Vec::new(),
            encode: Vec::new(),
        };
        let mut jobs = Vec::new();
        plan_device(0, device, amf_block, &mut out, &mut jobs);
        (out, jobs)
    }

    #[test]
    fn the_default_amd_adapter_gets_amf_jobs() {
        let (apu, dgpu) = apu_and_dgpu();
        let adapters = [apu.clone(), dgpu.clone()];
        let default = devices::amf_default_adapter(&adapters).expect("default");
        assert_eq!(default.dxgi_index, 0);

        let block = amf_block_reason(&apu, Some(&apu));
        assert!(block.is_none(), "the default adapter is not blocked");
        let (_, jobs) = jobs_for(&apu, block.as_deref());
        assert!(
            jobs.iter().any(|job| matches!(
                job,
                Job::Decode {
                    backend: DecodeBackend::Amf { .. },
                    ..
                }
            )),
            "AMF decode jobs on the default adapter"
        );
        assert!(
            jobs.iter().any(|job| matches!(
                job,
                Job::Encode {
                    backend: EncoderBackend::Amf { .. },
                    ..
                }
            )),
            "AMF encode jobs on the default adapter"
        );
    }

    #[test]
    fn a_non_default_amd_adapter_gets_no_amf_jobs_and_says_why() {
        let (apu, dgpu) = apu_and_dgpu();
        let block = amf_block_reason(&dgpu, Some(&apu));
        let reason = block.clone().expect("the discrete card is blocked");
        assert!(reason.contains("default AMD adapter"), "{reason}");
        assert!(
            reason.contains("Radeon 890M"),
            "the owner is named: {reason}"
        );

        let (out, jobs) = jobs_for(&dgpu, block.as_deref());
        assert!(
            !jobs.iter().any(|job| matches!(
                job,
                Job::Decode {
                    backend: DecodeBackend::Amf { .. },
                    ..
                } | Job::Encode {
                    backend: EncoderBackend::Amf { .. },
                    ..
                }
            )),
            "no AMF job may run on the non-default adapter"
        );
        // D3D11VA still runs on this adapter.
        assert!(jobs.iter().any(|job| matches!(
            job,
            Job::Decode {
                backend: DecodeBackend::D3d11va { dxgi_index: 1 },
                ..
            }
        )));

        let amf_decodes: Vec<&DecodeCap> = out
            .decode
            .iter()
            .filter(|cap| cap.backend == "amf")
            .collect();
        assert_eq!(amf_decodes.len(), 3, "one unsupported entry per fixture");
        assert!(
            amf_decodes
                .iter()
                .all(|cap| cap.status == CapStatus::Unsupported
                    && cap.error.as_deref() == Some(reason.as_str()))
        );
        let amf_encodes: Vec<&EncodeCap> = out
            .encode
            .iter()
            .filter(|cap| cap.backend == "amf")
            .collect();
        assert_eq!(amf_encodes.len(), 3);
        assert!(
            amf_encodes
                .iter()
                .all(|cap| cap.status == CapStatus::Unsupported)
        );
        assert!(out.scale.iter().all(|cap| cap.backend != "amf"));
    }

    #[test]
    fn amf_is_not_blocked_on_nvidia_or_without_a_default() {
        let nvidia = gpu("NVIDIA GeForce RTX 4090", 0x10DE, 0);
        let (apu, _) = apu_and_dgpu();
        assert!(amf_block_reason(&nvidia, Some(&apu)).is_none());
        assert!(amf_block_reason(&apu, None).is_none());
    }

    #[test]
    fn a_cancelled_listing_is_an_error_not_an_empty_table() {
        let Some(paths) = FfmpegPaths::discover_dev() else {
            eprintln!("skipping: no staged FFmpeg build");
            return;
        };
        // The token is set before the child starts, so the wait kills it on its first poll and reports cancellation.
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = Tables::load(&paths, Duration::from_secs(20), &cancel);
        assert!(matches!(result, Err(HwError::Cancelled)), "{result:?}");
    }

    #[test]
    fn the_benchmark_references_each_codec_with_its_own_cpu_encoder() {
        // The CPU reference for a codec must be the encoder of that codec, so selection compares like with like.
        for codec in BENCH_CPU_CODECS {
            assert_eq!(
                caps::cpu_encoder_name(codec),
                match codec {
                    VideoCodec::H264 => "libx264",
                    VideoCodec::Hevc => "libx265",
                    _ => "libsvtav1",
                }
            );
        }
    }
}

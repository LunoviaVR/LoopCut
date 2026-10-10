//! Spawning FFmpeg and ffprobe safely.
//!
//! [`FfmpegCommand`] is an argument builder that enforces the rules every LoopCut invocation follows:
//!
//! * `-hide_banner` and an explicit `-loglevel` come first; ffmpeg also gets `-nostdin` and `-progress pipe:2`.
//! * Every file input gets `-protocol_whitelist file,pipe` and (ffmpeg only) `-noautorotate` immediately before its
//!   `-i`, so rotation is handled by LoopCut's compositor and protocols other than local files are refused.
//! * URL-like paths (`http:`, `concat:`, `file:`, `subfile,`, `pipe:`, `-`, anything with `://`) are rejected.
//!
//! [`FfmpegCommand::spawn`] starts the process with no console window, assigns it to the process-wide kill-on-close
//! Job Object, and drains stderr on a background thread. That thread parses `-progress` blocks (delivering
//! [`Progress`] to the callback) and keeps the last [`STDERR_TAIL_LINES`] diagnostic lines for error messages.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::MediaError;
use crate::job;
use crate::paths::FfmpegPaths;

/// `CREATE_NO_WINDOW`: child processes of this GUI app must not flash a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Number of recent non-progress stderr lines kept for error messages.
pub const STDERR_TAIL_LINES: usize = 50;

/// Longest stderr line kept, in bytes. A longer line is cut and marked `[truncated]`, so a misbehaving or hostile
/// child cannot make the diagnostic buffer grow without bound.
pub const MAX_STDERR_LINE_BYTES: usize = 8 * 1024;

/// The only protocols LoopCut allows FFmpeg to open for inputs.
const PROTOCOL_WHITELIST: &str = "file,pipe";

/// How often timed and cancellable waits check the process state.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Options that the builder owns or that read or write files outside the declared input and output. They are refused
/// in every caller-supplied position: global, input and output arguments alike.
const RESERVED_OPTIONS: [&str; 8] = [
    "-i",
    "-protocol_whitelist",
    "-filter_script",
    "-filter_complex_script",
    "-attach",
    "-passlogfile",
    "-sdp_file",
    "-dump_attachment",
];

/// Input options that the builder owns; passing them through [`InputOptions`] would bypass the safety rules.
const RESERVED_INPUT_OPTIONS: [&str; 1] = ["-f"];

/// Substrings that make a filter graph or lavfi source read, fetch or write other data. Trusted, code-generated
/// graphs never contain them. Caller-supplied arguments are scanned for them, so `-vf movie=...` is refused.
const FORBIDDEN_GRAPH_FRAGMENTS: [&str; 9] = [
    "://",
    "movie",
    "filename=",
    "textfile=",
    "file=",
    "subtitles=",
    "ass=",
    "subfile",
    "concat:",
];

/// Process-wide lock that serializes `Command::spawn`. Two concurrent spawns can each inherit the other's pipe ends
/// while both are being created, which keeps a pipe open after its owner closed it and can stall a later `wait`.
static SPAWN_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Callback type for progress snapshots. It runs on the stderr drain thread.
pub type ProgressCallback = Box<dyn FnMut(Progress) + Send + 'static>;

/// One snapshot of FFmpeg's `-progress` output, emitted when a block ends with `progress=`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// Frames written so far.
    pub frame: Option<u64>,
    /// Output position in microseconds, from `out_time_us`. (`out_time_ms` is also microseconds, so it is ignored.)
    pub out_time_us: Option<i64>,
    /// Processing speed relative to realtime (`speed=1.5x` is 1.5).
    pub speed: Option<f64>,
    /// True for the final block (`progress=end`).
    pub done: bool,
}

/// Classification of one stderr line by [`ProgressParser::feed`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Feed {
    /// A `key=value` line belonging to a progress block; the block is not complete yet.
    Partial,
    /// The `progress=` line closed a block; the value is that block's snapshot.
    Block(Progress),
    /// Any other diagnostic text.
    Message,
}

/// Incremental parser for `-progress pipe:2` output.
///
/// Feed it every stderr line in order. Progress keys are recognised by name, so error messages that merely contain
/// `=` are still treated as diagnostics.
#[derive(Debug, Default)]
pub struct ProgressParser {
    current: Progress,
}

impl ProgressParser {
    /// Classifies one line (without its line terminator).
    pub fn feed(&mut self, line: &str) -> Feed {
        let Some((key, value)) = line.split_once('=') else {
            return Feed::Message;
        };
        if !is_progress_key(key) {
            return Feed::Message;
        }
        let value = value.trim();
        match key {
            "frame" => self.current.frame = value.parse().ok(),
            "out_time_us" => self.current.out_time_us = value.parse().ok(),
            "speed" => {
                self.current.speed = value
                    .trim_end_matches('x')
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|speed| speed.is_finite());
            }
            "progress" => {
                let mut block = std::mem::take(&mut self.current);
                block.done = value == "end";
                return Feed::Block(block);
            }
            _ => {}
        }
        Feed::Partial
    }
}

/// Keys FFmpeg writes in `-progress` blocks.
fn is_progress_key(key: &str) -> bool {
    matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "out_time_us"
            | "out_time_ms"
            | "out_time"
            | "dup_frames"
            | "drop_frames"
            | "speed"
            | "progress"
    ) || key.starts_with("stream_")
}

/// Log verbosity passed to `-loglevel`. Defaults to [`LogLevel::Error`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogLevel {
    Quiet,
    Fatal,
    #[default]
    Error,
    Warning,
    Info,
    Verbose,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Fatal => "fatal",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Verbose => "verbose",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Options for one input, placed before its `-i`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputOptions {
    /// Input seek in seconds (`-ss` before `-i`).
    pub seek: Option<f64>,
    /// Maximum input duration in seconds (`-t` before `-i`).
    pub duration: Option<f64>,
    /// Hardware decode options, e.g. `["-hwaccel", "amf", "-hwaccel_output_format", "amf"]`.
    pub hwaccel_args: Vec<String>,
    /// Input demuxer (`-f`), e.g. `rawvideo` for raw frames on stdin.
    pub format: Option<String>,
    /// Other input options. `-i`, `-f` and `-protocol_whitelist` are rejected here; use the fields above.
    pub extra: Vec<String>,
    /// The input is a lavfi filtergraph (`-f lavfi -i <graph>`). Only trusted, code-generated graphs may use this.
    pub lavfi: bool,
}

/// Which standard streams of the child the caller wants piped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpawnIo {
    /// Pipe stdin so the caller can write (for example raw frames for an encoder).
    pub stdin: bool,
    /// Pipe stdout so the caller can read (for example decoded raw frames or ffprobe JSON).
    pub stdout: bool,
}

/// Which executable a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Ffmpeg,
    Ffprobe,
}

/// Kind of input being added, which decides which safety options apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    File,
    Stdin,
    Lavfi,
}

/// Builder for one ffmpeg or ffprobe invocation. See the module documentation for the enforced rules.
pub struct FfmpegCommand {
    program: PathBuf,
    tool: Tool,
    loglevel: LogLevel,
    global: Vec<OsString>,
    inputs: Vec<OsString>,
    output_args: Vec<OsString>,
    output_target: Option<OsString>,
    progress: Option<ProgressCallback>,
}

impl fmt::Debug for FfmpegCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfmpegCommand")
            .field("program", &self.program)
            .field("args", &self.args())
            .finish()
    }
}

impl FfmpegCommand {
    /// Starts a command for the `ffmpeg` executable in `paths`.
    pub fn new_ffmpeg(paths: &FfmpegPaths) -> Self {
        Self::new(paths.ffmpeg.clone(), Tool::Ffmpeg)
    }

    /// Starts a command for the `ffprobe` executable in `paths`.
    pub fn new_ffprobe(paths: &FfmpegPaths) -> Self {
        Self::new(paths.ffprobe.clone(), Tool::Ffprobe)
    }

    fn new(program: PathBuf, tool: Tool) -> Self {
        Self {
            program,
            tool,
            loglevel: LogLevel::default(),
            global: Vec::new(),
            inputs: Vec::new(),
            output_args: Vec::new(),
            output_target: None,
            progress: None,
        }
    }

    /// Sets `-loglevel` (default [`LogLevel::Error`]).
    #[must_use]
    pub fn loglevel(mut self, level: LogLevel) -> Self {
        self.loglevel = level;
        self
    }

    /// Adds one global option, such as `-y` or `-threads`.
    ///
    /// # Errors
    ///
    /// [`MediaError::InvalidArgument`] for options that read or write other files or replace the inputs (see
    /// [`RESERVED_OPTIONS`]), and for values that contain file-reading filter fragments.
    pub fn global_arg(mut self, arg: impl Into<OsString>) -> Result<Self, MediaError> {
        let arg = arg.into();
        validate_caller_args(std::slice::from_ref(&arg))?;
        self.global.push(arg);
        Ok(self)
    }

    /// Adds several global options in order. See [`Self::global_arg`] for the refused options.
    ///
    /// # Errors
    ///
    /// The same errors as [`Self::global_arg`]. Nothing is added when any argument is refused.
    pub fn global_args<I, S>(mut self, args: I) -> Result<Self, MediaError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        validate_caller_args(&args)?;
        self.global.extend(args);
        Ok(self)
    }

    /// Adds a local file input (or a trusted lavfi graph when `opts.lavfi` is set).
    ///
    /// # Errors
    ///
    /// [`MediaError::UrlNotAllowed`] for URL-like paths, [`MediaError::LavfiRejected`] for unsafe graphs, and
    /// [`MediaError::InvalidArgument`] for malformed options.
    pub fn input(
        self,
        path_or_spec: impl AsRef<OsStr>,
        opts: &InputOptions,
    ) -> Result<Self, MediaError> {
        let target = path_or_spec.as_ref();
        if opts.lavfi {
            self.push_input(target.to_os_string(), InputKind::Lavfi, opts)
        } else {
            validate_path(target)?;
            self.push_input(target.to_os_string(), InputKind::File, opts)
        }
    }

    /// Adds stdin as an input (`-i pipe:0`). Pair it with [`SpawnIo::stdin`] when spawning.
    ///
    /// # Errors
    ///
    /// [`MediaError::InvalidArgument`] for malformed options.
    pub fn input_stdin(self, opts: &InputOptions) -> Result<Self, MediaError> {
        self.push_input(OsString::from("pipe:0"), InputKind::Stdin, opts)
    }

    /// Adds options that follow the inputs and precede the output (for example codec and filter options).
    ///
    /// # Errors
    ///
    /// The same refusals as [`Self::global_arg`]. Nothing is added when any argument is refused.
    pub fn output_args<I, S>(mut self, args: I) -> Result<Self, MediaError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        validate_caller_args(&args)?;
        self.output_args.extend(args);
        Ok(self)
    }

    /// Sets the output file, which is always the last argument.
    ///
    /// # Errors
    ///
    /// [`MediaError::UrlNotAllowed`] for URL-like output targets. Use [`Self::output_stdout`] for stdout.
    pub fn output(mut self, path: impl AsRef<OsStr>) -> Result<Self, MediaError> {
        validate_path(path.as_ref())?;
        self.output_target = Some(path.as_ref().to_os_string());
        Ok(self)
    }

    /// Writes the output to stdout (`pipe:1`). Pair it with [`SpawnIo::stdout`] when spawning.
    #[must_use]
    pub fn output_stdout(mut self) -> Self {
        self.output_target = Some(OsString::from("pipe:1"));
        self
    }

    /// Receives progress snapshots on the stderr drain thread.
    #[must_use]
    pub fn on_progress(mut self, callback: impl FnMut(Progress) + Send + 'static) -> Self {
        self.progress = Some(Box::new(callback));
        self
    }

    /// The full argument list, in the order the process receives it. The program itself is not included.
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from(self.loglevel.as_arg()),
        ];
        if self.tool == Tool::Ffmpeg {
            args.extend(["-nostdin", "-progress", "pipe:2", "-nostats"].map(OsString::from));
        }
        args.extend(self.global.iter().cloned());
        args.extend(self.inputs.iter().cloned());
        args.extend(self.output_args.iter().cloned());
        if let Some(target) = &self.output_target {
            args.push(target.clone());
        }
        args
    }

    /// The executable this command runs.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Starts the process, assigns it to the kill-on-close job and begins draining stderr.
    ///
    /// # Errors
    ///
    /// [`MediaError::Spawn`] if the executable cannot start, or [`MediaError::Io`] if the process cannot be
    /// contained. An uncontained child is killed before the error is returned.
    pub fn spawn(self, pipes: SpawnIo) -> Result<FfmpegProcess, MediaError> {
        let mut command = Command::new(&self.program);
        command
            .args(self.args())
            .stdin(if pipes.stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if pipes.stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let spawned = {
            let _serialized = SPAWN_LOCK.lock();
            command.spawn()
        };
        let mut child = spawned.map_err(|source| MediaError::Spawn {
            program: self.program.clone(),
            source,
        })?;
        contain(&mut child)?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let Some(stderr) = child.stderr.take() else {
            let _ = child.kill();
            return Err(MediaError::Io(io::Error::other(
                "FFmpeg stderr pipe is missing",
            )));
        };
        let on_progress = self.progress;
        let stderr_thread = match thread::Builder::new()
            .name("ffmpeg-stderr".to_owned())
            .spawn(move || drain_stderr(stderr, on_progress))
        {
            Ok(handle) => handle,
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MediaError::Io(source));
            }
        };

        Ok(FfmpegProcess {
            child,
            stdin,
            stdout,
            stderr_thread: Some(stderr_thread),
            finished: false,
        })
    }

    /// Appends one input, with the safety options the kind requires.
    fn push_input(
        mut self,
        target: OsString,
        kind: InputKind,
        opts: &InputOptions,
    ) -> Result<Self, MediaError> {
        validate_input_options(opts)?;
        if kind == InputKind::Lavfi {
            check_lavfi_graph(&target)?;
        }

        let mut args: Vec<OsString> = Vec::new();
        args.extend(opts.hwaccel_args.iter().map(OsString::from));
        if let Some(seek) = opts.seek {
            args.push(OsString::from("-ss"));
            args.push(OsString::from(format_seconds(seek)));
        }
        if let Some(duration) = opts.duration {
            args.push(OsString::from("-t"));
            args.push(OsString::from(format_seconds(duration)));
        }
        if kind == InputKind::Lavfi {
            args.extend(["-f", "lavfi"].map(OsString::from));
        } else if let Some(format) = &opts.format {
            args.push(OsString::from("-f"));
            args.push(OsString::from(format));
        }
        args.extend(opts.extra.iter().map(OsString::from));
        if kind != InputKind::Lavfi {
            args.push(OsString::from("-protocol_whitelist"));
            args.push(OsString::from(PROTOCOL_WHITELIST));
            if self.tool == Tool::Ffmpeg {
                args.push(OsString::from("-noautorotate"));
            }
        }
        args.push(OsString::from("-i"));
        args.push(target);

        self.inputs.extend(args);
        Ok(self)
    }
}

/// Checks that a value can be used as a local file path and is not a protocol specifier.
fn validate_path(value: &OsStr) -> Result<(), MediaError> {
    let text = value.to_string_lossy();
    if is_url_like(&text) {
        Err(MediaError::UrlNotAllowed(text.into_owned()))
    } else {
        Ok(())
    }
}

/// True when FFmpeg would read `text` as a protocol or special input instead of a local path.
///
/// Windows drive letters (`C:\clip.mp4`) are allowed; any longer scheme before `:` is not, and neither is `subfile,`
/// or `-` (stdin). Leading whitespace is ignored because it cannot change how the path is opened.
pub fn is_url_like(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.contains("://") || trimmed == "-" {
        return true;
    }
    let scheme_len = trimmed
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
        .unwrap_or(trimmed.len());
    let (scheme, rest) = trimmed.split_at(scheme_len);
    if scheme.is_empty() {
        return false;
    }
    match rest.chars().next() {
        Some(':') => scheme.len() > 1,
        Some(',') => scheme.eq_ignore_ascii_case("subfile"),
        _ => false,
    }
}

/// Checks caller-supplied global, input or output arguments: no reserved option, and no token that would make FFmpeg
/// read or write another file. The check is per token, so a graph split across two calls is still caught.
fn validate_caller_args(args: &[OsString]) -> Result<(), MediaError> {
    for arg in args {
        let text = arg.to_string_lossy();
        if RESERVED_OPTIONS.contains(&text.as_ref()) {
            return Err(MediaError::InvalidArgument(format!(
                "{text} cannot be passed as a raw argument; the builder owns it"
            )));
        }
        let lowered = text.to_ascii_lowercase();
        if let Some(fragment) = FORBIDDEN_GRAPH_FRAGMENTS
            .iter()
            .find(|fragment| lowered.contains(**fragment))
        {
            return Err(MediaError::InvalidArgument(format!(
                "argument {text:?} contains {fragment:?}, which would read or write another file"
            )));
        }
    }
    Ok(())
}

/// Rejects option values that would override builder-owned input options or read other files.
fn validate_input_options(opts: &InputOptions) -> Result<(), MediaError> {
    let caller_args: Vec<OsString> = opts
        .hwaccel_args
        .iter()
        .chain(opts.extra.iter())
        .map(OsString::from)
        .collect();
    validate_caller_args(&caller_args)?;
    for arg in opts.hwaccel_args.iter().chain(opts.extra.iter()) {
        if RESERVED_INPUT_OPTIONS.contains(&arg.as_str()) {
            return Err(MediaError::InvalidArgument(format!(
                "{arg} cannot be passed through InputOptions; use the dedicated field instead"
            )));
        }
    }
    if let Some(seek) = opts.seek
        && !(seek.is_finite() && seek >= 0.0)
    {
        return Err(MediaError::InvalidArgument(format!(
            "seek must be a finite, non-negative number of seconds, got {seek}"
        )));
    }
    if let Some(duration) = opts.duration
        && !(duration.is_finite() && duration > 0.0)
    {
        return Err(MediaError::InvalidArgument(format!(
            "duration must be a finite, positive number of seconds, got {duration}"
        )));
    }
    if let Some(format) = &opts.format {
        if format.is_empty() || format.starts_with('-') {
            return Err(MediaError::InvalidArgument(format!(
                "invalid input format {format:?}"
            )));
        }
        if format.eq_ignore_ascii_case("lavfi") {
            return Err(MediaError::LavfiRejected(
                "format \"lavfi\" requires InputOptions::lavfi = true".to_owned(),
            ));
        }
        if opts.lavfi {
            return Err(MediaError::InvalidArgument(
                "lavfi inputs take no format; the builder sets -f lavfi".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Rejects lavfi graphs that could fetch or read files, and graphs that are not valid UTF-8.
fn check_lavfi_graph(graph: &OsStr) -> Result<(), MediaError> {
    let text = graph
        .to_str()
        .ok_or_else(|| MediaError::LavfiRejected("graph is not valid UTF-8".to_owned()))?;
    if text.trim().is_empty() {
        return Err(MediaError::LavfiRejected("graph is empty".to_owned()));
    }
    let lowered = text.to_ascii_lowercase();
    for fragment in FORBIDDEN_GRAPH_FRAGMENTS {
        if lowered.contains(fragment) {
            return Err(MediaError::LavfiRejected(format!(
                "graph contains forbidden fragment {fragment:?}"
            )));
        }
    }
    Ok(())
}

/// Formats seconds for an FFmpeg time option with fixed precision, so command lines are deterministic.
fn format_seconds(seconds: f64) -> String {
    format!("{seconds:.6}")
}

/// Assigns `child` to the job object. If that fails for a child that is still running, the child is killed so it
/// cannot outlive LoopCut uncontained. A child that already exited has nothing to contain.
fn contain(child: &mut Child) -> Result<(), MediaError> {
    match job::assign(child) {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Ok(Some(_)) = child.try_wait() {
                return Ok(());
            }
            let _ = child.kill();
            let _ = child.wait();
            Err(MediaError::Io(err))
        }
    }
}

/// Final result of a successful FFmpeg/ffprobe run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// Exit code (always `Some(0)` on success on Windows).
    pub code: Option<i32>,
    /// The last diagnostic stderr lines, newline separated (usually warnings only).
    pub stderr_tail: String,
}

/// A running FFmpeg or ffprobe child.
///
/// Dropping it without waiting kills the child. Use [`Self::wait`], [`Self::wait_timeout`] or
/// [`Self::wait_cancellable`] to collect the result.
pub struct FfmpegProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr_thread: Option<JoinHandle<Vec<String>>>,
    finished: bool,
}

impl FfmpegProcess {
    /// The OS process id, for diagnostics.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Takes the stdin pipe (only present if spawned with [`SpawnIo::stdin`]). Dropping it closes the pipe.
    ///
    /// The caller owns the handle. **Drop it before calling [`Self::wait`], [`Self::wait_timeout`] or
    /// [`Self::wait_cancellable`]**: an encoder reads stdin until end of file and never exits while the write end is
    /// still open, so waiting with the handle alive hangs until the timeout or forever without one.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// Takes the stdout pipe (only present if spawned with [`SpawnIo::stdout`]).
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// Closes the pipes and terminates the child. Does not wait for it to exit; call a `wait` method afterwards.
    pub fn kill(&mut self) {
        self.stdin = None;
        self.stdout = None;
        let _ = self.child.kill();
    }

    /// Waits for the process to finish. Unread stdout is closed first, so a child blocked on output fails rather
    /// than hanging.
    ///
    /// # Errors
    ///
    /// [`MediaError::Failed`] for a non-zero exit, or [`MediaError::Io`] if waiting fails.
    pub fn wait(mut self) -> Result<ExitInfo, MediaError> {
        self.stdin = None;
        self.stdout = None;
        let status = self.child.wait()?;
        self.complete(status)
    }

    /// Like [`Self::wait`], but kills the child and returns [`MediaError::Timeout`] after `timeout`.
    ///
    /// # Errors
    ///
    /// [`MediaError::Timeout`] if the deadline passes, otherwise the same errors as [`Self::wait`].
    pub fn wait_timeout(mut self, timeout: Duration) -> Result<ExitInfo, MediaError> {
        self.poll(Some(timeout), None)
    }

    /// Waits until the process exits, `cancel` is set, or `timeout` (if any) passes. Cancellation and timeouts kill
    /// the child.
    ///
    /// # Errors
    ///
    /// [`MediaError::Cancelled`], [`MediaError::Timeout`], or the errors of [`Self::wait`].
    pub fn wait_cancellable(
        mut self,
        cancel: &CancelToken,
        timeout: Option<Duration>,
    ) -> Result<ExitInfo, MediaError> {
        self.poll(timeout, Some(cancel))
    }

    /// Polls the child until it exits, is cancelled, or times out.
    fn poll(
        &mut self,
        timeout: Option<Duration>,
        cancel: Option<&CancelToken>,
    ) -> Result<ExitInfo, MediaError> {
        let deadline = timeout.map(|limit| Instant::now() + limit);
        self.stdin = None;
        self.stdout = None;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return self.complete(status);
            }
            if cancel.is_some_and(|token| token.is_cancelled()) {
                self.kill_and_reap();
                return Err(MediaError::Cancelled);
            }
            if let (Some(limit), Some(deadline)) = (timeout, deadline)
                && Instant::now() >= deadline
            {
                self.kill_and_reap();
                return Err(MediaError::Timeout(limit));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Kills the child and reaps it. The stderr drain thread is left to finish on its own.
    fn kill_and_reap(&mut self) {
        self.kill();
        let _ = self.child.wait();
        self.finished = true;
        self.stderr_thread = None;
    }

    /// Builds the result once the child has exited, collecting the stderr tail.
    fn complete(&mut self, status: ExitStatus) -> Result<ExitInfo, MediaError> {
        self.finished = true;
        let tail = self
            .stderr_thread
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
            .join("\n");
        if status.success() {
            Ok(ExitInfo {
                code: status.code(),
                stderr_tail: tail,
            })
        } else {
            Err(MediaError::Failed {
                code: status.code(),
                stderr_tail: tail,
            })
        }
    }
}

impl Drop for FfmpegProcess {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Reads one line into `buf`, without its terminator, keeping at most `cap` bytes of it. The rest of an over-long line
/// is consumed and discarded. Returns `Ok(None)` at end of stream, otherwise `Ok(Some(truncated))`.
fn read_capped_line<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> io::Result<Option<bool>> {
    buf.clear();
    let mut saw_bytes = false;
    let mut truncated = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if available.is_empty() {
            break;
        }
        saw_bytes = true;
        let (take, terminated) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        let content_len = if terminated { take - 1 } else { take };
        let keep = content_len.min(cap.saturating_sub(buf.len()));
        buf.extend_from_slice(&available[..keep]);
        truncated |= keep < content_len;
        reader.consume(take);
        if terminated {
            break;
        }
    }
    Ok(saw_bytes.then_some(truncated))
}

/// Reads stderr to EOF: progress blocks go to the callback, other lines are kept as a bounded tail. Each line is capped
/// at [`MAX_STDERR_LINE_BYTES`].
fn drain_stderr(stderr: ChildStderr, mut on_progress: Option<ProgressCallback>) -> Vec<String> {
    let mut reader = BufReader::new(stderr);
    let mut parser = ProgressParser::default();
    let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_LINES);
    let mut raw = Vec::new();
    while let Ok(Some(truncated)) = read_capped_line(&mut reader, &mut raw, MAX_STDERR_LINE_BYTES) {
        let text = String::from_utf8_lossy(&raw);
        let mut line = text.trim_end_matches(['\r', '\n']).to_owned();
        if truncated {
            line.push_str(" [truncated]");
        }
        match parser.feed(&line) {
            Feed::Partial => {}
            Feed::Block(progress) => {
                if let Some(callback) = on_progress.as_mut() {
                    callback(progress);
                }
            }
            Feed::Message => {
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        }
    }
    Vec::from(tail)
}

/// True for Windows UNC paths (`\\server\share\...`), including the verbatim `\\?\UNC\` form.
///
/// LoopCut accepts UNC inputs because media often lives on a NAS. Opening one still contacts another host, so the
/// application should confirm before it opens a UNC path that came from a project file. Device paths (`\\.\...`) and
/// verbatim local paths (`\\?\C:\...`) are not UNC paths. Forward slashes count as separators.
pub fn is_unc_path(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(verbatim) = text.strip_prefix(r"\\?\") {
        return verbatim
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"UNC\"));
    }
    if text.starts_with(r"\\.\") {
        return false;
    }
    text.starts_with(r"\\")
}

/// Cooperative cancellation flag shared between the caller and a waiting process.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Creates a token that is not yet cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Every clone observes it.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// True once [`Self::cancel`] has been called on any clone.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Reads fixed-size raw frames (for example `rawvideo` from ffmpeg stdout) from any reader.
#[derive(Debug)]
pub struct RawFrameReader<R: Read> {
    inner: R,
    frame_size: usize,
}

impl<R: Read> RawFrameReader<R> {
    /// Wraps `inner`, which yields frames of `frame_size` bytes each.
    pub fn new(inner: R, frame_size: usize) -> Self {
        Self { inner, frame_size }
    }

    /// Bytes per frame.
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Returns the wrapped reader.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Fills `buf` with the next frame.
    ///
    /// Returns `Ok(true)` when a whole frame was read, and `Ok(false)` at a clean end of stream (no bytes left).
    ///
    /// # Errors
    ///
    /// `InvalidInput` if `buf` is not exactly one frame long or the frame size is zero. `UnexpectedEof` if the stream
    /// ends partway through a frame. Other I/O errors are passed through.
    pub fn read_frame(&mut self, buf: &mut [u8]) -> io::Result<bool> {
        if self.frame_size == 0 || buf.len() != self.frame_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "buffer is {} bytes but frames are {} bytes",
                    buf.len(),
                    self.frame_size
                ),
            ));
        }
        let mut filled = 0;
        while filled < buf.len() {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        match filled {
            0 => Ok(false),
            full if full == buf.len() => Ok(true),
            partial => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("truncated raw frame: got {partial} of {} bytes", buf.len()),
            )),
        }
    }
}

/// Runs `cmd` to completion and returns its stdout, killing the process if it runs longer than `timeout`.
///
/// Intended for ffprobe and other short commands whose output fits in memory.
///
/// # Errors
///
/// Any error from [`FfmpegCommand::spawn`] or [`FfmpegProcess::wait_timeout`].
pub fn run_capture(cmd: FfmpegCommand, timeout: Duration) -> Result<Vec<u8>, MediaError> {
    let mut process = cmd.spawn(SpawnIo {
        stdin: false,
        stdout: true,
    })?;
    let mut stdout = process
        .take_stdout()
        .ok_or_else(|| MediaError::Io(io::Error::other("stdout pipe is missing")))?;
    // Drain on another thread so a large output cannot stall the child while we wait for it to exit.
    let reader = thread::Builder::new()
        .name("ffmpeg-stdout".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        })?;
    process.wait_timeout(timeout)?;
    let bytes = reader
        .join()
        .map_err(|_| MediaError::Io(io::Error::other("stdout reader thread panicked")))??;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_like_inputs_are_rejected() {
        for value in [
            "http://example.com/a.mp4",
            "https://x/y",
            "rtmp://host/app",
            "concat:a.mp4|b.mp4",
            "file:clip.mp4",
            "subfile,,start:0,end:10,,:clip.mp4",
            "pipe:0",
            "data:abc",
            "-",
            "  http://leading-space.example/a",
        ] {
            assert!(is_url_like(value), "{value:?} should be rejected");
        }
    }

    #[test]
    fn local_paths_are_allowed() {
        for value in [
            r"C:\x\y.mp4",
            "D:/GitHub/LoopCut/clip.mp4",
            "vidéo テスト 🎬.mp4",
            "a,b.mp4",
            r"\\server\share\clip.mov",
            r"\\?\C:\a very long path\clip.mp4",
            "relative/clip.mkv",
            "clip",
        ] {
            assert!(!is_url_like(value), "{value:?} should be allowed");
        }
    }

    #[test]
    fn progress_block_is_parsed_into_one_snapshot() {
        let block = [
            "frame=10",
            "fps=0.00",
            "stream_0_0_q=-0.0",
            "bitrate=N/A",
            "total_size=N/A",
            "out_time_us=1000000",
            "out_time_ms=1000000",
            "out_time=00:00:01.000000",
            "dup_frames=0",
            "drop_frames=0",
            "speed= 1.5x",
            "progress=end",
        ];
        let mut parser = ProgressParser::default();
        let mut results = Vec::new();
        for line in block {
            results.push(parser.feed(line));
        }
        assert!(results[..11].iter().all(|feed| *feed == Feed::Partial));
        let Feed::Block(progress) = results[11] else {
            panic!("expected block, got {:?}", results[11]);
        };
        assert_eq!(progress.frame, Some(10));
        assert_eq!(progress.out_time_us, Some(1_000_000));
        assert_eq!(progress.speed, Some(1.5));
        assert!(progress.done);
    }

    #[test]
    fn recorded_ffmpeg_capture_yields_one_block_per_progress_line() {
        // Captured from a real `ffmpeg -progress pipe:2 -nostats -re` run: a continue block, then the end block.
        const RECORDED: &str =
            include_str!("../tests/fixtures/progress/ffmpeg_progress_blocks.txt");
        let mut parser = ProgressParser::default();
        let blocks: Vec<Progress> = RECORDED
            .lines()
            .filter_map(|line| match parser.feed(line) {
                Feed::Block(block) => Some(block),
                Feed::Partial | Feed::Message => None,
            })
            .collect();
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert!(!blocks[0].done);
        assert_eq!(blocks[0].frame, Some(10));
        assert_eq!(blocks[0].out_time_us, Some(1_000_000));
        assert_eq!(blocks[0].speed, Some(1.96));
        assert!(blocks[1].done);
        assert_eq!(blocks[1].frame, Some(15));
        assert_eq!(blocks[1].out_time_us, Some(1_500_000));
        assert_eq!(blocks[1].speed, Some(1.66));
    }

    #[test]
    fn out_time_ms_is_never_used_for_position() {
        let mut parser = ProgressParser::default();
        parser.feed("out_time_ms=5000000");
        let Feed::Block(progress) = parser.feed("progress=continue") else {
            panic!("expected block");
        };
        assert_eq!(progress.out_time_us, None);
        assert!(!progress.done);
    }

    #[test]
    fn na_values_become_none_and_diagnostics_are_messages() {
        let mut parser = ProgressParser::default();
        parser.feed("frame=N/A");
        parser.feed("speed=N/A");
        let Feed::Block(progress) = parser.feed("progress=continue") else {
            panic!("expected block");
        };
        assert_eq!(progress, Progress::default());
        assert_eq!(
            parser.feed("[h264 @ 0x1234] error while decoding"),
            Feed::Message
        );
        assert_eq!(
            parser.feed("Error opening input: No such file"),
            Feed::Message
        );
    }

    #[test]
    fn raw_reader_reads_whole_frames_then_clean_eof() {
        let data = [7u8; 8];
        let mut reader = RawFrameReader::new(&data[..], 4);
        let mut buf = [0u8; 4];
        assert!(reader.read_frame(&mut buf).expect("frame 1"));
        assert!(reader.read_frame(&mut buf).expect("frame 2"));
        assert!(!reader.read_frame(&mut buf).expect("clean eof"));
    }

    #[test]
    fn raw_reader_reports_partial_frame() {
        let data = [1u8; 6];
        let mut reader = RawFrameReader::new(&data[..], 4);
        let mut buf = [0u8; 4];
        assert!(reader.read_frame(&mut buf).expect("first frame is whole"));
        let err = reader
            .read_frame(&mut buf)
            .expect_err("two bytes are a partial frame");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(err.to_string().contains("got 2 of 4"), "{err}");
    }

    #[test]
    fn raw_reader_rejects_wrong_buffer_size() {
        let data = [0u8; 4];
        let mut reader = RawFrameReader::new(&data[..], 4);
        let mut small = [0u8; 3];
        let err = reader.read_frame(&mut small).expect_err("buffer too small");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn cancel_token_is_shared_between_clones() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn seconds_are_formatted_with_fixed_precision() {
        assert_eq!(format_seconds(1.5), "1.500000");
        assert_eq!(format_seconds(0.0), "0.000000");
    }

    #[test]
    fn lavfi_graphs_that_read_files_are_refused() {
        assert!(check_lavfi_graph(OsStr::new("testsrc2=size=320x240:duration=1")).is_ok());
        assert!(check_lavfi_graph(OsStr::new("movie=secret.mp4")).is_err());
        assert!(check_lavfi_graph(OsStr::new("amovie=secret.mp4")).is_err());
        assert!(check_lavfi_graph(OsStr::new("  ")).is_err());
    }

    fn fake_paths() -> FfmpegPaths {
        FfmpegPaths {
            ffmpeg: PathBuf::from("ffmpeg.exe"),
            ffprobe: PathBuf::from("ffprobe.exe"),
        }
    }

    #[test]
    fn reserved_options_are_refused_in_every_caller_position() {
        let paths = fake_paths();
        for option in RESERVED_OPTIONS {
            let global = FfmpegCommand::new_ffmpeg(&paths).global_arg(option);
            assert!(
                matches!(global, Err(MediaError::InvalidArgument(_))),
                "global {option}"
            );
            let output = FfmpegCommand::new_ffmpeg(&paths).output_args([option]);
            assert!(
                matches!(output, Err(MediaError::InvalidArgument(_))),
                "output {option}"
            );
        }
    }

    #[test]
    fn file_reading_filters_are_refused_in_raw_arguments() {
        let paths = fake_paths();
        for value in [
            "movie=/secret/clip.mp4",
            "[0:v]amovie=clip.mp4",
            "drawtext=textfile=notes.txt",
            "lut3d=file=grade.cube",
            "subtitles=sub.srt",
        ] {
            let result = FfmpegCommand::new_ffmpeg(&paths).output_args(["-vf", value]);
            assert!(
                matches!(result, Err(MediaError::InvalidArgument(_))),
                "{value:?} must be refused"
            );
        }
        let split = FfmpegCommand::new_ffmpeg(&paths)
            .output_args(["-vf"])
            .expect("flag alone is harmless")
            .output_args(["movie=x.mp4"]);
        assert!(split.is_err(), "a graph split across calls is still caught");
    }

    #[test]
    fn a_refused_list_is_rejected_as_a_whole() {
        let paths = fake_paths();
        // The valid entries before the refused one must not be kept either.
        let result = FfmpegCommand::new_ffmpeg(&paths).output_args(["-c:v", "libx264", "-i"]);
        assert!(result.is_err());
        let result = FfmpegCommand::new_ffmpeg(&paths).global_args(["-y", "-i"]);
        assert!(result.is_err());
        let cmd = FfmpegCommand::new_ffmpeg(&paths)
            .global_args(["-y"])
            .expect("valid global");
        let args = cmd.args();
        assert!(args.iter().any(|arg| arg == "-y"));
        assert!(!args.iter().any(|arg| arg == "-i"), "{args:?}");
    }

    #[test]
    fn input_options_refuse_reserved_and_file_reading_values() {
        let paths = fake_paths();
        let attach = FfmpegCommand::new_ffmpeg(&paths).input(
            r"C:\a.mp4",
            &InputOptions {
                extra: vec!["-attach".to_owned(), "C:/secret.ttf".to_owned()],
                ..InputOptions::default()
            },
        );
        assert!(matches!(attach, Err(MediaError::InvalidArgument(_))));
        let movie = FfmpegCommand::new_ffmpeg(&paths).input(
            r"C:\a.mp4",
            &InputOptions {
                extra: vec!["-vf".to_owned(), "movie=x.mp4".to_owned()],
                ..InputOptions::default()
            },
        );
        assert!(matches!(movie, Err(MediaError::InvalidArgument(_))));
    }

    #[test]
    fn long_stderr_lines_are_capped_and_marked() {
        let long = "x".repeat(MAX_STDERR_LINE_BYTES * 3);
        let input = format!("short\n{long}\nafter\n");
        let mut reader = BufReader::with_capacity(64, input.as_bytes());
        let mut buf = Vec::new();

        assert_eq!(
            read_capped_line(&mut reader, &mut buf, MAX_STDERR_LINE_BYTES).expect("read"),
            Some(false)
        );
        assert_eq!(buf, b"short");

        assert_eq!(
            read_capped_line(&mut reader, &mut buf, MAX_STDERR_LINE_BYTES).expect("read"),
            Some(true)
        );
        assert_eq!(buf.len(), MAX_STDERR_LINE_BYTES);

        // The over-long line's remainder was discarded, so the next read starts at "after".
        assert_eq!(
            read_capped_line(&mut reader, &mut buf, MAX_STDERR_LINE_BYTES).expect("read"),
            Some(false)
        );
        assert_eq!(buf, b"after");
        assert_eq!(
            read_capped_line(&mut reader, &mut buf, MAX_STDERR_LINE_BYTES).expect("eof"),
            None
        );
    }

    #[test]
    fn a_line_that_exactly_fits_is_not_truncated() {
        let line = "y".repeat(MAX_STDERR_LINE_BYTES);
        let input = format!("{line}\n");
        let mut reader = BufReader::new(input.as_bytes());
        let mut buf = Vec::new();
        assert_eq!(
            read_capped_line(&mut reader, &mut buf, MAX_STDERR_LINE_BYTES).expect("read"),
            Some(false),
            "only the terminator was cut, the content fits"
        );
        assert_eq!(buf.len(), MAX_STDERR_LINE_BYTES);
    }

    #[test]
    fn unc_paths_are_detected_without_flagging_local_paths() {
        for unc in [
            r"\\server\share\clip.mov",
            "//server/share/clip.mov",
            r"\\?\UNC\server\share\clip.mov",
            r"\\?\unc\server\share\clip.mov",
        ] {
            assert!(is_unc_path(Path::new(unc)), "{unc} is UNC");
        }
        for local in [
            r"C:\clips\clip.mov",
            r"\\?\C:\a very long path\clip.mp4",
            r"\\.\C:",
            "relative/clip.mp4",
            "vidéo テスト 🎬.mp4",
        ] {
            assert!(!is_unc_path(Path::new(local)), "{local} is not UNC");
        }
    }
}

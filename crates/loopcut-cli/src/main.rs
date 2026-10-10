//! Headless LoopCut driver: probe media, inspect a project's frame plan, run an export, or create a project file.
//!
//! Examples:
//!
//! ```text
//! loopcut-cli probe clip.mp4
//! loopcut-cli new-project clip.mp4 clip.loopcut --start 0.5 --end 2.5 --fps 15 --width 320
//! loopcut-cli plan clip.loopcut
//! loopcut-cli export clip.loopcut clip.gif --gpu auto --decode d3d11va
//! ```
//!
//! Ctrl+C is not handled: the process has no signal handler and no `ctrlc` dependency. Ctrl+C ends the process at once.
//! Its FFmpeg children end with it (they run in a Job Object that kills them on close), but the `<output>.partial` file
//! and the export's temporary folder `loopcut-cli-<pid>` in the system temp directory are left behind. No later run
//! removes them, because each run uses its own process id, so delete them by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use loopcut_core::geometry::OutputSize;
use loopcut_core::gif_timing::{clamp_gif_fps, effective_fps};
use loopcut_core::{
    Clip, DecodeBackend, DecodeChain, EncoderBackend, ExportFormat, GpuScaler, Project, Rational,
    TimeRange, migrate, sequencer,
};
use loopcut_export::{ExportContext, ExportEvent, ExportRequest, run_export};
use loopcut_media::framestore::{FrameStore, FrameStoreConfig};
use loopcut_media::{CancelToken, FfmpegPaths, probe};
use loopcut_render::gpu::{AdapterChoice, GpuContext};
use tracing_subscriber::EnvFilter;

/// RAM budget for decoded frames in the CLI. Frames beyond it spill to disk.
const CLI_RAM_BUDGET: u64 = 1024 * 1024 * 1024;
/// Disk budget for spilled frames.
const CLI_DISK_BUDGET: u64 = 10 * 1024 * 1024 * 1024;
/// Default clip length, in seconds, for `new-project` when no end is given.
const DEFAULT_CLIP_SECONDS: f64 = 6.0;

#[derive(Parser)]
#[command(name = "loopcut-cli", version, about = "Headless LoopCut driver")]
struct Cli {
    /// Folder that holds `ffmpeg` and `ffprobe`. Defaults to the staged development build.
    #[arg(long, global = true, value_name = "DIR")]
    ffmpeg_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the probed description of a media file as JSON.
    Probe {
        /// The media file.
        file: PathBuf,
    },
    /// Print the output frames of a project: count, duration and the centisecond delays.
    Plan {
        /// The `.loopcut` project file.
        project: PathBuf,
    },
    /// Export a project to a GIF, MP4 or WebM file, reporting progress on stderr. Ctrl+C is not handled (see the module
    /// docs): an interrupted run leaves `<output>.partial` behind.
    Export {
        /// The `.loopcut` project file.
        project: PathBuf,
        /// The output file. Written as `<output>.partial` and renamed when complete.
        output: PathBuf,
        /// Renderer: a hardware adapter, or the software (WARP) adapter.
        #[arg(long, value_enum, default_value_t = GpuArg::Auto)]
        gpu: GpuArg,
        /// Decoder for the source.
        #[arg(long, value_enum, default_value_t = DecodeArg::Cpu)]
        decode: DecodeArg,
        /// Encoder for MP4 and WebM exports.
        #[arg(long, value_enum, default_value_t = EncoderArg::Cpu)]
        encoder: EncoderArg,
    },
    /// Create a project for a media file. Refuses to overwrite an existing file unless `--force` is given.
    NewProject {
        /// The media file.
        media: PathBuf,
        /// The `.loopcut` project file to create.
        out: PathBuf,
        /// Overwrite `out` when it already exists.
        #[arg(long)]
        force: bool,
        /// Start of the kept range, in seconds of source time.
        #[arg(long)]
        start: Option<f64>,
        /// End of the kept range, in seconds of source time. Defaults to six seconds or the end of the file.
        #[arg(long)]
        end: Option<f64>,
        /// Output frame rate.
        #[arg(long, default_value_t = 20)]
        fps: u32,
        /// Output width in pixels. The height follows the aspect ratio.
        #[arg(long, default_value_t = 480)]
        width: u32,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum GpuArg {
    Auto,
    Software,
}

#[derive(Clone, Copy, ValueEnum)]
enum DecodeArg {
    Cpu,
    D3d11va,
    Amf,
    Cuda,
    Qsv,
}

#[derive(Clone, Copy, ValueEnum)]
enum EncoderArg {
    Cpu,
    Amf,
    Nvenc,
    Qsv,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Probe { file } => {
            let paths = ffmpeg_paths(cli.ffmpeg_dir.as_deref())?;
            let info =
                probe(&paths, &file).with_context(|| format!("probing {}", file.display()))?;
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(())
        }
        Command::Plan { project } => {
            let project = load_project(&project)?;
            print_plan(&project)
        }
        Command::Export {
            project,
            output,
            gpu,
            decode,
            encoder,
        } => {
            let paths = ffmpeg_paths(cli.ffmpeg_dir.as_deref())?;
            export(&paths, &project, &output, gpu, decode, encoder)
        }
        Command::NewProject {
            media,
            out,
            force,
            start,
            end,
            fps,
            width,
        } => {
            let paths = ffmpeg_paths(cli.ffmpeg_dir.as_deref())?;
            let args = NewProjectArgs {
                media,
                out,
                force,
                start,
                end,
                fps,
                width,
            };
            new_project(&paths, &args)
        }
    }
}

/// The FFmpeg executables from `--ffmpeg-dir`, or the staged development build.
fn ffmpeg_paths(dir: Option<&Path>) -> Result<FfmpegPaths> {
    match dir {
        Some(dir) => FfmpegPaths::from_dir(dir)
            .with_context(|| format!("looking for FFmpeg in {}", dir.display())),
        None => FfmpegPaths::discover_dev().ok_or_else(|| {
            anyhow!("no FFmpeg build found; pass --ffmpeg-dir or set LOOPCUT_FFMPEG_DIR")
        }),
    }
}

/// Loads a `.loopcut` file and resolves its media paths against the file's folder.
fn load_project(path: &Path) -> Result<Project> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (mut project, _migrated) =
        migrate::load(&text).with_context(|| format!("loading {}", path.display()))?;
    let folder = absolute_folder(path)?;
    migrate::resolve_paths(&mut project, &folder);
    Ok(project)
}

/// The absolute folder that holds `path`. A bare file name refers to the current folder.
fn absolute_folder(path: &Path) -> Result<PathBuf> {
    let absolute =
        std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))?;
    Ok(absolute
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf))
}

/// The output frame rate the project will actually export at (GIF is clamped to 50 fps).
fn export_fps(project: &Project) -> Rational {
    if project.export.format == ExportFormat::Gif {
        clamp_gif_fps(project.output.fps)
    } else {
        project.output.fps
    }
}

fn print_plan(project: &Project) -> Result<()> {
    let clip = project
        .clip()
        .ok_or_else(|| anyhow!("the project has no clip"))?;
    let fps = export_fps(project);
    let plan = sequencer::plan(clip, &project.timing, fps)
        .map_err(|err| anyhow!("the frame plan is invalid: {err}"))?;
    println!("frames: {}", plan.len());
    println!("duration: {:.3} s", plan.duration);
    println!("output fps: {fps}");
    if project.export.format == ExportFormat::Gif {
        println!(
            "effective GIF fps: {:.3} (delays are whole centiseconds)",
            effective_fps(fps)
        );
    }
    let mut counts: Vec<(u16, usize)> = Vec::new();
    for frame in &plan.frames {
        match counts
            .iter_mut()
            .find(|(delay, _)| *delay == frame.delay_cs)
        {
            Some((_, count)) => *count += 1,
            None => counts.push((frame.delay_cs, 1)),
        }
    }
    let summary: Vec<String> = counts
        .iter()
        .map(|(delay, count)| format!("{count} x {delay} cs"))
        .collect();
    println!("delays: {}", summary.join(", "));
    Ok(())
}

fn export(
    paths: &FfmpegPaths,
    project_path: &Path,
    output: &Path,
    gpu: GpuArg,
    decode: DecodeArg,
    encoder: EncoderArg,
) -> Result<()> {
    let project = load_project(project_path)?;
    let clip = project
        .clip()
        .ok_or_else(|| anyhow!("the project has no clip"))?;
    let media =
        probe(paths, Path::new(&clip.path)).with_context(|| format!("probing {}", clip.path))?;

    let choice = match gpu {
        GpuArg::Auto => AdapterChoice::Auto,
        GpuArg::Software => AdapterChoice::Software,
    };
    let gpu_context = GpuContext::new(&choice).context("creating the GPU device")?;
    eprintln!(
        "GPU: {}{}",
        gpu_context.desc.name,
        if gpu_context.is_software() {
            " (software)"
        } else {
            ""
        }
    );
    // This run's frame-store root. It is declared before the context, so the context (and the store's own folder inside
    // the root) is dropped first, and the root is removed last, on success and on every error path.
    let spill_root = std::env::temp_dir().join(format!("loopcut-cli-{}", std::process::id()));
    let _spill_root = TempRoot(spill_root.clone());
    let framestore = FrameStore::new(FrameStoreConfig {
        ram_budget_bytes: CLI_RAM_BUDGET,
        disk_budget_bytes: CLI_DISK_BUDGET,
        spill_dir: spill_root,
    })
    .context("creating the frame store")?;
    let ctx = ExportContext {
        paths: paths.clone(),
        gpu: Arc::new(gpu_context),
        framestore: Arc::new(framestore),
        captions: None,
    };
    let request = ExportRequest {
        project,
        media,
        output: std::path::absolute(output)
            .with_context(|| format!("resolving {}", output.display()))?,
        decode: decode_chain(decode),
        encoder: encoder_backend(encoder),
    };

    let cancel = CancelToken::new();
    let result = run_export(&ctx, &request, &mut print_event, &cancel)?;
    println!(
        "wrote {} ({} bytes, {} frames, {:.2} s)",
        result.path.display(),
        result.bytes,
        result.frames,
        result.seconds
    );
    Ok(())
}

/// Removes a folder tree when dropped. Used for the frame-store root of one CLI run.
struct TempRoot(PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        // Best effort: a folder that cannot be removed is left for the user, and must not hide the export's result.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes export events to stderr: progress lines, and each fallback so it is visible in logs.
fn print_event(event: ExportEvent) {
    match event {
        ExportEvent::Progress {
            frames_done,
            frames_total,
            bytes,
        } => eprintln!("progress: {frames_done}/{frames_total} frames, {bytes} bytes"),
        ExportEvent::Fallback {
            stage,
            from,
            to,
            reason,
        } => eprintln!("fallback ({stage}): {from} -> {to}: {reason}"),
        ExportEvent::TargetPass {
            pass,
            description,
            bytes,
        } => eprintln!("target pass {pass}: {description} -> {bytes} bytes"),
        ExportEvent::Done { .. } => {}
    }
}

/// The decode chain for a `--decode` choice. Device indices are the first device of each kind.
fn decode_chain(choice: DecodeArg) -> DecodeChain {
    match choice {
        DecodeArg::Cpu => DecodeChain::CPU,
        DecodeArg::D3d11va => DecodeChain {
            backend: DecodeBackend::D3d11va { dxgi_index: 0 },
            scaler: GpuScaler::None,
        },
        DecodeArg::Amf => DecodeChain {
            backend: DecodeBackend::Amf { dxgi_index: 0 },
            scaler: GpuScaler::Amf,
        },
        DecodeArg::Cuda => DecodeChain {
            backend: DecodeBackend::Cuda { cuda_index: 0 },
            scaler: GpuScaler::Cuda,
        },
        DecodeArg::Qsv => DecodeChain {
            backend: DecodeBackend::Qsv { dxgi_index: 0 },
            scaler: GpuScaler::Qsv,
        },
    }
}

/// The video encoder for an `--encoder` choice. Device indices are the first device of each kind.
fn encoder_backend(choice: EncoderArg) -> EncoderBackend {
    match choice {
        EncoderArg::Cpu => EncoderBackend::Cpu,
        EncoderArg::Amf => EncoderBackend::Amf { dxgi_index: 0 },
        EncoderArg::Nvenc => EncoderBackend::Nvenc { cuda_index: 0 },
        EncoderArg::Qsv => EncoderBackend::Qsv { dxgi_index: 0 },
    }
}

/// The options of `new-project`.
struct NewProjectArgs {
    media: PathBuf,
    out: PathBuf,
    /// Overwrite `out` when it exists.
    force: bool,
    start: Option<f64>,
    end: Option<f64>,
    fps: u32,
    width: u32,
}

fn new_project(paths: &FfmpegPaths, args: &NewProjectArgs) -> Result<()> {
    let (media, out) = (args.media.as_path(), args.out.as_path());
    if args.fps == 0 {
        bail!("--fps must be at least 1");
    }
    if args.width == 0 {
        bail!("--width must be at least 1");
    }
    if out.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            out.display()
        );
    }
    if let (Ok(out_path), Ok(media_path)) = (fs::canonicalize(out), fs::canonicalize(media))
        && out_path == media_path
    {
        bail!(
            "refusing to overwrite the media file {}; choose another project name",
            media.display()
        );
    }
    let info = probe(paths, media).with_context(|| format!("probing {}", media.display()))?;
    let start = args.start.unwrap_or(0.0);
    let end = args
        .end
        .unwrap_or_else(|| info.duration.min(start + DEFAULT_CLIP_SECONDS));
    let range = TimeRange::new(start, end);
    if !range.is_valid() || end > info.duration + 1e-6 {
        bail!(
            "the range {start}..{end} s is not inside the {:.3} s file",
            info.duration
        );
    }

    let source =
        std::path::absolute(media).with_context(|| format!("resolving {}", media.display()))?;
    let clip = Clip {
        id: "clip-1".to_owned(),
        path: source.to_string_lossy().into_owned(),
        stream_index: info.video.index,
        segments: vec![range],
        holds: Vec::new(),
    };
    let name = media.file_stem().map_or_else(
        || "Untitled".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let mut project = Project::new(name, clip);
    project.output.fps = Rational::integer(args.fps);
    project.output.size = OutputSize::Width { width: args.width };

    let folder = absolute_folder(out)?;
    migrate::relativize_paths(&mut project, &folder);
    let json = migrate::save(&project).context("serializing the project")?;
    let temporary = {
        let mut name = out.as_os_str().to_owned();
        name.push(".tmp");
        PathBuf::from(name)
    };
    fs::write(&temporary, json).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, out).with_context(|| format!("saving {}", out.display()))?;
    println!("created {} ({start}..{end} s)", out.display());
    Ok(())
}

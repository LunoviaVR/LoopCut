//! Fast GIF engine: rendered RGBA frames piped into FFmpeg's `palettegen` and `paletteuse`, in two passes.
//!
//! The builders here only describe the FFmpeg invocations. The export loop in [`crate::job`] writes the frames into the
//! children's stdin.
//!
//! * The palette pass pipes up to [`crate::job::PALETTE_SAMPLES`] evenly spaced frames of the plan into `palettegen`,
//!   which writes one palette image ([`palette_command`]).
//! * The encode pass streams every frame of the plan into `paletteuse`, which applies that fixed palette and writes the
//!   GIF ([`encode_command`]).
//!
//! Timing: the GIF muxer takes its delays from the input frame rate, so every frame gets the same delay, rounded to
//! whole centiseconds by FFmpeg. The Quality engine keeps the plan's exact per-frame delays; this engine does not.
//!
//! Memory: no pass holds more than one frame, because the palette is fixed before the first frame of the encode pass.
//! The palette is built from a sample, so for a plan longer than the sample it is an approximation of a palette built
//! from every frame.

use std::ffi::OsString;
use std::path::Path;

use loopcut_core::Rational;
use loopcut_core::project::Dither;
use loopcut_media::runner::InputOptions;
use loopcut_media::{FfmpegCommand, FfmpegPaths, MediaError};

/// Fewest palette colours `palettegen` accepts.
pub const MIN_COLORS: u16 = 4;
/// Most palette colours `palettegen` accepts.
pub const MAX_COLORS: u16 = 256;

/// What the FFmpeg invocations encode.
#[derive(Debug, Clone, PartialEq)]
pub struct FastOptions {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Frame rate of the piped frames, which becomes the GIF's frame rate.
    pub fps: Rational,
    /// Palette colours, clamped to `MIN_COLORS..=MAX_COLORS`.
    pub colors: u16,
    /// The dither setting, mapped to a `paletteuse` mode.
    pub dither: Dither,
    /// Play count, following the project model: 0 loops forever, 1 plays once, N plays N times.
    pub loop_count: u16,
}

/// The `paletteuse` dither mode for a project's dither setting. Blue noise has no FFmpeg equivalent, so it uses
/// Sierra 2-4A, the same mode as the Sierra setting.
pub fn ffmpeg_dither(dither: Dither) -> &'static str {
    match dither {
        Dither::None => "none",
        Dither::Bayer => "bayer:bayer_scale=3",
        Dither::FloydSteinberg => "floyd_steinberg",
        Dither::Sierra2_4a | Dither::BlueNoise => "sierra2_4a",
    }
}

/// The `-loop` value for a play count. FFmpeg's GIF muxer uses 0 for forever, -1 for no loop block, and N for N repeats.
pub fn loop_arg(loop_count: u16) -> i32 {
    match loop_count {
        0 => 0,
        1 => -1,
        plays => i32::from(plays) - 1,
    }
}

/// The palette pass: one palette image over the frames it receives. `stats_mode=diff` is kept: measured against a one-pass
/// palette graph, `full` came out further (1.08 and 1.22 of the reference, against 1.01 and 1.10 for `diff`).
pub fn palette_filter(colors: u16) -> String {
    let colors = colors.clamp(MIN_COLORS, MAX_COLORS);
    format!("palettegen=max_colors={colors}:stats_mode=diff:reserve_transparent=0")
}

/// The encode pass: the piped frames (input 0) are dithered with the palette image (input 1).
pub fn paletteuse_graph(dither: Dither) -> String {
    format!(
        "[0][1]paletteuse=dither={}:diff_mode=rectangle",
        ffmpeg_dither(dither)
    )
}

/// The raw RGBA input the piped frames arrive as.
fn raw_input(options: &FastOptions) -> InputOptions {
    InputOptions {
        format: Some("rawvideo".to_owned()),
        extra: vec![
            "-pix_fmt".to_owned(),
            "rgba".to_owned(),
            "-s".to_owned(),
            format!("{}x{}", options.width, options.height),
            "-r".to_owned(),
            options.fps.to_string(),
        ],
        ..InputOptions::default()
    }
}

fn check_size(options: &FastOptions) -> Result<(), MediaError> {
    if options.width == 0 || options.height == 0 {
        return Err(MediaError::InvalidArgument(format!(
            "GIF frame size {}x{} must not be empty",
            options.width, options.height
        )));
    }
    Ok(())
}

/// The palette pass: reads raw RGBA frames from stdin and writes one palette image to `palette`.
///
/// `palette` must be a file in a folder the export owns. The builder passes `-y`, so only that path may be overwritten.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] when the frame size is not positive, or any error the command builder reports for
/// its arguments.
pub fn palette_command(
    paths: &FfmpegPaths,
    options: &FastOptions,
    palette: &Path,
) -> Result<FfmpegCommand, MediaError> {
    check_size(options)?;
    let output_args: Vec<OsString> = vec![
        OsString::from("-vf"),
        OsString::from(palette_filter(options.colors)),
    ];
    FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")?
        .input_stdin(&raw_input(options))?
        .output_args(output_args)?
        .output(palette)
}

/// The encode pass: reads raw RGBA frames from stdin, applies the palette image at `palette`, and writes the GIF to
/// `output`.
///
/// `output` must be the export's partial file. The builder passes `-y`, so only that path may be overwritten.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] when the frame size is not positive, or any error the command builder reports for
/// its arguments.
pub fn encode_command(
    paths: &FfmpegPaths,
    options: &FastOptions,
    palette: &Path,
    output: &Path,
) -> Result<FfmpegCommand, MediaError> {
    check_size(options)?;
    let output_args: Vec<OsString> = vec![
        OsString::from("-filter_complex"),
        OsString::from(paletteuse_graph(options.dither)),
        OsString::from("-loop"),
        OsString::from(loop_arg(options.loop_count).to_string()),
        OsString::from("-f"),
        OsString::from("gif"),
    ];
    FfmpegCommand::new_ffmpeg(paths)
        .global_arg("-y")?
        .input_stdin(&raw_input(options))?
        .input(palette, &InputOptions::default())?
        .output_args(output_args)?
        .output(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Executable paths for building commands. The commands are only inspected, never run.
    fn test_paths() -> FfmpegPaths {
        FfmpegPaths {
            ffmpeg: std::path::PathBuf::from("ffmpeg"),
            ffprobe: std::path::PathBuf::from("ffprobe"),
        }
    }

    fn options(width: u32, height: u32) -> FastOptions {
        FastOptions {
            width,
            height,
            fps: Rational::integer(20),
            colors: 128,
            dither: Dither::Sierra2_4a,
            loop_count: 0,
        }
    }

    fn args_of(command: &FfmpegCommand) -> Vec<String> {
        command
            .args()
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn loop_count_maps_onto_the_muxer_loop_value() {
        assert_eq!(loop_arg(0), 0);
        assert_eq!(loop_arg(1), -1);
        assert_eq!(loop_arg(2), 1);
        assert_eq!(loop_arg(5), 4);
    }

    #[test]
    fn dither_settings_map_onto_paletteuse_modes() {
        assert_eq!(ffmpeg_dither(Dither::None), "none");
        assert_eq!(ffmpeg_dither(Dither::Bayer), "bayer:bayer_scale=3");
        assert_eq!(ffmpeg_dither(Dither::FloydSteinberg), "floyd_steinberg");
        assert_eq!(ffmpeg_dither(Dither::Sierra2_4a), "sierra2_4a");
        assert_eq!(ffmpeg_dither(Dither::BlueNoise), "sierra2_4a");
    }

    #[test]
    fn the_graphs_carry_the_palette_size_and_dither() {
        assert_eq!(
            palette_filter(64),
            "palettegen=max_colors=64:stats_mode=diff:reserve_transparent=0"
        );
        assert_eq!(
            paletteuse_graph(Dither::Bayer),
            "[0][1]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle"
        );
    }

    #[test]
    fn colours_are_clamped_to_what_palettegen_accepts() {
        assert!(palette_filter(2).contains("max_colors=4:"));
        assert!(palette_filter(999).contains("max_colors=256:"));
    }

    #[test]
    fn the_palette_pass_reads_rgba_and_writes_one_image() {
        let command = palette_command(&test_paths(), &options(64, 48), Path::new("palette.png"))
            .expect("command");
        let args = args_of(&command);
        let pair = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
        assert_eq!(pair("-pix_fmt").as_deref(), Some("rgba"));
        assert_eq!(pair("-s").as_deref(), Some("64x48"));
        assert_eq!(pair("-r").as_deref(), Some("20"));
        assert!(pair("-vf").is_some_and(|graph| graph.starts_with("palettegen")));
        assert!(!args.iter().any(|arg| arg == "-filter_complex"));
        assert_eq!(args.last().map(String::as_str), Some("palette.png"));
        assert!(args.iter().any(|arg| arg == "pipe:0"));
    }

    #[test]
    fn the_encode_pass_reads_frames_and_the_palette_and_writes_a_gif() {
        let command = encode_command(
            &test_paths(),
            &options(64, 48),
            Path::new("palette.png"),
            Path::new("out.gif.partial"),
        )
        .expect("command");
        let args = args_of(&command);
        let pair = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
        assert_eq!(pair("-pix_fmt").as_deref(), Some("rgba"));
        assert_eq!(pair("-loop").as_deref(), Some("0"));
        assert!(args.windows(2).any(|w| w[0] == "-f" && w[1] == "gif"));
        assert!(pair("-filter_complex").is_some_and(|graph| graph.contains("paletteuse")));
        // The palette image is the second input, after stdin.
        let inputs: Vec<&String> = args
            .windows(2)
            .filter(|w| w[0] == "-i")
            .map(|w| &w[1])
            .collect();
        assert_eq!(inputs.len(), 2, "{args:?}");
        assert_eq!(inputs[1], "palette.png");
        assert_eq!(args.last().map(String::as_str), Some("out.gif.partial"));
    }

    #[test]
    fn an_empty_frame_is_refused_by_both_passes() {
        let paths = test_paths();
        let empty = options(0, 48);
        assert!(matches!(
            palette_command(&paths, &empty, Path::new("palette.png")),
            Err(MediaError::InvalidArgument(_))
        ));
        assert!(matches!(
            encode_command(
                &paths,
                &empty,
                Path::new("palette.png"),
                Path::new("out.gif.partial")
            ),
            Err(MediaError::InvalidArgument(_))
        ));
    }
}

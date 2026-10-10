#!/usr/bin/env node
// Generates the deterministic media corpus for the Rust integration tests with the pinned FFmpeg staged by
// scripts/fetch-ffmpeg.mjs. The corpus covers containers, codecs, color and alpha variants, VFR, rotation,
// multi-stream and cover-art files, non-ASCII names and paths longer than 260 characters.
//
// Every clip is 1-2 s long, uses small dimensions and fast presets. Output goes to
// crates/loopcut-media/tests/media/generated/ by default (gitignored). Existing files are skipped unless
// --force is given, and each file is written to a temporary name first so an interrupted run never leaves a
// truncated fixture behind.
//
// Usage: node scripts/gen-test-media.mjs [--force] [--out <dir>]

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, statSync } from "node:fs";
import { dirname, extname, join, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const lock = JSON.parse(readFileSync(join(root, "scripts", "ffmpeg.lock.json"), "utf8"));
const exeSuffix = process.platform === "win32" ? ".exe" : "";
const stageDir = join(root, "src-tauri", "resources", "ffmpeg", lock.version);
const ffmpeg = join(stageDir, `ffmpeg${exeSuffix}`);
const ffprobe = join(stageDir, `ffprobe${exeSuffix}`);

const { force, outDir } = parseArgs(process.argv.slice(2));

/** Parses `--force` and `--out <dir>`; anything else is a usage error. */
function parseArgs(argv) {
  let force = false;
  let outDir = join(root, "crates", "loopcut-media", "tests", "media", "generated");
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--force") force = true;
    else if (arg === "--out" && argv[i + 1]) {
      outDir = resolve(argv[i + 1]);
      i += 1;
    } else {
      console.error(`gen-test-media: unknown argument "${arg}"\nusage: node scripts/gen-test-media.mjs [--force] [--out <dir>]`);
      process.exit(2);
    }
  }
  return { force, outDir };
}

// ---------------------------------------------------------------------------------------------------------
// Inputs. All sources are FFmpeg lavfi generators, so the corpus needs no checked-in media.
// ---------------------------------------------------------------------------------------------------------

/** A test pattern with a moving timer, at `fps` frames per second. */
const testPattern = (w, h, seconds, fps = 30) => [
  "-f",
  "lavfi",
  "-i",
  `testsrc2=size=${w}x${h}:duration=${seconds}:rate=${fps}`,
];

/** A blue-to-transparent horizontal gradient (alpha ramps 0 to 255 across the frame). */
const alphaGradient = (w, h, seconds) => [
  "-f",
  "lavfi",
  "-i",
  `color=c=0x3080ff:size=${w}x${h}:duration=${seconds}:rate=30,format=rgba,` +
    "geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='255*X/W'",
];

/** Options shared by every output: no ffmpeg/encoder version strings, so reruns are byte-identical. */
const DETERMINISTIC = ["-map_metadata", "-1", "-fflags", "+bitexact", "-flags", "+bitexact"];

/** Options for the H.264 base clips: a silent AAC track too, like a phone recording. */
const H264 = ["-c:v", "libx264", "-preset", "ultrafast", "-crf", "28", "-pix_fmt", "yuv420p"];

/**
 * HDR10 / HLG tagging for libx265 10-bit output. The tags are what matters to the color path; the pixels are
 * the test pattern converted to 10-bit.
 */
const hdr = (transfer, extra = []) => [
  "-c:v",
  "libx265",
  "-preset",
  "ultrafast",
  "-crf",
  "28",
  "-pix_fmt",
  "yuv420p10le",
  "-color_primaries",
  "bt2020",
  "-color_trc",
  transfer,
  "-colorspace",
  "bt2020nc",
  "-color_range",
  "tv",
  "-x265-params",
  ["log-level=error", ...extra].join(":"),
];

// ---------------------------------------------------------------------------------------------------------
// Fixtures. `args` returns the FFmpeg argv for one output (inputs and output options); the final output path
// is appended by the runner. `name` is relative to the output directory.
// ---------------------------------------------------------------------------------------------------------

/**
 * Long path: four ~66-character directories under generated/, so the absolute path is well past MAX_PATH (260).
 * Each segment stays under the 255-character per-component limit.
 */
const LONG_NAME = Array.from(
  { length: 4 },
  (_, i) => `loopcut-long-path-fixture-segment-${i + 1}-${"abcdefghij".repeat(3)}`,
).join("/");

const FIXTURES = [
  {
    name: "h264_320x240.mp4",
    args: () => [
      ...testPattern(320, 240, 2),
      "-f",
      "lavfi",
      "-i",
      "anullsrc=channel_layout=stereo:sample_rate=48000",
      "-map",
      "0:v",
      "-map",
      "1:a",
      ...H264,
      "-c:a",
      "aac",
      "-b:a",
      "64k",
      "-shortest",
      "-movflags",
      "+faststart",
      ...DETERMINISTIC,
    ],
  },
  {
    // Frame timestamps advance unevenly (1.25, 1.0, 0.25 frame intervals), so the stream is VFR.
    name: "h264_vfr.mp4",
    args: () => [
      ...testPattern(320, 240, 2),
      "-vf",
      "setpts='(N+0.25*mod(N,3))/(30*TB)'",
      "-fps_mode",
      "passthrough",
      ...H264,
      "-an",
      "-movflags",
      "+faststart",
      ...DETERMINISTIC,
    ],
  },
  {
    // Remux with a 90 degree display rotation: the coded frame stays 320x240 landscape, the display is portrait.
    name: "portrait_rotated.mp4",
    requires: "h264_320x240.mp4",
    args: (dir) => [
      "-display_rotation",
      "90",
      "-i",
      join(dir, "h264_320x240.mp4"),
      "-map",
      "0",
      "-c",
      "copy",
      "-movflags",
      "+faststart",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "hevc10_640x360.mkv",
    args: () => [
      ...testPattern(640, 360, 1),
      "-c:v",
      "libx265",
      "-preset",
      "ultrafast",
      "-crf",
      "28",
      "-pix_fmt",
      "yuv420p10le",
      "-x265-params",
      "log-level=error",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "hdr_pq.mp4",
    args: () => [
      ...testPattern(320, 240, 1),
      ...hdr("smpte2084", [
        "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)",
        "max-cll=1000,400",
      ]),
      "-an",
      "-movflags",
      "+faststart",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "hdr_hlg.mp4",
    args: () => [...testPattern(320, 240, 1), ...hdr("arib-std-b67"), "-an", "-movflags", "+faststart", ...DETERMINISTIC],
  },
  {
    name: "yuv422_prores.mov",
    args: () => [
      ...testPattern(320, 240, 1),
      "-c:v",
      "prores_ks",
      "-profile:v",
      "2",
      "-pix_fmt",
      "yuv422p10le",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "alpha_prores4444.mov",
    args: () => [
      ...alphaGradient(320, 240, 1),
      "-c:v",
      "prores_ks",
      "-profile:v",
      "4444",
      "-pix_fmt",
      "yuva444p10le",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "alpha_vp9.webm",
    args: () => [
      ...alphaGradient(320, 240, 1),
      "-c:v",
      "libvpx-vp9",
      "-pix_fmt",
      "yuva420p",
      "-b:v",
      "0",
      "-crf",
      "40",
      "-auto-alt-ref",
      "0",
      "-deadline",
      "realtime",
      "-cpu-used",
      "8",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "mjpeg.avi",
    args: () => [
      ...testPattern(320, 240, 1),
      "-c:v",
      "mjpeg",
      "-q:v",
      "5",
      "-pix_fmt",
      "yuvj420p",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "vp9.webm",
    args: () => [
      ...testPattern(320, 240, 1),
      "-c:v",
      "libvpx-vp9",
      "-pix_fmt",
      "yuv420p",
      "-b:v",
      "0",
      "-crf",
      "40",
      "-deadline",
      "realtime",
      "-cpu-used",
      "8",
      "-row-mt",
      "1",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "av1.mp4",
    args: () => [
      ...testPattern(320, 240, 1),
      "-c:v",
      "libsvtav1",
      "-preset",
      "10",
      "-crf",
      "45",
      "-pix_fmt",
      "yuv420p",
      "-an",
      "-movflags",
      "+faststart",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "input.gif",
    args: () => [
      ...testPattern(160, 120, 1, 10),
      "-filter_complex",
      "[0:v]split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer",
      "-loop",
      "0",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    // Cover art: the main video plus a one-frame PNG marked attached_pic, as phone and tag-embedded MP4s carry.
    name: "cover_art.mp4",
    args: () => [
      ...testPattern(320, 240, 1),
      "-f",
      "lavfi",
      "-i",
      "color=c=0x335577:size=320x240:duration=0.04",
      "-map",
      "0:v",
      "-map",
      "1:v",
      "-c:v:0",
      "libx264",
      "-preset",
      "ultrafast",
      "-crf",
      "28",
      "-pix_fmt:v:0",
      "yuv420p",
      "-c:v:1",
      "png",
      "-frames:v:1",
      "1",
      "-disposition:v:1",
      "attached_pic",
      "-an",
      "-movflags",
      "+faststart",
      ...DETERMINISTIC,
    ],
  },
  {
    // Two video streams with different sizes and frame rates (stream selection must choose one).
    name: "multi_video.mkv",
    args: () => [
      ...testPattern(320, 240, 1),
      "-f",
      "lavfi",
      "-i",
      "smptebars=size=160x120:duration=1:rate=24",
      "-map",
      "0:v",
      "-map",
      "1:v",
      "-c:v",
      "libx264",
      "-preset",
      "ultrafast",
      "-crf",
      "28",
      "-pix_fmt",
      "yuv420p",
      "-an",
      ...DETERMINISTIC,
    ],
  },
  {
    name: "unicode vidéo テスト 🎬.mp4",
    args: () => [...testPattern(320, 240, 1), ...H264, "-an", "-movflags", "+faststart", ...DETERMINISTIC],
  },
  {
    name: `long-path/${LONG_NAME}/long_path_fixture.mp4`,
    args: () => [...testPattern(320, 240, 1), ...H264, "-an", "-movflags", "+faststart", ...DETERMINISTIC],
  },
];

// ---------------------------------------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------------------------------------

/** Runs ffmpeg with `argv`, returning null on success or the last stderr lines on failure. */
function runFfmpeg(argv) {
  const result = spawnSync(ffmpeg, ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", ...argv], {
    encoding: "utf8",
    windowsHide: true,
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) return result.error.message;
  if (result.status !== 0) {
    return (result.stderr || `exit ${result.status}`).trim().split(/\r?\n/).slice(-3).join(" | ");
  }
  return null;
}

/** Summarizes the streams of a generated file with ffprobe, e.g. "h264/yuv420p 320x240, aac". */
function describe(file) {
  const result = spawnSync(
    ffprobe,
    ["-v", "error", "-show_entries", "stream=codec_type,codec_name,pix_fmt,width,height", "-of", "json", file],
    { encoding: "utf8", windowsHide: true },
  );
  if (result.status !== 0) return "ffprobe failed";
  const streams = JSON.parse(result.stdout).streams ?? [];
  return streams
    .map((s) => {
      if (s.codec_type === "video") return `${s.codec_name}/${s.pix_fmt ?? "?"} ${s.width}x${s.height}`;
      return s.codec_type === "audio" ? s.codec_name : `${s.codec_type}:${s.codec_name ?? "?"}`;
    })
    .join(", ");
}

function main() {
  if (!existsSync(ffmpeg) || !existsSync(ffprobe)) {
    console.error(`gen-test-media: ${stageDir} has no ffmpeg/ffprobe; run "node scripts/fetch-ffmpeg.mjs" first`);
    process.exit(1);
  }
  mkdirSync(outDir, { recursive: true });

  const results = [];
  for (const fixture of FIXTURES) {
    const target = join(outDir, fixture.name);
    const ext = extname(fixture.name);
    const temp = join(dirname(target), `${basename(target, ext)}.tmp${ext}`);
    mkdirSync(dirname(target), { recursive: true });

    if (!force && existsSync(target) && statSync(target).size > 0) {
      results.push({ name: fixture.name, status: "skipped", streams: describe(target), error: "" });
      continue;
    }
    if (fixture.requires && !existsSync(join(outDir, fixture.requires))) {
      results.push({ name: fixture.name, status: "failed", streams: "", error: `missing ${fixture.requires}` });
      continue;
    }

    rmSync(temp, { force: true });
    const error = runFfmpeg([...fixture.args(outDir), temp]);
    if (error) {
      rmSync(temp, { force: true });
      results.push({ name: fixture.name, status: "failed", streams: "", error });
      continue;
    }
    renameSync(temp, target);
    results.push({ name: fixture.name, status: "created", streams: describe(target), error: "" });
  }

  const failed = results.filter((r) => r.status === "failed");
  const width = Math.max(...results.map((r) => r.name.length), 4);
  console.log(`gen-test-media: ${outDir}`);
  console.log("");
  for (const r of results) {
    const line = `${r.status.padEnd(8)} ${r.name.padEnd(width)}  ${r.streams || r.error}`;
    console.log(line.trimEnd());
  }
  console.log("");
  const created = results.filter((r) => r.status === "created").length;
  const skipped = results.filter((r) => r.status === "skipped").length;
  console.log(`${results.length} fixtures: ${created} created, ${skipped} skipped, ${failed.length} failed`);
  if (failed.length > 0) {
    console.error(`gen-test-media: ${failed.length} fixture(s) failed`);
    process.exit(1);
  }
}

main();

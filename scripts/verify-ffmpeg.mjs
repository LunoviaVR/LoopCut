#!/usr/bin/env node
// Verifies the staged, pinned FFmpeg build that LoopCut bundles and tests against.
//
// The build must list every REQUIRED encoder, decoder, filter and hardware-acceleration method that the
// filtergraph builder and the probes rely on; anything missing fails the check (exit code 1). OPTIONAL
// names are reported as warnings only. The staged ffprobe must run, and the staged marker must match
// scripts/ffmpeg.lock.json.
//
// Usage: node scripts/verify-ffmpeg.mjs

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PLATFORM_KEY = "windows-x86_64"; // Same key scripts/fetch-ffmpeg.mjs uses.

/** Names that must be present. Keyed by the `ffmpeg -<kind>` listing they appear in. */
const REQUIRED = {
  encoders: [
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "libx264",
    "libx265",
    "libsvtav1",
    "libvpx-vp9",
  ],
  decoders: [
    "h264_cuvid",
    "hevc_cuvid",
    "av1_cuvid",
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
  ],
  filters: [
    "scale_cuda",
    "vpp_qsv",
    "vpp_amf",
    "palettegen",
    "paletteuse",
    "crop",
    "hwupload",
    "hwdownload",
    "fps",
    "scale",
    "format",
  ],
  hwaccels: ["cuda", "d3d11va", "qsv", "amf"],
};

/** Names that are useful when present but must not fail the build. */
const OPTIONAL = {
  encoders: [],
  decoders: [],
  filters: ["scale_d3d11", "scale_d3d12", "libplacebo"],
  hwaccels: ["d3d12va"],
};

/**
 * Extracts the names from one `ffmpeg -<kind>` listing.
 *
 * Listings print a legend, a dashed separator, then one entry per line. Codec entries are a space,
 * six flag characters, a space and the name; filter entries are a space, two flag characters, a space
 * and the name. `-hwaccels` prints a header followed by one bare name per line.
 */
function parseListing(kind, text) {
  const names = new Set();
  const lines = text.split(/\r?\n/);

  if (kind === "hwaccels") {
    for (const line of lines) {
      const name = line.trim();
      if (/^[a-z0-9_]+$/.test(name)) names.add(name);
    }
    return names;
  }

  const separator = lines.findIndex((line) => /^-{3,}$/.test(line.trim()));
  if (separator < 0) return names;
  const flagWidth = kind === "filters" ? 2 : 6;
  const entry = new RegExp(`^ .{${flagWidth}} (\\S+)`);
  for (const line of lines.slice(separator + 1)) {
    const match = entry.exec(line);
    if (match) names.add(match[1]);
  }
  return names;
}

function fail(message) {
  console.error(`verify-ffmpeg: ${message}`);
  process.exitCode = 1;
}

function main() {
  let lock;
  try {
    lock = JSON.parse(readFileSync(join(root, "scripts", "ffmpeg.lock.json"), "utf8"));
  } catch (err) {
    fail(`cannot read scripts/ffmpeg.lock.json: ${err.message}`);
    return;
  }
  const pin = lock[PLATFORM_KEY];
  if (!pin) {
    fail(`scripts/ffmpeg.lock.json has no ${PLATFORM_KEY} entry`);
    return;
  }

  const stageDir = join(root, "src-tauri", "resources", "ffmpeg", lock.version);
  const exe = (name) => join(stageDir, process.platform === "win32" ? `${name}.exe` : name);
  const ffmpeg = exe("ffmpeg");
  const ffprobe = exe("ffprobe");

  if (!existsSync(ffmpeg)) {
    fail(`${ffmpeg} is missing; run "node scripts/fetch-ffmpeg.mjs" first`);
    return;
  }

  // The staged marker records which archive was extracted. A mismatch means the folder was edited by hand
  // or staged from another pin.
  const marker = join(stageDir, "loopcut-ffmpeg.json");
  if (!existsSync(marker)) {
    console.warn("verify-ffmpeg: warning: no loopcut-ffmpeg.json marker in the staged folder");
  } else {
    try {
      const staged = JSON.parse(readFileSync(marker, "utf8"));
      if (staged.version !== lock.version) {
        fail(`staged version ${staged.version ?? "?"} does not match scripts/ffmpeg.lock.json (${lock.version})`);
      } else if (staged.sha256 !== pin.sha256) {
        fail("staged archive SHA-256 does not match scripts/ffmpeg.lock.json; run node scripts/fetch-ffmpeg.mjs --force");
      }
    } catch (err) {
      fail(`cannot parse ${marker}: ${err.message}`);
    }
  }

  const run = (bin, args) => {
    const result = spawnSync(bin, args, {
      encoding: "utf8",
      windowsHide: true,
      maxBuffer: 64 * 1024 * 1024,
    });
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new Error(`${bin} ${args.join(" ")} exited with ${result.status}: ${result.stderr.trim()}`);
    }
    return result.stdout;
  };

  let ffmpegVersion;
  let ffprobeVersion;
  try {
    ffmpegVersion = run(ffmpeg, ["-hide_banner", "-version"]).split(/\r?\n/)[0];
    ffprobeVersion = run(ffprobe, ["-hide_banner", "-version"]).split(/\r?\n/)[0];
  } catch (err) {
    fail(err.message);
    return;
  }

  // One row per (kind, name). Listings are fetched once per kind.
  const rows = [];
  const listings = {};
  for (const kind of ["encoders", "decoders", "filters", "hwaccels"]) {
    try {
      const flag = kind === "hwaccels" ? "-hwaccels" : `-${kind}`;
      listings[kind] = parseListing(kind, run(ffmpeg, ["-hide_banner", flag]));
    } catch (err) {
      fail(err.message);
      return;
    }
    for (const name of REQUIRED[kind]) rows.push({ kind, name, level: "required" });
    for (const name of OPTIONAL[kind]) rows.push({ kind, name, level: "optional" });
  }

  let missingRequired = 0;
  let missingOptional = 0;
  const table = rows.map((row) => {
    const present = listings[row.kind].has(row.name);
    let status;
    if (present) status = "ok";
    else if (row.level === "required") {
      status = "MISSING";
      missingRequired += 1;
    } else {
      status = "warn (optional)";
      missingOptional += 1;
    }
    return { ...row, status };
  });

  const width = Math.max(...table.map((row) => row.name.length), 4);
  console.log(`ffmpeg:  ${ffmpegVersion}`);
  console.log(`ffprobe: ${ffprobeVersion}`);
  console.log(`dir:     ${stageDir}`);
  console.log("");
  console.log(`${"Kind".padEnd(10)}${"Name".padEnd(width + 2)}${"Level".padEnd(10)}Status`);
  console.log(`${"-".repeat(10)}${"-".repeat(width + 2)}${"-".repeat(10)}${"-".repeat(15)}`);
  for (const row of table) {
    console.log(
      `${row.kind.padEnd(10)}${row.name.padEnd(width + 2)}${row.level.padEnd(10)}${row.status}`,
    );
  }
  console.log("");
  console.log(
    `${table.length - missingRequired - missingOptional}/${table.length} present; ` +
      `${missingRequired} required missing; ${missingOptional} optional missing`,
  );

  if (missingRequired > 0) {
    fail(`${missingRequired} required FFmpeg capabilities are missing from ${ffmpeg}`);
  } else {
    console.log("verify-ffmpeg: all required FFmpeg capabilities are present");
  }
}

main();

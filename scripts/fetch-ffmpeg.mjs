#!/usr/bin/env node

// Downloads the pinned FFmpeg build (scripts/ffmpeg.lock.json), verifies its SHA-256 and stages the
// binaries into src-tauri/resources/ffmpeg/<version>/ for bundling and for tests.
//
// Usage: node scripts/fetch-ffmpeg.mjs [--force]

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  createReadStream,
  createWriteStream,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const lock = JSON.parse(readFileSync(join(root, "scripts", "ffmpeg.lock.json"), "utf8"));
const force = process.argv.includes("--force");

const platformKey =
  process.platform === "win32" && process.arch === "x64" ? "windows-x86_64" : null;
if (!platformKey || !lock[platformKey]) {
  console.error(`fetch-ffmpeg: no pinned FFmpeg build for ${process.platform}-${process.arch}`);
  process.exit(1);
}
const pin = lock[platformKey];

const stageRoot = join(root, "src-tauri", "resources", "ffmpeg");
const stageDir = join(stageRoot, lock.version);
const marker = join(stageDir, "loopcut-ffmpeg.json");

if (!force && existsSync(marker)) {
  const current = JSON.parse(readFileSync(marker, "utf8"));
  if (current.sha256 === pin.sha256) {
    console.log(`fetch-ffmpeg: ${lock.version} already staged at ${stageDir}`);
    process.exit(0);
  }
}

const cacheDir = join(root, ".cache", "ffmpeg");
mkdirSync(cacheDir, { recursive: true });
const zipPath = join(cacheDir, pin.asset);

async function sha256(file) {
  const hash = createHash("sha256");
  await pipeline(createReadStream(file), hash);
  return hash.digest("hex");
}

async function download(url, dest) {
  const res = await fetch(url, { redirect: "follow", headers: { "User-Agent": "loopcut-build" } });
  if (!res.ok || !res.body) throw new Error(`HTTP ${res.status} for ${url}`);
  const tmp = `${dest}.partial`;
  await pipeline(Readable.fromWeb(res.body), createWriteStream(tmp));
  renameSync(tmp, dest);
}

if (!existsSync(zipPath) || (await sha256(zipPath)) !== pin.sha256) {
  let ok = false;
  for (const url of pin.urls) {
    try {
      console.log(`fetch-ffmpeg: downloading ${url}`);
      await download(url, zipPath);
      const got = await sha256(zipPath);
      if (got !== pin.sha256) {
        console.warn(`fetch-ffmpeg: checksum mismatch from ${url} (got ${got})`);
        rmSync(zipPath, { force: true });
        continue;
      }
      ok = true;
      break;
    } catch (err) {
      console.warn(`fetch-ffmpeg: ${err.message}`);
    }
  }
  if (!ok) {
    console.error("fetch-ffmpeg: could not download a build matching the pinned SHA-256");
    process.exit(1);
  }
}

// Extract into a fresh, empty directory (downloaded archives are untrusted until verified).
const extractDir = join(cacheDir, `extract-${lock.version}`);
rmSync(extractDir, { recursive: true, force: true });
mkdirSync(extractDir, { recursive: true });
// Prefer Windows' bundled bsdtar (reads zip); GNU tar from Git Bash treats "D:" as a remote host.
const tarBin =
  process.platform === "win32"
    ? join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe")
    : "tar";
let extracted = spawnSync(tarBin, ["-xf", zipPath, "-C", extractDir], { stdio: "inherit" });
if (extracted.status !== 0) {
  extracted = spawnSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-Command",
      `Expand-Archive -LiteralPath '${zipPath}' -DestinationPath '${extractDir}' -Force`,
    ],
    { stdio: "inherit" },
  );
}
if (extracted.status !== 0) {
  console.error("fetch-ffmpeg: failed to extract archive");
  process.exit(1);
}

const [top] = readdirSync(extractDir);
const buildDir = join(extractDir, top);
const binDir = join(buildDir, "bin");

// Replace any previously staged versions so stale binaries can never be bundled.
rmSync(stageRoot, { recursive: true, force: true });
mkdirSync(stageDir, { recursive: true });
const wanted = readdirSync(binDir).filter(
  (f) => f === "ffmpeg.exe" || f === "ffprobe.exe" || f.toLowerCase().endsWith(".dll"),
);
for (const f of wanted) copyFileSync(join(binDir, f), join(stageDir, f));
copyFileSync(join(buildDir, "LICENSE.txt"), join(stageDir, "LICENSE.txt"));
writeFileSync(
  marker,
  `${JSON.stringify({ version: lock.version, asset: pin.asset, sha256: pin.sha256, files: wanted }, null, 2)}\n`,
);
rmSync(extractDir, { recursive: true, force: true });
console.log(`fetch-ffmpeg: staged ${wanted.length} files into ${stageDir}`);

/**
 * Browser-only backend for `npm run dev` outside Tauri. Every export mirrors one command in
 * src/ipc/commands.ts and returns synthetic data: one 1920x1080 30 fps 12.4 s H.264 source, an AMD GPU
 * plus Microsoft's software renderer in the capability table, and procedurally drawn preview frames.
 * Nothing here touches the filesystem or the network. Failures are thrown as plain AppError objects.
 */

import { defaultCaptionStyle, defaultExportSettings, defaultGrade } from "@/domain/defaults";
import { buildSequencePlan } from "@/domain/sequence";
import { toFloat } from "@/domain/time";
import type { AppError } from "./bindings/AppError";
import type { AppInfo } from "./bindings/AppInfo";
import type { AppSettings } from "./bindings/AppSettings";
import type { CacheUsage } from "./bindings/CacheUsage";
import type { Capabilities } from "./bindings/Capabilities";
import type { Caption } from "./bindings/Caption";
import type { DecodeCap } from "./bindings/DecodeCap";
import type { EncodeCap } from "./bindings/EncodeCap";
import type { ImportedMedia } from "./bindings/ImportedMedia";
import type { LutInfo } from "./bindings/LutInfo";
import type { OpenedProject } from "./bindings/OpenedProject";
import type { PlatformPreset } from "./bindings/PlatformPreset";
import type { PreviewInfo } from "./bindings/PreviewInfo";
import type { ProbeEvent } from "./bindings/ProbeEvent";
import type { Project } from "./bindings/Project";
import type { ProxyEvent } from "./bindings/ProxyEvent";
import type { RecentProject } from "./bindings/RecentProject";
import type { RecoverySnapshot } from "./bindings/RecoverySnapshot";
import type { SequencePlanSummary } from "./bindings/SequencePlanSummary";
import type { ThumbnailSet } from "./bindings/ThumbnailSet";
import type { Viewport } from "./bindings/Viewport";
import { encodeFrame } from "./frames";
import type {
  ExportEvent,
  PreviewMode,
  PreviewStatus,
  SizeEstimate,
  ThumbnailPixels,
} from "./types";

const SAMPLE_PATH = "C:\\Users\\Demo\\Videos\\ranked-ace.mp4";
const SAMPLE_WIDTH = 1920;
const SAMPLE_HEIGHT = 1080;
const SAMPLE_DURATION = 12.4;
const SAMPLE_FPS = 30;
const SAMPLE_BYTES = 21_800_000;
const ESTIMATE_BYTES = 3_565_000;
const MOCK_DELAY_MS = 120;

function fail(code: string, message: string, detail?: string): never {
  const error: AppError = detail === undefined ? { code, message } : { code, message, detail };
  throw error;
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

const mediaByToken = new Map<string, ImportedMedia>();
let tokenCounter = 0;
let preview: { project: Project; viewport: Viewport; info: PreviewInfo } | null = null;
let stopPlayback: (() => void) | null = null;
/** When the last preview was set up; the mock reports decode progress from it. */
let decodeStartedAt = 0;
/** How long the mock takes to "decode" after a preview is set up. */
const MOCK_DECODE_MS = 1200;
const exportTimers = new Map<string, ReturnType<typeof setTimeout>[]>();
let exportCounter = 0;

function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function syntheticMedia(path: string, token: string): ImportedMedia {
  const fileName = baseName(path);
  return {
    token,
    path,
    fileName,
    info: {
      container: "mp4",
      duration: SAMPLE_DURATION,
      sizeBytes: SAMPLE_BYTES,
      video: {
        index: 0,
        codec: "h264",
        profile: "High",
        width: SAMPLE_WIDTH,
        height: SAMPLE_HEIGHT,
        sampleAspect: { num: 1, den: 1 },
        pixFmt: "yuv420p",
        bitDepth: 8,
        hasAlpha: false,
        rotation: 0,
        frameRate: { num: SAMPLE_FPS, den: 1 },
        avgFrameRate: { num: SAMPLE_FPS, den: 1 },
        variableFrameRate: false,
        color: { matrix: "bt709", range: "limited", transfer: "sdr", wideGamut: false },
        startTime: 0,
        duration: SAMPLE_DURATION,
        frameCount: Math.round(SAMPLE_DURATION * SAMPLE_FPS),
        interlaced: false,
        webPlayable: true,
      },
      alternateVideoStreams: [],
      hasAudio: true,
    },
    sourceUrl: null,
    needsProxy: false,
    proxyUrl: null,
    keyframeInterval: 60,
  };
}

function requireMedia(token: string): ImportedMedia {
  const media = mediaByToken.get(token);
  if (!media) fail("media_not_found", "That media is no longer open. Import the file again.");
  return media;
}

function defaultProject(media: ImportedMedia): Project {
  return {
    schemaVersion: 1,
    name: media.fileName.replace(/\.[^.]+$/, ""),
    clips: [
      {
        id: "clip-1",
        path: media.path,
        streamIndex: media.info.video.index,
        segments: [{ start: 0, end: 6 }],
        holds: [],
      },
    ],
    timing: { speed: 1, direction: "forward", crossfade: 0 },
    transform: {
      crop: { x: 0, y: 0, w: 1, h: 1 },
      cropAspect: null,
      rotation: 0,
      flipH: false,
      flipV: false,
    },
    output: {
      fps: { num: 20, den: 1 },
      size: { mode: "width", width: 480 },
      loopCount: 0,
      background: [0, 0, 0, 255],
    },
    grade: defaultGrade(),
    captions: [],
    export: defaultExportSettings(),
  };
}

function outputWidthOf(project: Project): number {
  const size = project.output.size;
  if (size.mode === "width") return size.width;
  if (size.mode === "fit")
    return Math.min(size.maxWidth, Math.round(size.maxHeight * (SAMPLE_WIDTH / SAMPLE_HEIGHT)));
  return Math.round(SAMPLE_WIDTH * size.factor);
}

function evenUp(n: number): number {
  return Math.max(2, Math.round(n / 2) * 2);
}

/** Output frame count from the same plan the timeline uses (the mirror of the Rust sequencer). */
function frameCountOf(project: Project): number {
  return Math.max(1, buildSequencePlan(project)?.frames.length ?? 1);
}

/** Procedural frame: a moving gradient plus a 12-bit binary frame counter across the top. */
function renderPreview(frame: number, w: number, h: number): ArrayBuffer {
  const rgba = new Uint8ClampedArray(w * h * 4);
  const shift = frame * 4;
  for (let y = 0; y < h; y++) {
    const g = Math.round((y / h) * 255);
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      rgba[i] = (Math.round((x / w) * 255) + shift) & 255;
      rgba[i + 1] = g;
      rgba[i + 2] = (255 - Math.round((x / w) * 255) + shift * 2) & 255;
      rgba[i + 3] = 255;
    }
  }
  const cell = Math.max(2, Math.round(w / 64));
  for (let bit = 0; bit < 12; bit++) {
    const on = (frame >> bit) & 1;
    const x0 = cell + bit * (cell + 2);
    for (let y = cell; y < cell * 3 && y < h; y++) {
      for (let x = x0; x < x0 + cell && x < w; x++) {
        const i = (y * w + x) * 4;
        const v = on ? 255 : 40;
        rgba[i] = v;
        rgba[i + 1] = v;
        rgba[i + 2] = v;
      }
    }
  }
  return encodeFrame(w, h, rgba);
}

/** Synthetic probe result shaped like the real RX 9060 XT run on FFmpeg 9.0.2 (see docs/architecture.md). */
const CAPABILITIES: Capabilities = {
  schema: 1,
  ffmpegVersion: "n9.0.2-25-g67b60c310b-20261010",
  probedAtUnix: 1_791_652_800,
  devices: [
    {
      device: {
        key: { vendorId: 0x1002, deviceId: 0x7590, ordinal: 0, name: "AMD Radeon RX 9060 XT" },
        vendor: "amd",
        dxgiIndex: 0,
        luid: 0x1a2b3c,
        dedicatedVramMb: 16_112,
        driverVersion: "32.0.31041.1004",
        isSoftware: false,
        cudaIndex: null,
      },
      decode: [
        decodeCap("h264", false, "d3d11va"),
        decodeCap("h264", false, "amf"),
        decodeCap("hevc", true, "d3d11va"),
        decodeCap("hevc", true, "amf"),
        decodeCap("av1", false, "d3d11va"),
        decodeCap("av1", false, "amf"),
      ],
      scale: [
        { codec: "h264", scaler: "amf", backend: "amf", status: "ok", error: null },
        {
          codec: "h264",
          scaler: "d3d11",
          backend: "d3d11va",
          status: "failed",
          error: "Could not create the texture (80070057)",
        },
      ],
      encode: [
        encodeCap("h264", "amf", 256, 144),
        encodeCap("hevc", "amf", 426, 240),
        encodeCap("av1", "amf", 320, 180),
      ],
    },
    {
      device: {
        key: {
          vendorId: 0x1414,
          deviceId: 0x008c,
          ordinal: 0,
          name: "Microsoft Basic Render Driver",
        },
        vendor: "microsoft",
        dxgiIndex: 1,
        luid: 0,
        dedicatedVramMb: 0,
        driverVersion: null,
        isSoftware: true,
        cudaIndex: null,
      },
      decode: [],
      scale: [],
      encode: [],
    },
  ],
  cpuEncode: [
    {
      codec: "h264",
      backend: "libx264",
      status: "ok",
      minWidth: null,
      minHeight: null,
      speedX: null,
      error: null,
    },
    {
      codec: "hevc",
      backend: "libx265",
      status: "ok",
      minWidth: null,
      minHeight: null,
      speedX: null,
      error: null,
    },
    {
      codec: "av1",
      backend: "libsvtav1",
      status: "ok",
      minWidth: null,
      minHeight: null,
      speedX: null,
      error: null,
    },
    {
      codec: "vp9",
      backend: "libvpx-vp9",
      status: "ok",
      minWidth: null,
      minHeight: null,
      speedX: null,
      error: null,
    },
  ],
  warnings: [],
};

function decodeCap(codec: DecodeCap["codec"], tenBit: boolean, backend: string): DecodeCap {
  return { codec, tenBit, backend, status: "ok", speedX: null, error: null };
}

function encodeCap(
  codec: EncodeCap["codec"],
  backend: string,
  minWidth: number,
  minHeight: number,
): EncodeCap {
  return { codec, backend, status: "ok", minWidth, minHeight, speedX: null, error: null };
}

const DEFAULT_SETTINGS: AppSettings = {
  hw: {
    enabled: true,
    gpu: { mode: "auto" },
    decode: "auto",
    proxyEncoder: "auto",
    exportEncoder: "auto",
    effects: "gpu",
    gpuScaling: true,
  },
  updates: { checkOnStartup: true, skippedVersion: null, lastCheck: null },
  cache: { maxDiskGb: 10, ramBudgetMb: 1024 },
  general: { defaultExportDir: null, recentLimit: 10 },
};

let settings: AppSettings = JSON.parse(JSON.stringify(DEFAULT_SETTINGS)) as AppSettings;

export async function mediaImport(path: string, confirmNetwork?: boolean): Promise<ImportedMedia> {
  await sleep(MOCK_DELAY_MS);
  if (!path.trim()) fail("invalid_path", "Choose a video file to open.");
  if (path.startsWith("\\\\") && confirmNetwork !== true) {
    fail("network_path", "This file is on a network share. Confirm before LoopCut reads it.");
  }
  tokenCounter += 1;
  const token = `tok-${tokenCounter}`;
  const media = syntheticMedia(path, token);
  mediaByToken.set(token, media);
  return media;
}

export async function mediaThumbnails(
  token: string,
  count: number,
  height: number,
): Promise<ThumbnailSet> {
  requireMedia(token);
  const realHeight = evenDown(height);
  const width = evenDown(Math.round((realHeight * SAMPLE_WIDTH) / SAMPLE_HEIGHT));
  const times = Array.from({ length: Math.max(1, count) }, (_, i) =>
    Number(((SAMPLE_DURATION * i) / Math.max(1, count)).toFixed(3)),
  );
  return { width, height: realHeight, times };
}

/** Synthetic filmstrip pixels: a hue sweep that moves with the thumbnail index, so neighbours read differently. */
export async function thumbnailPixels(
  token: string,
  height: number,
  index: number,
): Promise<ThumbnailPixels> {
  requireMedia(token);
  const h = evenDown(height);
  const w = evenDown(Math.round((h * SAMPLE_WIDTH) / SAMPLE_HEIGHT));
  const rgba = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      rgba[i] = (Math.round((x / w) * 255) + index * 23) & 255;
      rgba[i + 1] = Math.round((y / h) * 160) + 40;
      rgba[i + 2] = (255 - Math.round((x / w) * 255) + index * 11) & 255;
      rgba[i + 3] = 255;
    }
  }
  return { width: w, height: h, rgba };
}

function evenDown(n: number): number {
  return Math.max(2, n - (n % 2));
}

export async function mediaProxyStart(
  token: string,
  onProgress: (e: ProxyEvent) => void,
): Promise<void> {
  requireMedia(token);
  for (const value of [0.25, 0.5, 0.75]) {
    await sleep(MOCK_DELAY_MS);
    onProgress({ kind: "progress", value });
  }
  onProgress({ kind: "done", url: `mock://proxy/${token}` });
}

export async function mediaRelease(token: string): Promise<void> {
  mediaByToken.delete(token);
}

export async function projectNew(token: string): Promise<Project> {
  return defaultProject(requireMedia(token));
}

export async function projectOpen(path: string): Promise<OpenedProject> {
  const media = await mediaImport(path);
  return {
    project: defaultProject(media),
    path,
    tokens: { "clip-1": media.token },
    migrated: false,
    missingMedia: [],
    networkMedia: [],
  };
}

export async function projectSave(_path: string, _project: Project): Promise<void> {
  await sleep(MOCK_DELAY_MS);
}

export async function projectAutosave(_project: Project): Promise<void> {}

export async function projectRecovery(): Promise<RecoverySnapshot | null> {
  return null;
}

export async function projectRecoveryClear(): Promise<void> {}

export async function recentProjects(): Promise<RecentProject[]> {
  return [
    {
      path: "C:\\Users\\Demo\\Videos\\LoopCut\\ranked-ace.loopcut",
      name: "ranked-ace",
      openedAt: "2026-10-09T18:42:00Z",
    },
    {
      path: "C:\\Users\\Demo\\Videos\\LoopCut\\reaction-takes.loopcut",
      name: "reaction-takes",
      openedAt: "2026-10-07T21:05:00Z",
    },
  ];
}

export async function sequencePlan(project: Project): Promise<SequencePlanSummary> {
  const plan = buildSequencePlan(project);
  if (!plan) fail("invalid_argument", "This project has no kept range to plan.");
  const fps = toFloat(plan.fps);
  return {
    frames: plan.frames.length,
    durationSeconds: plan.frames.length / fps,
    effectiveFps: fps,
    delaysCs: plan.frames.map((frame) => frame.delayCs),
  };
}

export async function previewSetProject(
  project: Project,
  viewport: Viewport,
): Promise<PreviewInfo> {
  const outputWidth = outputWidthOf(project);
  const aspect = SAMPLE_HEIGHT / SAMPLE_WIDTH;
  const outputHeight = evenUp(outputWidth * aspect);
  const target = Math.max(160, Math.round(viewport.width * viewport.dpr));
  const renderWidth = Math.min(outputWidth, target);
  const renderHeight = evenUp(renderWidth * aspect);
  const info: PreviewInfo = {
    outputWidth,
    outputHeight,
    renderWidth,
    renderHeight,
    frames: frameCountOf(project),
    fps: project.output.fps,
    delaysCs: (buildSequencePlan(project)?.frames ?? []).map((frame) => frame.delayCs),
    quality: renderWidth < outputWidth ? "reduced" : "full",
  };
  preview = { project, viewport, info };
  decodeStartedAt = Date.now();
  return info;
}

export async function previewFrame(
  frame: number,
  mode: PreviewMode,
  sourceTime?: number,
  maxWidth?: number,
  maxHeight?: number,
): Promise<ArrayBuffer> {
  if (!preview) fail("preview_no_project", "Open a video before previewing.");
  if (mode === "output" && (!Number.isFinite(frame) || frame < 0 || frame >= preview.info.frames)) {
    fail("preview_frame_out_of_range", `Frame ${frame} is outside the plan.`);
  }
  if (mode === "source") {
    // The uncropped source, fitted into the bounds without enlarging past the source size.
    const boundW = Math.max(2, Math.floor(maxWidth ?? 640));
    const boundH = Math.max(2, Math.floor(maxHeight ?? 360));
    const scale = Math.min(boundW / SAMPLE_WIDTH, boundH / SAMPLE_HEIGHT);
    const w = evenDown(Math.round(SAMPLE_WIDTH * scale));
    const h = evenDown(Math.round(SAMPLE_HEIGHT * scale));
    const sourceFrame = Math.round(Math.max(0, sourceTime ?? 0) * SAMPLE_FPS);
    return renderPreview(sourceFrame, w, h);
  }
  return renderPreview(Math.floor(frame), preview.info.renderWidth, preview.info.renderHeight);
}

/** Decode progress ramps from 0 to 1 over the mock's decode time; the mock never reports a fallback. */
export async function previewStatus(): Promise<PreviewStatus> {
  const elapsed = Date.now() - decodeStartedAt;
  return {
    decodeProgress: preview ? Math.min(1, Math.max(0, elapsed / MOCK_DECODE_MS)) : 0,
    failedRanges: 0,
    fallbacks: [],
    playing: stopPlayback !== null,
    lastError: null,
  };
}

export function previewPlay(
  fromFrame: number,
  loop: boolean,
  onFrame: (frame: ArrayBuffer) => void,
): Promise<void> {
  if (!preview)
    return Promise.reject({
      code: "preview_no_project",
      message: "Open a video before previewing.",
    });
  stopPlayback?.();
  const info = preview.info;
  return new Promise<void>((resolve) => {
    let frame = Math.min(Math.max(0, Math.floor(fromFrame)), info.frames - 1);
    const timer = setInterval(() => {
      onFrame(renderPreview(frame, info.renderWidth, info.renderHeight));
      frame += 1;
      if (frame >= info.frames) {
        if (loop) frame = 0;
        else finish();
      }
    }, 1000 / toFloat(info.fps));
    const finish = () => {
      clearInterval(timer);
      if (stopPlayback === finish) stopPlayback = null;
      resolve();
    };
    stopPlayback = finish;
  });
}

export async function previewStop(): Promise<void> {
  stopPlayback?.();
}

/** The default export (120 frames, 480 px wide, quality 90) is the size ESTIMATE_BYTES describes. */
const ESTIMATE_BASELINE = { frames: 120, width: 480, quality: 90 };

/** Quality's share of the size: a linear weight, so quality 90 weighs 1 and quality 0 about half of that. */
function qualityWeight(quality: number): number {
  return 0.5 + Math.min(100, Math.max(0, quality)) / 180;
}

/** Mock estimate. Bytes scale with the frame count, with the pixel count (the width squared) and with quality. */
export async function exportEstimate(project: Project): Promise<SizeEstimate> {
  const frames = frameCountOf(project);
  const width = outputWidthOf(project);
  const bytes = Math.round(
    ESTIMATE_BYTES *
      (frames / ESTIMATE_BASELINE.frames) *
      (width / ESTIMATE_BASELINE.width) ** 2 *
      (qualityWeight(project.export.quality) / qualityWeight(ESTIMATE_BASELINE.quality)),
  );
  return {
    bytes,
    low: Math.round(bytes * 0.92),
    high: Math.round(bytes * 1.08),
    frames,
    basis: "sample",
  };
}

/** Export job listeners by id, so a cancel can report the terminal `cancelled` error like the backend does. */
const exportListeners = new Map<string, (e: ExportEvent) => void>();

/**
 * Mock export: eight progress steps, one hardware fallback when the output name contains "fallback", then a
 * `done` event. Cancelling sends the `cancelled` error. Nothing is written.
 */
export async function exportStart(
  project: Project,
  outputPath: string,
  onEvent: (e: ExportEvent) => void,
): Promise<string> {
  exportCounter += 1;
  const jobId = `export:${exportCounter}`;
  const framesTotal = frameCountOf(project);
  const timers: ReturnType<typeof setTimeout>[] = [];
  exportTimers.set(jobId, timers);
  exportListeners.set(jobId, onEvent);
  const finish = () => {
    exportTimers.delete(jobId);
    exportListeners.delete(jobId);
  };
  const step = (i: number) => {
    const framesDone = Math.min(framesTotal, Math.round((framesTotal * i) / 8));
    const bytes = Math.round((ESTIMATE_BYTES * framesDone) / framesTotal);
    if (i === 2 && outputPath.includes("fallback")) {
      onEvent({
        kind: "fallback",
        stage: "encode",
        from: "nvenc",
        to: "cpu",
        reason: "Simulated hardware failure",
      });
    }
    onEvent({ kind: "progress", framesDone, framesTotal, bytes });
    if (i >= 8) {
      finish();
      onEvent({
        kind: "done",
        path: outputPath,
        bytes: ESTIMATE_BYTES,
        seconds: 1.6,
        frames: framesTotal,
      });
      return;
    }
    timers.push(setTimeout(() => step(i + 1), 200));
  };
  timers.push(setTimeout(() => step(1), 200));
  return jobId;
}

export async function exportCancel(jobId: string): Promise<void> {
  for (const timer of exportTimers.get(jobId) ?? []) clearTimeout(timer);
  exportTimers.delete(jobId);
  const listener = exportListeners.get(jobId);
  exportListeners.delete(jobId);
  listener?.({
    kind: "error",
    error: { code: "cancelled", message: "The export was cancelled." },
  });
}

/** Platform presets as the Rust table ships them (values from crates/loopcut-core/src/presets.rs). */
export async function exportPresets(): Promise<PlatformPreset[]> {
  return [
    {
      id: "discord",
      name: "Discord",
      format: "gif",
      maxBytes: 10_000_000,
      maxWidth: null,
      maxHeight: null,
      maxFrames: null,
      maxFps: 50,
      recommendedWidth: 480,
      recommendedFps: 20,
      verified: false,
      checkedOn: "2026-10-10",
      source: null,
      note: "Conservative limit for free accounts; Nitro plans allow larger uploads.",
    },
    {
      id: "x",
      name: "X (Twitter)",
      format: "gif",
      maxBytes: 15_000_000,
      maxWidth: 1280,
      maxHeight: 1080,
      maxFrames: 350,
      maxFps: 50,
      recommendedWidth: 640,
      recommendedFps: 20,
      verified: true,
      checkedOn: "2026-10-10",
      source:
        "https://developer.x.com/en/docs/x-api/v1/media/upload-media/uploading-media/media-best-practices",
      note: "X converts GIFs to MP4 on upload; at most 350 frames.",
    },
    {
      id: "telegram",
      name: "Telegram",
      format: "mp4H264",
      maxBytes: 50_000_000,
      maxWidth: null,
      maxHeight: null,
      maxFrames: null,
      maxFps: 60,
      recommendedWidth: 720,
      recommendedFps: 30,
      verified: false,
      checkedOn: "2026-10-10",
      source: "https://core.telegram.org/bots/api#sendanimation",
      note: "Telegram turns GIFs into silent MP4 loops; MP4 keeps more quality.",
    },
    {
      id: "whatsapp",
      name: "WhatsApp",
      format: "mp4H264",
      maxBytes: 16_000_000,
      maxWidth: null,
      maxHeight: null,
      maxFrames: null,
      maxFps: 30,
      recommendedWidth: 640,
      recommendedFps: 30,
      verified: false,
      checkedOn: "2026-10-10",
      source: "https://developers.facebook.com/docs/whatsapp/cloud-api/reference/media",
      note: "WhatsApp plays short H.264 MP4 loops as GIFs.",
    },
  ];
}

/** A LUT header as the parser reports it. Nothing is read from disk. */
export async function lutInspect(path: string): Promise<LutInfo> {
  if (!/\.cube$/i.test(path)) fail("invalid_argument", "Choose a .cube LUT file.");
  return { path, title: "Demo warm grade", size: 33, kind: "3d" };
}

/** Two cues on the frame grid of `fps`, in the default caption style. Nothing is read from disk. */
export async function captionsImportSrt(_path: string, fps: number): Promise<Caption[]> {
  const snap = (seconds: number) => Math.round(seconds * fps) / fps;
  const cues: Array<[number, number, string]> = [
    [1, 2.5, "First line from the SubRip file"],
    [3, 4.2, "Second cue, centred low"],
  ];
  return cues.map(([start, end, text], index) => ({
    id: `cap-demo${index + 1}`,
    text,
    range: { start: snap(start), end: snap(end) },
    x: 0.5,
    y: 0.88,
    style: defaultCaptionStyle(),
    animation: { kind: "none" },
  }));
}

export async function fontsList(): Promise<string[]> {
  return ["Arial", "Bahnschrift", "Consolas", "Impact", "Segoe UI", "Segoe UI Variable", "Verdana"];
}

export async function pickLutFile(): Promise<string | null> {
  return "C:\\LUTs\\demo-warm.cube";
}

export async function pickSrtFile(): Promise<string | null> {
  return "C:\\Captions\\demo.srt";
}

export async function pickExportPath(defaultPath: string): Promise<string | null> {
  return defaultPath.includes("\\") || defaultPath.includes("/")
    ? defaultPath
    : `C:\\Users\\Demo\\Videos\\${defaultPath}`;
}

export async function revealInFolder(_path: string): Promise<void> {}

export async function hwCapabilities(): Promise<Capabilities | null> {
  return CAPABILITIES;
}

export async function hwProbe(
  _force: boolean,
  onProgress: (e: ProbeEvent) => void,
): Promise<Capabilities> {
  const labels = ["decode", "encode"];
  for (const { device } of CAPABILITIES.devices) {
    if (device.isSoftware) continue;
    for (const [index, label] of labels.entries()) {
      const step = `${device.key.name}: ${label}`;
      await sleep(MOCK_DELAY_MS);
      onProgress({ kind: "step", label: step, done: index, total: labels.length });
      await sleep(MOCK_DELAY_MS);
      onProgress({ kind: "result", label: step, ok: true });
    }
  }
  return CAPABILITIES;
}

export async function settingsGet(): Promise<AppSettings> {
  return JSON.parse(JSON.stringify(settings)) as AppSettings;
}

export async function settingsSet(next: AppSettings): Promise<void> {
  settings = JSON.parse(JSON.stringify(next)) as AppSettings;
}

export async function cacheUsage(): Promise<CacheUsage> {
  return { bytes: 412_000_000, files: 37, path: "C:\\Users\\Demo\\AppData\\Local\\LoopCut\\cache" };
}

export async function cacheClear(): Promise<void> {}

export async function appInfo(): Promise<AppInfo> {
  return {
    version: "0.0.1",
    ffmpegVersion: "9.0.2",
    logDir: "C:\\Users\\Demo\\AppData\\Local\\LoopCut\\logs",
    dataDir: "C:\\Users\\Demo\\AppData\\Local\\LoopCut",
  };
}

export async function appDiagnostics(): Promise<string> {
  const adapters = CAPABILITIES.devices.map(({ device }) => device.key.name).join(", ");
  return [
    "LoopCut 0.0.1 (browser mock)",
    "FFmpeg 9.0.2",
    `Adapters: ${adapters}`,
    "Hardware acceleration: enabled, GPU auto, decode auto",
  ].join("\n");
}

/** The browser build has no diagnostics run: the spike flag is always off, so these are never reached. */
export async function spikeReport(_json: string): Promise<string> {
  fail("unsupported", "The diagnostics spikes need the desktop app.");
}

export async function benchChannel(
  _width: number,
  _height: number,
  _count: number,
  _onFrame: (frame: ArrayBuffer) => void,
): Promise<void> {
  fail("unsupported", "The diagnostics spikes need the desktop app.");
}

export async function benchInvokeFrame(
  _width: number,
  _height: number,
  _index: number,
): Promise<ArrayBuffer> {
  fail("unsupported", "The diagnostics spikes need the desktop app.");
}

export async function appPrepareUpdate(): Promise<void> {
  stopPlayback?.();
}

export async function appResumeBackground(): Promise<void> {}

export async function appOpenPathArg(): Promise<string | null> {
  return null;
}

export async function pickVideoFile(): Promise<string | null> {
  return SAMPLE_PATH;
}

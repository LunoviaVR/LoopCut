/**
 * Export helpers that do not touch the backend: format and engine choices, the file name and the default folder, the
 * size status against a limit, what a platform preset changes, and the progress arithmetic (elapsed time and ETA).
 */
import { MB_BYTES } from "@/domain/bytes";
import { gpuDevices, gpuValueOf } from "@/features/settings/hw";
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import type { Dither } from "@/ipc/bindings/Dither";
import type { ExportFormat } from "@/ipc/bindings/ExportFormat";
import type { ExportSettings } from "@/ipc/bindings/ExportSettings";
import type { GifEngine } from "@/ipc/bindings/GifEngine";
import type { HwSettings } from "@/ipc/bindings/HwSettings";
import type { PlatformPreset } from "@/ipc/bindings/PlatformPreset";
import type { SizeEstimate } from "@/ipc/types";

export const EXPORT_FORMATS: readonly { value: ExportFormat; labelKey: string }[] = [
  { value: "gif", labelKey: "export.format.gif" },
  { value: "mp4H264", labelKey: "export.format.mp4H264" },
  { value: "mp4Hevc", labelKey: "export.format.mp4Hevc" },
  { value: "webmVp9", labelKey: "export.format.webmVp9" },
  { value: "webmAv1", labelKey: "export.format.webmAv1" },
];

/** The GPU engine has no fixed hint: its text names the device the dither runs on (see ditherDevice). */
export const GIF_ENGINES: readonly { value: GifEngine; labelKey: string; hintKey?: string }[] = [
  {
    value: "quality",
    labelKey: "export.gif.engineQuality",
    hintKey: "export.gif.engineQualityHint",
  },
  { value: "fast", labelKey: "export.gif.engineFast", hintKey: "export.gif.engineFastHint" },
  { value: "gpuDither", labelKey: "export.gif.engineGpu" },
];

export const DITHERS: readonly { value: Dither; labelKey: string }[] = [
  { value: "none", labelKey: "export.gif.ditherNone" },
  { value: "bayer", labelKey: "export.gif.ditherBayer" },
  { value: "floydSteinberg", labelKey: "export.gif.ditherFloyd" },
  { value: "sierra2_4a", labelKey: "export.gif.ditherSierra" },
  { value: "blueNoise", labelKey: "export.gif.ditherBlueNoise" },
];

/** Where the GIF dither runs: on a named GPU, on the CPU, or unknown until a hardware test has run. */
export type DitherDevice = { kind: "gpu"; name: string } | { kind: "cpu" } | { kind: "unknown" };

/**
 * The device the GIF dither runs on, as the renderer picks it from the hardware settings (choose_effects in loopcut-hw).
 * The master switch, the effects renderer and the GPU choice decide, not the FFmpeg probes. Auto takes the first
 * hardware adapter in DXGI order; a pinned GPU is used when the last test listed it. Unknown when the settings or the
 * adapter list are not loaded, or when a pinned GPU is not in the list.
 */
export function ditherDevice(
  capabilities: Capabilities | null,
  hw: HwSettings | null,
): DitherDevice {
  if (!hw) return { kind: "unknown" };
  if (!hw.enabled || hw.effects === "cpu" || hw.gpu.mode === "software") return { kind: "cpu" };
  if (!capabilities) return { kind: "unknown" };
  const adapters = gpuDevices(capabilities);
  if (hw.gpu.mode === "specific") {
    const pinned = gpuValueOf(hw.gpu.key);
    const match = adapters.find((entry) => gpuValueOf(entry.device.key) === pinned);
    return match ? { kind: "gpu", name: match.device.key.name } : { kind: "unknown" };
  }
  const first = [...adapters].sort((a, b) => a.device.dxgiIndex - b.device.dxgiIndex)[0];
  return first ? { kind: "gpu", name: first.device.key.name } : { kind: "cpu" };
}

/** Engines that take a palette size and a dither mode (the Quality engine has its own palette rules). */
export function engineUsesPalette(engine: GifEngine): boolean {
  return engine !== "quality";
}

/** The file extension for a format, without the dot. */
export function extensionOf(format: ExportFormat): string {
  if (format === "gif") return "gif";
  if (format === "mp4H264" || format === "mp4Hevc") return "mp4";
  return "webm";
}

/** Windows names that cannot appear in a file name, and control characters. */
const REFUSED_NAME_CHARACTERS = '\\/:*?"<>|';

/** Replaces each run of refused or control characters with one underscore. */
function sanitizeName(name: string): string {
  let out = "";
  let inRun = false;
  for (const character of name) {
    const refused = character.charCodeAt(0) < 32 || REFUSED_NAME_CHARACTERS.includes(character);
    if (refused) {
      if (!inRun) out += "_";
      inRun = true;
    } else {
      out += character;
      inRun = false;
    }
  }
  return out;
}

/** A default file name for an export: the project name with unsafe characters replaced, plus the format's extension. */
export function suggestedExportName(projectName: string, format: ExportFormat): string {
  const base = sanitizeName(projectName.trim()).replace(/[. ]+$/, "");
  return `${base || "export"}.${extensionOf(format)}`;
}

/**
 * A path made of the folder and a file name. The folder keeps its own separator style. Without a folder the file name
 * is returned alone and the save dialog picks the folder.
 */
export function joinPath(directory: string | null | undefined, fileName: string): string {
  if (!directory) return fileName;
  const trimmed = directory.replace(/[\\/]+$/, "");
  const separator = directory.includes("/") && !directory.includes("\\") ? "/" : "\\";
  return `${trimmed}${separator}${fileName}`;
}

export type FitStatus = "none" | "fits" | "near" | "over";

/**
 * Compares an estimate with a limit. "fits" when even the high end is under it, "near" when only the likely size is,
 * "over" when the likely size is above it. "none" without an estimate or a limit.
 */
export function fitStatus(estimate: SizeEstimate | null, limitBytes: number | null): FitStatus {
  if (!estimate || limitBytes === null) return "none";
  if (estimate.high <= limitBytes) return "fits";
  if (estimate.bytes > limitBytes) return "over";
  return "near";
}

/** The limit an export is judged against: the target size when one is set, otherwise the preset's upload limit. */
export function sizeLimit(settings: ExportSettings, preset: PlatformPreset | null): number | null {
  if (settings.targetBytes !== null) return settings.targetBytes;
  return preset?.maxBytes ?? null;
}

/** What applying a platform preset sets. The caller writes these in one undo step. */
export interface PresetPlan {
  presetId: string;
  format: ExportFormat;
  width: number;
  fps: number;
  targetBytes: number | null;
}

export function presetPlan(preset: PlatformPreset): PresetPlan {
  return {
    presetId: preset.id,
    format: preset.format,
    width: preset.recommendedWidth,
    fps: preset.recommendedFps,
    targetBytes: preset.maxBytes,
  };
}

/** Megabytes (decimal, as the rest of the app shows sizes) to bytes. */
export function megabytesToBytes(megabytes: number): number {
  return Math.round(megabytes * MB_BYTES);
}

/** Bytes to decimal megabytes. */
export function bytesToMegabytes(bytes: number): number {
  return bytes / MB_BYTES;
}

/** Time since the start as `m:ss`, or `h:mm:ss` past an hour. */
export function formatElapsed(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  const pad = (value: number) => String(value).padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(secs)}` : `${minutes}:${pad(secs)}`;
}

/**
 * Seconds left, from the rate so far. Null until a frame is done (there is no rate to extrapolate yet); zero once the
 * work is complete.
 */
export function etaSeconds(elapsedSeconds: number, done: number, total: number): number | null {
  if (total <= 0 || done <= 0) return null;
  if (done >= total) return 0;
  return (elapsedSeconds * (total - done)) / done;
}

/**
 * The folder a path names, or null when the path has no folder part. Both separator styles are understood, so a
 * path the save dialog returned can seed the next dialog's folder.
 */
export function folderOf(path: string): string | null {
  const index = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return index > 0 ? path.slice(0, index) : null;
}

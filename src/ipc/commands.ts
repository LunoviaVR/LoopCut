/**
 * Typed wrappers for every command in docs/ipc.md. Components call these and never `invoke` directly, and no
 * other module builds protocol URLs by hand (see ./protocol).
 *
 * Inside Tauri each wrapper calls the Rust command; outside Tauri (browser dev, tests) it routes to the
 * synthetic backend in ./mock, loaded lazily. Failures are normalized to CommandError, which carries the
 * AppError fields (`code`, `message`, `detail`).
 *
 * Channel arguments are passed as plain callbacks. The Tauri Channel is only constructed on the native
 * path, because constructing one needs the Tauri runtime.
 */
import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppError } from "./bindings/AppError";
import type { AppInfo } from "./bindings/AppInfo";
import type { AppSettings } from "./bindings/AppSettings";
import type { CacheUsage } from "./bindings/CacheUsage";
import type { Capabilities } from "./bindings/Capabilities";
import type { Caption } from "./bindings/Caption";
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
import { protocolUrl } from "./protocol";
import type {
  ExportEvent,
  PreviewMode,
  PreviewStatus,
  SizeEstimate,
  ThumbnailPixels,
} from "./types";

type MockBackend = typeof import("./mock");

/** Error thrown by every wrapper. Mirrors the generated `AppError`; `detail` is absent when Rust sent none. */
export class CommandError extends Error implements AppError {
  readonly code: string;
  readonly detail?: string;

  constructor(error: Pick<AppError, "code" | "message" | "detail">) {
    super(error.message);
    this.name = "CommandError";
    this.code = error.code;
    this.detail = error.detail;
  }
}

function isAppErrorLike(value: unknown): value is AppError {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === "string" && typeof candidate.message === "string";
}

function toCommandError(value: unknown): CommandError {
  if (value instanceof CommandError) return value;
  if (isAppErrorLike(value)) return new CommandError(value);
  const message = value instanceof Error ? value.message : String(value);
  return new CommandError({ code: "unknown", message });
}

/** Runs the native path inside Tauri, the mock path otherwise, and normalizes thrown errors. */
async function call<T>(
  native: () => Promise<T>,
  mock: (backend: MockBackend) => Promise<T>,
): Promise<T> {
  try {
    if (isTauri()) return await native();
    return await mock(await import("./mock"));
  } catch (error) {
    throw toCommandError(error);
  }
}

// ---- Media -----------------------------------------------------------------------------------------

export function mediaImport(path: string, confirmNetwork?: boolean): Promise<ImportedMedia> {
  return call(
    () => invoke<ImportedMedia>("media_import", { path, confirmNetwork }),
    (m) => m.mediaImport(path, confirmNetwork),
  );
}

export function mediaThumbnails(
  token: string,
  count: number,
  height: number,
): Promise<ThumbnailSet> {
  return call(
    () => invoke<ThumbnailSet>("media_thumbnails", { token, count, height }),
    (m) => m.mediaThumbnails(token, count, height),
  );
}

/**
 * One filmstrip thumbnail as raw RGBA. Inside Tauri it is read from the `loopcut` protocol (headers `x-width`,
 * `x-height`); the browser mock serves synthetic pixels. `height` must be the set's real height (ThumbnailSet).
 */
export function fetchThumbnail(
  token: string,
  height: number,
  index: number,
): Promise<ThumbnailPixels> {
  return call(
    async () => {
      const response = await fetch(protocolUrl("loopcut", `thumb/${token}/${height}/${index}`));
      if (!response.ok) {
        throw new CommandError({ code: "not_found", message: "That thumbnail is not cached." });
      }
      const width = Number(response.headers.get("x-width"));
      const pixelHeight = Number(response.headers.get("x-height"));
      const rgba = new Uint8ClampedArray(await response.arrayBuffer());
      if (!Number.isInteger(width) || !Number.isInteger(pixelHeight) || width <= 0) {
        throw new CommandError({ code: "io", message: "The thumbnail header is malformed." });
      }
      if (rgba.length !== width * pixelHeight * 4) {
        throw new CommandError({ code: "io", message: "The thumbnail is the wrong size." });
      }
      return { width, height: pixelHeight, rgba };
    },
    (m) => m.thumbnailPixels(token, height, index),
  );
}

/**
 * Resolves once the proxy job has finished or failed. A failure does not reject: it arrives on the channel as
 * `{ kind: "error" }`, so callers read the outcome from `onProgress`. Rejections are reserved for failures
 * before the job starts.
 */
export function mediaProxyStart(
  token: string,
  onProgress: (event: ProxyEvent) => void,
): Promise<void> {
  return call(
    () =>
      invoke<void>("media_proxy_start", { token, onProgress: new Channel<ProxyEvent>(onProgress) }),
    (m) => m.mediaProxyStart(token, onProgress),
  );
}

export function mediaRelease(token: string): Promise<void> {
  return call(
    () => invoke<void>("media_release", { token }),
    (m) => m.mediaRelease(token),
  );
}

// ---- Project ---------------------------------------------------------------------------------------

export function projectNew(token: string): Promise<Project> {
  return call(
    () => invoke<Project>("project_new", { token }),
    (m) => m.projectNew(token),
  );
}

export function projectOpen(path: string): Promise<OpenedProject> {
  return call(
    () => invoke<OpenedProject>("project_open", { path }),
    (m) => m.projectOpen(path),
  );
}

export function projectSave(path: string, project: Project): Promise<void> {
  return call(
    () => invoke<void>("project_save", { path, project }),
    (m) => m.projectSave(path, project),
  );
}

export function projectAutosave(project: Project): Promise<void> {
  return call(
    () => invoke<void>("project_autosave", { project }),
    (m) => m.projectAutosave(project),
  );
}

export function projectRecovery(): Promise<RecoverySnapshot | null> {
  return call(
    () => invoke<RecoverySnapshot | null>("project_recovery"),
    (m) => m.projectRecovery(),
  );
}

export function projectRecoveryClear(): Promise<void> {
  return call(
    () => invoke<void>("project_recovery_clear"),
    (m) => m.projectRecoveryClear(),
  );
}

export function recentProjects(): Promise<RecentProject[]> {
  return call(
    () => invoke<RecentProject[]>("recent_projects"),
    (m) => m.recentProjects(),
  );
}

export function sequencePlan(project: Project): Promise<SequencePlanSummary> {
  return call(
    () => invoke<SequencePlanSummary>("sequence_plan", { project }),
    (m) => m.sequencePlan(project),
  );
}

// ---- Preview ---------------------------------------------------------------------------------------

export function previewSetProject(project: Project, viewport: Viewport): Promise<PreviewInfo> {
  return call(
    () => invoke<PreviewInfo>("preview_set_project", { project, viewport }),
    (m) => m.previewSetProject(project, viewport),
  );
}

/**
 * Raw bytes: 8-byte header (u32 LE width, u32 LE height) followed by RGBA. Decode with ./frames.
 * `mode: "source"` renders the uncropped display frame at `sourceTime`, fitted into `maxWidth x maxHeight` device
 * pixels (the crop tool passes the monitor bed size).
 */
export function previewFrame(
  frame: number,
  mode: PreviewMode,
  sourceTime?: number,
  maxWidth?: number,
  maxHeight?: number,
): Promise<ArrayBuffer> {
  return call(
    () => invoke<ArrayBuffer>("preview_frame", { frame, mode, sourceTime, maxWidth, maxHeight }),
    (m) => m.previewFrame(frame, mode, sourceTime, maxWidth, maxHeight),
  );
}

/** Decoder progress, fallbacks and playback state. Cheap; the monitor polls it while decoding. */
export function previewStatus(): Promise<PreviewStatus> {
  return call(
    () => invoke<PreviewStatus>("preview_status"),
    (m) => m.previewStatus(),
  );
}

/**
 * Streams frames at the output fps from `fromFrame`. `onFrame` receives each raw frame. The promise
 * settles when playback ends (end of clip with `loop: false`) or when previewStop is called.
 */
export function previewPlay(
  fromFrame: number,
  loop: boolean,
  onFrame: (frame: ArrayBuffer) => void,
): Promise<void> {
  return call(
    () =>
      invoke<void>("preview_play", { fromFrame, loop, onFrame: new Channel<ArrayBuffer>(onFrame) }),
    (m) => m.previewPlay(fromFrame, loop, onFrame),
  );
}

export function previewStop(): Promise<void> {
  return call(
    () => invoke<void>("preview_stop"),
    (m) => m.previewStop(),
  );
}

// ---- Export ----------------------------------------------------------------------------------------

export function exportEstimate(project: Project): Promise<SizeEstimate> {
  return call(
    () => invoke<SizeEstimate>("export_estimate", { project }),
    (m) => m.exportEstimate(project),
  );
}

/** Resolves with the job id. Progress, fallbacks, completion and failure arrive through `onEvent`. */
export function exportStart(
  project: Project,
  outputPath: string,
  onEvent: (event: ExportEvent) => void,
): Promise<string> {
  return call(
    () =>
      invoke<string>("export_start", {
        project,
        outputPath,
        onEvent: new Channel<ExportEvent>(onEvent),
      }),
    (m) => m.exportStart(project, outputPath, onEvent),
  );
}

export function exportCancel(jobId: string): Promise<void> {
  return call(
    () => invoke<void>("export_cancel", { jobId }),
    (m) => m.exportCancel(jobId),
  );
}

/** The platform presets (versioned data built into the app). */
export function exportPresets(): Promise<PlatformPreset[]> {
  return call(
    () => invoke<PlatformPreset[]>("export_presets"),
    (m) => m.exportPresets(),
  );
}

// ---- Color, captions and fonts ---------------------------------------------------------------------

/** Parses a `.cube` file so the UI can show what it holds before a project refers to it. */
export function lutInspect(path: string): Promise<LutInfo> {
  return call(
    () => invoke<LutInfo>("lut_inspect", { path }),
    (m) => m.lutInspect(path),
  );
}

/** One caption per SubRip cue, snapped to the frame grid of `fps`. Captions use the default style. */
export function captionsImportSrt(path: string, fps: number): Promise<Caption[]> {
  return call(
    () => invoke<Caption[]>("captions_import_srt", { path, fps }),
    (m) => m.captionsImportSrt(path, fps),
  );
}

/** Font family names the caption rasterizer can draw, sorted. */
export function fontsList(): Promise<string[]> {
  return call(
    () => invoke<string[]>("fonts_list"),
    (m) => m.fontsList(),
  );
}

// ---- Hardware and settings -------------------------------------------------------------------------

export function hwCapabilities(): Promise<Capabilities | null> {
  return call(
    () => invoke<Capabilities | null>("hw_capabilities"),
    (m) => m.hwCapabilities(),
  );
}

export function hwProbe(
  force: boolean,
  onProgress: (event: ProbeEvent) => void,
): Promise<Capabilities> {
  return call(
    () =>
      invoke<Capabilities>("hw_probe", { force, onProgress: new Channel<ProbeEvent>(onProgress) }),
    (m) => m.hwProbe(force, onProgress),
  );
}

export function settingsGet(): Promise<AppSettings> {
  return call(
    () => invoke<AppSettings>("settings_get"),
    (m) => m.settingsGet(),
  );
}

export function settingsSet(settings: AppSettings): Promise<void> {
  return call(
    () => invoke<void>("settings_set", { settings }),
    (m) => m.settingsSet(settings),
  );
}

export function cacheUsage(): Promise<CacheUsage> {
  return call(
    () => invoke<CacheUsage>("cache_usage"),
    (m) => m.cacheUsage(),
  );
}

export function cacheClear(): Promise<void> {
  return call(
    () => invoke<void>("cache_clear"),
    (m) => m.cacheClear(),
  );
}

// ---- App -------------------------------------------------------------------------------------------

export function appInfo(): Promise<AppInfo> {
  return call(
    () => invoke<AppInfo>("app_info"),
    (m) => m.appInfo(),
  );
}

export function appDiagnostics(): Promise<string> {
  return call(
    () => invoke<string>("app_diagnostics"),
    (m) => m.appDiagnostics(),
  );
}

export function appPrepareUpdate(): Promise<void> {
  return call(
    () => invoke<void>("app_prepare_update"),
    (m) => m.appPrepareUpdate(),
  );
}

export function appOpenPathArg(): Promise<string | null> {
  return call(
    () => invoke<string | null>("app_open_path_arg"),
    (m) => m.appOpenPathArg(),
  );
}

/**
 * Re-opens the background jobs after an update was prepared but its install failed. `app_prepare_update` closes the
 * registry; the install path calls this so the app keeps working until the next attempt.
 */
export function appResumeBackground(): Promise<void> {
  return call(
    () => invoke<void>("app_resume_background"),
    (m) => m.appResumeBackground(),
  );
}

// ---- Diagnostics spikes (M0 measurements) and the real-app self-test -------------------------------
// Used only by the LOOPCUT_RUN_SPIKES=1 diagnostics run and the self-test run. The bench commands exist in debug builds.

export interface SpikeFlags {
  run: boolean;
  /** The media file the real-app self-test imports. Absent, null or empty when no self-test was requested. */
  selftest?: string | null;
  /** The folder the self-test writes its exports and report into. */
  selftestOut?: string | null;
}

/** Whether the app was started with the spike run or the self-test requested. Always false in the browser build. */
export function spikeFlags(): Promise<SpikeFlags> {
  return call(
    () => invoke<SpikeFlags>("spike_flags"),
    async () => ({ run: false, selftest: null, selftestOut: null }),
  );
}

/** Writes the spike report through Rust and resolves with the file path. */
export function spikeReport(json: string): Promise<string> {
  return call(
    () => invoke<string>("spike_report", { json }),
    (m) => m.spikeReport(json),
  );
}

/** Pushes `count` raw RGBA frames over a channel. Each `onFrame` call receives one frame. */
export function benchChannel(
  width: number,
  height: number,
  count: number,
  onFrame: (frame: ArrayBuffer) => void,
): Promise<void> {
  return call(
    () =>
      invoke<void>("bench_channel", {
        width,
        height,
        count,
        onFrame: new Channel<ArrayBuffer>(onFrame),
      }),
    (m) => m.benchChannel(width, height, count, onFrame),
  );
}

/** One raw RGBA frame as an `invoke` response (request/response latency). */
export function benchInvokeFrame(
  width: number,
  height: number,
  index: number,
): Promise<ArrayBuffer> {
  return call(
    () => invoke<ArrayBuffer>("bench_invoke_frame", { width, height, index }),
    (m) => m.benchInvokeFrame(width, height, index),
  );
}

// ---- Events and dialogs ----------------------------------------------------------------------------

/** Fires when a second instance forwards a file path. Returns the unlisten function. */
export async function onOpenPath(handler: (path: string) => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  return listen<string>("app://open-path", (event) => handler(event.payload));
}

/** Paths dropped onto the window. Browser builds never receive OS drops, so this is a no-op there. */
export async function onFileDrop(handler: (paths: string[]) => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "drop") handler(event.payload.paths);
  });
}

/** Native file picker for one video. Resolves null when the user cancels. */
export function pickVideoFile(): Promise<string | null> {
  return call(
    async () => {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [
          { name: "Video", extensions: ["mp4", "m4v", "mov", "mkv", "webm", "avi", "gif"] },
        ],
      });
      return typeof selected === "string" ? selected : null;
    },
    (m) => m.pickVideoFile(),
  );
}

/** Native file picker for a `.cube` LUT. Resolves null when cancelled. */
export function pickLutFile(): Promise<string | null> {
  return call(
    async () => {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "LUT", extensions: ["cube"] }],
      });
      return typeof selected === "string" ? selected : null;
    },
    (m) => m.pickLutFile(),
  );
}

/** Native file picker for a SubRip (`.srt`) file. Resolves null when cancelled. */
export function pickSrtFile(): Promise<string | null> {
  return call(
    async () => {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        multiple: false,
        directory: false,
        filters: [{ name: "SubRip", extensions: ["srt"] }],
      });
      return typeof selected === "string" ? selected : null;
    },
    (m) => m.pickSrtFile(),
  );
}

/**
 * Native "save as" dialog for an export. `defaultPath` may be a bare name or a full path; `extension` has no dot.
 * Resolves null when cancelled.
 */
export function pickExportPath(
  defaultPath: string,
  extension: string,
  filterName: string,
): Promise<string | null> {
  return call(
    async () => {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const selected = await save({
        defaultPath,
        filters: [{ name: filterName, extensions: [extension] }],
      });
      return selected ?? null;
    },
    (m) => m.pickExportPath(defaultPath),
  );
}

/** Opens the folder that holds `path` with the file selected. */
export function revealInFolder(path: string): Promise<void> {
  return call(
    async () => {
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(path);
    },
    (m) => m.revealInFolder(path),
  );
}

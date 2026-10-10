/**
 * Real-app self-test (dev diagnostics). Started only when the app was launched with a media file to test
 * (`spikeFlags().selftest`). It drives the real backend through the same typed commands the editor uses, one step at
 * a time, and records each step as {step, ok, ms, detail}. A failing step is recorded and the run goes on: a step that
 * needs the failed one is recorded as skipped, and independent steps still run. The whole report goes to Rust through
 * `spikeReport`, which returns the path it wrote.
 */
import { captionForPreset } from "@/features/captions/captionFactory";
import { joinPath } from "@/features/export/exportFormat";
import { errorCodeOf, messageOf } from "@/features/import/errors";
import { useMediaSession } from "@/features/import/session";
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import type { ExportEvent } from "@/ipc/bindings/ExportEvent";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import type { SequencePlanSummary } from "@/ipc/bindings/SequencePlanSummary";
import {
  appDiagnostics,
  exportCancel,
  exportEstimate,
  exportStart,
  hwCapabilities,
  mediaImport,
  previewFrame,
  previewPlay,
  previewSetProject,
  previewStatus,
  previewStop,
  projectNew,
  sequencePlan,
  spikeReport,
} from "@/ipc/commands";
import { parseFrame } from "@/ipc/frames";
import type { SizeEstimate } from "@/ipc/types";
import { useEditorStore } from "@/state/editor";

/** The viewport the self-test renders with: 960 by 540 device pixels, one per CSS pixel. */
export const SELFTEST_VIEWPORT = { width: 960, height: 540, dpr: 1 } as const;
/** How long the hardware probe may take before the run records it as failed. */
export const HW_TIMEOUT_MS = 60_000;
const HW_POLL_MS = 1_000;
/** How long one export may run before the run records it as failed. */
export const EXPORT_TIMEOUT_MS = 180_000;
/** How long the playback check runs before it stops. */
export const PLAYBACK_MS = 2_000;
/**
 * How many more times the estimate step asks after the backend reports the estimate was superseded: the editor's own
 * estimate for the same clip can replace the self-test's while it runs.
 */
export const ESTIMATE_RETRIES = 3;
/** Output of the grade and caption check: the caption sits in the first two seconds. */
const CAPTION_TEXT = "LoopCut self-test";

export interface SelftestStep {
  step: string;
  ok: boolean;
  ms: number;
  detail: string;
}

export interface SelftestReport {
  kind: "loopcut-selftest";
  mediaPath: string;
  outDir: string;
  startedAt: string;
  finishedAt: string;
  ok: boolean;
  steps: SelftestStep[];
  /** The `app_diagnostics` text, or null when that step did not pass. */
  diagnostics: string | null;
}

export interface SelftestResult {
  /** Where the report was written, as `spikeReport` returned it. */
  path: string;
  ok: boolean;
  steps: SelftestStep[];
}

export interface SelftestOptions {
  mediaPath: string;
  outDir: string;
  /** Clock in milliseconds. Defaults to `performance.now`. Tests inject a fake. */
  now?: () => number;
  /** Waits for a delay. Defaults to `setTimeout`. Tests inject an immediate one. */
  wait?: (ms: number) => Promise<void>;
  /** Wall-clock time for the report's start and finish stamps. */
  clock?: () => Date;
}

/** What one export produced, as the export events reported it. */
export interface ExportOutcome {
  bytes: number;
  frames: number;
  seconds: number;
  progressEvents: number;
  fallbacks: string[];
}

const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Summary of the probe: how many GPU devices and how many encoders answered `ok`. */
export function summarizeCapabilities(caps: Capabilities): string {
  const gpuEncoders = caps.devices
    .flatMap((device) => device.encode)
    .filter((cap) => cap.status === "ok");
  const cpuEncoders = caps.cpuEncode.filter((cap) => cap.status === "ok");
  const names = caps.devices.map((device) => device.device.key.name).join(", ") || "none";
  return `devices ${caps.devices.length} (${names}); encoders ok ${gpuEncoders.length} gpu, ${cpuEncoders.length} cpu`;
}

/** Whether the frame's pixels are not all one color. */
export function hasVariation(rgba: Uint8ClampedArray): boolean {
  const r = rgba[0];
  const g = rgba[1];
  const b = rgba[2];
  const a = rgba[3];
  for (let i = 4; i < rgba.length; i += 4) {
    if (rgba[i] !== r || rgba[i + 1] !== g || rgba[i + 2] !== b || rgba[i + 3] !== a) return true;
  }
  return false;
}

/** How many bytes differ between two equally sized RGBA buffers. */
export function changedBytes(a: Uint8ClampedArray, b: Uint8ClampedArray): number {
  const length = Math.min(a.length, b.length);
  let changed = 0;
  for (let i = 0; i < length; i++) {
    if (a[i] !== b[i]) changed++;
  }
  return changed + Math.abs(a.length - b.length);
}

/** Polls the hardware record until the startup probe has finished, or fails after `HW_TIMEOUT_MS`. */
export async function waitForCapabilities(
  now: () => number,
  wait: (ms: number) => Promise<void>,
): Promise<Capabilities> {
  const started = now();
  for (;;) {
    const caps = await hwCapabilities();
    if (caps) return caps;
    if (now() - started >= HW_TIMEOUT_MS) {
      throw new Error(`hardware probe did not finish within ${HW_TIMEOUT_MS / 1000} s`);
    }
    await wait(HW_POLL_MS);
  }
}

/**
 * The error a timed-out export reports. The job is cancelled first, so the next steps do not run beside it, and the
 * message says whether the cancel went through.
 */
async function timedOutExport(jobId: string | undefined, timeoutMs: number): Promise<Error> {
  const message = `export timed out after ${timeoutMs / 1000} s`;
  if (jobId === undefined) return new Error(`${message}; no job id to cancel`);
  try {
    await exportCancel(jobId);
    return new Error(`${message}; cancelled ${jobId}`);
  } catch (error) {
    return new Error(`${message}; cancel of ${jobId} failed: ${messageOf(error)}`);
  }
}

/**
 * Starts one export and resolves with its outcome when the terminal event arrives. An `error` event or a start that
 * fails rejects. No terminal event within `timeoutMs` cancels the job and rejects with the cancel's result. A start
 * that answers only after the timeout still has its job cancelled, since the timeout reported no id to cancel.
 */
export function runExport(
  project: Project,
  outputPath: string,
  timeoutMs: number = EXPORT_TIMEOUT_MS,
): Promise<ExportOutcome> {
  return new Promise<ExportOutcome>((resolve, reject) => {
    let settled = false;
    let timedOut = false;
    let progressEvents = 0;
    const fallbacks: string[] = [];
    let jobId: string | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finish = (action: () => void) => {
      if (settled) return;
      settled = true;
      if (timer !== undefined) clearTimeout(timer);
      action();
    };
    timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      timedOut = true;
      void timedOutExport(jobId, timeoutMs).then((error) => reject(error));
    }, timeoutMs);
    const onEvent = (event: ExportEvent) => {
      switch (event.kind) {
        case "progress":
          progressEvents++;
          return;
        case "fallback":
          fallbacks.push(`${event.stage} ${event.from} to ${event.to}: ${event.reason}`);
          return;
        case "targetPass":
          return;
        case "done":
          finish(() =>
            resolve({
              bytes: event.bytes,
              frames: event.frames,
              seconds: event.seconds,
              progressEvents,
              fallbacks,
            }),
          );
          return;
        case "error":
          finish(() => reject(new Error(`${event.error.code}: ${event.error.message}`)));
          return;
      }
    };
    exportStart(project, outputPath, onEvent).then(
      (id) => {
        jobId = id;
        // The timeout has already reported, so a late job is cancelled here; the report cannot carry the outcome.
        if (timedOut) void exportCancel(id).catch(() => undefined);
      },
      (error: unknown) => {
        finish(() => reject(error));
      },
    );
  });
}

/**
 * Asks for a size estimate again while the backend answers `cancelled`, which is how it reports an estimate that a
 * newer one for the same clip superseded. Any other failure is thrown at once, and so is the last one after the retries.
 */
export async function estimateWithRetries(
  ask: () => Promise<SizeEstimate>,
  retries: number = ESTIMATE_RETRIES,
): Promise<SizeEstimate> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await ask();
    } catch (error) {
      if (attempt >= retries || errorCodeOf(error) !== "cancelled") throw error;
    }
  }
}

/** Describes one export outcome in the step detail. */
function describeExport(outcome: ExportOutcome, outputPath: string): string {
  const fallbacks = outcome.fallbacks.length > 0 ? outcome.fallbacks.join("; ") : "none";
  return `${outputPath}: ${outcome.frames} frames, ${outcome.bytes} bytes in ${outcome.seconds.toFixed(2)} s, progress events ${outcome.progressEvents}, fallbacks ${fallbacks}`;
}

/**
 * Runs every self-test step in order and writes the report. Resolves with the report path even when steps failed;
 * the `ok` flag and each step's record say which did not pass.
 */
export async function runSelftest(options: SelftestOptions): Promise<SelftestResult> {
  const now = options.now ?? (() => performance.now());
  const wait = options.wait ?? delay;
  const clock = options.clock ?? (() => new Date());
  const startedAt = clock().toISOString();
  const steps: SelftestStep[] = [];
  const passed = new Map<string, boolean>();
  const values: {
    media?: ImportedMedia;
    info?: PreviewInfo;
    diagnostics: string | null;
  } = { diagnostics: null };

  const record = (step: string, ok: boolean, ms: number, detail: string) => {
    steps.push({ step, ok, ms: Math.round(ms), detail });
    passed.set(step, ok);
  };

  /** Runs one step unless a step it needs did not pass. A throw is recorded, never rethrown. */
  const run = async (step: string, needs: readonly string[], body: () => Promise<string>) => {
    const missing = needs.find((need) => passed.get(need) !== true);
    if (missing) {
      record(step, false, 0, `skipped: ${missing} did not pass`);
      return;
    }
    const started = now();
    try {
      const detail = await body();
      record(step, true, now() - started, detail);
    } catch (error) {
      record(step, false, now() - started, messageOf(error));
    }
  };

  const outputPath = (name: string) => joinPath(options.outDir, name);
  const project = () => {
    const current = useEditorStore.getState().project;
    if (!current) throw new Error("no project is open");
    return current;
  };
  const info = () => {
    if (!values.info) throw new Error("no preview info");
    return values.info;
  };
  /** Renders output frame 0 of the project as it stands. */
  const renderFrameZero = async (): Promise<Uint8ClampedArray> => {
    await previewSetProject(project(), SELFTEST_VIEWPORT);
    return parseFrame(await previewFrame(0, "output")).rgba;
  };

  await run("mediaImport", [], async () => {
    const media = await mediaImport(options.mediaPath);
    values.media = media;
    return `${media.info.video.width}x${media.info.video.height}, ${media.info.duration.toFixed(2)} s, token ${media.token}`;
  });

  await run("projectNew", ["mediaImport"], async () => {
    const media = values.media;
    if (!media) throw new Error("no media");
    const created = await projectNew(media.token);
    useEditorStore.getState().loadProject(created);
    useEditorStore.getState().registerMedia(media);
    useMediaSession.setState({ media });
    return `project ${created.name}, ${created.clips.length} clip`;
  });

  await run("hwCapabilities", [], async () =>
    summarizeCapabilities(await waitForCapabilities(now, wait)),
  );

  await run("previewSetProject", ["projectNew"], async () => {
    const preview = await previewSetProject(project(), SELFTEST_VIEWPORT);
    values.info = preview;
    return `output ${preview.outputWidth}x${preview.outputHeight}, render ${preview.renderWidth}x${preview.renderHeight}, ${preview.frames} frames`;
  });

  await run("previewFrameOutputStart", ["previewSetProject"], async () => {
    const preview = info();
    const frame = parseFrame(await previewFrame(0, "output"));
    if (frame.width !== preview.renderWidth || frame.height !== preview.renderHeight) {
      throw new Error(
        `frame ${frame.width}x${frame.height} differs from render ${preview.renderWidth}x${preview.renderHeight}`,
      );
    }
    if (!hasVariation(frame.rgba)) throw new Error("frame is one color");
    return `frame 0 ${frame.width}x${frame.height}, not one color`;
  });

  await run("previewFrameOutputMid", ["previewSetProject"], async () => {
    const preview = info();
    const middle = Math.floor(preview.frames / 2);
    const frame = parseFrame(await previewFrame(middle, "output"));
    return `frame ${middle} ${frame.width}x${frame.height}`;
  });

  await run("previewFrameSource", ["previewSetProject"], async () => {
    const frame = parseFrame(
      await previewFrame(0, "source", 0, SELFTEST_VIEWPORT.width, SELFTEST_VIEWPORT.height),
    );
    if (frame.width > SELFTEST_VIEWPORT.width || frame.height > SELFTEST_VIEWPORT.height) {
      throw new Error(`source ${frame.width}x${frame.height} exceeds the bounds`);
    }
    if (!hasVariation(frame.rgba)) throw new Error("source frame is one color");
    return `source ${frame.width}x${frame.height}`;
  });

  await run("previewPlay", ["previewSetProject"], async () => {
    let received = 0;
    const failures: unknown[] = [];
    // The failure is kept as it happens, so the rejection is handled while the check waits and is reported below.
    const playing = previewPlay(0, true, () => {
      received++;
    }).catch((error: unknown) => {
      failures.push(error);
    });
    await wait(PLAYBACK_MS);
    await previewStop();
    await playing;
    if (failures.length > 0) throw failures[0];
    if (received === 0) throw new Error("no playback frames arrived");
    return `${received} frames in ${PLAYBACK_MS / 1000} s`;
  });

  await run("previewStatus", [], async () => {
    const status = await previewStatus();
    const fallbacks = status.fallbacks.length;
    return `decode ${Math.round(status.decodeProgress * 100)}%, failed ranges ${status.failedRanges}, fallbacks ${fallbacks}, last error ${status.lastError ?? "none"}`;
  });

  await run("sequencePlan", ["projectNew"], async () => {
    const summary: SequencePlanSummary = await sequencePlan(project());
    return `${summary.frames} frames, ${summary.durationSeconds.toFixed(2)} s, effective ${summary.effectiveFps} fps`;
  });

  await run("gradeAndCaption", ["projectNew"], async () => {
    const editor = useEditorStore.getState();
    editor.setGrade({ exposure: 0.5, saturation: 0.3 });
    const id = editor.addCaption(captionForPreset("top", { start: 0, end: 2 }, CAPTION_TEXT));
    if (!id) throw new Error("the caption was not added");
    return `grade exposure 0.5 saturation 0.3, caption ${id}`;
  });

  await run("previewGraded", ["gradeAndCaption", "previewSetProject"], async () => {
    const editor = useEditorStore.getState();
    const gradeOn = project().grade.enabled;
    // The baseline keeps the caption and bypasses the grade, so a grade the backend ignores leaves the frames equal.
    editor.setGrade({ enabled: false });
    const captionOnly = await renderFrameZero().finally(() =>
      editor.setGrade({ enabled: gradeOn }),
    );
    const graded = await renderFrameZero();
    const changed = changedBytes(captionOnly, graded);
    if (changed === 0) throw new Error("the graded frame matches the caption-only frame");
    return `${changed} bytes changed against the caption-only frame 0`;
  });

  await run("exportEstimate", ["projectNew"], async () => {
    let attempts = 0;
    try {
      const estimate: SizeEstimate = await estimateWithRetries(() => {
        attempts++;
        return exportEstimate(project());
      });
      return `${estimate.bytes} bytes (low ${estimate.low}, high ${estimate.high}), ${estimate.frames} frames, ${estimate.basis}, attempts ${attempts}`;
    } catch (error) {
      throw new Error(`${messageOf(error)} (attempts ${attempts})`);
    }
  });

  const gifPath = outputPath("selftest.gif");
  await run("exportGif", ["projectNew"], async () =>
    describeExport(await runExport(project(), gifPath), gifPath),
  );

  const mp4Path = outputPath("selftest.mp4");
  await run("exportMp4", ["projectNew"], async () => {
    useEditorStore.getState().setExport({ format: "mp4H264" });
    return describeExport(await runExport(project(), mp4Path), mp4Path);
  });

  const fastPath = outputPath("selftest-fast.gif");
  await run("exportGifFast", ["projectNew"], async () => {
    useEditorStore.getState().setExport({ format: "gif", gifEngine: "fast" });
    return describeExport(await runExport(project(), fastPath), fastPath);
  });

  const gpuPath = outputPath("selftest-gpu-dither.gif");
  await run("exportGifGpuDither", ["projectNew"], async () => {
    useEditorStore.getState().setExport({ gifEngine: "gpuDither" });
    return describeExport(await runExport(project(), gpuPath), gpuPath);
  });

  const targetPath = outputPath("selftest-1mb.gif");
  await run("exportGifTarget1Mb", ["projectNew"], async () => {
    useEditorStore.getState().setExport({ gifEngine: "quality", targetBytes: 1_000_000 });
    return describeExport(await runExport(project(), targetPath), targetPath);
  });

  await run("appDiagnostics", [], async () => {
    const text = await appDiagnostics();
    values.diagnostics = text;
    return `${text.length} characters`;
  });

  const report: SelftestReport = {
    kind: "loopcut-selftest",
    mediaPath: options.mediaPath,
    outDir: options.outDir,
    startedAt,
    finishedAt: clock().toISOString(),
    ok: steps.every((step) => step.ok),
    steps,
    diagnostics: values.diagnostics,
  };
  const path = await spikeReport(JSON.stringify(report, null, 2));
  return { path, ok: report.ok, steps };
}

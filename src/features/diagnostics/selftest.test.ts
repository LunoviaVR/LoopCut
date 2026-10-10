import "@/ui/test/env";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sampleCapabilities, sampleMedia } from "@/features/testFixtures";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import {
  appDiagnostics,
  CommandError,
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
import { encodeFrame } from "@/ipc/frames";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import {
  changedBytes,
  estimateWithRetries,
  hasVariation,
  runExport,
  runSelftest,
  type SelftestStep,
} from "./selftest";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    mediaImport: vi.fn(),
    projectNew: vi.fn(),
    hwCapabilities: vi.fn(),
    previewSetProject: vi.fn(),
    previewFrame: vi.fn(),
    previewPlay: vi.fn(),
    previewStop: vi.fn(),
    previewStatus: vi.fn(),
    sequencePlan: vi.fn(),
    exportEstimate: vi.fn(),
    exportStart: vi.fn(),
    exportCancel: vi.fn(),
    appDiagnostics: vi.fn(),
    spikeReport: vi.fn(),
  };
});

const WIDTH = 4;
const HEIGHT = 2;

/** A frame whose pixels all share one value per channel, or vary across the frame when `varied` is set. */
function pixels(varied: boolean, shade = 0): Uint8ClampedArray {
  const rgba = new Uint8ClampedArray(WIDTH * HEIGHT * 4);
  for (let i = 0; i < WIDTH * HEIGHT; i++) {
    const value = varied ? (i * 17 + shade) % 256 : shade;
    rgba.set([value, value, value, 255], i * 4);
  }
  return rgba;
}

const INFO: PreviewInfo = {
  outputWidth: WIDTH,
  outputHeight: HEIGHT,
  renderWidth: WIDTH,
  renderHeight: HEIGHT,
  frames: 6,
  fps: { num: 20, den: 1 },
  quality: "full",
  delaysCs: [5, 5, 5, 5, 5, 5],
};

const OUT_DIR = "C:\\Clips\\out";

/**
 * Makes every command succeed. `graded` decides whether the grade changes the frame: when false, the frame ignores the
 * grade and only the caption shows. A caption always marks the frame, as the backend draws it.
 */
function succeedAll({ graded = true }: { graded?: boolean } = {}) {
  vi.mocked(mediaImport).mockResolvedValue(sampleMedia());
  vi.mocked(projectNew).mockResolvedValue(sampleProject());
  vi.mocked(hwCapabilities).mockResolvedValue(sampleCapabilities());
  vi.mocked(previewSetProject).mockResolvedValue(INFO);
  vi.mocked(previewFrame).mockImplementation(async () => {
    const project = useEditorStore.getState().project;
    const gradeShade = graded && project?.grade.enabled && project.grade.exposure !== 0 ? 90 : 0;
    const captionShade = project && project.captions.length > 0 ? 40 : 0;
    return encodeFrame(WIDTH, HEIGHT, pixels(true, gradeShade + captionShade));
  });
  vi.mocked(previewPlay).mockImplementation(async (_from, _loop, onFrame) => {
    onFrame(encodeFrame(WIDTH, HEIGHT, pixels(true)));
  });
  vi.mocked(previewStop).mockResolvedValue(undefined);
  vi.mocked(previewStatus).mockResolvedValue({
    decodeProgress: 1,
    failedRanges: 0,
    fallbacks: [],
    playing: false,
    lastError: null,
  });
  vi.mocked(sequencePlan).mockResolvedValue({
    frames: [],
    durationSeconds: 0.3,
    effectiveFps: 20,
    delaysCs: [],
  } as unknown as Awaited<ReturnType<typeof sequencePlan>>);
  vi.mocked(exportEstimate).mockResolvedValue({
    bytes: 3_000_000,
    low: 2_900_000,
    high: 3_100_000,
    frames: 6,
    basis: "sample",
  });
  vi.mocked(exportStart).mockImplementation(async (_project, _path, onEvent) => {
    onEvent({ kind: "progress", framesDone: 1, framesTotal: 6, bytes: 100 });
    onEvent({ kind: "done", path: _path, bytes: 2_000_000, seconds: 1.5, frames: 6 });
    return "export:1";
  });
  vi.mocked(appDiagnostics).mockResolvedValue("LoopCut diagnostics");
  vi.mocked(spikeReport).mockResolvedValue("C:\\Clips\\out\\selftest-report.json");
}

function stepsOf(result: { steps: SelftestStep[] }): Map<string, SelftestStep> {
  return new Map(result.steps.map((step) => [step.step, step]));
}

const run = (overrides: Partial<Parameters<typeof runSelftest>[0]> = {}) =>
  runSelftest({
    mediaPath: "C:\\Clips\\ranked-ace.mp4",
    outDir: OUT_DIR,
    wait: async () => {},
    now: () => 0,
    ...overrides,
  });

beforeEach(() => {
  vi.resetAllMocks();
  resetEditorState();
});

afterEach(() => {
  resetEditorState();
});

describe("self-test runner", () => {
  it("runs the steps in order and writes the whole report through spikeReport", async () => {
    succeedAll();

    const result = await run();

    expect(result.steps.map((step) => step.step)).toEqual([
      "mediaImport",
      "projectNew",
      "hwCapabilities",
      "previewSetProject",
      "previewFrameOutputStart",
      "previewFrameOutputMid",
      "previewFrameSource",
      "previewPlay",
      "previewStatus",
      "sequencePlan",
      "gradeAndCaption",
      "previewGraded",
      "exportEstimate",
      "exportGif",
      "exportMp4",
      "exportGifFast",
      "exportGifGpuDither",
      "exportGifTarget1Mb",
      "appDiagnostics",
    ]);
    expect(result.ok).toBe(true);
    expect(result.path).toBe("C:\\Clips\\out\\selftest-report.json");
    const json = JSON.parse(vi.mocked(spikeReport).mock.calls[0]?.[0] ?? "{}") as {
      ok: boolean;
      steps: SelftestStep[];
      diagnostics: string | null;
    };
    expect(json.ok).toBe(true);
    expect(json.steps).toHaveLength(result.steps.length);
    expect(json.diagnostics).toBe("LoopCut diagnostics");
  });

  it("records a failing step, skips the steps that need it, and still runs the independent ones", async () => {
    succeedAll();
    vi.mocked(previewSetProject).mockRejectedValue({
      code: "preview_render",
      message: "render failed",
    });

    const result = await run();
    const steps = stepsOf(result);

    expect(result.ok).toBe(false);
    expect(steps.get("previewSetProject")).toMatchObject({ ok: false, detail: "render failed" });
    expect(steps.get("previewFrameOutputMid")).toMatchObject({
      ok: false,
      detail: "skipped: previewSetProject did not pass",
    });
    expect(steps.get("previewPlay")?.ok).toBe(false);
    expect(steps.get("exportEstimate")?.ok).toBe(true);
    expect(steps.get("exportGif")?.ok).toBe(true);
    expect(steps.get("hwCapabilities")?.ok).toBe(true);
    expect(steps.get("appDiagnostics")?.ok).toBe(true);
    expect(spikeReport).toHaveBeenCalledTimes(1);
  });

  it("stops the media steps at a failed import but still reports and runs the probe", async () => {
    succeedAll();
    vi.mocked(mediaImport).mockRejectedValue({ code: "unsupported_media", message: "not a video" });

    const steps = stepsOf(await run());

    expect(steps.get("mediaImport")).toMatchObject({ ok: false, detail: "not a video" });
    expect(steps.get("projectNew")?.ok).toBe(false);
    expect(steps.get("hwCapabilities")?.ok).toBe(true);
    expect(steps.get("appDiagnostics")?.ok).toBe(true);
  });

  it("polls the hardware probe until it answers", async () => {
    succeedAll();
    vi.mocked(hwCapabilities)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(null)
      .mockResolvedValue(sampleCapabilities());

    const steps = stepsOf(await run());

    expect(hwCapabilities).toHaveBeenCalledTimes(3);
    expect(steps.get("hwCapabilities")?.ok).toBe(true);
    expect(steps.get("hwCapabilities")?.detail).toMatch(/^devices \d+/);
  });

  it("fails the hardware step once the probe has run for 60 s", async () => {
    succeedAll();
    vi.mocked(hwCapabilities).mockResolvedValue(null);
    let clock = 0;

    const steps = stepsOf(await run({ now: () => (clock += 30_000) }));

    expect(steps.get("hwCapabilities")).toMatchObject({ ok: false });
    expect(steps.get("hwCapabilities")?.detail).toContain("hardware probe");
  });

  it("fails the grade check when the grade is ignored and only the caption shows", async () => {
    succeedAll({ graded: false });

    const steps = stepsOf(await run());

    expect(steps.get("previewGraded")).toMatchObject({ ok: false });
    expect(steps.get("previewGraded")?.detail).toContain("matches");
  });

  it("passes the grade check when the graded frame changes, and leaves the grade on", async () => {
    succeedAll({ graded: true });

    const steps = stepsOf(await run());

    expect(steps.get("previewGraded")?.ok).toBe(true);
    expect(useEditorStore.getState().project?.grade.exposure).toBe(0.5);
    expect(useEditorStore.getState().project?.grade.enabled).toBe(true);
    expect(useEditorStore.getState().project?.captions).toHaveLength(1);
  });

  it("turns the grade back on when the caption-only render fails", async () => {
    succeedAll();
    vi.mocked(previewSetProject).mockImplementation(async (project) => {
      if (!project.grade.enabled)
        throw new CommandError({ code: "preview_render", message: "render failed" });
      return INFO;
    });

    const steps = stepsOf(await run());

    expect(steps.get("previewGraded")).toMatchObject({ ok: false, detail: "render failed" });
    expect(useEditorStore.getState().project?.grade.enabled).toBe(true);
  });

  it("records a playback failure that arrives during the playback check", async () => {
    succeedAll();
    vi.mocked(previewPlay).mockRejectedValue(
      new CommandError({ code: "preview_render", message: "playback broke" }),
    );

    const steps = stepsOf(await run());

    expect(steps.get("previewPlay")).toMatchObject({ ok: false, detail: "playback broke" });
    expect(steps.get("previewStatus")?.ok).toBe(true);
  });

  it("writes the GIF into the output folder and records progress events", async () => {
    succeedAll();

    const steps = stepsOf(await run());

    expect(exportStart).toHaveBeenCalledWith(
      expect.anything(),
      "C:\\Clips\\out\\selftest.gif",
      expect.any(Function),
    );
    expect(steps.get("exportGif")?.detail).toContain("progress events 1");
    expect(steps.get("exportGif")?.detail).toContain("2000000 bytes");
  });

  it("records an export error and still runs the remaining exports", async () => {
    succeedAll();
    vi.mocked(exportStart).mockImplementationOnce(async (_project, _path, onEvent) => {
      onEvent({ kind: "error", error: { code: "ffmpeg_failed", message: "encoder stopped" } });
      return "export:1";
    });

    const steps = stepsOf(await run());

    expect(steps.get("exportGif")).toMatchObject({
      ok: false,
      detail: "ffmpeg_failed: encoder stopped",
    });
    expect(steps.get("exportMp4")?.ok).toBe(true);
    expect(steps.get("exportGifTarget1Mb")?.ok).toBe(true);
  });

  it("switches the export format and engine for each later export", async () => {
    succeedAll();

    await run();

    const outputs = vi.mocked(exportStart).mock.calls.map((call) => call[1]);
    expect(outputs).toEqual([
      "C:\\Clips\\out\\selftest.gif",
      "C:\\Clips\\out\\selftest.mp4",
      "C:\\Clips\\out\\selftest-fast.gif",
      "C:\\Clips\\out\\selftest-gpu-dither.gif",
      "C:\\Clips\\out\\selftest-1mb.gif",
    ]);
    expect(useEditorStore.getState().project?.export.targetBytes).toBe(1_000_000);
  });
});

describe("export estimate retries", () => {
  it("asks again when a newer estimate for the clip superseded it, and records the attempts", async () => {
    succeedAll();
    vi.mocked(exportEstimate).mockRejectedValueOnce(
      new CommandError({
        code: "cancelled",
        message: "A newer estimate for this clip replaced this one.",
      }),
    );

    const steps = stepsOf(await run());

    expect(exportEstimate).toHaveBeenCalledTimes(2);
    expect(steps.get("exportEstimate")?.ok).toBe(true);
    expect(steps.get("exportEstimate")?.detail).toContain("attempts 2");
  });

  it("gives up after three retries and records every attempt in the failure", async () => {
    succeedAll();
    vi.mocked(exportEstimate).mockRejectedValue(
      new CommandError({
        code: "cancelled",
        message: "A newer estimate for this clip replaced this one.",
      }),
    );

    const steps = stepsOf(await run());

    expect(exportEstimate).toHaveBeenCalledTimes(4);
    expect(steps.get("exportEstimate")).toMatchObject({
      ok: false,
      detail: "A newer estimate for this clip replaced this one. (attempts 4)",
    });
  });

  it("does not ask again for an estimate that failed for another reason", async () => {
    succeedAll();
    vi.mocked(exportEstimate).mockRejectedValueOnce(
      new CommandError({ code: "ffmpeg_failed", message: "encoder stopped" }),
    );

    const steps = stepsOf(await run());

    expect(exportEstimate).toHaveBeenCalledTimes(1);
    expect(steps.get("exportEstimate")).toMatchObject({
      ok: false,
      detail: "encoder stopped (attempts 1)",
    });
  });

  it("returns the first estimate that is not superseded", async () => {
    const estimate = { bytes: 1, low: 1, high: 1, frames: 1, basis: "sample" as const };
    const ask = vi
      .fn<() => Promise<typeof estimate>>()
      .mockRejectedValueOnce(new CommandError({ code: "cancelled", message: "replaced" }))
      .mockResolvedValue(estimate);

    await expect(estimateWithRetries(ask)).resolves.toEqual(estimate);
    expect(ask).toHaveBeenCalledTimes(2);
  });
});

describe("export timeout", () => {
  it("cancels a timed-out export and says in the failure that the cancel went through", async () => {
    vi.mocked(exportStart).mockResolvedValue("export:7");
    vi.mocked(exportCancel).mockResolvedValue(undefined);

    await expect(runExport(sampleProject(), "C:\\Clips\\out\\slow.gif", 1)).rejects.toThrow(
      "export timed out after 0.001 s; cancelled export:7",
    );
    expect(exportCancel).toHaveBeenCalledWith("export:7");
  });

  it("says in the failure when the cancel itself failed", async () => {
    vi.mocked(exportStart).mockResolvedValue("export:8");
    vi.mocked(exportCancel).mockRejectedValue(
      new CommandError({ code: "invalid_argument", message: "unknown job" }),
    );

    await expect(runExport(sampleProject(), "C:\\Clips\\out\\slow.gif", 1)).rejects.toThrow(
      "cancel of export:8 failed: unknown job",
    );
  });

  it("cancels a job whose start answers only after the timeout", async () => {
    let answerStart: (id: string) => void = () => undefined;
    vi.mocked(exportStart).mockReturnValue(
      new Promise<string>((resolve) => {
        answerStart = resolve;
      }),
    );
    vi.mocked(exportCancel).mockResolvedValue(undefined);

    await expect(runExport(sampleProject(), "C:\\Clips\\out\\slow.gif", 1)).rejects.toThrow(
      "export timed out after 0.001 s; no job id to cancel",
    );
    answerStart("export:9");
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(exportCancel).toHaveBeenCalledWith("export:9");
  });
});

describe("self-test helpers", () => {
  it("tells a varied frame from a flat one", () => {
    expect(hasVariation(pixels(true))).toBe(true);
    expect(hasVariation(pixels(false, 40))).toBe(false);
  });

  it("counts the bytes that differ between two frames", () => {
    const a = pixels(false, 0);
    const b = pixels(false, 0);
    b[5] = 9;
    expect(changedBytes(a, a)).toBe(0);
    expect(changedBytes(a, b)).toBe(1);
  });
});

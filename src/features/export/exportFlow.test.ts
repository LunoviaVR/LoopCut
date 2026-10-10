import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSettingsStore } from "@/features/settings/settingsStore";
import { sampleSettings } from "@/features/testFixtures";
import type { ExportEvent } from "@/ipc/bindings/ExportEvent";
import { exportCancel, exportStart, pickExportPath, revealInFolder } from "@/ipc/commands";
import { canUndo, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { toast } from "@/ui";
import { cancelExport, failureText, handleExportEvent, startExport } from "./exportFlow";
import { resetExportJob, useExportStore } from "./exportStore";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    pickExportPath: vi.fn(async () => "C:\\Out\\clip.gif"),
    exportStart: vi.fn(async () => "export:1"),
    exportCancel: vi.fn(async () => {}),
    revealInFolder: vi.fn(async () => {}),
  };
});

vi.mock("@/ui", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ui")>();
  return { ...actual, toast: vi.fn(() => "toast-1") };
});

beforeEach(() => {
  resetEditorState();
  resetExportJob();
  useEditorStore.getState().loadProject(sampleProject());
  vi.clearAllMocks();
});

/** The event callback the flow passed to `export_start`. */
function eventSink(): (event: ExportEvent) => void {
  const call = vi.mocked(exportStart).mock.calls.at(-1);
  const sink = call?.[2];
  if (!sink) throw new Error("export_start was not called");
  return sink;
}

/** The toast whose title matches, from the calls made so far. */
function toastTitled(title: string) {
  return vi.mocked(toast).mock.calls.find((call) => call[0].title === title)?.[0];
}

describe("starting an export", () => {
  it("asks for the file, starts the job and raises the editor's export flag", async () => {
    await startExport();

    expect(pickExportPath).toHaveBeenCalledWith("Sample loop.gif", "gif", "GIF");
    expect(exportStart).toHaveBeenCalledWith(
      expect.objectContaining({ name: "Sample loop" }),
      "C:\\Out\\clip.gif",
      expect.any(Function),
    );
    expect(useEditorStore.getState().isExporting).toBe(true);
    expect(useExportStore.getState()).toMatchObject({
      status: "running",
      jobId: "export:1",
      path: "C:\\Out\\clip.gif",
    });
  });

  it("does nothing when the save dialog is cancelled", async () => {
    vi.mocked(pickExportPath).mockResolvedValueOnce(null);

    await startExport();

    expect(exportStart).not.toHaveBeenCalled();
    expect(useEditorStore.getState().isExporting).toBe(false);
    expect(useExportStore.getState().status).toBe("idle");
  });

  it("does not start a second job while one runs", async () => {
    await startExport();
    await startExport();

    expect(exportStart).toHaveBeenCalledTimes(1);
  });

  it("clears the flag and reports the failure when the job cannot start", async () => {
    vi.mocked(exportStart).mockRejectedValueOnce(new Error("busy"));

    await startExport();

    expect(useEditorStore.getState().isExporting).toBe(false);
    expect(useExportStore.getState()).toMatchObject({ status: "failed", error: "busy" });
    expect(toastTitled("Export failed")).toMatchObject({ variant: "error", description: "busy" });
  });

  it("does nothing without a project", async () => {
    resetEditorState();
    await startExport();
    expect(pickExportPath).not.toHaveBeenCalled();
  });
});

describe("the save dialog", () => {
  it("remembers the folder of the chosen file as the default for the next export", async () => {
    useSettingsStore.setState({ settings: sampleSettings() });
    vi.mocked(pickExportPath).mockResolvedValueOnce("D:\\Clips\\Out\\loop.gif");

    await startExport();

    await vi.waitFor(() =>
      expect(useSettingsStore.getState().settings?.general.defaultExportDir).toBe("D:\\Clips\\Out"),
    );
  });

  it("opens no second dialog while one is open, and starts no second job", async () => {
    let answer: ((value: string | null) => void) | undefined;
    vi.mocked(pickExportPath).mockImplementationOnce(
      () =>
        new Promise<string | null>((resolve) => {
          answer = resolve;
        }),
    );

    const first = startExport();
    await startExport();
    expect(pickExportPath).toHaveBeenCalledTimes(1);

    answer?.("C:\\Out\\clip.gif");
    await first;
    expect(exportStart).toHaveBeenCalledTimes(1);
  });
});

describe("following the job", () => {
  it("shows progress as the events arrive", async () => {
    await startExport();

    eventSink()({ kind: "progress", framesDone: 4, framesTotal: 8, bytes: 1_000_000 });

    expect(useExportStore.getState()).toMatchObject({
      framesDone: 4,
      framesTotal: 8,
      bytes: 1_000_000,
    });
  });

  it("records target passes", async () => {
    await startExport();

    eventSink()({ kind: "targetPass", pass: 2, settings: "quality 80", bytes: 9_500_000 });

    expect(useExportStore.getState().passes).toEqual([
      { pass: 2, settings: "quality 80", bytes: 9_500_000 },
    ]);
  });

  it("raises one warning per hardware fallback, worded by stage", async () => {
    await startExport();

    eventSink()({
      kind: "fallback",
      stage: "encode",
      from: "nvenc",
      to: "cpu",
      reason: "device lost",
    });
    eventSink()({
      kind: "fallback",
      stage: "decode",
      from: "d3d11va",
      to: "cpu",
      reason: "unsupported",
    });

    expect(useExportStore.getState().fallbacks).toBe(2);
    expect(toastTitled("Hardware encoding failed, finished on the CPU")).toMatchObject({
      variant: "warn",
    });
    expect(toastTitled("Hardware decoding failed, continuing on the CPU")).toBeDefined();
  });

  it("finishes with a result, lowers the flag and offers to show the file", async () => {
    await startExport();

    eventSink()({
      kind: "done",
      path: "C:\\Out\\clip.gif",
      bytes: 3_500_000,
      seconds: 1.6,
      frames: 8,
    });

    expect(useEditorStore.getState().isExporting).toBe(false);
    expect(useExportStore.getState()).toMatchObject({
      status: "done",
      result: { path: "C:\\Out\\clip.gif", bytes: 3_500_000, seconds: 1.6, frames: 8 },
    });
    const done = toastTitled("Exported 3.5 MB");
    expect(done).toMatchObject({ variant: "success", action: { label: "Show in folder" } });

    done?.action?.onAction();
    expect(revealInFolder).toHaveBeenCalledWith("C:\\Out\\clip.gif");
  });

  it("reports a failure with its message and lowers the flag", async () => {
    await startExport();

    eventSink()({ kind: "error", error: { code: "ffmpeg_failed", message: "encoder exited" } });

    expect(useEditorStore.getState().isExporting).toBe(false);
    expect(useExportStore.getState()).toMatchObject({ status: "failed", error: "encoder exited" });
    expect(toastTitled("Export failed")).toMatchObject({
      variant: "error",
      description: "encoder exited",
    });
  });

  it("treats the cancelled error as a cancel, not a failure", async () => {
    await startExport();

    await cancelExport();
    expect(exportCancel).toHaveBeenCalledWith("export:1");
    expect(useExportStore.getState().status).toBe("cancelling");

    eventSink()({ kind: "error", error: { code: "cancelled", message: "cancelled" } });

    expect(useExportStore.getState().status).toBe("cancelled");
    expect(useEditorStore.getState().isExporting).toBe(false);
    expect(toastTitled("Export cancelled")).toBeDefined();
  });

  it("sends the cancel as soon as a job id arrives, when Cancel came first", async () => {
    let resolveId: (id: string) => void = () => {};
    vi.mocked(exportStart).mockReturnValueOnce(
      new Promise<string>((resolve) => {
        resolveId = resolve;
      }),
    );

    const starting = startExport();
    // Wait until the job is running but its id has not arrived yet.
    await vi.waitFor(() => expect(exportStart).toHaveBeenCalled());
    await cancelExport();
    expect(exportCancel).not.toHaveBeenCalled();
    expect(useExportStore.getState()).toMatchObject({
      cancelRequested: true,
      status: "cancelling",
    });

    resolveId("export:7");
    await starting;
    expect(exportCancel).toHaveBeenCalledWith("export:7");
  });
});

describe("hardware fallbacks and failure detail", () => {
  it("says the GPU dithering fell back to the CPU, not that a hardware encode failed", async () => {
    await startExport();

    eventSink()({
      kind: "fallback",
      stage: "encode",
      from: "gpu-dither",
      to: "cpu-dither",
      reason: "adapter lost",
    });

    expect(useExportStore.getState().fallbacks).toBe(1);
    expect(
      toastTitled("GPU dithering was not available, so the GIF was dithered on the CPU"),
    ).toMatchObject({ variant: "warn", description: "adapter lost" });
    expect(toastTitled("Hardware encoding failed, finished on the CPU")).toBeUndefined();
  });

  it("keeps the failure's detail with the message, for the panel and the toast", async () => {
    await startExport();

    eventSink()({
      kind: "error",
      error: {
        code: "gpu_failed",
        message: "The GPU renderer failed during the export.",
        detail: "device lost (DXGI_ERROR_DEVICE_REMOVED)",
      },
    });

    expect(useExportStore.getState()).toMatchObject({
      status: "failed",
      error: "The GPU renderer failed during the export.",
      errorDetail: "device lost (DXGI_ERROR_DEVICE_REMOVED)",
    });
    expect(toastTitled("Export failed")).toMatchObject({
      description:
        "The GPU renderer failed during the export. device lost (DXGI_ERROR_DEVICE_REMOVED)",
    });
  });

  it("clears the detail of an earlier failure when the next export starts", async () => {
    useExportStore.setState({ errorDetail: "old detail", error: "old" });

    await startExport();

    expect(useExportStore.getState()).toMatchObject({ error: null, errorDetail: null });
  });

  it("joins a message and a detail into one line, and leaves a bare message alone", () => {
    expect(failureText("The export failed.", "encoder exited")).toBe(
      "The export failed. encoder exited",
    );
    expect(failureText("The export failed.", null)).toBe("The export failed.");
    expect(failureText("The export failed.")).toBe("The export failed.");
  });
});

describe("the export as one undoable edit", () => {
  it("never records an export in the project history", async () => {
    await startExport();
    eventSink()({ kind: "done", path: "C:\\Out\\clip.gif", bytes: 1, seconds: 0.1, frames: 1 });

    expect(canUndo()).toBe(false);
    expect(undo()).toBe(false);
  });

  it("handles a bare event without touching the job's identity", () => {
    useExportStore.setState({ jobId: "export:3", status: "running" });
    handleExportEvent({ kind: "progress", framesDone: 1, framesTotal: 2, bytes: 3 });
    expect(useExportStore.getState().jobId).toBe("export:3");
  });
});

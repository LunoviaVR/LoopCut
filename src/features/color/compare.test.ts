import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import { previewFrame, previewSetProject } from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { renderUngradedFrame } from "./compare";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    previewSetProject: vi.fn(async () => ({}) as PreviewInfo),
    previewFrame: vi.fn(async () => new ArrayBuffer(8)),
  };
});

const viewport = { width: 480, height: 270, dpr: 1 };

beforeEach(() => {
  resetEditorState();
  vi.clearAllMocks();
  useEditorStore.getState().loadProject(sampleProject());
});

describe("ungraded frame", () => {
  it("sets a grade-off copy, renders the frame, and then restores the project", async () => {
    const before = useEditorStore.getState().project;

    await renderUngradedFrame(3, viewport);

    const setProject = vi.mocked(previewSetProject);
    expect(setProject).toHaveBeenCalledTimes(2);
    expect(setProject.mock.calls[0]?.[0].grade.enabled).toBe(false);
    expect(setProject.mock.calls[1]?.[0]).toEqual(before);
    expect(vi.mocked(previewFrame)).toHaveBeenCalledWith(3, "output");
  });

  it("renders in order: copy first, then the frame, then the restore", async () => {
    await renderUngradedFrame(0, viewport);

    const setOrder = vi.mocked(previewSetProject).mock.invocationCallOrder;
    const frameOrder = vi.mocked(previewFrame).mock.invocationCallOrder[0] ?? 0;
    expect(setOrder[0]).toBeLessThan(frameOrder);
    expect(frameOrder).toBeLessThan(setOrder[1] ?? 0);
  });

  it("skips a before render that a newer request has overtaken, so only the newest reaches the backend", async () => {
    vi.mocked(previewFrame).mockClear();
    const first = renderUngradedFrame(1, viewport);
    const second = renderUngradedFrame(2, viewport);

    expect(await first).toBeNull();
    expect(await second).toBeInstanceOf(ArrayBuffer);
    expect(vi.mocked(previewFrame)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(previewFrame)).toHaveBeenCalledWith(2, "output");
  });

  it("leaves the editor's own grade untouched", async () => {
    await renderUngradedFrame(1, viewport);

    expect(useEditorStore.getState().project?.grade.enabled).toBe(true);
  });

  it("restores the project even when the frame fails", async () => {
    vi.mocked(previewFrame).mockRejectedValueOnce(new Error("frame unavailable"));

    await expect(renderUngradedFrame(2, viewport)).rejects.toThrow("frame unavailable");
    expect(vi.mocked(previewSetProject)).toHaveBeenCalledTimes(2);
    expect(vi.mocked(previewSetProject).mock.calls[1]?.[0].grade.enabled).toBe(true);
  });
});

import { beforeEach, describe, expect, it, vi } from "vitest";
import { defaultCaptionAnimation, defaultCaptionStyle } from "@/domain/defaults";
import { captionsImportSrt, pickSrtFile } from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import {
  captionForPreset,
  DEFAULT_CAPTION_SECONDS,
  defaultCaptionRange,
  importSrtCaptions,
  PRESET_Y,
} from "./captionFactory";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    pickSrtFile: vi.fn(actual.pickSrtFile),
    captionsImportSrt: vi.fn(actual.captionsImportSrt),
  };
});

beforeEach(() => {
  resetEditorState();
  vi.clearAllMocks();
});

describe("preset captions", () => {
  it("builds the meme caption at the top with the default style", () => {
    const caption = captionForPreset("top", { start: 1, end: 4 }, "Top text");
    expect(caption.y).toBe(PRESET_Y.top);
    expect(caption.x).toBe(0.5);
    expect(caption.style).toEqual(defaultCaptionStyle());
    expect(caption.animation).toEqual(defaultCaptionAnimation());
    expect(caption.range).toEqual({ start: 1, end: 4 });
    expect(caption.text).toBe("Top text");
  });

  it("anchors the bottom caption at 0.88 and the plain text box at the middle", () => {
    expect(captionForPreset("bottom", { start: 0, end: 1 }, "x").y).toBe(0.88);
    const text = captionForPreset("text", { start: 0, end: 1 }, "Text");
    expect(text.y).toBe(0.5);
    expect(text.style.uppercase).toBe(false);
    expect(text.style.fontSize).toBeLessThan(defaultCaptionStyle().fontSize);
  });
});

describe("default caption range", () => {
  const segments = [
    { start: 0, end: 4 },
    { start: 6, end: 10 },
  ];

  it("takes the kept range under the playhead", () => {
    expect(defaultCaptionRange(segments, 7, 12)).toEqual({ start: 6, end: 10 });
  });

  it("takes a few seconds from the playhead when it is over a cut", () => {
    expect(defaultCaptionRange(segments, 5, 12)).toEqual({
      start: 5,
      end: 5 + DEFAULT_CAPTION_SECONDS,
    });
  });

  it("shortens the range to the clip near its end", () => {
    expect(defaultCaptionRange([{ start: 0, end: 12 }], 11.5, 12)).toEqual({ start: 0, end: 12 });
    expect(defaultCaptionRange([], 11.5, 12)).toEqual({ start: 9, end: 12 });
  });

  it("starts at zero without a plan", () => {
    expect(defaultCaptionRange([], null, 12)).toEqual({ start: 0, end: DEFAULT_CAPTION_SECONDS });
  });
});

describe("SubRip import", () => {
  it("appends the imported cues to the project as one step and returns how many", async () => {
    useEditorStore.getState().loadProject(sampleProject());
    const project = useEditorStore.getState().project;
    if (!project) throw new Error("no project");

    const count = await importSrtCaptions(project);

    expect(count).toBe(2);
    expect(vi.mocked(captionsImportSrt)).toHaveBeenCalledWith(expect.stringContaining(".srt"), 20);
    expect(useEditorStore.getState().project?.captions).toHaveLength(2);
    expect(useEditorStore.getState().dirty).toBe(true);
  });

  it("does nothing when the dialog is cancelled", async () => {
    useEditorStore.getState().loadProject(sampleProject());
    const project = useEditorStore.getState().project;
    if (!project) throw new Error("no project");
    vi.mocked(pickSrtFile).mockResolvedValueOnce(null);

    await expect(importSrtCaptions(project)).resolves.toBeNull();
    expect(useEditorStore.getState().project?.captions).toHaveLength(0);
    expect(useEditorStore.getState().dirty).toBe(false);
  });

  it("passes a rejection from the backend on to the caller", async () => {
    useEditorStore.getState().loadProject(sampleProject());
    const project = useEditorStore.getState().project;
    if (!project) throw new Error("no project");
    vi.mocked(captionsImportSrt).mockRejectedValueOnce(new Error("too many cues"));

    await expect(importSrtCaptions(project)).rejects.toThrow("too many cues");
    expect(useEditorStore.getState().project?.captions).toHaveLength(0);
  });
});

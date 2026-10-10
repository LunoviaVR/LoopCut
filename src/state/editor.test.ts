import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { defaultCaptionAnimation, defaultCaptionStyle } from "@/domain/defaults";
import type { Project } from "@/ipc/bindings/Project";
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";
import {
  beginGesture,
  canRedo,
  canUndo,
  endGesture,
  redo,
  resetEditorState,
  undo,
  useActiveTool,
  useCanRedo,
  useCanUndo,
  useClip,
  useEditorStore,
  withGesture,
} from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";

beforeEach(() => {
  resetEditorState();
});

const store = () => useEditorStore.getState();
const pastCount = () => useEditorStore.temporal.getState().pastStates.length;
const futureCount = () => useEditorStore.temporal.getState().futureStates.length;

function current(): Project {
  const project = useEditorStore.getState().project;
  if (!project) throw new Error("expected an open project");
  return project;
}

function load(project: Project = sampleProject()): void {
  store().loadProject(project);
}

function segmentsOf(project: Project) {
  return project.clips[0]?.segments ?? [];
}

function planWithFrames(count: number): SequencePlan {
  return {
    fps: { num: 20, den: 1 },
    duration: count / 20,
    frames: Array.from({ length: count }, (_, i) => ({
      sample: { kind: "at" as const, t: i / 20 },
      delayCs: 5,
    })),
  };
}

describe("loadProject", () => {
  it("replaces the project, marks it clean and clears history", () => {
    load();
    store().setSpeed(2);
    expect(store().dirty).toBe(true);
    expect(pastCount()).toBe(1);

    load(sampleProject({ name: "Other" }));
    expect(current().name).toBe("Other");
    expect(store().dirty).toBe(false);
    expect(pastCount()).toBe(0);
    expect(store().playhead).toBe(0);
    expect(store().selection).toBeNull();
  });
});

describe("document edits", () => {
  it("records each edit as one history entry that undo and redo restore", () => {
    load();
    store().setSpeed(2);
    expect(current().timing.speed).toBe(2);
    expect(pastCount()).toBe(1);

    expect(undo()).toBe(true);
    expect(current().timing.speed).toBe(1);
    expect(canRedo()).toBe(true);

    expect(redo()).toBe(true);
    expect(current().timing.speed).toBe(2);
  });

  it("clamps speed into 0.25 to 4 and ignores non-finite input", () => {
    load();
    store().setSpeed(10);
    expect(current().timing.speed).toBe(4);
    store().setSpeed(0.01);
    expect(current().timing.speed).toBe(0.25);
    const entries = pastCount();
    store().setSpeed(Number.NaN);
    expect(current().timing.speed).toBe(0.25);
    expect(pastCount()).toBe(entries);
  });

  it("drops the redo branch when a new edit follows an undo", () => {
    load();
    store().setSpeed(2);
    undo();
    expect(canRedo()).toBe(true);
    store().setSpeed(3);
    expect(canRedo()).toBe(false);
    expect(futureCount()).toBe(0);
  });

  it("leaves no entry and no dirty flag for an edit that changes nothing", () => {
    load();
    store().setSpeed(1);
    store().setCrop({ x: 0, y: 0, w: 1, h: 1 });
    expect(pastCount()).toBe(0);
    expect(store().dirty).toBe(false);
  });

  it("applies a raw recipe as one undoable edit", () => {
    load();
    store().updateProject((draft) => {
      draft.timing.crossfade = 0.5;
    });
    expect(current().timing.crossfade).toBe(0.5);
    expect(pastCount()).toBe(1);
  });

  it("makes undo and redo mark the project dirty", () => {
    load();
    store().setSpeed(2);
    store().markSaved();
    expect(store().dirty).toBe(false);
    undo();
    expect(store().dirty).toBe(true);
  });

  it("does nothing without an open project", () => {
    store().setSpeed(2);
    expect(store().project).toBeNull();
    expect(pastCount()).toBe(0);
    expect(store().setSegment(0, 0, 1)).toBe(false);
  });
});

describe("segments", () => {
  const twoSegments = () =>
    sampleProject({
      clips: [
        {
          id: "clip-1",
          path: "C:/Clips/sample.mp4",
          streamIndex: 0,
          segments: [
            { start: 0, end: 2 },
            { start: 4, end: 6 },
          ],
          holds: [],
        },
      ],
    });

  it("clamps a segment to its neighbours and to the minimum length", () => {
    load(twoSegments());
    expect(store().setSegment(0, -1, 3)).toBe(true);
    expect(segmentsOf(current())).toEqual([
      { start: 0, end: 3 },
      { start: 4, end: 6 },
    ]);
    store().setSegment(1, 2, 5);
    expect(segmentsOf(current())).toEqual([
      { start: 0, end: 3 },
      { start: 3, end: 5 },
    ]);
  });

  it("keeps a minimum length when start and end collide", () => {
    load();
    store().setSegment(0, 5.9, 5.9);
    const [segment] = segmentsOf(current());
    expect(segment?.start).toBeCloseTo(5.9, 9);
    expect(segment?.end).toBeCloseTo(5.95, 9);
  });

  it("rejects an unknown index or a non-finite value", () => {
    load();
    expect(store().setSegment(5, 0, 1)).toBe(false);
    expect(store().setSegment(0, Number.NaN, 1)).toBe(false);
    expect(pastCount()).toBe(0);
  });

  it("splits the range under a time and rejects cuts too close to an edge", () => {
    load();
    expect(store().splitAt(2)).toBe(true);
    expect(segmentsOf(current())).toEqual([
      { start: 0, end: 2 },
      { start: 2, end: 6 },
    ]);
    expect(store().splitAt(2.01)).toBe(false);
    expect(store().splitAt(9)).toBe(false);
    undo();
    expect(segmentsOf(current())).toEqual([{ start: 0, end: 6 }]);
  });

  it("moves the selected segment index right when a split happens before it", () => {
    load(twoSegments());
    store().setSelection({ kind: "segment", index: 1 });
    store().splitAt(1);
    expect(store().selection).toEqual({ kind: "segment", index: 2 });
  });

  it("refuses to delete the last kept range", () => {
    load();
    expect(store().deleteSegment(0)).toBe(false);
    expect(segmentsOf(current())).toHaveLength(1);
  });

  it("deletes a range and keeps the selection pointing at the same range", () => {
    load(twoSegments());
    store().setSelection({ kind: "segment", index: 1 });
    expect(store().deleteSegment(0)).toBe(true);
    expect(segmentsOf(current())).toEqual([{ start: 4, end: 6 }]);
    expect(store().selection).toEqual({ kind: "segment", index: 0 });
  });

  it("clears the selection when the selected range is deleted", () => {
    load(twoSegments());
    store().setSelection({ kind: "segment", index: 0 });
    store().deleteSegment(0);
    expect(store().selection).toBeNull();
  });

  it("collapses every range into one trim", () => {
    load(twoSegments());
    store().setSelection({ kind: "segment", index: 1 });
    expect(store().setTrim(1, 5)).toBe(true);
    expect(segmentsOf(current())).toEqual([{ start: 1, end: 5 }]);
    expect(store().selection).toBeNull();
  });
});

describe("transform and output", () => {
  it("wraps rotation in quarter turns", () => {
    load();
    for (let i = 0; i < 4; i++) store().rotate(90);
    expect(current().transform.rotation).toBe(0);
    store().rotate(-90);
    expect(current().transform.rotation).toBe(270);
  });

  it("toggles flips", () => {
    load();
    store().toggleFlipH();
    store().toggleFlipV();
    expect(current().transform.flipH).toBe(true);
    expect(current().transform.flipV).toBe(true);
    store().toggleFlipH();
    expect(current().transform.flipH).toBe(false);
  });

  it("keeps the crop inside the frame", () => {
    load();
    store().setCrop({ x: 0.9, y: -1, w: 0.5, h: 0.2 });
    expect(current().transform.crop).toEqual({ x: 0.5, y: 0, w: 0.5, h: 0.2 });
  });

  it("stores a valid crop aspect and clears an invalid one", () => {
    load();
    store().setCropAspect({ num: 16, den: 9 });
    expect(current().transform.cropAspect).toEqual({ num: 16, den: 9 });
    store().setCropAspect({ num: 0, den: 1 });
    expect(current().transform.cropAspect).toBeNull();
  });

  it("accepts valid output rates and rejects rates above 240 fps", () => {
    load();
    expect(store().setOutputFps({ num: 25, den: 2 })).toBe(true);
    expect(current().output.fps).toEqual({ num: 25, den: 2 });
    expect(store().setOutputFps({ num: 0, den: 1 })).toBe(false);
    expect(store().setOutputFps({ num: 300, den: 1 })).toBe(false);
  });

  it("validates output sizes by mode", () => {
    load();
    expect(store().setOutputSize({ mode: "width", width: 0 })).toBe(false);
    expect(store().setOutputSize({ mode: "fit", maxWidth: 320, maxHeight: 320 })).toBe(true);
    expect(current().output.size).toEqual({ mode: "fit", maxWidth: 320, maxHeight: 320 });
    expect(store().setOutputSize({ mode: "scale", factor: 0 })).toBe(false);
  });

  it("clamps the loop count to the GIF range", () => {
    load();
    store().setLoopCount(-3);
    expect(current().output.loopCount).toBe(0);
    store().setLoopCount(70_000);
    expect(current().output.loopCount).toBe(65_535);
  });

  it("merges export settings, clamps them, and ignores undefined patch values", () => {
    load();
    store().setExport({ quality: 500, colors: 1, targetBytes: -5 });
    expect(current().export.quality).toBe(100);
    expect(current().export.colors).toBe(2);
    expect(current().export.targetBytes).toBeNull();

    store().setExport({ quality: undefined, colors: 64 });
    expect(current().export.quality).toBe(100);
    expect(current().export.colors).toBe(64);
  });

  it("merges grade fields and leaves the rest alone", () => {
    load();
    store().setGrade({ exposure: 0.5 });
    expect(current().grade.exposure).toBe(0.5);
    expect(current().grade.contrast).toBe(0);
    expect(current().grade.gain).toEqual([1, 1, 1]);
  });
});

describe("captions", () => {
  it("adds, updates and removes captions", () => {
    load();
    const id = store().addCaption({
      id: "caption-1",
      text: "wait for it",
      range: { start: 0, end: 2 },
      x: 0.5,
      y: 0.9,
      style: defaultCaptionStyle(),
      animation: defaultCaptionAnimation(),
    });
    expect(id).toBe("caption-1");
    expect(store().selection).toEqual({ kind: "caption", id: "caption-1" });

    store().updateCaption("caption-1", { text: "now" });
    expect(current().captions[0]?.text).toBe("now");

    store().removeCaption("caption-1");
    expect(current().captions).toHaveLength(0);
    expect(store().selection).toBeNull();
  });

  it("generates an id when none is given", () => {
    load();
    const id = store().addCaption({
      text: "hi",
      range: { start: 0, end: 1 },
      x: 0.5,
      y: 0.5,
      style: defaultCaptionStyle(),
      animation: defaultCaptionAnimation(),
    });
    expect(typeof id).toBe("string");
    expect(id?.length ?? 0).toBeGreaterThan(0);
    expect(current().captions[0]?.id).toBe(id);
  });

  it("returns null when adding without a project", () => {
    expect(
      store().addCaption({
        text: "x",
        range: { start: 0, end: 1 },
        x: 0,
        y: 0,
        style: defaultCaptionStyle(),
        animation: defaultCaptionAnimation(),
      }),
    ).toBeNull();
  });

  it("ignores updates to unknown caption ids", () => {
    load();
    store().updateCaption("missing", { text: "x" });
    expect(pastCount()).toBe(0);
  });
});

describe("gesture grouping", () => {
  it("records a whole drag as one undo step", () => {
    load();
    beginGesture();
    store().setSpeed(1.5);
    store().setSpeed(2);
    store().setSpeed(3.5);
    expect(current().timing.speed).toBe(3.5);
    expect(pastCount()).toBe(0);

    endGesture();
    expect(pastCount()).toBe(1);

    undo();
    expect(current().timing.speed).toBe(1);
    expect(canUndo()).toBe(false);
  });

  it("records nothing when the drag ends where it started, and a clean project stays clean", () => {
    load();
    beginGesture();
    store().setSpeed(2);
    store().setSpeed(1);
    endGesture();
    expect(pastCount()).toBe(0);
    expect(store().dirty).toBe(false);
  });

  it("keeps a dirty project dirty when a gesture returns it to its start", () => {
    load();
    store().setSpeed(2);
    expect(store().dirty).toBe(true);
    beginGesture();
    store().setSpeed(3);
    store().setSpeed(2);
    endGesture();
    expect(store().dirty).toBe(true);
  });

  it("counts nested gestures once, at the outermost end", () => {
    load();
    beginGesture();
    beginGesture();
    store().setSpeed(2);
    endGesture();
    expect(store().gestureDepth).toBe(1);
    expect(pastCount()).toBe(0);

    store().setSpeed(3);
    endGesture();
    expect(store().gestureDepth).toBe(0);
    expect(pastCount()).toBe(1);
  });

  it("ignores undo while a gesture is open", () => {
    load();
    store().setSpeed(2);
    beginGesture();
    store().setSpeed(3);
    expect(canUndo()).toBe(false);
    expect(undo()).toBe(false);
    endGesture();
    expect(current().timing.speed).toBe(3);
    expect(canUndo()).toBe(true);
  });

  it("drops an open gesture when a project loads", () => {
    load();
    beginGesture();
    store().setSpeed(2);
    load(sampleProject({ name: "Fresh" }));
    expect(store().gestureDepth).toBe(0);
    store().setSpeed(3);
    expect(pastCount()).toBe(1);
  });

  it("records the step from withGesture even when the callback throws", () => {
    load();
    expect(() =>
      withGesture(() => {
        store().setSpeed(2);
        throw new Error("boom");
      }),
    ).toThrow("boom");
    expect(store().gestureDepth).toBe(0);
    expect(pastCount()).toBe(1);
  });

  it("does nothing when endGesture has no matching begin", () => {
    load();
    endGesture();
    expect(store().gestureDepth).toBe(0);
    expect(pastCount()).toBe(0);
  });
});

describe("history limit", () => {
  it("keeps the newest 200 entries", () => {
    load();
    for (let i = 0; i < 250; i++) {
      store().setSpeed(i % 2 === 0 ? 2 : 3);
    }
    expect(pastCount()).toBe(200);
    // Undoing all 200 entries lands on the oldest kept state: after change 50, which set speed 3.
    for (let i = 0; i < 200; i++) undo();
    expect(current().timing.speed).toBe(3);
    expect(canUndo()).toBe(false);
  });
});

describe("ui state", () => {
  it("is never recorded in history", () => {
    load();
    store().setPlayhead(4);
    store().setActiveTool("crop");
    store().setSelection({ kind: "segment", index: 0 });
    store().setTimelineZoom(2);
    store().setLoopPlayback(false);
    store().setExporting(true);
    expect(pastCount()).toBe(0);
    expect(store().dirty).toBe(false);
  });

  it("clamps the playhead to the plan, or to zero when there is no plan", () => {
    load();
    store().setPlayhead(-5);
    expect(store().playhead).toBe(0);
    store().setPlayhead(12.7);
    expect(store().playhead).toBe(12);

    store().setSequencePlan(planWithFrames(10));
    store().setPlayhead(50);
    expect(store().playhead).toBe(9);
  });

  it("pulls the playhead back when a shorter plan arrives", () => {
    load();
    store().setSequencePlan(planWithFrames(10));
    store().setPlayhead(8);
    store().setSequencePlan(planWithFrames(4));
    expect(store().playhead).toBe(3);
  });

  it("normalizes transport rates and keeps their sign", () => {
    store().setTransport({ isPlaying: true, playRate: -8 });
    expect(store().isPlaying).toBe(true);
    expect(store().playRate).toBe(-4);
    store().setTransport({ playRate: 0 });
    expect(store().playRate).toBe(-4);
    store().setTransport({ playRate: 0.1 });
    expect(store().playRate).toBe(0.25);
  });

  it("clamps the timeline zoom", () => {
    store().setTimelineZoom(100);
    expect(store().timelineZoom).toBe(16);
    store().setTimelineZoom(0.01);
    expect(store().timelineZoom).toBe(0.25);
  });

  it("registers and forgets media by token", () => {
    const media = {
      token: "t-1",
      path: "C:/Clips/a.mp4",
      fileName: "a.mp4",
      info: {} as never,
      sourceUrl: null,
      needsProxy: false,
      proxyUrl: null,
      keyframeInterval: null,
    };
    store().registerMedia(media);
    expect(store().mediaByToken["t-1"]).toBe(media);
    store().forgetMedia("t-1");
    expect(store().mediaByToken).toEqual({});
  });
});

describe("hooks", () => {
  it("reports undo and redo availability reactively", () => {
    load();
    const { result } = renderHook(() => ({ undo: useCanUndo(), redo: useCanRedo() }));
    expect(result.current).toEqual({ undo: false, redo: false });

    act(() => {
      store().setSpeed(2);
    });
    expect(result.current).toEqual({ undo: true, redo: false });

    act(() => {
      undo();
    });
    expect(result.current).toEqual({ undo: false, redo: true });
  });

  it("reports the open clip and the active tool", () => {
    load();
    const { result } = renderHook(() => ({ clip: useClip(), tool: useActiveTool() }));
    expect(result.current.clip?.path).toBe("C:/Clips/sample.mp4");
    expect(result.current.tool).toBe("media");

    act(() => {
      store().setActiveTool("export");
    });
    expect(result.current.tool).toBe("export");
  });
});

describe("inspector, size preset, preview frames and the export flag", () => {
  it("choosing a tool opens the inspector, and the inspector toggles on its own", () => {
    load();
    store().setInspectorOpen(false);
    store().setActiveTool("trim");
    expect(store().inspectorOpen).toBe(true);

    store().setInspectorOpen(false);
    expect(store().inspectorOpen).toBe(false);
    expect(store().activeTool).toBe("trim");
  });

  it("bounds the playhead by the preview's frame count and clamps it when the count shrinks", () => {
    load();
    store().setFrameCount(120);
    store().setPlayhead(500);
    expect(store().playhead).toBe(119);

    store().setPlayhead(100);
    store().setFrameCount(50);
    expect(store().frameCount).toBe(50);
    expect(store().playhead).toBe(49);
  });

  it("falls back to the sequence plan's frames when no preview has reported a count", () => {
    load();
    store().setSequencePlan(planWithFrames(10));
    store().setPlayhead(50);
    expect(store().playhead).toBe(9);
  });

  it("treats a preview count of zero as not known yet", () => {
    load();
    store().setFrameCount(0);
    store().setPlayhead(7);
    expect(store().playhead).toBe(7);
  });

  // TODO(export): the export milestone sets isExporting when a job starts and clears it on done, error and cancel.
  // Until then nothing in the app changes it, so the guards that read it (import, update, Ctrl+E) stay dormant.
  // This test pins that the flag only moves through setExporting, so the export UI has one place to wire.
  it("moves the export flag only through setExporting", () => {
    load();
    expect(store().isExporting).toBe(false);

    store().setActiveTool("export");
    store().setPlayhead(0);
    store().setTimelineZoom(2);
    expect(store().isExporting).toBe(false);

    store().setExporting(true);
    expect(store().isExporting).toBe(true);
    store().setExporting(false);
    expect(store().isExporting).toBe(false);
  });
});

import "@/ui/test/env";
import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defaultCaptionStyle } from "@/domain/defaults";
import type { Caption } from "@/ipc/bindings/Caption";
import { canUndo, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { CaptionOverlay } from "./CaptionOverlay";
import { captionBox, measureWithCanvas } from "./captionGeometry";

/** The frame is 1000 by 500 CSS pixels, the same size as the output. */
const OUTPUT = { width: 1000, height: 500 };
const FRAME = {
  left: 0,
  top: 0,
  width: 1000,
  height: 500,
  right: 1000,
  bottom: 500,
  x: 0,
  y: 0,
  toJSON: () => ({}),
};

function caption(id: string, text: string): Caption {
  return {
    id,
    text,
    range: { start: 1, end: 3 },
    x: 0.5,
    y: 0.88,
    style: defaultCaptionStyle(),
    animation: { kind: "none" },
  };
}

const stored = () => useEditorStore.getState().project?.captions.find((item) => item.id === "a");

/** Resolves on the next animation frame, so a paint scheduled by a pointer move has run. */
const nextFrame = () =>
  new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));

beforeEach(() => {
  resetEditorState();
  useEditorStore.getState().loadProject(sampleProject({ captions: [caption("a", "Hello")] }));
  useEditorStore.getState().setSelection({ kind: "caption", id: "a" });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(FRAME as DOMRect);
});

afterEach(() => {
  vi.restoreAllMocks();
});

/** The overlay under the store's project, so a commit re-renders it the way the monitor does. */
function Harness() {
  const project = useEditorStore((state) => state.project);
  if (!project) return null;
  return <CaptionOverlay project={project} output={OUTPUT} />;
}

describe("caption overlay drag", () => {
  it("moves the box by DOM writes during the drag and writes the anchor once, on release", async () => {
    render(<Harness />);
    const box = screen.getByLabelText("Caption box, drag to move");
    const startLeft = captionBox(caption("a", "Hello"), OUTPUT, measureWithCanvas).left;
    let projectWrites = 0;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.project !== previous.project) projectWrites += 1;
    });

    fireEvent.pointerDown(box, { pointerId: 1, button: 0, clientX: 500, clientY: 440 });
    for (let step = 1; step <= 10; step += 1) {
      fireEvent.pointerMove(box, { pointerId: 1, clientX: 500 + step * 10, clientY: 440 });
      await nextFrame();
    }

    // 100 px of a 1000 px frame: the anchor moves by 0.1 and the box with it.
    expect(projectWrites).toBe(0);
    expect(Number.parseFloat(box.style.left)).toBeCloseTo((startLeft + 0.1) * 100, 6);
    expect(stored()?.x).toBeCloseTo(0.5, 9);

    fireEvent.pointerUp(box, { pointerId: 1, clientX: 600, clientY: 440 });
    unsubscribe();

    expect(projectWrites).toBe(1);
    expect(stored()?.x).toBeCloseTo(0.6, 9);
    expect(stored()?.y).toBeCloseTo(0.88, 9);
    expect(Number.parseFloat(box.style.left)).toBeCloseTo((startLeft + 0.1) * 100, 6);
  });

  it("is one undo step for the whole drag", () => {
    render(<Harness />);
    const box = screen.getByLabelText("Caption box, drag to move");

    fireEvent.pointerDown(box, { pointerId: 1, button: 0, clientX: 500, clientY: 440 });
    fireEvent.pointerMove(box, { pointerId: 1, clientX: 550, clientY: 440 });
    fireEvent.pointerMove(box, { pointerId: 1, clientX: 600, clientY: 440 });
    fireEvent.pointerUp(box, { pointerId: 1, clientX: 600, clientY: 440 });

    expect(stored()?.x).toBeCloseTo(0.6, 9);
    undo();
    expect(stored()?.x).toBeCloseTo(0.5, 9);
    expect(canUndo()).toBe(false);
  });

  it("writes nothing for a press that never moves", () => {
    render(<Harness />);
    const box = screen.getByLabelText("Caption box, drag to move");
    let projectWrites = 0;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.project !== previous.project) projectWrites += 1;
    });

    fireEvent.pointerDown(box, { pointerId: 1, button: 0, clientX: 500, clientY: 440 });
    fireEvent.pointerUp(box, { pointerId: 1, clientX: 500, clientY: 440 });
    unsubscribe();

    expect(projectWrites).toBe(0);
    expect(canUndo()).toBe(false);
  });
});

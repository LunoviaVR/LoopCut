import "@/ui/test/env";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import { canUndo, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { CropOverlay } from "./CropOverlay";

/** The overlay covers a 1000 x 562.5 CSS pixel frame in these tests. */
const BOUNDS = {
  left: 0,
  top: 0,
  width: 1000,
  height: 562.5,
  right: 1000,
  bottom: 562.5,
  x: 0,
  y: 0,
  toJSON: () => ({}),
};

const crop = () => useEditorStore.getState().project?.transform.crop;

beforeEach(() => {
  resetEditorState();
  const base = sampleProject();
  useEditorStore.getState().loadProject({
    ...base,
    transform: { ...base.transform, crop: { x: 0.1, y: 0.1, w: 0.5, h: 0.5 } },
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(BOUNDS as DOMRect);
});

function renderOverlay() {
  const project = useEditorStore.getState().project;
  if (!project) throw new Error("project");
  const view = render(<CropOverlay media={sampleMedia()} project={project} />);
  const root = screen.getByRole("application", { name: /Crop area/ });
  return { ...view, root };
}

describe("CropOverlay keyboard", () => {
  it("nudges the crop by one display pixel, and by ten with Shift", () => {
    const { root } = renderOverlay();

    fireEvent.keyDown(root, { key: "ArrowRight" });
    expect(crop()?.x).toBeCloseTo(0.1 + 1 / 1920, 9);

    fireEvent.keyDown(root, { key: "ArrowDown", shiftKey: true });
    expect(crop()?.y).toBeCloseTo(0.1 + 10 / 1080, 9);
  });

  it("keeps the crop inside the frame when nudged past an edge", () => {
    const { root } = renderOverlay();
    for (let index = 0; index < 25; index++)
      fireEvent.keyDown(root, { key: "ArrowLeft", shiftKey: true });
    for (let index = 0; index < 25; index++)
      fireEvent.keyDown(root, { key: "ArrowUp", shiftKey: true });
    expect(crop()?.x).toBe(0);
    expect(crop()?.y).toBe(0);
  });
});

describe("CropOverlay handles", () => {
  it("gives every handle a 24px hit area around an 8px drawn square", () => {
    const { container } = renderOverlay();
    const handles = container.querySelectorAll<HTMLElement>("[data-handle]");

    expect(handles).toHaveLength(8);
    for (const handle of handles) {
      expect(handle).toHaveClass("size-6");
      expect(handle.firstElementChild).toHaveClass("size-2");
    }
  });
});

describe("CropOverlay drags", () => {
  it("moves the crop with the pointer and commits with Enter as one undo step", () => {
    const { root } = renderOverlay();
    const body = root.querySelector(".cursor-move") as HTMLElement;

    fireEvent.pointerDown(body, { pointerId: 1, button: 0, clientX: 300, clientY: 100 });
    fireEvent.pointerMove(root, { pointerId: 1, clientX: 400, clientY: 100 });
    fireEvent.keyDown(root, { key: "Enter" });

    expect(crop()?.x).toBeCloseTo(0.2, 9);
    undo();
    expect(crop()?.x).toBeCloseTo(0.1, 9);
    expect(canUndo()).toBe(false);
  });

  it("writes the crop once per release, however many moves the drag made", () => {
    const { root } = renderOverlay();
    const body = root.querySelector(".cursor-move") as HTMLElement;
    let projectWrites = 0;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.project !== previous.project) projectWrites += 1;
    });

    fireEvent.pointerDown(body, { pointerId: 1, button: 0, clientX: 300, clientY: 100 });
    for (let step = 1; step <= 20; step += 1) {
      fireEvent.pointerMove(root, { pointerId: 1, clientX: 300 + step * 5, clientY: 100 });
    }
    expect(projectWrites).toBe(0);

    fireEvent.pointerUp(root, { pointerId: 1, clientX: 400, clientY: 100 });
    unsubscribe();

    expect(projectWrites).toBe(1);
    expect(crop()?.x).toBeCloseTo(0.2, 9);
  });

  it("cancels a drag with Escape and restores the crop it started from", () => {
    const { root } = renderOverlay();
    const body = root.querySelector(".cursor-move") as HTMLElement;

    fireEvent.pointerDown(body, { pointerId: 1, button: 0, clientX: 300, clientY: 100 });
    fireEvent.pointerMove(root, { pointerId: 1, clientX: 450, clientY: 160 });
    fireEvent.keyDown(root, { key: "Escape" });

    expect(crop()?.x).toBeCloseTo(0.1, 9);
    expect(crop()?.y).toBeCloseTo(0.1, 9);
    expect(canUndo()).toBe(false);
  });

  it("opens the undo gesture on the first move, not on the press", () => {
    const { root } = renderOverlay();
    const body = root.querySelector(".cursor-move") as HTMLElement;

    fireEvent.pointerDown(body, { pointerId: 1, button: 0, clientX: 300, clientY: 100 });
    expect(useEditorStore.getState().gestureDepth).toBe(0);

    fireEvent.pointerMove(root, { pointerId: 1, clientX: 320, clientY: 100 });
    expect(useEditorStore.getState().gestureDepth).toBe(1);

    fireEvent.pointerUp(root, { pointerId: 1, clientX: 320, clientY: 100 });
    expect(useEditorStore.getState().gestureDepth).toBe(0);
  });

  it("records no undo step for a press that never moves", () => {
    const { root } = renderOverlay();
    const body = root.querySelector(".cursor-move") as HTMLElement;

    fireEvent.pointerDown(body, { pointerId: 1, button: 0, clientX: 300, clientY: 100 });
    fireEvent.pointerUp(root, { pointerId: 1, clientX: 300, clientY: 100 });

    expect(crop()?.x).toBeCloseTo(0.1, 9);
    expect(canUndo()).toBe(false);
  });

  it("draws the rule-of-thirds guides only while a drag runs", async () => {
    const { root } = renderOverlay();
    const body = root.querySelector(".cursor-move") as HTMLElement;
    expect(root.querySelectorAll("[data-guide]")).toHaveLength(0);

    fireEvent.pointerDown(body, { pointerId: 1, button: 0, clientX: 300, clientY: 100 });
    fireEvent.pointerMove(root, { pointerId: 1, clientX: 320, clientY: 100 });
    await waitFor(() => expect(root.querySelectorAll("[data-guide]")).toHaveLength(4));

    fireEvent.keyDown(root, { key: "Escape" });
    expect(root.querySelectorAll("[data-guide]")).toHaveLength(0);
  });
});

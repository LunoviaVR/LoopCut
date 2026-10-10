import "@/ui/test/env";
import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { defaultCaptionStyle } from "@/domain/defaults";
import type { ViewScale } from "@/features/timeline/mapping";
import type { Caption } from "@/ipc/bindings/Caption";
import { resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { CaptionBars } from "./CaptionBars";
import { captionBarRect } from "./captionTimeline";

/** An 800 px wide view of a 12.4 s clip: one second is about 64.5 px. */
const SCALE: ViewScale = { duration: 12.4, viewWidth: 800, zoom: 1, scrollPx: 0 };
const PIXELS_PER_SECOND = 800 / 12.4;

/** Resolves on the next animation frame, so a paint scheduled by a pointer move has run. */
const nextFrame = () =>
  new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));

function caption(id: string, text: string, start: number, end: number): Caption {
  return {
    id,
    text,
    range: { start, end },
    x: 0.5,
    y: 0.88,
    style: defaultCaptionStyle(),
    animation: { kind: "none" },
  };
}

beforeEach(() => {
  resetEditorState();
  useEditorStore.getState().loadProject(
    sampleProject({
      captions: [caption("a", "First", 1, 3), caption("b", "Second", 5, 6)],
    }),
  );
});

function renderBars(selectedId: string | null = null) {
  const captions = useEditorStore.getState().project?.captions ?? [];
  return render(
    <CaptionBars
      scale={SCALE}
      duration={SCALE.duration}
      captions={captions}
      selectedId={selectedId}
      top={0}
      height={20}
    />,
  );
}

const range = (id: string) =>
  useEditorStore.getState().project?.captions.find((item) => item.id === id)?.range;

describe("caption bars", () => {
  it("shows one bar per caption, labelled with its first line", () => {
    renderBars();
    expect(screen.getByRole("button", { name: "First" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Second" })).toBeInTheDocument();
  });

  it("selects a caption on click and shows the Captions tool with it", async () => {
    renderBars();

    await userEvent.click(screen.getByRole("button", { name: "Second" }));

    expect(useEditorStore.getState().selection).toEqual({ kind: "caption", id: "b" });
    expect(useEditorStore.getState().activeTool).toBe("captions");
  });

  it("moves a caption when its body is dragged, by the distance in seconds", () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "First" });

    fireEvent.pointerDown(bar, { clientX: 100, clientY: 5, button: 0 });
    fireEvent.pointerMove(bar, { clientX: 100 + PIXELS_PER_SECOND, clientY: 5 });
    fireEvent.pointerUp(bar, { clientX: 100 + PIXELS_PER_SECOND, clientY: 5 });

    expect(range("a")?.start).toBeCloseTo(2, 6);
    expect(range("a")?.end).toBeCloseTo(4, 6);
  });

  it("does not seek the playhead when a bar is pressed", () => {
    renderBars();
    fireEvent.pointerDown(screen.getByRole("button", { name: "First" }), {
      clientX: 100,
      button: 0,
    });
    expect(useEditorStore.getState().playhead).toBe(0);
  });

  it("writes the project once for a whole drag, however many pointer moves it made", async () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "First" });
    let projectWrites = 0;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.project !== previous.project) projectWrites += 1;
    });

    fireEvent.pointerDown(bar, { clientX: 100, clientY: 5, button: 0 });
    for (let step = 1; step <= 10; step += 1) {
      fireEvent.pointerMove(bar, { clientX: 100 + step * 10, clientY: 5 });
      await nextFrame();
    }
    expect(projectWrites).toBe(0);

    // The bar follows the pointer by DOM writes while the store still holds the old range.
    const moved = 100 / PIXELS_PER_SECOND;
    const expected = captionBarRect({ start: 1 + moved, end: 3 + moved }, SCALE);
    expect(Number.parseFloat(bar.style.left)).toBeCloseTo(expected.left, 6);
    expect(Number.parseFloat(bar.style.width)).toBeCloseTo(expected.width, 6);
    expect(range("a")).toEqual({ start: 1, end: 3 });

    fireEvent.pointerUp(bar, { clientX: 200, clientY: 5 });
    unsubscribe();

    expect(projectWrites).toBe(1);
    expect(range("a")?.start).toBeCloseTo(1 + moved, 6);
  });

  it("does not write the project for a press that never moves", () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "First" });
    let projectWrites = 0;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.project !== previous.project) projectWrites += 1;
    });

    fireEvent.pointerDown(bar, { clientX: 100, clientY: 5, button: 0 });
    fireEvent.pointerUp(bar, { clientX: 100, clientY: 5 });
    unsubscribe();

    expect(projectWrites).toBe(0);
    expect(range("a")).toEqual({ start: 1, end: 3 });
  });

  it("is one undo step for the whole drag", () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "Second" });

    fireEvent.pointerDown(bar, { clientX: 400, clientY: 5, button: 0 });
    fireEvent.pointerMove(bar, { clientX: 300, clientY: 5 });
    fireEvent.pointerMove(bar, { clientX: 200, clientY: 5 });
    fireEvent.pointerUp(bar, { clientX: 200, clientY: 5 });

    expect(range("b")?.start).toBeLessThan(5);
    undo();
    expect(range("b")).toEqual({ start: 5, end: 6 });
  });
});

describe("caption bars from the keyboard", () => {
  const selection = () => useEditorStore.getState().selection;
  const captionIds = () =>
    (useEditorStore.getState().project?.captions ?? []).map((item) => item.id);

  it("selects a caption with Enter, as a click does", async () => {
    renderBars();
    act(() => screen.getByRole("button", { name: "Second" }).focus());

    await userEvent.keyboard("{Enter}");

    expect(selection()).toEqual({ kind: "caption", id: "b" });
    expect(useEditorStore.getState().activeTool).toBe("captions");
  });

  it("selects a caption with Space, as a click does", async () => {
    renderBars();
    act(() => screen.getByRole("button", { name: "First" }).focus());

    await userEvent.keyboard(" ");

    expect(selection()).toEqual({ kind: "caption", id: "a" });
  });

  it("removes the focused caption with Delete, as one undo step", async () => {
    renderBars();
    act(() => screen.getByRole("button", { name: "Second" }).focus());

    await userEvent.keyboard("{Delete}");

    expect(captionIds()).toEqual(["a"]);
    undo();
    expect(captionIds()).toEqual(["a", "b"]);
  });

  it("moves focus to the next caption when the focused one is deleted", async () => {
    renderBars();
    act(() => screen.getByRole("button", { name: "First" }).focus());

    await userEvent.keyboard("{Delete}");

    expect(captionIds()).toEqual(["b"]);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Second" }));
  });

  it("moves focus to the previous caption when the last one is deleted", async () => {
    renderBars();
    act(() => screen.getByRole("button", { name: "Second" }).focus());

    await userEvent.keyboard("{Delete}");

    expect(captionIds()).toEqual(["a"]);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "First" }));
  });

  it("moves focus to the timeline when the only caption is deleted", () => {
    useEditorStore.getState().removeCaption("b");
    render(
      <div
        role="application"
        aria-label="Timeline"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: the timeline is a keyboard composite that takes focus
        tabIndex={0}
      >
        <CaptionBars
          scale={SCALE}
          duration={SCALE.duration}
          captions={useEditorStore.getState().project?.captions ?? []}
          selectedId={null}
          top={0}
          height={20}
        />
      </div>,
    );
    act(() => screen.getByRole("button", { name: "First" }).focus());

    fireEvent.keyDown(screen.getByRole("button", { name: "First" }), { key: "Delete" });

    expect(captionIds()).toEqual([]);
    expect(document.activeElement).toBe(screen.getByRole("application"));
  });

  it("moves the focused caption one output frame with an arrow key", () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "First" });
    // The sample project is 20 fps at speed 1, so one output frame is 0.05 s of the clip.
    const frame = 1 / 20;

    fireEvent.keyDown(bar, { key: "ArrowRight" });

    expect(range("a")?.start).toBeCloseTo(1 + frame, 6);
    expect(range("a")?.end).toBeCloseTo(3 + frame, 6);
  });

  it("moves the focused caption ten output frames with Shift and an arrow key", () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "First" });

    fireEvent.keyDown(bar, { key: "ArrowRight", shiftKey: true });
    expect(range("a")?.start).toBeCloseTo(1 + 10 / 20, 6);

    fireEvent.keyDown(bar, { key: "ArrowLeft" });
    expect(range("a")?.start).toBeCloseTo(1 + 10 / 20 - 1 / 20, 6);
  });

  it("steps a caption by the source time of one frame at the clip speed", () => {
    useEditorStore.getState().setSpeed(2);
    renderBars();

    fireEvent.keyDown(screen.getByRole("button", { name: "First" }), { key: "ArrowRight" });

    // At 2x each output frame covers two frames' worth of clip time: 2 / 20 s.
    expect(range("a")?.start).toBeCloseTo(1 + 2 / 20, 6);
  });

  it("makes each nudge one undo step", () => {
    renderBars();
    const bar = screen.getByRole("button", { name: "First" });

    fireEvent.keyDown(bar, { key: "ArrowRight" });
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    undo();
    expect(range("a")?.start).toBeCloseTo(1 + 1 / 20, 6);

    undo();
    expect(range("a")).toEqual({ start: 1, end: 3 });
  });
});

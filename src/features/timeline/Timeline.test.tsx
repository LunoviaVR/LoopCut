import "@/ui/test/env";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { registerDefaultCommands } from "@/commands/defaults";
import { clearCommandRegistry, runCommand } from "@/commands/registry";
import { buildSequencePlan } from "@/domain/sequence";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import { mediaImport } from "@/ipc/commands";
import { canUndo, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { Timeline } from "./Timeline";

/** The timeline's canvas area is 800 CSS pixels wide in these tests, so 1 s is 800 / 12.4 px. */
const AREA = {
  left: 0,
  top: 0,
  width: 800,
  height: 124,
  right: 800,
  bottom: 124,
  x: 0,
  y: 0,
  toJSON: () => ({}),
};
const PIXELS_PER_SECOND = 800 / 12.4;
const pointerAt = (seconds: number) => Math.round(seconds * PIXELS_PER_SECOND);

let media: ImportedMedia;

beforeEach(async () => {
  resetEditorState();
  registerDefaultCommands();
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(AREA as DOMRect);
  media = await mediaImport("C:\\Clips\\ranked-ace.mp4");
  const project = sampleProject();
  const clip = project.clips[0];
  if (!clip) throw new Error("clip");
  useEditorStore.getState().loadProject({ ...project, clips: [{ ...clip, path: media.path }] });
  const plan = buildSequencePlan(useEditorStore.getState().project ?? project);
  useEditorStore.getState().setSequencePlan(plan);
});

afterEach(() => {
  clearCommandRegistry();
  vi.restoreAllMocks();
});

const segments = () => useEditorStore.getState().project?.clips[0]?.segments ?? [];
const outPoint = () => screen.getByRole("slider", { name: "Out point, range 1" });
const inPoint = () => screen.getByRole("slider", { name: "In point, range 1" });

describe("Timeline handles", () => {
  it("names each in and out point and reports its timecode", () => {
    render(<Timeline media={media} />);

    expect(inPoint()).toHaveAttribute("aria-valuetext", "00:00.00");
    expect(outPoint()).toHaveAttribute("aria-valuetext", "00:06.00");
    expect(outPoint()).toHaveAttribute("tabindex", "0");
  });

  it("moves an out point one source frame with the arrow keys, and a second with Shift", () => {
    render(<Timeline media={media} />);
    const frame = 1 / 30;

    fireEvent.keyDown(outPoint(), { key: "ArrowLeft" });
    expect(segments()[0]?.end).toBeCloseTo(6 - frame, 6);

    fireEvent.keyDown(outPoint(), { key: "ArrowLeft", shiftKey: true });
    expect(segments()[0]?.end).toBeCloseTo(6 - frame - 1, 6);
  });

  it("makes each key step one undo step", () => {
    render(<Timeline media={media} />);
    fireEvent.keyDown(outPoint(), { key: "ArrowLeft" });
    fireEvent.keyDown(outPoint(), { key: "ArrowLeft" });

    undo();
    expect(segments()[0]?.end).toBeCloseTo(6 - 1 / 30, 6);
    undo();
    expect(segments()[0]?.end).toBe(6);
    expect(canUndo()).toBe(false);
  });

  it("drags an out point and records the drag as one undo step on release", () => {
    render(<Timeline media={media} />);
    const handle = outPoint();

    fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: pointerAt(6) });
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: pointerAt(4) });
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: pointerAt(4) });

    expect(segments()[0]?.end).toBeCloseTo(4, 2);
    undo();
    expect(segments()[0]?.end).toBe(6);
    expect(canUndo()).toBe(false);
  });

  it("writes the store once per release, however many moves the drag made", () => {
    render(<Timeline media={media} />);
    const handle = outPoint();
    let projectWrites = 0;
    const unsubscribe = useEditorStore.subscribe((state, previous) => {
      if (state.project !== previous.project) projectWrites += 1;
    });

    fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: pointerAt(6) });
    for (let step = 1; step <= 20; step += 1) {
      fireEvent.pointerMove(handle, { pointerId: 1, clientX: pointerAt(6 - step * 0.1) });
    }
    expect(projectWrites).toBe(0);

    fireEvent.pointerUp(handle, { pointerId: 1, clientX: pointerAt(4) });
    unsubscribe();

    expect(projectWrites).toBe(1);
    expect(segments()[0]?.end).toBeCloseTo(4, 2);
  });

  it("cancels a drag with Escape and leaves nothing to undo", () => {
    render(<Timeline media={media} />);
    const handle = outPoint();

    fireEvent.pointerDown(handle, { pointerId: 1, button: 0, clientX: pointerAt(6) });
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: pointerAt(4) });
    fireEvent.keyDown(handle, { key: "Escape" });
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: pointerAt(4) });

    expect(segments()[0]?.end).toBe(6);
    expect(canUndo()).toBe(false);
  });
});

describe("Timeline editing", () => {
  it("cuts at the playhead and removes the selected range, never the last one", () => {
    render(<Timeline media={media} />);
    useEditorStore.getState().setPlayhead(60);

    runCommand("timeline.split");
    expect(segments()).toHaveLength(2);

    useEditorStore.getState().setSelection({ kind: "segment", index: 1 });
    runCommand("timeline.deleteSegment");
    expect(segments()).toHaveLength(1);

    useEditorStore.getState().setSelection({ kind: "segment", index: 0 });
    runCommand("timeline.deleteSegment");
    expect(segments()).toHaveLength(1);
  });

  it("selects the kept range under a click on the clip row", () => {
    render(<Timeline media={media} />);
    const area = screen.getByRole("application", { name: /Timeline\./ });

    fireEvent.pointerDown(area, { pointerId: 2, button: 0, clientX: pointerAt(1.5), clientY: 56 });

    expect(useEditorStore.getState().selection).toEqual({ kind: "segment", index: 0 });
  });
});

describe("Timeline keyboard on handles", () => {
  /** Cuts the clip at 3 s, so the kept ranges are 0 to 3 s and 3 to 6 s. */
  const cutInHalf = () =>
    act(() => {
      useEditorStore.getState().setPlayhead(60);
      runCommand("timeline.split");
    });
  const inPointOf = (range: number) =>
    screen.getByRole("slider", { name: `In point, range ${range}` });
  const outPointOf = (range: number) =>
    screen.getByRole("slider", { name: `Out point, range ${range}` });

  it("selects the kept range of a handle when the handle takes focus", () => {
    render(<Timeline media={media} />);
    cutInHalf();

    act(() => inPointOf(2).focus());

    expect(useEditorStore.getState().selection).toEqual({ kind: "segment", index: 1 });
  });

  it("deletes the focused handle's range with Delete, as one undo step", () => {
    render(<Timeline media={media} />);
    cutInHalf();
    act(() => inPointOf(2).focus());

    fireEvent.keyDown(inPointOf(2), { key: "Delete" });

    expect(segments()).toHaveLength(1);
    expect(segments()[0]?.end).toBeCloseTo(3, 6);
    expect(useEditorStore.getState().selection).toEqual({ kind: "segment", index: 0 });

    undo();
    expect(segments()).toHaveLength(2);
    undo();
    expect(segments()).toHaveLength(1);
    expect(canUndo()).toBe(false);
  });

  it("deletes the range of a handle with Backspace too", () => {
    render(<Timeline media={media} />);
    cutInHalf();

    fireEvent.keyDown(inPointOf(1), { key: "Backspace" });

    expect(segments()).toHaveLength(1);
    expect(segments()[0]?.start).toBeCloseTo(3, 6);
    expect(useEditorStore.getState().selection).toEqual({ kind: "segment", index: 0 });
  });

  it("keeps the last kept range when Delete is pressed on its handle", () => {
    render(<Timeline media={media} />);

    fireEvent.keyDown(inPointOf(1), { key: "Delete" });

    expect(segments()).toHaveLength(1);
    expect(canUndo()).toBe(false);
  });

  it("keeps focus on the same edge when an earlier range is deleted", () => {
    render(<Timeline media={media} />);
    cutInHalf();
    act(() => outPointOf(1).focus());

    fireEvent.keyDown(outPointOf(1), { key: "Delete" });

    expect(segments()).toHaveLength(1);
    expect(document.activeElement).toBe(outPointOf(1));
    expect(document.activeElement).toHaveAttribute("aria-valuetext", "00:06.00");
  });

  it("moves focus to the same edge of the new last range when the last range is deleted", () => {
    render(<Timeline media={media} />);
    cutInHalf();
    act(() => outPointOf(2).focus());

    fireEvent.keyDown(outPointOf(2), { key: "Delete" });

    expect(segments()).toHaveLength(1);
    expect(document.activeElement).toBe(outPointOf(1));
  });
});

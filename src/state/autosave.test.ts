import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAutosave } from "@/state/autosave";
import { beginGesture, endGesture, resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";

beforeEach(() => {
  vi.useFakeTimers();
  resetEditorState();
});

afterEach(() => {
  vi.useRealTimers();
  Reflect.deleteProperty(document, "visibilityState");
});

const store = () => useEditorStore.getState();

describe("useAutosave", () => {
  it("saves once, with the latest project, after edits settle", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());

    store().setSpeed(2);
    vi.advanceTimersByTime(600);
    store().setSpeed(3);
    vi.advanceTimersByTime(600);
    expect(save).not.toHaveBeenCalled();

    vi.advanceTimersByTime(400);
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0]?.[0].timing.speed).toBe(3);
  });

  it("does not save a project that was only loaded", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    vi.advanceTimersByTime(5000);
    expect(save).not.toHaveBeenCalled();
  });

  it("holds the save while a gesture is open and starts the timer when it ends", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());

    beginGesture();
    store().setSpeed(2);
    vi.advanceTimersByTime(5000);
    expect(save).not.toHaveBeenCalled();

    endGesture();
    vi.advanceTimersByTime(999);
    expect(save).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0]?.[0].timing.speed).toBe(2);
  });

  it("flushes at once when the page becomes hidden, without a second save later", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);

    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "hidden",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    expect(save).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(5000);
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("does not flush when the page is only visible", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);

    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "visible",
    });
    document.dispatchEvent(new Event("visibilitychange"));
    expect(save).not.toHaveBeenCalled();
  });

  it("flushes pending work on beforeunload", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);

    window.dispatchEvent(new Event("beforeunload"));
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("saves pending work on unmount", () => {
    const save = vi.fn();
    const { unmount } = renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);

    unmount();
    expect(save).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(5000);
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("uses the latest save function after a re-render", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ fn }) => useAutosave(fn, 1000), {
      initialProps: { fn: first },
    });
    store().loadProject(sampleProject());
    rerender({ fn: second });

    store().setSpeed(2);
    vi.advanceTimersByTime(1000);
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("saves after an undo, since the restored project is a dirty edit", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);
    vi.advanceTimersByTime(1000);
    expect(save).toHaveBeenCalledTimes(1);

    undo();
    vi.advanceTimersByTime(1000);
    expect(save).toHaveBeenCalledTimes(2);
    expect(save.mock.calls[1]?.[0].timing.speed).toBe(1);
  });

  it("does nothing once the project is closed", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);
    resetEditorState();
    vi.advanceTimersByTime(1000);
    expect(save).not.toHaveBeenCalled();
  });

  it("cancels the pending save when the project is saved, since the file already holds it", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);

    store().markSaved();
    vi.advanceTimersByTime(5000);
    expect(save).not.toHaveBeenCalled();
  });

  it("does not flush a clean project on unload", () => {
    const save = vi.fn();
    renderHook(() => useAutosave(save, 1000));
    store().loadProject(sampleProject());
    store().setSpeed(2);
    store().markSaved();

    window.dispatchEvent(new Event("beforeunload"));
    expect(save).not.toHaveBeenCalled();
  });
});

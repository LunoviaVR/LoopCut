import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  editorCommands,
  registerDefaultCommands,
  resetCommandHandlers,
  setCommandHandlers,
} from "@/commands/defaults";
import { clearCommandRegistry, getShortcut, listCommands, runCommand } from "@/commands/registry";
import { useHotkeys } from "@/commands/useHotkeys";
import i18n from "@/i18n";
import type { Project } from "@/ipc/bindings/Project";
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";

const store = () => useEditorStore.getState();

function currentProject(): Project {
  const project = store().project;
  if (!project) throw new Error("expected an open project");
  return project;
}

/** A 20 fps plan over the 6 s kept range: output frame i shows source time i / 20. */
function planFor(frames: number): SequencePlan {
  return {
    fps: { num: 20, den: 1 },
    duration: frames / 20,
    frames: Array.from({ length: frames }, (_, i) => ({
      sample: { kind: "at" as const, t: i / 20 },
      delayCs: 5,
    })),
  };
}

function press(init: { key: string; code: string } & Partial<KeyboardEventInit>): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  document.body.dispatchEvent(event);
  return event;
}

const space = () => press({ key: " ", code: "Space" });
/** The `key` value a browser reports for a physical `code` on a US layout. */
function keyFor(code: string): string {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter?.[1]) return letter[1].toLowerCase();
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit?.[1]) return digit[1];
  return code;
}

const key = (code: string, extra: Partial<KeyboardEventInit> = {}) =>
  press({ key: keyFor(code), code, ...extra });

let disposeCommands: () => void;
const handlers = {
  open: vi.fn(),
  save: vi.fn(),
  export: vi.fn(),
  settings: vi.fn(),
};

beforeEach(() => {
  clearCommandRegistry();
  resetEditorState();
  resetCommandHandlers();
  setCommandHandlers(handlers);
  for (const handler of Object.values(handlers)) handler.mockClear();
  disposeCommands = registerDefaultCommands();
});

afterEach(() => {
  disposeCommands();
  document.body.innerHTML = "";
});

describe("default command table", () => {
  it("gives every command a unique id and a commands.* label key", () => {
    const ids = editorCommands.map((command) => command.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const command of editorCommands) {
      expect(command.labelKey.startsWith("commands.")).toBe(true);
    }
  });

  it("has a translated label for every command", () => {
    for (const command of editorCommands) {
      expect(i18n.exists(command.labelKey), command.labelKey).toBe(true);
    }
  });

  it("binds each shortcut to at most one command", () => {
    const keys = editorCommands.flatMap((command) => command.defaultKeys);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("registers every default command", () => {
    expect(listCommands()).toHaveLength(editorCommands.length);
    expect(getShortcut("edit.undo")).toBe("Ctrl+Z");
    expect(getShortcut("edit.redo")).toBe("Ctrl+Shift+Z");
    expect(getShortcut("tool.crop")).toBe("Ctrl+3");
    expect(getShortcut("timeline.zoomIn")).toBe("Ctrl+=");
    expect(getShortcut("view.inspector")).toBe("Ctrl+\\");
    expect(getShortcut("app.settings")).toBe("Ctrl+,");
  });
});

describe("playback", () => {
  it("toggles play with Space, and the store holds the transport", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setFrameCount(120);

    space();
    expect(store().isPlaying).toBe(true);
    expect(store().playRate).toBe(1);

    space();
    expect(store().isPlaying).toBe(false);
  });

  it("does nothing for Space without an open project or any frames", () => {
    renderHook(() => useHotkeys());
    const event = space();
    expect(store().isPlaying).toBe(false);
    expect(event.defaultPrevented).toBe(false);

    store().loadProject(sampleProject());
    space();
    expect(store().isPlaying).toBe(false);
  });

  it("shuttles with J, K and L, doubling speed up to 4x while moving the same way", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setFrameCount(120);

    key("KeyL");
    expect(store().playRate).toBe(1);
    key("KeyL");
    expect(store().playRate).toBe(2);
    key("KeyL");
    expect(store().playRate).toBe(4);
    key("KeyL");
    expect(store().playRate).toBe(4);

    key("KeyJ");
    expect(store().playRate).toBe(-1);
    expect(store().isPlaying).toBe(true);

    key("KeyK");
    expect(store().isPlaying).toBe(false);
    expect(store().playRate).toBe(1);
  });
});

describe("frame stepping", () => {
  it("steps by frame and by second, and jumps to the ends", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setSequencePlan(planFor(120));

    key("ArrowRight");
    expect(store().playhead).toBe(1);
    key("ArrowRight", { repeat: true });
    expect(store().playhead).toBe(2);
    key("ArrowLeft");
    expect(store().playhead).toBe(1);

    key("ArrowRight", { shiftKey: true });
    expect(store().playhead).toBe(21);
    key("ArrowLeft", { shiftKey: true });
    expect(store().playhead).toBe(1);

    key("End");
    expect(store().playhead).toBe(119);
    key("Home");
    expect(store().playhead).toBe(0);
  });

  it("leaves arrow keys alone until a plan is loaded", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    const event = key("ArrowRight");
    expect(store().playhead).toBe(0);
    expect(event.defaultPrevented).toBe(false);
  });
});

describe("marking and cutting", () => {
  it("sets the in point at the playhead with I and the out point with O", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setSequencePlan(planFor(120));

    store().setPlayhead(40);
    key("KeyI");
    expect(currentProject().clips[0]?.segments).toEqual([{ start: 2, end: 6 }]);

    store().setPlayhead(100);
    key("KeyO");
    expect(currentProject().clips[0]?.segments).toEqual([{ start: 2, end: 5 }]);
  });

  it("cuts at the playhead with S", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setSequencePlan(planFor(120));

    store().setPlayhead(60);
    key("KeyS");
    expect(currentProject().clips[0]?.segments).toEqual([
      { start: 0, end: 3 },
      { start: 3, end: 6 },
    ]);
  });

  it("does not mark when the playhead has no plan frame", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    const event = key("KeyI");
    expect(currentProject().clips[0]?.segments).toEqual([{ start: 0, end: 6 }]);
    expect(event.defaultPrevented).toBe(false);
  });
});

describe("undo and redo", () => {
  it("undoes and redoes with Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setSequencePlan(planFor(120));
    store().setPlayhead(60);
    key("KeyS");
    expect(currentProject().clips[0]?.segments).toHaveLength(2);

    key("KeyZ", { ctrlKey: true });
    expect(currentProject().clips[0]?.segments).toHaveLength(1);

    key("KeyZ", { ctrlKey: true, shiftKey: true });
    expect(currentProject().clips[0]?.segments).toHaveLength(2);

    key("KeyZ", { ctrlKey: true });
    key("KeyY", { ctrlKey: true });
    expect(currentProject().clips[0]?.segments).toHaveLength(2);
  });

  it("matches the undo and redo functions", () => {
    store().loadProject(sampleProject());
    store().setSpeed(2);
    expect(runCommand("edit.undo")).toBe(true);
    expect(store().project?.timing.speed).toBe(1);
    expect(runCommand("edit.redo")).toBe(true);
    expect(store().project?.timing.speed).toBe(2);
  });
});

describe("tools", () => {
  it("switches tools with Ctrl+1 to Ctrl+6, and Alt+C selects crop", () => {
    renderHook(() => useHotkeys());
    key("Digit3", { ctrlKey: true });
    expect(store().activeTool).toBe("crop");
    key("Digit6", { ctrlKey: true });
    expect(store().activeTool).toBe("export");
    key("Digit1", { ctrlKey: true });
    expect(store().activeTool).toBe("media");
    key("KeyC", { altKey: true });
    expect(store().activeTool).toBe("crop");
  });

  it("leaves plain digits alone, so typing numbers never switches tools", () => {
    renderHook(() => useHotkeys());
    key("Digit3");
    expect(store().activeTool).toBe("media");
  });

  it("choosing a tool shows the inspector, and Ctrl+\\ toggles it", () => {
    renderHook(() => useHotkeys());
    store().setInspectorOpen(false);
    key("Digit2", { ctrlKey: true });
    expect(store().inspectorOpen).toBe(true);

    key("Backslash", { ctrlKey: true });
    expect(store().inspectorOpen).toBe(false);
    key("Backslash", { ctrlKey: true });
    expect(store().inspectorOpen).toBe(true);
  });

  it("opens settings with Ctrl+,", () => {
    renderHook(() => useHotkeys());
    key("Comma", { ctrlKey: true });
    expect(handlers.settings).toHaveBeenCalledTimes(1);
  });
});

describe("timeline zoom", () => {
  it("zooms in and out by a fixed step with Ctrl+= and Ctrl+-, and fits with Ctrl+0", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());

    key("Equal", { ctrlKey: true });
    expect(store().timelineZoom).toBeCloseTo(1.25);
    key("Minus", { ctrlKey: true });
    expect(store().timelineZoom).toBeCloseTo(1);
    key("Equal", { ctrlKey: true });
    key("Digit0", { ctrlKey: true });
    expect(store().timelineZoom).toBe(1);
  });

  it("does nothing without an open project", () => {
    renderHook(() => useHotkeys());
    key("Equal", { ctrlKey: true });
    expect(store().timelineZoom).toBe(1);
  });
});

describe("stepping during playback", () => {
  it("pauses first, then moves the playhead", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    store().setSequencePlan(planFor(120));
    store().setTransport({ isPlaying: true, playRate: 1 });

    key("ArrowRight");
    expect(store().isPlaying).toBe(false);
    expect(store().playhead).toBe(1);
  });
});

describe("file actions", () => {
  it("opens without a project and saves only with one", () => {
    renderHook(() => useHotkeys());
    key("KeyO", { ctrlKey: true });
    expect(handlers.open).toHaveBeenCalledTimes(1);

    key("KeyS", { ctrlKey: true });
    expect(handlers.save).not.toHaveBeenCalled();

    store().loadProject(sampleProject());
    key("KeyS", { ctrlKey: true });
    expect(handlers.save).toHaveBeenCalledTimes(1);
  });

  it("exports with Ctrl+E unless an export is already running", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());

    key("KeyE", { ctrlKey: true });
    expect(handlers.export).toHaveBeenCalledTimes(1);

    store().setExporting(true);
    key("KeyE", { ctrlKey: true });
    expect(handlers.export).toHaveBeenCalledTimes(1);
  });

  it("leaves typing in a text field alone", () => {
    renderHook(() => useHotkeys());
    store().loadProject(sampleProject());
    const input = document.createElement("input");
    document.body.append(input);

    input.dispatchEvent(new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true }));
    expect(store().isPlaying).toBe(false);
  });
});

describe("setCommandHandlers", () => {
  it("replaces only the handlers it is given", () => {
    const openOverride = vi.fn();
    setCommandHandlers({ open: openOverride });
    renderHook(() => useHotkeys());

    key("KeyO", { ctrlKey: true });
    expect(openOverride).toHaveBeenCalledTimes(1);
    expect(handlers.open).not.toHaveBeenCalled();

    store().loadProject(sampleProject());
    key("KeyS", { ctrlKey: true });
    expect(handlers.save).toHaveBeenCalledTimes(1);
  });
});

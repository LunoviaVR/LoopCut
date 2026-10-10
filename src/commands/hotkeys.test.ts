import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type CommandDef, clearCommandRegistry, registerCommands } from "@/commands/registry";
import { isEditableTarget, shouldIgnoreHotkey, useHotkeys } from "@/commands/useHotkeys";

beforeEach(() => {
  clearCommandRegistry();
});

afterEach(() => {
  document.body.innerHTML = "";
});

function command(overrides: Partial<CommandDef> & { id: string }): CommandDef {
  return {
    labelKey: `commands.${overrides.id}`,
    group: "edit",
    defaultKeys: [],
    run: () => {},
    ...overrides,
  };
}

/** Dispatches a keydown from `target`; it bubbles to window, where tinykeys listens. */
function press(
  target: EventTarget,
  init: { key: string; code: string } & Partial<KeyboardEventInit>,
): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

describe("useHotkeys", () => {
  it("runs a bound command from the page and prevents the default action", () => {
    const run = vi.fn();
    registerCommands([command({ id: "a", defaultKeys: ["KeyA"], run })]);
    renderHook(() => useHotkeys());

    const event = press(document.body, { key: "a", code: "KeyA" });
    expect(run).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("ignores keys typed into inputs, textareas, selects and contenteditable regions", () => {
    const run = vi.fn();
    registerCommands([command({ id: "a", defaultKeys: ["Space"], run })]);
    renderHook(() => useHotkeys());

    const input = document.createElement("input");
    const textarea = document.createElement("textarea");
    const select = document.createElement("select");
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    const inner = document.createElement("span");
    editable.append(inner);
    document.body.append(input, textarea, select, editable);

    for (const target of [input, textarea, select, editable, inner]) {
      const event = press(target, { key: " ", code: "Space" });
      expect(event.defaultPrevented).toBe(false);
    }
    expect(run).not.toHaveBeenCalled();
  });

  it("still delivers Escape from a text field", () => {
    const run = vi.fn();
    registerCommands([command({ id: "esc", defaultKeys: ["Escape"], run })]);
    renderHook(() => useHotkeys());

    const input = document.createElement("input");
    document.body.append(input);
    press(input, { key: "Escape", code: "Escape" });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("skips a command whose when() is false and leaves the key alone", () => {
    const run = vi.fn();
    registerCommands([command({ id: "gated", defaultKeys: ["KeyG"], run, when: () => false })]);
    renderHook(() => useHotkeys());

    const event = press(document.body, { key: "g", code: "KeyG" });
    expect(run).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  });

  it("runs the first command in list order whose when() holds for a shared key", () => {
    const first = vi.fn();
    const second = vi.fn();
    registerCommands([
      command({ id: "first", defaultKeys: ["KeyT"], run: first, when: () => false }),
      command({ id: "second", defaultKeys: ["KeyT"], run: second }),
    ]);
    renderHook(() => useHotkeys());

    press(document.body, { key: "t", code: "KeyT" });
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("requires modifiers to match exactly", () => {
    const plain = vi.fn();
    const mod = vi.fn();
    registerCommands([
      command({ id: "plain", defaultKeys: ["KeyS"], run: plain }),
      command({ id: "mod", defaultKeys: ["$mod+KeyS"], run: mod }),
    ]);
    renderHook(() => useHotkeys());

    press(document.body, { key: "s", code: "KeyS", ctrlKey: true });
    expect(plain).not.toHaveBeenCalled();
    expect(mod).toHaveBeenCalledTimes(1);

    press(document.body, { key: "s", code: "KeyS", ctrlKey: true, shiftKey: true });
    expect(mod).toHaveBeenCalledTimes(1);

    press(document.body, { key: "s", code: "KeyS" });
    expect(plain).toHaveBeenCalledTimes(1);
  });

  it("delivers auto-repeat only to commands that opt in", () => {
    const once = vi.fn();
    const repeating = vi.fn();
    registerCommands([
      command({ id: "once", defaultKeys: ["Space"], run: once }),
      command({ id: "repeating", defaultKeys: ["ArrowLeft"], run: repeating, repeat: true }),
    ]);
    renderHook(() => useHotkeys());

    press(document.body, { key: " ", code: "Space", repeat: true });
    press(document.body, { key: "ArrowLeft", code: "ArrowLeft", repeat: true });
    expect(once).not.toHaveBeenCalled();
    expect(repeating).toHaveBeenCalledTimes(1);
  });

  it("ignores events the focused control already handled", () => {
    const run = vi.fn();
    registerCommands([command({ id: "step", defaultKeys: ["ArrowRight"], run })]);
    renderHook(() => useHotkeys());

    const slider = document.createElement("div");
    slider.setAttribute("role", "slider");
    slider.tabIndex = 0;
    slider.addEventListener("keydown", (event) => event.preventDefault());
    document.body.append(slider);

    press(slider, { key: "ArrowRight", code: "ArrowRight" });
    expect(run).not.toHaveBeenCalled();
  });

  it("binds commands registered after mounting", () => {
    const run = vi.fn();
    renderHook(() => useHotkeys());
    registerCommands([command({ id: "late", defaultKeys: ["KeyL"], run })]);

    press(document.body, { key: "l", code: "KeyL" });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("unbinds when unmounted", () => {
    const run = vi.fn();
    registerCommands([command({ id: "a", defaultKeys: ["KeyA"], run })]);
    const { unmount } = renderHook(() => useHotkeys());
    unmount();

    press(document.body, { key: "a", code: "KeyA" });
    expect(run).not.toHaveBeenCalled();
  });
});

describe("isEditableTarget and shouldIgnoreHotkey", () => {
  it("treats a contenteditable=false island as not editable", () => {
    const outer = document.createElement("div");
    outer.setAttribute("contenteditable", "true");
    const island = document.createElement("div");
    island.setAttribute("contenteditable", "false");
    outer.append(island);
    document.body.append(outer);

    expect(isEditableTarget(island)).toBe(false);
    expect(isEditableTarget(outer)).toBe(true);
    expect(isEditableTarget(document.body)).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
    expect(isEditableTarget(window)).toBe(false);
  });

  it("ignores composing input", () => {
    const event = new KeyboardEvent("keydown", { key: "a", code: "KeyA", isComposing: true });
    expect(shouldIgnoreHotkey(event)).toBe(true);
  });
});

describe("keys inside dialogs and focused controls", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("ignores every key pressed inside a dialog, so the editor stays idle behind it", () => {
    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    const inside = document.createElement("p");
    dialog.append(inside);
    document.body.append(dialog);

    const plain = new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true });
    inside.dispatchEvent(plain);
    expect(shouldIgnoreHotkey(plain)).toBe(true);

    const modified = new KeyboardEvent("keydown", {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
      bubbles: true,
    });
    inside.dispatchEvent(modified);
    expect(shouldIgnoreHotkey(modified)).toBe(true);
  });

  it("ignores plain keys on a focused button, but not modified shortcuts", () => {
    const button = document.createElement("button");
    document.body.append(button);

    const space = new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true });
    button.dispatchEvent(space);
    expect(shouldIgnoreHotkey(space)).toBe(true);

    const save = new KeyboardEvent("keydown", {
      key: "s",
      code: "KeyS",
      ctrlKey: true,
      bubbles: true,
    });
    button.dispatchEvent(save);
    expect(shouldIgnoreHotkey(save)).toBe(false);
  });

  it("leaves arrow keys to a focused tab", () => {
    const tab = document.createElement("button");
    tab.setAttribute("role", "tab");
    document.body.append(tab);

    const arrow = new KeyboardEvent("keydown", {
      key: "ArrowDown",
      code: "ArrowDown",
      bubbles: true,
    });
    tab.dispatchEvent(arrow);
    expect(shouldIgnoreHotkey(arrow)).toBe(true);
  });

  it("lets commands run from the page body and from a non-interactive element", () => {
    const panel = document.createElement("div");
    document.body.append(panel);

    const space = new KeyboardEvent("keydown", { key: " ", code: "Space", bubbles: true });
    panel.dispatchEvent(space);
    expect(shouldIgnoreHotkey(space)).toBe(false);
  });
});

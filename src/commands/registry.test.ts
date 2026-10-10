import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  type CommandDef,
  clearCommandRegistry,
  formatKeybinding,
  getCommand,
  getShortcut,
  listCommands,
  registerCommands,
  runCommand,
  subscribeCommands,
} from "@/commands/registry";

beforeEach(() => {
  clearCommandRegistry();
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

describe("runCommand", () => {
  it("runs a registered command by id", () => {
    const run = vi.fn();
    registerCommands([command({ id: "a.run", run })]);
    expect(runCommand("a.run")).toBe(true);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("returns false for an unknown id", () => {
    expect(runCommand("missing")).toBe(false);
  });

  it("skips a command whose when() is false", () => {
    const run = vi.fn();
    registerCommands([command({ id: "a.gated", run, when: () => false })]);
    expect(runCommand("a.gated")).toBe(false);
    expect(run).not.toHaveBeenCalled();
  });
});

describe("registerCommands", () => {
  it("replaces a command that has the same id", () => {
    const first = command({ id: "a.same" });
    const second = command({ id: "a.same", labelKey: "commands.second" });
    registerCommands([first]);
    registerCommands([second]);
    expect(getCommand("a.same")).toBe(second);
    expect(listCommands()).toHaveLength(1);
  });

  it("returns a disposer that removes only its own definitions", () => {
    const original = command({ id: "a.owned" });
    const dispose = registerCommands([original]);
    const replacement = command({ id: "a.owned" });
    registerCommands([replacement]);

    dispose();
    // The replacement was registered later, so disposing the original leaves it in place.
    expect(getCommand("a.owned")).toBe(replacement);

    const disposeReplacement = registerCommands([replacement]);
    disposeReplacement();
    expect(getCommand("a.owned")).toBeUndefined();
  });

  it("notifies subscribers on registration and removal", () => {
    const listener = vi.fn();
    const unsubscribe = subscribeCommands(listener);
    const dispose = registerCommands([command({ id: "a.notify" })]);
    expect(listener).toHaveBeenCalledTimes(1);
    dispose();
    expect(listener).toHaveBeenCalledTimes(2);

    unsubscribe();
    registerCommands([command({ id: "a.quiet" })]);
    expect(listener).toHaveBeenCalledTimes(2);
  });
});

describe("getShortcut and formatKeybinding", () => {
  it("formats the first default shortcut for display", () => {
    registerCommands([
      command({ id: "edit.undo", defaultKeys: ["$mod+KeyZ"] }),
      command({ id: "edit.redo", defaultKeys: ["$mod+Shift+KeyZ", "$mod+KeyY"] }),
      command({ id: "playback.toggle", defaultKeys: ["Space"] }),
      command({ id: "timeline.secondBack", defaultKeys: ["Shift+ArrowLeft"] }),
      command({ id: "tool.crop", defaultKeys: ["Digit3", "Alt+KeyC"] }),
      command({ id: "file.none", defaultKeys: [] }),
    ]);
    expect(getShortcut("edit.undo")).toBe("Ctrl+Z");
    expect(getShortcut("edit.redo")).toBe("Ctrl+Shift+Z");
    expect(getShortcut("playback.toggle")).toBe("Space");
    expect(getShortcut("timeline.secondBack")).toBe("Shift+Left");
    expect(getShortcut("tool.crop")).toBe("3");
    expect(getShortcut("file.none")).toBeNull();
    expect(getShortcut("missing")).toBeNull();
  });

  it("labels modifiers and keys", () => {
    expect(formatKeybinding("Alt+KeyC")).toBe("Alt+C");
    expect(formatKeybinding("$mod+Shift+Digit7")).toBe("Ctrl+Shift+7");
    expect(formatKeybinding("[Shift]+Home")).toBe("Shift+Home");
    expect(formatKeybinding("Escape")).toBe("Esc");
    expect(formatKeybinding("Control+KeyK")).toBe("Ctrl+K");
  });
});

/**
 * Command registry: every user action the editor exposes, with its default shortcuts and its
 * availability. Hotkeys (`useHotkeys`) and menus or buttons both run commands from here.
 */

export type CommandGroup = "file" | "edit" | "playback" | "timeline" | "tools";

export interface CommandDef {
  /** Stable identifier, e.g. `playback.toggle`. */
  id: string;
  /** i18n key for the label, resolved with react-i18next `t()`. */
  labelKey: string;
  group: CommandGroup;
  /** Shortcuts in tinykeys syntax, e.g. `$mod+KeyZ`, `Space`, `Shift+ArrowLeft`. The first is displayed. */
  defaultKeys: string[];
  run: () => void;
  /** When this returns false the command is unavailable: `runCommand` skips it and its key falls through. */
  when?: () => boolean;
  /** Whether the command also runs for auto-repeated key presses (holding an arrow key). Default false. */
  repeat?: boolean;
}

const commands = new Map<string, CommandDef>();
const listeners = new Set<() => void>();
let version = 0;

function notify(): void {
  version += 1;
  for (const listener of listeners) listener();
}

/**
 * Registers commands. A command with an existing id replaces it, so re-registering after hot reload is
 * safe. Returns a disposer that removes exactly the definitions registered by this call.
 */
export function registerCommands(list: readonly CommandDef[]): () => void {
  const registered = [...list];
  for (const command of registered) commands.set(command.id, command);
  notify();
  return () => {
    for (const command of registered) {
      if (commands.get(command.id) === command) commands.delete(command.id);
    }
    notify();
  };
}

export function getCommand(id: string): CommandDef | undefined {
  return commands.get(id);
}

export function listCommands(): CommandDef[] {
  return [...commands.values()];
}

/**
 * Runs a command by id. Returns false when the id is unknown or its `when()` is false, in which case
 * nothing runs.
 */
export function runCommand(id: string): boolean {
  const command = commands.get(id);
  if (!command) return false;
  if (command.when && !command.when()) return false;
  command.run();
  return true;
}

/** The first default shortcut of a command, formatted for display (`Ctrl+Z`), or null when it has none. */
export function getShortcut(id: string): string | null {
  const key = commands.get(id)?.defaultKeys[0];
  return key ? formatKeybinding(key) : null;
}

const MODIFIER_LABELS: Record<string, string> = {
  $mod: "Ctrl",
  Control: "Ctrl",
  Alt: "Alt",
  Shift: "Shift",
  Meta: "Win",
};

const KEY_LABELS: Record<string, string> = {
  Space: "Space",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  ArrowUp: "Up",
  ArrowDown: "Down",
  Escape: "Esc",
  Delete: "Del",
  Enter: "Enter",
  Home: "Home",
  End: "End",
  Tab: "Tab",
  Backspace: "Backspace",
  Minus: "-",
  Equal: "=",
  Comma: ",",
  Backslash: "\\",
  NumpadAdd: "Num +",
  NumpadSubtract: "Num -",
};

function keyLabel(key: string): string {
  const letter = /^Key([A-Z])$/.exec(key);
  if (letter?.[1]) return letter[1];
  const digit = /^Digit([0-9])$/.exec(key);
  if (digit?.[1]) return digit[1];
  return KEY_LABELS[key] ?? key;
}

/**
 * Human label for a tinykeys binding: `$mod+Shift+KeyZ` becomes `Ctrl+Shift+Z`. Optional modifiers
 * written in `[brackets]` are shown like required ones. Labels are Windows-style.
 */
export function formatKeybinding(binding: string): string {
  const parts = binding
    .trim()
    .split("+")
    .map((part) => part.replace(/^\[(.*)\]$/, "$1"));
  const key = parts.pop() ?? "";
  const modifiers = parts.map((part) => MODIFIER_LABELS[part] ?? part);
  return [...modifiers, keyLabel(key)].join("+");
}

/** Subscribes to registry changes (registration and removal). Returns an unsubscribe function. */
export function subscribeCommands(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Changes whenever commands are registered or removed. */
export function getCommandsVersion(): number {
  return version;
}

/** Removes every command. Intended for tests. */
export function clearCommandRegistry(): void {
  commands.clear();
  notify();
}

import { useEffect } from "react";
import { type KeybindingsMap, tinykeys } from "tinykeys";
import { type CommandDef, listCommands, subscribeCommands } from "@/commands/registry";

/** True when the event target is a text field, where typing must keep its normal meaning. */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target.matches("input, textarea, select")) return true;
  const editable = target.closest("[contenteditable]");
  return editable !== null && editable.getAttribute("contenteditable")?.toLowerCase() !== "false";
}

/**
 * Elements that keep their own keyboard behaviour. Unmodified keys (Space, arrows, Home) do not reach commands
 * while one of these has focus, so a focused button still activates on Space and a focused tab takes the arrows.
 */
const INTERACTIVE_SELECTOR = [
  "input",
  "textarea",
  "select",
  "button",
  "a[href]",
  "[contenteditable='true']",
  "[role='tab']",
  "[role='tablist']",
  "[role='slider']",
  "[role='switch']",
  "[role='combobox']",
  "[role='listbox']",
  "[role='option']",
  "[role='radio']",
  "[role='menuitem']",
  "[role='menu']",
].join(", ");

/** True when the target is a control that handles its own keys (see INTERACTIVE_SELECTOR). */
export function isInteractiveTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return target.closest(INTERACTIVE_SELECTOR) !== null;
}

/** True when the target is inside a modal dialog. Editor shortcuts stay off while a dialog is open. */
export function isInsideDialog(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return target.closest("[role='dialog'], [role='alertdialog']") !== null;
}

/**
 * Keys that never reach commands: IME composition, keys already handled by a focused control or a
 * prevented default, keys pressed inside a dialog, and typing in a text field. Escape is the one key that
 * still reaches commands from a text field. Modified shortcuts (Ctrl+S) still reach commands from a focused
 * button; plain keys do not.
 */
export function shouldIgnoreHotkey(event: KeyboardEvent): boolean {
  if (event.isComposing || event.defaultPrevented) return true;
  if (isInsideDialog(event.target)) return true;
  if (isEditableTarget(event.target)) return event.key !== "Escape";
  const modified = event.ctrlKey || event.metaKey || event.altKey;
  return !modified && isInteractiveTarget(event.target);
}

/**
 * Builds the tinykeys map for a command list. Commands sharing a shortcut are tried in list order: the
 * first one whose `when()` holds runs. Keys that run nothing are left alone (no preventDefault).
 */
export function buildKeybindingMap(list: readonly CommandDef[]): KeybindingsMap {
  const byKey = new Map<string, CommandDef[]>();
  for (const command of list) {
    for (const key of command.defaultKeys) {
      const bucket = byKey.get(key);
      if (bucket) bucket.push(command);
      else byKey.set(key, [command]);
    }
  }

  const map: KeybindingsMap = {};
  for (const [key, bucket] of byKey) {
    map[key] = (event) => {
      for (const command of bucket) {
        if (event.repeat && !command.repeat) continue;
        if (command.when && !command.when()) continue;
        event.preventDefault();
        command.run();
        return;
      }
    };
  }
  return map;
}

/**
 * Binds every registered command to the window while the component is mounted. The bindings follow
 * registry changes, so commands registered later also get their keys.
 */
export function useHotkeys(): void {
  useEffect(() => {
    const bind = () =>
      tinykeys(window, buildKeybindingMap(listCommands()), { ignore: shouldIgnoreHotkey });

    let unbind = bind();
    const unsubscribe = subscribeCommands(() => {
      unbind();
      unbind = bind();
    });

    return () => {
      unsubscribe();
      unbind();
    };
  }, []);
}

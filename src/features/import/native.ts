/**
 * Native dialogs and links. Inside Tauri they use the plugins; in the browser build they fall back to the
 * synthetic backend (or do nothing), so development works without the desktop shell.
 */
import { isTauri } from "@tauri-apps/api/core";
import i18n from "@/i18n";
import { pickVideoFile } from "@/ipc/commands";
import { PROJECT_EXTENSION, VIDEO_EXTENSIONS } from "./formats";

function openFilters() {
  return [
    { name: i18n.t("import.filterVideo"), extensions: [...VIDEO_EXTENSIONS] },
    { name: i18n.t("import.filterProject"), extensions: [PROJECT_EXTENSION] },
  ];
}

/** "Open" dialog for one video or one .loopcut project. Resolves null when cancelled. */
export async function pickOpenPath(): Promise<string | null> {
  if (!isTauri()) return pickVideoFile();
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({ multiple: false, directory: false, filters: openFilters() });
  return typeof selected === "string" ? selected : null;
}

/** "Save" dialog for a .loopcut file. Resolves null when cancelled. */
export async function pickSavePath(suggestedName: string): Promise<string | null> {
  if (!isTauri()) return suggestedName;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const selected = await save({
    defaultPath: suggestedName,
    filters: [{ name: i18n.t("import.filterProject"), extensions: [PROJECT_EXTENSION] }],
  });
  return selected ?? null;
}

/** Folder picker. Callers check `isTauri()` first; resolves null when cancelled. */
export async function pickFolder(): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({ directory: true, multiple: false });
  return typeof selected === "string" ? selected : null;
}

/** Opens a link in the default browser. Resolves false when the desktop app refused it. */
export async function openExternal(url: string): Promise<boolean> {
  if (!isTauri()) {
    window.open(url, "_blank", "noopener,noreferrer");
    return true;
  }
  try {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
    return true;
  } catch {
    return false;
  }
}

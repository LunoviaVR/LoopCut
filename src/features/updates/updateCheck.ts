/**
 * Update checks. The desktop app asks the signed release feed (plugin-updater); the browser build only answers
 * when `?fakeUpdate=1` is in the URL, with a simulated release for development. Every check records its time
 * and honours the skipped version. Errors come back as results, never as thrown exceptions.
 */
import { isTauri } from "@tauri-apps/api/core";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { messageOf } from "@/features/import/errors";
import { useSettingsStore } from "@/features/settings/settingsStore";
import i18n from "@/i18n";
import { useUpdateStore } from "./updateStore";

/** First automatic check, after the window has settled. */
export const STARTUP_CHECK_DELAY_MS = 5_000;
/** Automatic checks repeat at this interval while the app runs. */
export const CHECK_INTERVAL_MS = 4 * 60 * 60 * 1000;

/** The part of an update the UI and the install flow use. Built from a plugin `Update` or the dev fake. */
export interface AvailableUpdate {
  version: string;
  currentVersion: string;
  /** Release notes as the server sent them. Plain text: rendered without HTML. */
  body: string | null;
  date: string | null;
  downloadAndInstall: (onEvent: (event: DownloadEvent) => void) => Promise<void>;
  close: () => Promise<void>;
}

export type CheckResult =
  | { kind: "unavailable" }
  | { kind: "current" }
  | { kind: "available"; update: AvailableUpdate }
  | { kind: "error"; message: string };

export function fakeUpdateRequested(): boolean {
  return (
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).get("fakeUpdate") === "1"
  );
}

function fromPlugin(update: Update): AvailableUpdate {
  return {
    version: update.version,
    currentVersion: update.currentVersion,
    body: update.body ?? null,
    date: update.date ?? null,
    downloadAndInstall: (onEvent) => update.downloadAndInstall(onEvent),
    close: () => update.close(),
  };
}

/** A simulated release for the browser build. Its download only moves a progress bar. */
function developmentUpdate(): AvailableUpdate {
  return {
    version: "0.0.2",
    currentVersion: "0.0.1",
    body: i18n.t("updates.fakeNotes"),
    date: null,
    downloadAndInstall: async (onEvent) => {
      onEvent({ event: "Started", data: { contentLength: 1000 } });
      for (let step = 0; step < 4; step += 1) {
        await new Promise<void>((resolve) => setTimeout(resolve, 200));
        onEvent({ event: "Progress", data: { chunkLength: 250 } });
      }
      onEvent({ event: "Finished" });
    },
    close: async () => {},
  };
}

/** Asks the release feed once. Does not touch the store or the settings. */
export async function checkForUpdate(): Promise<CheckResult> {
  if (!isTauri()) {
    return fakeUpdateRequested()
      ? { kind: "available", update: developmentUpdate() }
      : { kind: "unavailable" };
  }
  try {
    const update = await check();
    if (!update) return { kind: "current" };
    return { kind: "available", update: fromPlugin(update) };
  } catch (error) {
    return { kind: "error", message: messageOf(error) };
  }
}

/** Whether `version` is the one the user chose to skip. */
export function isSkipped(version: string, skippedVersion: string | null): boolean {
  return skippedVersion !== null && skippedVersion === version;
}

/**
 * Runs a check and publishes the outcome: a new version that is not skipped goes to the banner, and a
 * successful check (current or available) records its time in the settings.
 */
export async function runUpdateCheck(): Promise<CheckResult> {
  const result = await checkForUpdate();
  const settings = useSettingsStore.getState();
  const skipped = settings.settings?.updates.skippedVersion ?? null;

  if (result.kind === "available") {
    if (!isSkipped(result.update.version, skipped)) {
      useUpdateStore.getState().setAvailable(result.update);
    }
  } else if (result.kind === "current") {
    useUpdateStore.getState().setAvailable(null);
  }

  if (result.kind === "available" || result.kind === "current") {
    void settings.update((draft) => {
      draft.updates.lastCheck = new Date().toISOString();
    });
  }
  return result;
}

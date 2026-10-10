/**
 * Installs the available update: refuses during an export, saves the project when it has unsaved changes,
 * stops the FFmpeg jobs, downloads and installs the package, then restarts. Progress and failure go to the
 * update store, which the banner shows.
 *
 * One install runs at a time. A request while one is in flight gets the same promise, so a double click never
 * downloads twice. The phase moves to "preparing" before the first await, which holds the banner's buttons.
 */
import { isTauri } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { messageOf } from "@/features/import/errors";
import { saveProject } from "@/features/project/saveProject";
import i18n from "@/i18n";
import { appPrepareUpdate, appResumeBackground } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { toast } from "@/ui";
import type { AvailableUpdate } from "./updateCheck";
import { useUpdateStore } from "./updateStore";

/** The install that is running, or null. Cleared when it settles, whatever its outcome. */
let inFlight: Promise<void> | null = null;

/** Starts the install of the available update, or joins the one already running. */
export function installAvailableUpdate(): Promise<void> {
  if (inFlight) return inFlight;
  const store = useUpdateStore.getState();
  const update = store.available;
  if (!update) return Promise.resolve();

  if (useEditorStore.getState().isExporting) {
    refuseDuringExport();
    return Promise.resolve();
  }

  // Set synchronously, before any await, so a second click sees the install already started.
  store.setPhase("preparing");
  inFlight = runInstall(update).finally(() => {
    inFlight = null;
  });
  return inFlight;
}

function refuseDuringExport(): void {
  toast({
    variant: "warn",
    title: i18n.t("updates.exportRunning"),
    description: i18n.t("updates.exportRunningBody"),
  });
}

async function runInstall(update: AvailableUpdate): Promise<void> {
  const store = useUpdateStore;
  // Set once the backend has closed its job registry. A failure after that point must reopen it.
  let prepared = false;
  try {
    if (useEditorStore.getState().dirty) {
      const outcome = await saveProject();
      if (outcome !== "saved" && outcome !== "unchanged") {
        if (outcome === "cancelled") toast({ variant: "info", title: i18n.t("updates.saveFirst") });
        store.getState().setPhase("idle");
        return;
      }
    }

    // The save may have waited on a dialog. An export that started meanwhile must still block the install.
    if (useEditorStore.getState().isExporting) {
      store.getState().setPhase("idle");
      refuseDuringExport();
      return;
    }

    await appPrepareUpdate();
    prepared = true;
    store.getState().setPhase("downloading", { progress: 0 });

    let total: number | null = null;
    let downloaded = 0;
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? null;
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        const fraction = total ? Math.min(1, downloaded / total) : null;
        store.getState().setPhase("downloading", { progress: fraction });
      }
    });

    if (!isTauri()) {
      // The browser build only simulates the download: say so and leave the banner usable.
      store.getState().setPhase("idle");
      toast({ variant: "info", title: i18n.t("updates.simulatedInstall") });
      return;
    }
    store.getState().setPhase("restarting", { progress: 1 });
    await restartAfterInstall();
  } catch (error) {
    if (prepared) await resumeBackground();
    store.getState().setPhase("failed", { error: messageOf(error) });
    toast({
      variant: "error",
      title: i18n.t("updates.installFailed"),
      description: i18n.t("updates.installFailedBody"),
      duration: 12000,
    });
  }
}

/**
 * Reopens the backend's job registry after a failed install, so previews, exports and proxies work again. A failure
 * to reopen is not shown on its own: the install failure is the message the user needs, and the app restarts cleanly.
 */
async function resumeBackground(): Promise<void> {
  try {
    await appResumeBackground();
  } catch {
    // Still closed: the install failure below is what the user sees.
  }
}

/**
 * Restarts into the new version. The package is already installed by now, so a failed restart must not read as
 * a failed install: it asks for a manual restart instead.
 */
async function restartAfterInstall(): Promise<void> {
  try {
    await relaunch();
  } catch {
    const message = i18n.t("updates.installedRestart");
    useUpdateStore.getState().setPhase("failed", { error: message });
    toast({ variant: "warn", title: message, duration: 12000 });
  }
}

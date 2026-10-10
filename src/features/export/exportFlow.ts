/**
 * The export flow: ask where to save, start the job, follow its events, and report the outcome.
 *
 * The flow owns the `isExporting` flag of the editor store, which the update installer and the file opener read. It is
 * set when the job starts and cleared on done, error and cancel, and also when the job never starts.
 *
 * Toasts: one per hardware fallback, one when the file is done (with "Show in folder"), one on failure or cancel.
 */

import { formatBytes } from "@/domain/bytes";
import { messageOf } from "@/features/import/errors";
import { useSettingsStore } from "@/features/settings/settingsStore";
import i18n from "@/i18n";
import type { ExportEvent } from "@/ipc/bindings/ExportEvent";
import { exportCancel, exportStart, pickExportPath, revealInFolder } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { toast } from "@/ui";
import { extensionOf, folderOf, joinPath, suggestedExportName } from "./exportFormat";
import { initialExportJob, isExportActive, useExportStore } from "./exportStore";

/**
 * True while a save dialog is open. A second start while one is open must not open another dialog or start a second job.
 */
let dialogOpen = false;

/** The filter name of the save dialog for each extension. */
function filterNameOf(extension: string): string {
  return i18n.t(`export.filter.${extension}`);
}

/**
 * Starts an export of the open project. Asks for the file first (in the default export folder when one is set); a
 * cancelled dialog changes nothing. Resolves when the job has started, or when the flow ended before that.
 */
export async function startExport(): Promise<void> {
  const editor = useEditorStore.getState();
  const project = editor.project;
  if (!project || editor.isExporting || isExportActive()) return;

  const format = project.export.format;
  const extension = extensionOf(format);
  const directory = useSettingsStore.getState().settings?.general.defaultExportDir ?? null;
  const suggested = joinPath(directory, suggestedExportName(project.name, format));

  if (dialogOpen) return;
  dialogOpen = true;
  let path: string | null;
  try {
    path = await pickExportPath(suggested, extension, filterNameOf(extension));
  } catch (failure) {
    toast({
      variant: "error",
      title: i18n.t("export.failedTitle"),
      description: messageOf(failure),
      duration: 10000,
    });
    return;
  } finally {
    dialogOpen = false;
  }
  if (!path) return;
  rememberExportFolder(path);

  // The dialog may have been open for a while: a second start in the meantime must not run a second job.
  if (useEditorStore.getState().isExporting || isExportActive()) return;

  useEditorStore.getState().setExporting(true);
  useExportStore.setState({
    ...initialExportJob(),
    status: "running",
    path,
    format,
    startedAt: Date.now(),
  });

  let jobId: string;
  try {
    jobId = await exportStart(project, path, handleExportEvent);
  } catch (failure) {
    finishExport("failed", { error: messageOf(failure) });
    toast({
      variant: "error",
      title: i18n.t("export.failedTitle"),
      description: messageOf(failure),
      duration: 10000,
    });
    return;
  }

  useExportStore.setState({ jobId });
  if (useExportStore.getState().cancelRequested) {
    await cancelJob(jobId);
  }
}

/**
 * Makes the folder of a chosen path the default for the next export, so the save dialog opens where the last export
 * went. A path with no folder part changes nothing.
 */
function rememberExportFolder(path: string): void {
  const folder = folderOf(path);
  if (!folder) return;
  const settings = useSettingsStore.getState();
  if (settings.settings?.general.defaultExportDir === folder) return;
  settings
    .update((draft) => {
      draft.general.defaultExportDir = folder;
    })
    .catch((failure: unknown) => {
      toast({
        variant: "warn",
        title: i18n.t("export.folderNotRemembered"),
        description: messageOf(failure),
      });
    });
}

/**
 * Cancels the running job. Before the job id is known the request waits for it. The job then ends with a `cancelled`
 * error event, which closes the flow.
 */
export async function cancelExport(): Promise<void> {
  const state = useExportStore.getState();
  if (state.status !== "running") return;
  useExportStore.setState({ cancelRequested: true, status: "cancelling" });
  if (state.jobId) await cancelJob(state.jobId);
}

async function cancelJob(jobId: string): Promise<void> {
  try {
    await exportCancel(jobId);
  } catch (failure) {
    toast({
      variant: "error",
      title: i18n.t("export.failedTitle"),
      description: messageOf(failure),
      duration: 10000,
    });
  }
}

/** Applies one event of the job to the job state, the editor flag and the toasts. */
export function handleExportEvent(event: ExportEvent): void {
  switch (event.kind) {
    case "progress":
      useExportStore.setState({
        framesDone: event.framesDone,
        framesTotal: event.framesTotal,
        bytes: event.bytes,
      });
      return;
    case "fallback": {
      useExportStore.setState((state) => ({ fallbacks: state.fallbacks + 1 }));
      // The GIF is always written on the CPU, so a GPU dithering fallback is not a failed hardware encode.
      let title: string;
      if (event.from === "gpu-dither") title = i18n.t("export.fallbackDither");
      else if (event.stage === "encode") title = i18n.t("export.fallbackEncode");
      else title = i18n.t("export.fallbackDecode");
      toast({ variant: "warn", title, description: event.reason });
      return;
    }
    case "targetPass":
      useExportStore.setState((state) => ({
        passes: [
          ...state.passes,
          { pass: event.pass, settings: event.settings, bytes: event.bytes },
        ],
      }));
      return;
    case "done": {
      const path = event.path;
      finishExport("done", {
        result: { path, bytes: event.bytes, seconds: event.seconds, frames: event.frames },
        framesDone: event.frames,
        framesTotal: event.frames,
        bytes: event.bytes,
      });
      toast({
        variant: "success",
        title: i18n.t("export.done", { size: formatBytes(event.bytes) }),
        description: path,
        duration: 8000,
        action: {
          label: i18n.t("export.showInFolder"),
          onAction: () => {
            revealInFolder(path).catch((failure: unknown) => {
              toast({
                variant: "error",
                title: i18n.t("export.revealFailed"),
                description: messageOf(failure),
              });
            });
          },
        },
      });
      return;
    }
    case "error": {
      if (event.error.code === "cancelled") {
        finishExport("cancelled", {});
        toast({ variant: "info", title: i18n.t("export.cancelled") });
        return;
      }
      const { message, detail } = event.error;
      finishExport("failed", { error: message, errorDetail: detail ?? null });
      toast({
        variant: "error",
        title: i18n.t("export.failedTitle"),
        description: failureText(message, detail),
        duration: 12000,
      });
      return;
    }
  }
}

/** A failure as one line of toast text: the message, then the technical detail when the backend gave one. */
export function failureText(message: string, detail?: string | null): string {
  return detail ? `${message} ${detail}` : message;
}

/** Ends the job: the status, the editor flag, and any final fields. */
function finishExport(
  status: "done" | "failed" | "cancelled",
  patch: Partial<ReturnType<typeof useExportStore.getState>>,
): void {
  useEditorStore.getState().setExporting(false);
  useExportStore.setState({ ...patch, status, jobId: useExportStore.getState().jobId });
}

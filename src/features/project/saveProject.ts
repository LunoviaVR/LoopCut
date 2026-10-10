/**
 * Writes the open project to its `.loopcut` file (Ctrl+S, Save as, and the save step before an update or a
 * replacement). The first save asks for a location. Failures are reported as toasts and returned as
 * "failed", so callers can stop the action that needed the save.
 */

import { messageOf } from "@/features/import/errors";
import {
  ensureProjectExtension,
  fileNameOf,
  suggestedProjectFileName,
} from "@/features/import/formats";
import { pickSavePath } from "@/features/import/native";
import i18n from "@/i18n";
import { projectRecoveryClear, projectSave } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { toast } from "@/ui";

/**
 * "saved": the file was written. "unchanged": there is no project to save. "cancelled": the user closed the
 * save dialog. "failed": the write failed (a toast explains why).
 */
export type SaveOutcome = "saved" | "unchanged" | "cancelled" | "failed";

export async function saveProject(options: { saveAs?: boolean } = {}): Promise<SaveOutcome> {
  const { project, projectPath } = useEditorStore.getState();
  if (!project) return "unchanged";

  let path = projectPath;
  if (!path || options.saveAs) {
    let picked: string | null;
    try {
      picked = await pickSavePath(suggestedProjectFileName(project.name));
    } catch (error) {
      toast({
        variant: "error",
        title: i18n.t("project.saveFailed"),
        description: messageOf(error),
        duration: 12000,
      });
      return "failed";
    }
    if (!picked) return "cancelled";
    path = ensureProjectExtension(picked);
  }

  try {
    await projectSave(path, project);
  } catch (error) {
    toast({
      variant: "error",
      title: i18n.t("project.saveFailed"),
      description: `${i18n.t("project.saveFailedBody")} (${messageOf(error)})`,
      duration: 12000,
    });
    return "failed";
  }

  const state = useEditorStore.getState();
  state.setProjectPath(path);
  // Only a save of the project that is still open may clear its recovery copy. Edits made while the write was
  // running keep the project dirty, and their recovery snapshot has to survive the save.
  if (state.project === project) {
    state.markSaved();
    void projectRecoveryClear().catch(() => {});
  }
  toast({
    variant: "success",
    title: i18n.t("project.saved", { name: fileNameOf(path) }),
    duration: 2500,
  });
  return "saved";
}

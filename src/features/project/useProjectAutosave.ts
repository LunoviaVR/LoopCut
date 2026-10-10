import { projectAutosave } from "@/ipc/commands";
import { useAutosave } from "@/state/autosave";

/**
 * Keeps the recovery slot current while the project is edited (about one second after the last edit). A failed
 * write is logged, not shown: the next edit tries again, and a toast on every failure would be noise.
 */
export function useProjectAutosave(): void {
  useAutosave((project) =>
    projectAutosave(project).catch((error: unknown) => {
      console.warn("LoopCut autosave failed:", error);
    }),
  );
}

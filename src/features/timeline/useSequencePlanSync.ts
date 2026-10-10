import { useEffect } from "react";
import { buildSequencePlan } from "@/domain/sequence";
import { sequencePlan } from "@/ipc/commands";
import { useEditorStore, useProject } from "@/state/editor";

/** Quiet time after the last edit before the plan is rebuilt. */
export const SEQUENCE_DEBOUNCE_MS = 150;

/**
 * Keeps the store's sequence plan in step with the project. 150 ms after the last edit the plan is rebuilt (the
 * timeline and the playhead read it), and the backend's `sequence_plan` summary is fetched to confirm the frame
 * count. A mismatch is logged; the local plan stays, because it carries the per-frame source times.
 */
export function useSequencePlanSync(): void {
  const project = useProject();

  useEffect(() => {
    if (!project) {
      useEditorStore.getState().setSequencePlan(null);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      const plan = buildSequencePlan(project);
      useEditorStore.getState().setSequencePlan(plan);
      if (!plan) return;
      sequencePlan(project).then(
        (summary) => {
          if (!cancelled && summary.frames !== plan.frames.length) {
            console.warn(
              `Sequence plan mismatch: ${plan.frames.length} frames in the UI, ${summary.frames} from the backend.`,
            );
          }
        },
        () => undefined,
      );
    }, SEQUENCE_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [project]);
}

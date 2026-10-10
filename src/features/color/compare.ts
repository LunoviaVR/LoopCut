/**
 * Renders the ungraded frame for the before side of the comparison. The preview backend holds one project, so the
 * grade-off copy is set, rendered and the current project restored inside one queued task: no other preview request
 * (the monitor's own frames included) can run in between and see the wrong grade.
 *
 * Latest wins: a request that a newer one has overtaken draws nothing when its turn comes, so a scrub queues at most
 * one before render that actually reaches the backend. The superseded request resolves null.
 */
import { enqueuePreviewTask } from "@/features/preview/backendQueue";
import type { Viewport } from "@/ipc/bindings/Viewport";
import { previewFrame, previewSetProject } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";

/** Sequence number of the newest before-render request. */
let newestRequest = 0;

/**
 * The output frame `frame` with the grade bypassed, as the wire format `previewFrame` returns, or null when a newer
 * request replaced this one before it ran.
 */
export function renderUngradedFrame(
  frame: number,
  viewport: Viewport,
): Promise<ArrayBuffer | null> {
  const request = ++newestRequest;
  return enqueuePreviewTask(async () => {
    if (request !== newestRequest) return null;
    const project = useEditorStore.getState().project;
    if (!project) throw new Error("No project is open");
    await previewSetProject({ ...project, grade: { ...project.grade, enabled: false } }, viewport);
    try {
      return await previewFrame(frame, "output");
    } finally {
      // Restore the project as it is now, which may already differ from the one this task started with.
      const current = useEditorStore.getState().project;
      if (current) await previewSetProject(current, viewport);
    }
  });
}

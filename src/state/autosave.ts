import { useEffect, useRef } from "react";
import type { Project } from "@/ipc/bindings/Project";
import { useEditorStore } from "@/state/editor";

export const DEFAULT_AUTOSAVE_DELAY_MS = 1000;

/**
 * Saves the project a little after the last edit (the recovery slot, not the `.loopcut` file).
 *
 * - Only edits to a dirty project schedule a save, so opening a project does not write one.
 * - A project that turns clean (saved, or loaded) cancels the pending save: the `.loopcut` file already holds it.
 * - Nothing is scheduled while a gesture (a drag) is open. When the gesture ends, the debounce starts.
 * - Pending work is flushed at once on `beforeunload`, when the page is hidden, and on unmount, but only while
 *   the project is still dirty.
 *
 * `saveFn` may return a promise. Its errors are left to the caller, which owns the error reporting.
 */
export function useAutosave(
  saveFn: (project: Project) => unknown,
  delayMs: number = DEFAULT_AUTOSAVE_DELAY_MS,
): void {
  const saveRef = useRef(saveFn);

  useEffect(() => {
    saveRef.current = saveFn;
  }, [saveFn]);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pending = false;

    const cancel = () => {
      if (timer !== undefined) {
        clearTimeout(timer);
        timer = undefined;
      }
    };

    const flush = () => {
      cancel();
      if (!pending) return;
      pending = false;
      const { project, dirty } = useEditorStore.getState();
      // A project that is clean is already in its file: writing it to the recovery slot would only add noise.
      if (project && dirty) void saveRef.current(project);
    };

    const schedule = () => {
      cancel();
      timer = setTimeout(flush, delayMs);
    };

    const unsubscribe = useEditorStore.subscribe((state, prev) => {
      if (!state.dirty) {
        pending = false;
        cancel();
        return;
      }
      const edited = state.project !== prev.project;
      const gestureEnded = prev.gestureDepth > 0 && state.gestureDepth === 0;
      if (edited) pending = true;
      if (!pending || (!edited && !gestureEnded)) return;
      if (state.gestureDepth > 0) {
        cancel();
        return;
      }
      schedule();
    });

    const onVisibilityChange = () => {
      if (document.visibilityState === "hidden") flush();
    };

    window.addEventListener("beforeunload", flush);
    document.addEventListener("visibilitychange", onVisibilityChange);

    return () => {
      unsubscribe();
      window.removeEventListener("beforeunload", flush);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      flush();
    };
  }, [delayMs]);
}

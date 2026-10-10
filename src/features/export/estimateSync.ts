/**
 * The live size estimate of the open project. `exportEstimate` encodes sample windows with the real encoder, so it runs
 * after the project has been still for a while (600 ms), and only the newest request may write the result: a slow
 * estimate that finishes after a newer request started is dropped by its sequence number.
 */
import { useEffect } from "react";
import { create } from "zustand";
import { messageOf } from "@/features/import/errors";
import type { Project } from "@/ipc/bindings/Project";
import { exportEstimate } from "@/ipc/commands";
import type { SizeEstimate } from "@/ipc/types";
import { useProject } from "@/state/editor";

export const ESTIMATE_DEBOUNCE_MS = 600;

export interface EstimateState {
  /** The newest estimate that succeeded. Kept while a newer one is pending, so the summary does not blank out. */
  estimate: SizeEstimate | null;
  /** Why the newest estimate failed, or null. */
  error: string | null;
  /** True between a project change and its result. */
  pending: boolean;
}

export interface EstimateScheduler {
  /** Asks for an estimate of `project` after the debounce. A newer call replaces the waiting one. */
  schedule: (project: Project) => void;
  /** Drops the waiting request and makes any running one stale. */
  cancel: () => void;
}

export interface EstimateSchedulerOptions {
  estimate: (project: Project) => Promise<SizeEstimate>;
  onState: (state: EstimateState) => void;
  delayMs?: number;
}

/** Debounce and stale-result guard for estimates. Pure apart from the timers it sets. */
export function createEstimateScheduler(options: EstimateSchedulerOptions): EstimateScheduler {
  const delay = options.delayMs ?? ESTIMATE_DEBOUNCE_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let sequence = 0;
  let shown: EstimateState = { estimate: null, error: null, pending: false };

  const publish = (next: EstimateState) => {
    shown = next;
    options.onState(next);
  };

  return {
    schedule(project) {
      sequence += 1;
      const mine = sequence;
      if (timer !== null) clearTimeout(timer);
      publish({ ...shown, pending: true });
      timer = setTimeout(() => {
        timer = null;
        options.estimate(project).then(
          (estimate) => {
            if (mine !== sequence) return;
            publish({ estimate, error: null, pending: false });
          },
          (failure: unknown) => {
            if (mine !== sequence) return;
            publish({ estimate: null, error: messageOf(failure), pending: false });
          },
        );
      }, delay);
    },
    cancel() {
      sequence += 1;
      if (timer !== null) clearTimeout(timer);
      timer = null;
      publish({ ...shown, pending: false });
    },
  };
}

export const useEstimateStore = create<EstimateState>()(() => ({
  estimate: null,
  error: null,
  pending: false,
}));

const scheduler = createEstimateScheduler({
  estimate: (project) => exportEstimate(project),
  onState: (state) => useEstimateStore.setState(state),
});

/** Estimates the project whenever it changes. Mounted once, by the editor shell. */
export function useEstimateSync(): void {
  const project = useProject();
  useEffect(() => {
    if (!project) {
      scheduler.cancel();
      useEstimateStore.setState({ estimate: null, error: null, pending: false });
      return;
    }
    scheduler.schedule(project);
  }, [project]);
  useEffect(() => () => scheduler.cancel(), []);
}

/** Asks for a fresh estimate of `project` after the debounce, for example after a failed one (the Try again button). */
export function requestEstimate(project: Project): void {
  scheduler.schedule(project);
}

/** Clears the shared estimate. Intended for tests. */
export function resetEstimateState(): void {
  scheduler.cancel();
  useEstimateStore.setState({ estimate: null, error: null, pending: false });
}

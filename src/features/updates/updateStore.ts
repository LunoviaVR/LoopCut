import { create } from "zustand";
import type { AvailableUpdate } from "./updateCheck";

export type InstallPhase = "idle" | "preparing" | "downloading" | "restarting" | "failed";

export interface UpdateState {
  /** The newest release the last check found, or null. */
  available: AvailableUpdate | null;
  /** Set by "Later": the banner hides until the next check finds a release. */
  dismissed: boolean;
  phase: InstallPhase;
  /** Download progress from 0 to 1, or null while the size is unknown. */
  progress: number | null;
  error: string | null;
  setAvailable: (update: AvailableUpdate | null) => void;
  dismiss: () => void;
  setPhase: (
    phase: InstallPhase,
    patch?: { progress?: number | null; error?: string | null },
  ) => void;
}

const INSTALLING: readonly InstallPhase[] = ["preparing", "downloading", "restarting"];

export const useUpdateStore = create<UpdateState>()((set, get) => ({
  available: null,
  dismissed: false,
  phase: "idle",
  progress: null,
  error: null,

  setAvailable: (update) => {
    // A check must not pull the release out from under an install that is already running.
    if (INSTALLING.includes(get().phase)) return;
    const previous = get().available;
    if (previous && previous !== update) void previous.close().catch(() => {});
    set({ available: update, dismissed: false, phase: "idle", progress: null, error: null });
  },

  dismiss: () => {
    set({ dismissed: true });
  },

  setPhase: (phase, patch = {}) => {
    set({
      phase,
      progress:
        "progress" in patch
          ? (patch.progress ?? null)
          : phase === "downloading"
            ? get().progress
            : null,
      error: "error" in patch ? (patch.error ?? null) : null,
    });
  },
}));

/** Resets the update state. Intended for tests. */
export function resetUpdateStore(): void {
  useUpdateStore.setState({
    available: null,
    dismissed: false,
    phase: "idle",
    progress: null,
    error: null,
  });
}

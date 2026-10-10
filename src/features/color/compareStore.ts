/**
 * Before and after: the monitor shows the frame with the grade bypassed on one side of a split. The state is UI only
 * (not in the project, not undoable): whether the split is on, and where the divider sits.
 */
import { create } from "zustand";

export interface CompareState {
  enabled: boolean;
  /** Divider position, 0 (all after) to 1 (all before). The before side is on the left. */
  split: number;
  setEnabled: (enabled: boolean) => void;
  setSplit: (split: number) => void;
}

export const useCompareStore = create<CompareState>()((set) => ({
  enabled: false,
  split: 0.5,
  setEnabled: (enabled) => set({ enabled }),
  setSplit: (split) => set({ split: Math.min(1, Math.max(0, split)) }),
}));

/** Restores the initial state. Intended for tests. */
export function resetCompareState(): void {
  useCompareStore.setState({ enabled: false, split: 0.5 });
}

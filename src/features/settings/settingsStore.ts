/**
 * App settings and hardware capabilities. Every change goes through `update()`, which applies it at once and
 * writes the whole settings object with `settings_set`. Writes run one after another, so the backend always
 * ends with the latest copy. A failed write rolls back to the last copy the backend accepted.
 */
import { useEffect, useRef } from "react";
import { create } from "zustand";
import { messageOf } from "@/features/import/errors";
import i18n from "@/i18n";
import type { AppSettings } from "@/ipc/bindings/AppSettings";
import type { Capabilities } from "@/ipc/bindings/Capabilities";
import { hwCapabilities, settingsGet, settingsSet } from "@/ipc/commands";
import { toast } from "@/ui";

export interface SettingsState {
  /** The current settings, or null until the first load completes. */
  settings: AppSettings | null;
  loadError: string | null;
  /** Last probe results (cached by the backend, refreshed by the hardware test). */
  capabilities: Capabilities | null;
  load: () => Promise<void>;
  refreshCapabilities: () => Promise<void>;
  setCapabilities: (capabilities: Capabilities | null) => void;
  /**
   * Applies `mutate` to a copy of the settings. With `persist: false` only the in-memory copy changes, which
   * is what a live drag uses; the final value is then saved with a normal update.
   */
  update: (mutate: (draft: AppSettings) => void, options?: { persist?: boolean }) => Promise<void>;
}

/** The last settings the backend accepted. Rolled back to after a failed write. */
let persisted: AppSettings | null = null;
/** Serializes writes so they reach the backend in the order they were made. */
let writeChain: Promise<void> = Promise.resolve();

function cloneSettings(settings: AppSettings): AppSettings {
  return JSON.parse(JSON.stringify(settings)) as AppSettings;
}

export const useSettingsStore = create<SettingsState>()((set, get) => ({
  settings: null,
  loadError: null,
  capabilities: null,

  load: async () => {
    try {
      const loaded = await settingsGet();
      persisted = loaded;
      set({ settings: loaded, loadError: null });
    } catch (error) {
      set({ loadError: messageOf(error) });
    }
  },

  refreshCapabilities: async () => {
    try {
      set({ capabilities: await hwCapabilities() });
    } catch {
      set({ capabilities: null });
    }
  },

  setCapabilities: (capabilities) => {
    set({ capabilities });
  },

  update: async (mutate, options) => {
    const current = get().settings;
    if (!current) return;
    const next = cloneSettings(current);
    mutate(next);
    set({ settings: next });
    if (options?.persist === false) return;

    const job = writeChain.then(() => settingsSet(next));
    writeChain = job.catch(() => {});
    try {
      await job;
      persisted = next;
    } catch (error) {
      // Only roll back when nothing newer replaced the copy this write was for.
      if (get().settings === next && persisted) set({ settings: persisted });
      toast({
        variant: "error",
        title: i18n.t("settings.saveFailed"),
        description: messageOf(error),
        duration: 10000,
      });
    }
  },
}));

/** Loads the settings once per mounted owner. Safe under React's double mount. */
export function useLoadSettings(): void {
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void useSettingsStore.getState().load();
  }, []);
}

/** Resets the store to its initial state. Intended for tests. */
export function resetSettingsStore(): void {
  persisted = null;
  writeChain = Promise.resolve();
  useSettingsStore.setState({ settings: null, loadError: null, capabilities: null });
}

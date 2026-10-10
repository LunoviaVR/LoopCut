import { isTauri } from "@tauri-apps/api/core";
import { useEffect } from "react";
import { useSettingsStore } from "@/features/settings/settingsStore";
import {
  CHECK_INTERVAL_MS,
  fakeUpdateRequested,
  runUpdateCheck,
  STARTUP_CHECK_DELAY_MS,
} from "./updateCheck";

/**
 * Automatic update checks: shortly after launch, then every few hours. Runs only when the setting is on, never
 * in a development server (the browser build opts in with `?fakeUpdate=1` instead), and never before the
 * settings have loaded.
 */
export function useUpdateCheck(): void {
  const checkOnStartup = useSettingsStore((state) => state.settings?.updates.checkOnStartup);

  useEffect(() => {
    if (checkOnStartup !== true) return;
    const enabled = isTauri() ? !import.meta.env.DEV : fakeUpdateRequested();
    if (!enabled) return;

    const automatic = () => {
      void runUpdateCheck().then((result) => {
        // Automatic checks stay quiet on failure: the next one retries, and the Updates tab reports errors.
        if (result.kind === "error") console.warn("LoopCut update check failed:", result.message);
      });
    };
    const first = window.setTimeout(automatic, STARTUP_CHECK_DELAY_MS);
    const repeat = window.setInterval(automatic, CHECK_INTERVAL_MS);
    return () => {
      window.clearTimeout(first);
      window.clearInterval(repeat);
    };
  }, [checkOnStartup]);
}

/**
 * The platform presets, loaded once per session from the backend's built-in table. A failed load is not cached, so the
 * next render asks again.
 */
import { useEffect, useState } from "react";
import { messageOf } from "@/features/import/errors";
import { rationalFromFps } from "@/features/trim/trimMath";
import type { PlatformPreset } from "@/ipc/bindings/PlatformPreset";
import { exportPresets } from "@/ipc/commands";
import { useEditorStore, withGesture } from "@/state/editor";
import { presetPlan } from "./exportFormat";

/**
 * Applies a platform preset to the open project as one undo step: the format, the preset id, the target size (the
 * preset's upload limit, or none), the width and the frame rate the preset recommends.
 */
export function applyPlatformPreset(preset: PlatformPreset): void {
  const plan = presetPlan(preset);
  const store = useEditorStore.getState();
  withGesture(() => {
    store.setExport({ format: plan.format, preset: plan.presetId, targetBytes: plan.targetBytes });
    store.setOutputSize({ mode: "width", width: plan.width });
    store.setOutputFps(rationalFromFps(plan.fps));
  });
}

let request: Promise<PlatformPreset[]> | null = null;

export function loadPlatformPresets(): Promise<PlatformPreset[]> {
  if (!request) {
    request = exportPresets().catch((failure: unknown) => {
      request = null;
      throw failure;
    });
  }
  return request;
}

/** Forgets the cached list. Intended for tests. */
export function resetPlatformPresets(): void {
  request = null;
}

export interface PlatformPresetsState {
  presets: PlatformPreset[] | null;
  error: string | null;
}

/** The presets, or the reason they could not be loaded. */
export function usePlatformPresets(): PlatformPresetsState {
  const [state, setState] = useState<PlatformPresetsState>({ presets: null, error: null });
  useEffect(() => {
    let cancelled = false;
    loadPlatformPresets().then(
      (presets) => {
        if (!cancelled) setState({ presets, error: null });
      },
      (failure: unknown) => {
        if (!cancelled) setState({ presets: null, error: messageOf(failure) });
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);
  return state;
}

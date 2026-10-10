/**
 * The preview backend's session for the open project: `previewSetProject` whenever the project or the monitor
 * bed changes (debounced), the frame count mirrored into the store, and decode progress polled until the
 * decoder is warm. A hardware fallback is announced once per clip. A failed session can be retried.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import { previewSetProject, previewStatus } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { toast } from "@/ui";
import { enqueuePreviewTask } from "./backendQueue";

/** Quiet time after the last project or bed change before the backend is updated. */
export const PREVIEW_DEBOUNCE_MS = 50;
/** How often decode progress is read while the decoder is still warming up. */
export const STATUS_POLL_MS = 500;
/** Viewport used until the bed has been measured. */
const FALLBACK_BED = { width: 640, height: 360 };

export interface PreviewSession {
  info: PreviewInfo | null;
  /** True when the backend could not draw the project. */
  failed: boolean;
  /** The backend's reason for the failure, when it gave one. */
  reason: string | null;
  /** Share of the source decoded so far, 0 to 1. 1 when there is nothing to wait for. */
  decodeProgress: number;
  /** Clears the failure and asks the backend for the project again. */
  retry: () => void;
}

interface Failure {
  reason: string | null;
}

export function usePreviewSession(
  project: Project | null,
  bed: { width: number; height: number },
  mediaToken: string | null,
  onPreviewChange: (info: PreviewInfo | null) => void,
): PreviewSession {
  const { t } = useTranslation();
  const [info, setInfo] = useState<PreviewInfo | null>(null);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [decodeProgress, setDecodeProgress] = useState(1);
  const announcedFor = useRef<string | null>(null);
  const bedWidth = bed.width;
  const bedHeight = bed.height;

  // Hand the project and the bed to the backend. A project change pauses playback first. A retry bumps `attempt`,
  // which sends the same project again.
  // biome-ignore lint/correctness/useExhaustiveDependencies: a retry is the same request, made again
  useEffect(() => {
    const state = useEditorStore.getState();
    if (state.isPlaying) state.setTransport({ isPlaying: false, playRate: 1 });
    if (!project) {
      setInfo(null);
      setFailure(null);
      onPreviewChange(null);
      state.setFrameCount(0);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      enqueuePreviewTask(() =>
        previewSetProject(project, {
          width: bedWidth || FALLBACK_BED.width,
          height: bedHeight || FALLBACK_BED.height,
          dpr: window.devicePixelRatio || 1,
        }),
      ).then(
        (next) => {
          if (cancelled) return;
          setInfo(next);
          setFailure(null);
          onPreviewChange(next);
          useEditorStore.getState().setFrameCount(next.frames);
        },
        (error: unknown) => {
          if (cancelled) return;
          setInfo(null);
          setFailure({ reason: messageOf(error) });
          onPreviewChange(null);
          const editor = useEditorStore.getState();
          editor.setFrameCount(0);
          editor.setTransport({ isPlaying: false, playRate: 1 });
        },
      );
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [project, bedWidth, bedHeight, onPreviewChange, attempt]);

  // A new clip starts its decode progress from zero. Later edits keep the last shown value until the next status answer,
  // so the bar does not flash back to zero on every change to the project.
  // biome-ignore lint/correctness/useExhaustiveDependencies: a new clip, and only a new clip, restarts the bar
  useEffect(() => {
    setDecodeProgress(0);
  }, [mediaToken]);

  // Poll decode progress while it is below 1, and announce hardware fallbacks once per clip.
  useEffect(() => {
    if (!info) return;
    let cancelled = false;
    let timer: number | undefined;
    const poll = () => {
      previewStatus().then(
        (status) => {
          if (cancelled) return;
          setDecodeProgress(status.decodeProgress);
          const clip = mediaToken ?? "";
          if (status.fallbacks.length > 0 && announcedFor.current !== clip) {
            announcedFor.current = clip;
            toast({ variant: "warn", title: t("preview.decodeFallback") });
          }
          if (status.decodeProgress < 1) timer = window.setTimeout(poll, STATUS_POLL_MS);
        },
        () => {
          if (!cancelled) setDecodeProgress(1);
        },
      );
    };
    poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [info, mediaToken, t]);

  const retry = useCallback(() => {
    setFailure(null);
    setAttempt((count) => count + 1);
  }, []);

  return {
    info,
    failed: failure !== null,
    reason: failure?.reason ?? null,
    decodeProgress,
    retry,
  };
}

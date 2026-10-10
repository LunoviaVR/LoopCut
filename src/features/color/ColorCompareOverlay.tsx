import { type KeyboardEvent, type PointerEvent, useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { drawInto } from "@/features/preview/usePlayback";
import type { Project } from "@/ipc/bindings/Project";
import type { Viewport } from "@/ipc/bindings/Viewport";
import { cn } from "@/lib/cn";
import { useEditorStore } from "@/state/editor";
import { renderUngradedFrame } from "./compare";
import { useCompareStore } from "./compareStore";

const KEY_STEP = 0.02;
const KEY_STEP_FAST = 0.1;

export interface ColorCompareOverlayProps {
  project: Project;
  /** The viewport the monitor gives the preview backend; the ungraded frame must use the same one. */
  viewport: Viewport;
}

/**
 * Before and after over the monitor frame. While paused, the ungraded frame is drawn on the left of a draggable
 * divider and the graded frame (the monitor's own canvas, underneath) on the right. Playing shows a hint instead.
 * The ungraded frame is refetched when the frame or anything except the grade changes, so dragging a grade control
 * does not render the before side again.
 *
 * The split is a child: it is the only part that follows the playhead and the project, so nothing of it runs while
 * the comparison is off or playing.
 */
export function ColorCompareOverlay({ project, viewport }: ColorCompareOverlayProps) {
  const { t } = useTranslation();
  const enabled = useCompareStore((state) => state.enabled);
  const playing = useEditorStore((state) => state.isPlaying);

  if (!enabled) return null;

  if (playing) {
    return (
      <div className="pointer-events-none absolute inset-0 flex items-start justify-center p-2">
        <span className="rounded-control bg-app/80 px-2 py-1 text-xs text-fg">
          {t("color.compare.pauseHint")}
        </span>
      </div>
    );
  }

  return <BeforeAfter project={project} viewport={viewport} />;
}

/** The split view while paused: the ungraded frame on the left of the divider, the monitor's own frame on the right. */
function BeforeAfter({ project, viewport }: ColorCompareOverlayProps) {
  const { t } = useTranslation();
  const split = useCompareStore((state) => state.split);
  const playhead = useEditorStore((state) => state.playhead);
  const canvas = useRef<HTMLCanvasElement>(null);
  const area = useRef<HTMLDivElement>(null);
  const { width, height, dpr } = viewport;
  // Everything but the grade decides the ungraded frame, so the key changes with the project's geometry and content,
  // and not when a grade control moves.
  const structure = useMemo(() => JSON.stringify({ ...project, grade: null }), [project]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the effect reads the store (renderUngradedFrame), not the prop. `structure` is listed so a change to anything but the grade refetches the before frame.
  useEffect(() => {
    let cancelled = false;
    renderUngradedFrame(playhead, { width, height, dpr }).then(
      (buffer) => {
        const element = canvas.current;
        // A null buffer is a request a newer one overtook: the newer request draws instead.
        if (cancelled || !element || !buffer) return;
        drawInto(element, buffer);
      },
      () => {
        // A failed before frame leaves the after frame alone; the monitor reports preview failures itself.
      },
    );
    return () => {
      cancelled = true;
    };
  }, [playhead, structure, width, height, dpr]);

  const setSplitFromPointer = (clientX: number) => {
    const rect = area.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return;
    useCompareStore.getState().setSplit((clientX - rect.left) / rect.width);
  };

  const onHandleDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    event.currentTarget.focus({ preventScroll: true });
    setSplitFromPointer(event.clientX);
  };

  const onHandleMove = (event: PointerEvent<HTMLDivElement>) => {
    if (!event.currentTarget.hasPointerCapture?.(event.pointerId)) return;
    setSplitFromPointer(event.clientX);
  };

  const onHandleKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? KEY_STEP_FAST : KEY_STEP;
    const current = useCompareStore.getState().split;
    if (event.key === "ArrowLeft") useCompareStore.getState().setSplit(current - step);
    else if (event.key === "ArrowRight") useCompareStore.getState().setSplit(current + step);
    else return;
    event.preventDefault();
  };

  const percent = `${Math.round(split * 100)}%`;
  return (
    <div ref={area} className="pointer-events-none absolute inset-0 overflow-hidden">
      <canvas
        ref={canvas}
        role="img"
        aria-label={t("color.compare.beforeLabel")}
        className="absolute inset-0 h-full w-full"
        style={{ clipPath: `inset(0 ${100 - split * 100}% 0 0)` }}
      />
      <span className="absolute top-2 left-2 rounded-control bg-app/70 px-1.5 py-0.5 text-xs text-fg">
        {t("color.compare.before")}
      </span>
      <span className="absolute top-2 right-2 rounded-control bg-app/70 px-1.5 py-0.5 text-xs text-fg">
        {t("color.compare.after")}
      </span>
      <div
        role="slider"
        tabIndex={0}
        aria-label={t("color.compare.split")}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(split * 100)}
        aria-valuetext={t("color.compare.splitValue", { percent: Math.round(split * 100) })}
        onPointerDown={onHandleDown}
        onPointerMove={onHandleMove}
        onKeyDown={onHandleKey}
        className={cn(
          "pointer-events-auto absolute inset-y-0 flex w-6 -translate-x-1/2 cursor-ew-resize touch-none justify-center outline-none",
          "focus-visible:outline-2 focus-visible:outline-accent",
        )}
        style={{ left: percent }}
      >
        <span aria-hidden="true" className="h-full w-px bg-fg" />
      </div>
    </div>
  );
}

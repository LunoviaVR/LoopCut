/**
 * The crop overlay on the uncropped frame: a dimmed surround, the crop frame with eight handles, and thirds guides
 * while a drag runs. A drag keeps its rectangle in a ref and paints the DOM once per animation frame; the store is
 * written once, when the drag ends, inside one gesture, so the whole drag is one undo step. Escape cancels a drag and restores
 * the start rectangle, Enter ends it where it is. Arrow keys nudge the crop by one display pixel (Shift: ten).
 *
 * The overlay covers exactly the frame it sits on (the monitor sizes its wrapper to the frame), so normalized
 * pointer positions map straight onto the stored crop.
 */
import { type KeyboardEvent, type PointerEvent, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { NormRect } from "@/ipc/bindings/NormRect";
import type { Project } from "@/ipc/bindings/Project";
import { cn } from "@/lib/cn";
import { useEditorStore, withGesture } from "@/state/editor";
import { EditGesture } from "@/state/gesture";
import {
  aspectValue,
  type CropHandle,
  displaySize,
  moveRect,
  nudgeRect,
  resizeRect,
  thirdsLines,
} from "./cropMath";

export interface CropOverlayProps {
  media: ImportedMedia;
  project: Project;
}

interface Drag {
  /** "move" for the body, or the handle being dragged. */
  kind: "move" | CropHandle;
  pointerId: number;
  start: { x: number; y: number };
  startRect: NormRect;
  gesture: EditGesture;
  draft: NormRect;
}

/** Handle positions as fractions of the frame, and the cursor each one shows. */
const HANDLE_LAYOUT: Record<
  CropHandle,
  { cursor: string; at: (r: NormRect) => { x: number; y: number } }
> = {
  nw: { cursor: "cursor-nwse-resize", at: (r) => ({ x: r.x, y: r.y }) },
  n: { cursor: "cursor-ns-resize", at: (r) => ({ x: r.x + r.w / 2, y: r.y }) },
  ne: { cursor: "cursor-nesw-resize", at: (r) => ({ x: r.x + r.w, y: r.y }) },
  e: { cursor: "cursor-ew-resize", at: (r) => ({ x: r.x + r.w, y: r.y + r.h / 2 }) },
  se: { cursor: "cursor-nwse-resize", at: (r) => ({ x: r.x + r.w, y: r.y + r.h }) },
  s: { cursor: "cursor-ns-resize", at: (r) => ({ x: r.x + r.w / 2, y: r.y + r.h }) },
  sw: { cursor: "cursor-nesw-resize", at: (r) => ({ x: r.x, y: r.y + r.h }) },
  w: { cursor: "cursor-ew-resize", at: (r) => ({ x: r.x, y: r.y + r.h / 2 }) },
};

const HANDLE_ORDER: readonly CropHandle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

/** Percent-positioned box from a normalized rectangle. */
function boxStyle(rect: NormRect) {
  return {
    left: `${rect.x * 100}%`,
    top: `${rect.y * 100}%`,
    width: `${rect.w * 100}%`,
    height: `${rect.h * 100}%`,
  };
}

export function CropOverlay({ media, project }: CropOverlayProps) {
  const { t } = useTranslation();
  const rootRef = useRef<HTMLDivElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const dimRefs = useRef<Record<"top" | "bottom" | "left" | "right", HTMLDivElement | null>>({
    top: null,
    bottom: null,
    left: null,
    right: null,
  });
  const handleRefs = useRef<Partial<Record<CropHandle, HTMLDivElement | null>>>({});
  const guideRefs = useRef<(HTMLSpanElement | null)[]>([]);
  const drag = useRef<Drag | null>(null);
  const frame = useRef<number | null>(null);
  /** Whether a drag runs. Changes once at each end of a drag, never per pointer move. */
  const [dragging, setDragging] = useState(false);

  const stored = project.transform.crop;
  const size = displaySize(media.info.video, project.transform.rotation);
  const aspect = aspectValue(project.transform.cropAspect);

  /** Writes a rectangle into the overlay's DOM. Runs once per animation frame during a drag, and once after it. */
  const paint = (rect: NormRect) => {
    const box = boxRef.current;
    if (box) Object.assign(box.style, boxStyle(rect));
    const dims = dimRefs.current;
    if (dims.top) dims.top.style.height = `${rect.y * 100}%`;
    if (dims.bottom) dims.bottom.style.height = `${(1 - rect.y - rect.h) * 100}%`;
    if (dims.left) {
      Object.assign(dims.left.style, {
        top: `${rect.y * 100}%`,
        height: `${rect.h * 100}%`,
        width: `${rect.x * 100}%`,
      });
    }
    if (dims.right) {
      Object.assign(dims.right.style, {
        top: `${rect.y * 100}%`,
        height: `${rect.h * 100}%`,
        width: `${(1 - rect.x - rect.w) * 100}%`,
      });
    }
    for (const handle of HANDLE_ORDER) {
      const element = handleRefs.current[handle];
      if (!element) continue;
      const spot = HANDLE_LAYOUT[handle].at(rect);
      element.style.left = `${spot.x * 100}%`;
      element.style.top = `${spot.y * 100}%`;
    }
    const lines = thirdsLines(rect);
    const guides = guideRefs.current;
    lines.vertical.forEach((position, index) => {
      const guide = guides[index];
      if (guide) guide.style.left = `${((position - rect.x) / rect.w) * 100}%`;
    });
    lines.horizontal.forEach((position, index) => {
      const guide = guides[2 + index];
      if (guide) guide.style.top = `${((position - rect.y) / rect.h) * 100}%`;
    });
  };

  // A drag still open when the overlay goes away is closed, so the undo gesture never stays open.
  useEffect(() => {
    return () => {
      if (frame.current !== null) window.cancelAnimationFrame(frame.current);
      const open = drag.current;
      drag.current = null;
      open?.gesture.end();
    };
  }, []);

  const pointerOf = (event: { clientX: number; clientY: number }) => {
    const bounds = rootRef.current?.getBoundingClientRect();
    if (!bounds || bounds.width <= 0 || bounds.height <= 0) return { x: 0, y: 0 };
    return {
      x: (event.clientX - bounds.left) / bounds.width,
      y: (event.clientY - bounds.top) / bounds.height,
    };
  };

  /**
   * Recomputes the draft from the pointer and paints it on the next frame. The store is not touched. The first move
   * opens the undo gesture, so a press that never moves records nothing.
   */
  const scheduleDraft = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    current.gesture.begin();
    const pointer = pointerOf(event);
    const dx = pointer.x - current.start.x;
    const dy = pointer.y - current.start.y;
    current.draft =
      current.kind === "move"
        ? moveRect(current.startRect, dx, dy)
        : resizeRect(current.startRect, current.kind, pointer, size, aspect);
    if (frame.current === null) {
      frame.current = window.requestAnimationFrame(() => {
        frame.current = null;
        if (drag.current) paint(drag.current.draft);
      });
    }
  };

  /**
   * Ends the drag. A commit writes the draft to the store once, inside the gesture, so the whole drag is one undo
   * step; a commit of a drag that never moved, and a cancel, write nothing. Either way the overlay is painted from the
   * store afterwards.
   */
  const finishDrag = (commit: boolean) => {
    const current = drag.current;
    if (!current) return;
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    frame.current = null;
    drag.current = null;
    if (commit && current.gesture.isOpen) {
      current.gesture.update(() => useEditorStore.getState().setCrop(current.draft));
    }
    current.gesture.end();
    setDragging(false);
    paint(useEditorStore.getState().project?.transform.crop ?? stored);
  };

  const beginDrag = (kind: "move" | CropHandle, event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    const root = rootRef.current;
    if (root) {
      root.focus({ preventScroll: true });
      root.setPointerCapture?.(event.pointerId);
    }
    drag.current = {
      kind,
      pointerId: event.pointerId,
      start: pointerOf(event),
      startRect: stored,
      gesture: new EditGesture(),
      draft: stored,
    };
    setDragging(true);
  };

  const endPointer = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    rootRef.current?.releasePointerCapture?.(event.pointerId);
    finishDrag(true);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (drag.current) {
      if (event.key === "Escape") {
        event.preventDefault();
        finishDrag(false);
      } else if (event.key === "Enter") {
        event.preventDefault();
        finishDrag(true);
      }
      return;
    }
    const step = event.shiftKey ? 10 : 1;
    const moves: Record<string, [number, number]> = {
      ArrowLeft: [-step, 0],
      ArrowRight: [step, 0],
      ArrowUp: [0, -step],
      ArrowDown: [0, step],
    };
    const move = moves[event.key];
    if (!move) return;
    event.preventDefault();
    const state = useEditorStore.getState();
    const current = state.project?.transform.crop;
    if (!current) return;
    withGesture(() => state.setCrop(nudgeRect(current, move[0], move[1], size)));
  };

  const lines = dragging ? thirdsLines(stored) : null;

  return (
    <div
      ref={rootRef}
      role="application"
      // biome-ignore lint/a11y/noNoninteractiveTabindex: a keyboard composite takes focus so the registry keys reach it
      tabIndex={0}
      aria-label={t("crop.area")}
      aria-roledescription={t("crop.roleDescription")}
      onKeyDown={onKeyDown}
      onPointerMove={(event) => scheduleDraft(event)}
      onPointerUp={endPointer}
      onPointerCancel={endPointer}
      className="absolute inset-0 touch-none outline-none select-none focus-visible:outline-2 focus-visible:outline-accent"
    >
      <div
        ref={(element) => {
          dimRefs.current.top = element;
        }}
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 bg-app/55"
        style={{ height: `${stored.y * 100}%` }}
      />
      <div
        ref={(element) => {
          dimRefs.current.bottom = element;
        }}
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 bottom-0 bg-app/55"
        style={{ height: `${(1 - stored.y - stored.h) * 100}%` }}
      />
      <div
        ref={(element) => {
          dimRefs.current.left = element;
        }}
        aria-hidden="true"
        className="pointer-events-none absolute left-0 bg-app/55"
        style={{
          top: `${stored.y * 100}%`,
          height: `${stored.h * 100}%`,
          width: `${stored.x * 100}%`,
        }}
      />
      <div
        ref={(element) => {
          dimRefs.current.right = element;
        }}
        aria-hidden="true"
        className="pointer-events-none absolute right-0 bg-app/55"
        style={{
          top: `${stored.y * 100}%`,
          height: `${stored.h * 100}%`,
          width: `${(1 - stored.x - stored.w) * 100}%`,
        }}
      />

      <div
        ref={boxRef}
        className="absolute cursor-move border border-accent"
        style={boxStyle(stored)}
        onPointerDown={(event) => beginDrag("move", event)}
      >
        {lines
          ? lines.vertical.map((position, index) => (
              <span
                key={`v${position}`}
                ref={(element) => {
                  guideRefs.current[index] = element;
                }}
                aria-hidden="true"
                className="pointer-events-none absolute inset-y-0 w-px bg-fg/35"
                data-guide="thirds"
                style={{ left: `${((position - stored.x) / stored.w) * 100}%` }}
              />
            ))
          : null}
        {lines
          ? lines.horizontal.map((position, index) => (
              <span
                key={`h${position}`}
                ref={(element) => {
                  guideRefs.current[2 + index] = element;
                }}
                aria-hidden="true"
                className="pointer-events-none absolute inset-x-0 h-px bg-fg/35"
                data-guide="thirds"
                style={{ top: `${((position - stored.y) / stored.h) * 100}%` }}
              />
            ))
          : null}
      </div>

      {HANDLE_ORDER.map((handle) => {
        const spot = HANDLE_LAYOUT[handle].at(stored);
        // The hit area is 24px square; the drawn square inside it stays 8px.
        return (
          <div
            key={handle}
            ref={(element) => {
              handleRefs.current[handle] = element;
            }}
            aria-hidden="true"
            data-handle={handle}
            className={cn(
              "absolute flex size-6 -translate-x-1/2 -translate-y-1/2 items-center justify-center",
              HANDLE_LAYOUT[handle].cursor,
            )}
            style={{ left: `${spot.x * 100}%`, top: `${spot.y * 100}%` }}
            onPointerDown={(event) => beginDrag(handle, event)}
          >
            <span className="size-2 rounded-[2px] border border-accent bg-fg" />
          </div>
        );
      })}
    </div>
  );
}

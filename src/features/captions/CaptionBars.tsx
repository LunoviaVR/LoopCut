import { type KeyboardEvent, type PointerEvent, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { frameToTime } from "@/domain/time";
import { pixelsPerSecond, type ViewScale } from "@/features/timeline/mapping";
import type { Caption } from "@/ipc/bindings/Caption";
import type { TimeRange } from "@/ipc/bindings/TimeRange";
import { cn } from "@/lib/cn";
import { useEditorStore, withGesture } from "@/state/editor";
import { selectedFillClass } from "@/ui";
import {
  type CaptionPart,
  captionBarRect,
  captionsInOrder,
  captionTitle,
  dragCaptionRange,
  partAtOffset,
} from "./captionTimeline";

/** A drag repaints on the next frame, or after this long when frames are paused (window hidden or covered). */
const PAINT_FALLBACK_MS = 50;
/** Output frames that Shift with an arrow key moves a focused caption by (one frame without Shift). */
const SHIFT_NUDGE_FRAMES = 10;

export interface CaptionBarsProps {
  scale: ViewScale;
  duration: number;
  captions: readonly Caption[];
  selectedId: string | null;
  /** Top and height of the caption lane in CSS pixels, inside the timeline area. */
  top: number;
  height: number;
}

/**
 * The caption lane: one bar per caption at its source-time range. A click selects the caption (and shows its editor);
 * dragging the body moves it, dragging an end changes its start or end. A drag keeps its live range in a ref and
 * moves the bar by DOM writes once per animation frame; the store is written once, on release, so each drag is one
 * undo step. From the keyboard, a focused bar takes Enter or Space to select, Delete to remove, and Left or Right to
 * move by one output frame (ten with Shift). Each key press is one undo step.
 */
export function CaptionBars({
  scale,
  duration,
  captions,
  selectedId,
  top,
  height,
}: CaptionBarsProps) {
  return (
    <>
      {captionsInOrder(captions).map((caption) => (
        <CaptionBar
          key={caption.id}
          caption={caption}
          selected={caption.id === selectedId}
          scale={scale}
          duration={duration}
          top={top}
          height={height}
        />
      ))}
    </>
  );
}

interface BarDrag {
  id: string;
  pointerId: number;
  part: CaptionPart;
  startClientX: number;
  original: TimeRange;
  /** The live range of the drag. The store has not seen it yet. */
  draft: TimeRange;
}

function CaptionBar({
  caption,
  selected,
  scale,
  duration,
  top,
  height,
}: {
  caption: Caption;
  selected: boolean;
  scale: ViewScale;
  duration: number;
  top: number;
  height: number;
}) {
  const { t } = useTranslation();
  const rect = captionBarRect(caption.range, scale);
  const title = captionTitle(caption.text) || t("captions.untitled");
  const bar = useRef<HTMLButtonElement>(null);
  const drag = useRef<BarDrag | null>(null);
  /** The scheduled paint: an animation frame, and the timeout that runs it when frames are paused. */
  const frame = useRef<number | null>(null);
  const fallback = useRef<number | null>(null);
  const latest = useRef(scale);
  latest.current = scale;

  useEffect(() => {
    return () => {
      // Clear the ids too: StrictMode re-runs effects after this cleanup, and a stale id would block every later paint.
      if (frame.current !== null) window.cancelAnimationFrame(frame.current);
      if (fallback.current !== null) window.clearTimeout(fallback.current);
      frame.current = null;
      fallback.current = null;
      drag.current = null;
    };
  }, []);

  /** Places the bar at a range through its DOM style; the store and the render are not involved. */
  const place = (range: TimeRange) => {
    const element = bar.current;
    if (!element) return;
    const placed = captionBarRect(range, latest.current);
    element.style.left = `${placed.left}px`;
    element.style.width = `${placed.width}px`;
  };

  /** Runs the scheduled paint: the bar moves to the live range, and the other half of the pair is cancelled. */
  const paint = () => {
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    if (fallback.current !== null) window.clearTimeout(fallback.current);
    frame.current = null;
    fallback.current = null;
    const current = drag.current;
    if (current) place(current.draft);
  };

  const schedulePaint = () => {
    if (frame.current !== null) return;
    frame.current = window.requestAnimationFrame(paint);
    fallback.current = window.setTimeout(paint, PAINT_FALLBACK_MS);
  };

  /** Selects this caption and shows the Captions tool with it: what a press and a click on the bar do. */
  const select = () => {
    const store = useEditorStore.getState();
    store.setSelection({ kind: "caption", id: caption.id });
    store.setActiveTool("captions");
  };

  const onPointerDown = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    const element = event.currentTarget;
    const bounds = element.getBoundingClientRect();
    const part = partAtOffset(event.clientX - bounds.left, bounds.width);
    element.focus({ preventScroll: true });
    element.setPointerCapture?.(event.pointerId);
    select();
    const original = { start: caption.range.start, end: caption.range.end };
    drag.current = {
      id: caption.id,
      pointerId: event.pointerId,
      part,
      startClientX: event.clientX,
      original,
      draft: original,
    };
  };

  const onPointerMove = (event: PointerEvent<HTMLButtonElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    const view = latest.current;
    const pps = pixelsPerSecond(view.duration, view.viewWidth, view.zoom);
    if (pps <= 0) return;
    const delta = (event.clientX - current.startClientX) / pps;
    current.draft = dragCaptionRange(current.original, current.part, delta, duration);
    schedulePaint();
  };

  /**
   * Ends the drag. A commit writes the live range to the store once, inside one gesture, when it changed the caption;
   * either way the bar is placed from the store afterwards.
   */
  const finish = (event: PointerEvent<HTMLButtonElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    if (fallback.current !== null) window.clearTimeout(fallback.current);
    frame.current = null;
    fallback.current = null;
    drag.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    const { id, draft } = current;
    const find = () => useEditorStore.getState().project?.captions.find((item) => item.id === id);
    const stored = find();
    if (stored && (stored.range.start !== draft.start || stored.range.end !== draft.end)) {
      withGesture(() => useEditorStore.getState().updateCaption(id, { range: draft }));
    }
    place(find()?.range ?? draft);
  };

  /**
   * Moves the caption by `frames` output frames. A frame at the output rate covers `speed / fps` seconds of the clip,
   * so the step follows the clip's speed. The range stays on the clip, and one press is one undo step.
   */
  const nudge = (frames: number) => {
    const project = useEditorStore.getState().project;
    const current = project?.captions.find((item) => item.id === caption.id);
    if (!project || !current) return;
    const step = frameToTime(1, project.output.fps) * project.timing.speed;
    const range = dragCaptionRange(current.range, "move", frames * step, duration);
    withGesture(() => useEditorStore.getState().updateCaption(caption.id, { range }));
  };

  /**
   * Moves focus off this bar before it unmounts: to the next bar in the lane, else the previous one, else the timeline
   * the lane sits in. Without this, a keyboard delete leaves focus on the page body.
   */
  const focusAfterRemoval = () => {
    const element = bar.current;
    if (!element) return;
    const lane = Array.from(element.parentElement?.children ?? []).filter(
      (child): child is HTMLButtonElement =>
        child instanceof HTMLButtonElement && child.hasAttribute("aria-pressed"),
    );
    const at = lane.indexOf(element);
    const target =
      lane[at + 1] ?? lane[at - 1] ?? element.closest<HTMLElement>('[role="application"]');
    target?.focus({ preventScroll: true });
  };

  /**
   * Keys on a focused bar. Enter and Space select through the click, which the button sends for them. Delete and
   * Backspace remove the caption and move focus to a neighbour; the arrows move it.
   */
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "Delete" || event.key === "Backspace") {
      event.preventDefault();
      if (drag.current) return;
      focusAfterRemoval();
      useEditorStore.getState().removeCaption(caption.id);
      return;
    }
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    if (drag.current) return;
    const direction = event.key === "ArrowLeft" ? -1 : 1;
    nudge(direction * (event.shiftKey ? SHIFT_NUDGE_FRAMES : 1));
  };

  return (
    <button
      ref={bar}
      type="button"
      aria-label={title}
      aria-pressed={selected}
      title={title}
      onClick={select}
      onKeyDown={onKeyDown}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={finish}
      onPointerCancel={finish}
      className={cn(
        "absolute z-10 flex min-w-0 touch-none cursor-grab items-center overflow-hidden rounded-control border text-left outline-none select-none",
        "active:cursor-grabbing focus-visible:outline-2 focus-visible:outline-accent",
        selected
          ? cn("border-transparent", selectedFillClass)
          : "border-line-strong bg-raised hover:border-fg-subtle",
      )}
      style={{ left: rect.left, width: rect.width, top, height }}
    >
      <span className="block truncate px-1.5 text-xs text-fg">{title}</span>
    </button>
  );
}

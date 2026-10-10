import { type PointerEvent, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { Project } from "@/ipc/bindings/Project";
import { useEditorStore, withGesture } from "@/state/editor";
import {
  captionBox,
  clampCaptionCentre,
  measureWithCanvas,
  type OutputSize,
} from "./captionGeometry";

/** A drag repaints on the next frame, or after this long when frames are paused (window hidden or covered). */
const PAINT_FALLBACK_MS = 50;

export interface CaptionOverlayProps {
  project: Project;
  /** The frame's size in output pixels (the preview's output size). */
  output: OutputSize;
}

interface Drag {
  id: string;
  pointerId: number;
  startX: number;
  startY: number;
  origin: { x: number; y: number };
  /** The box's top-left corner before the drag. It moves by the same fraction as the anchor. */
  box: { left: number; top: number };
  width: number;
  height: number;
  /** The live anchor of the drag. The store has not seen it yet. */
  draft: { x: number; y: number };
}

/**
 * The selected caption's box over the monitor, dashed, sized from its style and text. Dragging the box moves the
 * caption's anchor. A drag keeps the anchor in a ref and moves the box by DOM writes once per animation frame; the store
 * is written once, on release, so the whole drag is one undo step. It shows only while the Captions tool is active, and
 * the panel's X and Y fields do the same thing from the keyboard.
 */
export function CaptionOverlay({ project, output }: CaptionOverlayProps) {
  const { t } = useTranslation();
  const selection = useEditorStore((state) => state.selection);
  const root = useRef<HTMLDivElement>(null);
  const boxElement = useRef<HTMLDivElement>(null);
  const drag = useRef<Drag | null>(null);
  /** The scheduled paint: an animation frame, and the timeout that runs it when frames are paused. */
  const frame = useRef<number | null>(null);
  const fallback = useRef<number | null>(null);
  const selectedId = selection?.kind === "caption" ? selection.id : null;
  const caption = project.captions.find((item) => item.id === selectedId) ?? null;

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

  if (!caption) return null;

  const box = captionBox(caption, output, measureWithCanvas);

  /** Moves the box to an anchor by DOM writes. The box is linear in the anchor, so it shifts by the same offset. */
  const place = (from: Pick<Drag, "box" | "origin">, anchor: { x: number; y: number }) => {
    const element = boxElement.current;
    if (!element) return;
    element.style.left = `${(from.box.left + anchor.x - from.origin.x) * 100}%`;
    element.style.top = `${(from.box.top + anchor.y - from.origin.y) * 100}%`;
  };

  /** Runs the scheduled paint, and cancels the other half of the pair. */
  const paint = () => {
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    if (fallback.current !== null) window.clearTimeout(fallback.current);
    frame.current = null;
    fallback.current = null;
    const current = drag.current;
    if (current) place(current, current.draft);
  };

  const schedulePaint = () => {
    if (frame.current !== null) return;
    frame.current = window.requestAnimationFrame(paint);
    fallback.current = window.setTimeout(paint, PAINT_FALLBACK_MS);
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const frameBounds = root.current?.getBoundingClientRect();
    if (!frameBounds || frameBounds.width === 0 || frameBounds.height === 0) return;
    event.stopPropagation();
    event.currentTarget.setPointerCapture?.(event.pointerId);
    const origin = { x: caption.x, y: caption.y };
    drag.current = {
      id: caption.id,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      origin,
      box: { left: box.left, top: box.top },
      width: frameBounds.width,
      height: frameBounds.height,
      draft: origin,
    };
  };

  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    current.draft = clampCaptionCentre(
      current.origin.x + (event.clientX - current.startX) / current.width,
      current.origin.y + (event.clientY - current.startY) / current.height,
    );
    schedulePaint();
  };

  /**
   * Ends the drag. A commit writes the live anchor to the store once, inside one gesture, when it moved the caption;
   * either way the box is placed from the store afterwards.
   */
  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
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
    if (stored && (stored.x !== draft.x || stored.y !== draft.y)) {
      withGesture(() => useEditorStore.getState().updateCaption(id, { x: draft.x, y: draft.y }));
    }
    const settled = find();
    place(current, settled ? { x: settled.x, y: settled.y } : draft);
  };

  return (
    <div ref={root} className="pointer-events-none absolute inset-0">
      <div
        ref={boxElement}
        role="presentation"
        aria-label={t("captions.overlay.box")}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        className="pointer-events-auto absolute cursor-move border border-dashed border-accent"
        style={{
          left: `${box.left * 100}%`,
          top: `${box.top * 100}%`,
          width: `${box.width * 100}%`,
          height: `${box.height * 100}%`,
        }}
      />
    </div>
  );
}

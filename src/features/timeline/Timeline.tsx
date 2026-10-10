/**
 * The timeline: a canvas for the ruler, the filmstrip, the removed areas and the playhead, with DOM handles for the
 * in and out points of every kept range. The canvas is repainted at most once per animation frame, straight from
 * refs. A handle drag keeps its live edges in a ref: the handle, its readout and the canvas move by DOM writes in
 * one animation frame, and the store is written once, on release (Enter commits too; Escape writes nothing).
 * Playback moves the playhead about 20 times a second without re-rendering this component.
 */
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { clamp } from "@/domain/numeric";
import { formatTimecode, toFloat } from "@/domain/time";
import { clampSegmentRange, segmentIndexAt } from "@/domain/timeline";
import { CaptionBars } from "@/features/captions/CaptionBars";
import type { Caption } from "@/ipc/bindings/Caption";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { Rational } from "@/ipc/bindings/Rational";
import type { TimeRange } from "@/ipc/bindings/TimeRange";
import { fetchThumbnail, mediaThumbnails } from "@/ipc/commands";
import { cn } from "@/lib/cn";
import { useEditorStore, withGesture, ZOOM_MAX, ZOOM_MIN } from "@/state/editor";
import { drawTimeline, readPalette, TIMELINE_LAYOUT, type TimelinePalette } from "./draw";
import {
  clampScroll,
  frameForSourceTime,
  pixelsPerSecond,
  playheadSourceTime,
  snapTime,
  timeToX,
  type ViewScale,
  xToTime,
  zoomAround,
} from "./mapping";
import { ThumbnailStrip, thumbnailCountFor, thumbnailHeightFor } from "./thumbnails";

const NO_SEGMENTS: readonly TimeRange[] = [];
const NO_CAPTIONS: readonly Caption[] = [];
/** Keyboard step for a handle without Shift: one source frame at the clip's rate. */
const FALLBACK_FPS = 30;
const LABEL_GUTTER_CLASS = "w-16 shrink-0";

/** A pointer drag of one handle. `draft` holds the live edges of the kept range; the store has not seen them yet. */
interface HandleDrag {
  index: number;
  edge: "in" | "out";
  pointerId: number;
  draft: TimeRange;
}

/** Key of a handle's DOM element in the handle registry. */
const handleKey = (index: number, edge: "in" | "out") => `${index}-${edge}`;

/** The values the canvas and the event handlers need. Refreshed after every render. */
interface Latest {
  scale: ViewScale;
  segments: readonly TimeRange[];
  selected: number | null;
  sourceFps: number;
  frameRate: Rational;
  duration: number;
  dpr: number;
  palette: TimelinePalette;
}

export interface TimelineProps {
  /** The open media. The timeline is only mounted while a clip is open. */
  media: ImportedMedia;
}

/**
 * Timeline tracks. Zoom and scroll come from the store and this component (wheel); the playhead, selection and
 * segments come from the store. Keyboard: the registry owns the arrows, Home, End and Delete on the focused
 * timeline. A focused handle takes its own arrows, selects its kept range when it takes focus, deletes that range
 * with Delete or Backspace, and Escape cancels a drag.
 */
export function Timeline({ media }: TimelineProps) {
  const { t } = useTranslation();
  // Narrow selectors: the timeline re-renders for its own slices, never for every project edit.
  const segments = useEditorStore((state) => state.project?.clips[0]?.segments ?? NO_SEGMENTS);
  const captions = useEditorStore((state) => state.project?.captions ?? NO_CAPTIONS);
  const zoom = useEditorStore((state) => state.timelineZoom);
  const selection = useEditorStore((state) => state.selection);
  const duration = media.info.duration;
  const frameRate = media.info.video.frameRate;
  const sourceFps = toFloat(frameRate);
  const token = media.token;

  const areaRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const readoutRef = useRef<HTMLDivElement>(null);
  const handleEls = useRef(new Map<string, HTMLDivElement>());
  /** The handle to focus after a keyboard delete. Set by deleteRange, applied once the handles have rendered. */
  const pendingFocus = useRef<string | null>(null);
  const [viewWidth, setViewWidth] = useState(0);
  const [scrollState, setScrollState] = useState(0);
  /** Whether the readout is shown. Its position and text are written to the DOM during a drag, not rendered. */
  const [readoutOpen, setReadoutOpen] = useState(false);
  const [palette] = useState(() => readPalette());

  const scroll = clampScroll(scrollState, { duration, viewWidth, zoom });
  const scale: ViewScale = { duration, viewWidth, zoom, scrollPx: scroll };
  const selectedIndex = selection?.kind === "segment" ? selection.index : null;

  const latest = useRef<Latest | null>(null);
  const frameId = useRef<number | null>(null);
  const dragFrame = useRef<number | null>(null);
  const drag = useRef<HandleDrag | null>(null);
  const scrubPointer = useRef<number | null>(null);
  const thumbStrip = useRef<ThumbnailStrip | null>(null);
  const thumbTimes = useRef<number[]>([]);

  /** Paints the canvas from the refs (with the live drag draft in place of its kept range). Never from render. */
  const drawNow = useCallback(() => {
    frameId.current = null;
    const canvas = canvasRef.current;
    const current = latest.current;
    if (!canvas || !current || current.scale.viewWidth <= 0) return;
    const context = canvas.getContext("2d");
    if (!context) return;
    const width = current.scale.viewWidth;
    const pixelWidth = Math.round(width * current.dpr);
    const pixelHeight = Math.round(TIMELINE_LAYOUT.height * current.dpr);
    if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
    if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
    const state = useEditorStore.getState();
    const strip = thumbStrip.current;
    const live = drag.current;
    const shownSegments = live
      ? current.segments.map((range, index) => (index === live.index ? live.draft : range))
      : current.segments;
    drawTimeline(context, {
      width,
      dpr: current.dpr,
      scale: current.scale,
      segments: shownSegments,
      selected: current.selected,
      playheadTime: playheadSourceTime(state.sequencePlan, state.playhead),
      thumbs: strip ? { times: thumbTimes.current, get: (index) => strip.get(index) } : null,
      palette: current.palette,
    });
  }, []);

  /**
   * Schedules at most one paint per animation frame. Chromium pauses animation frames for hidden or fully occluded
   * windows, so a short timeout paints anyway; whichever fires first wins and cancels the other.
   */
  const fallbackId = useRef<number | null>(null);
  const requestDraw = useCallback(() => {
    if (frameId.current !== null) return;
    const run = () => {
      if (frameId.current !== null) window.cancelAnimationFrame(frameId.current);
      if (fallbackId.current !== null) window.clearTimeout(fallbackId.current);
      fallbackId.current = null;
      drawNow();
    };
    frameId.current = window.requestAnimationFrame(run);
    fallbackId.current = window.setTimeout(run, 50);
  }, [drawNow]);

  useEffect(
    () => () => {
      // Clear the ids too: StrictMode re-runs effects after this cleanup, and a stale id would block every later paint.
      if (frameId.current !== null) window.cancelAnimationFrame(frameId.current);
      if (fallbackId.current !== null) window.clearTimeout(fallbackId.current);
      frameId.current = null;
      fallbackId.current = null;
    },
    [],
  );

  // Refresh the values the paint and the handlers read, then repaint with them.
  useEffect(() => {
    latest.current = {
      scale: { duration, viewWidth, zoom, scrollPx: scroll },
      segments,
      selected: selectedIndex,
      sourceFps,
      frameRate,
      duration,
      dpr: window.devicePixelRatio || 1,
      palette,
    };
    requestDraw();
  });

  // Measure the visible width of the canvas area; it drives every x position.
  useLayoutEffect(() => {
    const area = areaRef.current;
    if (!area) return;
    const measure = () => setViewWidth(Math.round(area.getBoundingClientRect().width));
    measure();
    const observer = new ResizeObserver(() => measure());
    observer.observe(area);
    return () => observer.disconnect();
  }, []);

  // Applies the focus a keyboard delete left pending, now that the handles match the new segments. Runs after each
  // commit: it reads a ref and returns at once when nothing is pending.
  useLayoutEffect(() => {
    const key = pendingFocus.current;
    if (key === null) return;
    pendingFocus.current = null;
    handleEls.current.get(key)?.focus({ preventScroll: true });
  });

  /** Keeps the playhead inside the view while it plays (no React render: only scroll state changes when needed). */
  const followPlayhead = useCallback((): void => {
    const current = latest.current;
    const state = useEditorStore.getState();
    if (!current || current.scale.viewWidth <= 0) return;
    const time = playheadSourceTime(state.sequencePlan, state.playhead);
    if (time === null) return;
    const x = timeToX(time, current.scale);
    if (x >= 0 && x <= current.scale.viewWidth) return;
    const pps = pixelsPerSecond(
      current.scale.duration,
      current.scale.viewWidth,
      current.scale.zoom,
    );
    setScrollState(clampScroll(time * pps - current.scale.viewWidth * 0.25, current.scale));
  }, []);

  // Repaint when the store changes what the canvas shows, and keep the playhead on screen while playing.
  useEffect(() => {
    return useEditorStore.subscribe((state, previous) => {
      if (state.playhead !== previous.playhead && state.isPlaying) followPlayhead();
      if (
        state.playhead !== previous.playhead ||
        state.sequencePlan !== previous.sequencePlan ||
        state.selection !== previous.selection ||
        state.project !== previous.project
      ) {
        requestDraw();
      }
    });
  }, [requestDraw, followPlayhead]);

  // Load the filmstrip for this clip. A new clip or length replaces the set; leaving the timeline disposes it.
  useEffect(() => {
    let cancelled = false;
    const count = thumbnailCountFor(duration);
    const height = thumbnailHeightFor(window.devicePixelRatio || 1);
    mediaThumbnails(token, count, height).then(
      (set) => {
        if (cancelled) return;
        thumbStrip.current?.dispose();
        thumbStrip.current = new ThumbnailStrip({
          count: set.times.length,
          load: (index) => fetchThumbnail(token, set.height, index),
          onReady: requestDraw,
        });
        thumbTimes.current = [...set.times];
        requestDraw();
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
      thumbStrip.current?.dispose();
      thumbStrip.current = null;
      thumbTimes.current = [];
    };
  }, [token, duration, requestDraw]);

  // Wheel: Ctrl zooms around the cursor, otherwise the wheel scrolls. Attached natively to allow preventDefault.
  useEffect(() => {
    const area = areaRef.current;
    if (!area) return;
    const onWheel = (event: WheelEvent) => {
      const current = latest.current;
      if (!current || current.duration <= 0 || current.scale.viewWidth <= 0) return;
      if (event.ctrlKey) {
        event.preventDefault();
        const cursorX = event.clientX - area.getBoundingClientRect().left;
        const next = zoomAround(current.scale, cursorX, Math.exp(-event.deltaY * 0.0015), {
          min: ZOOM_MIN,
          max: ZOOM_MAX,
        });
        useEditorStore.getState().setTimelineZoom(next.zoom);
        setScrollState(next.scrollPx);
        return;
      }
      const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
      if (delta === 0) return;
      event.preventDefault();
      setScrollState(clampScroll(current.scale.scrollPx + delta, current.scale));
    };
    area.addEventListener("wheel", onWheel, { passive: false });
    return () => area.removeEventListener("wheel", onWheel);
  }, []);

  // Unmounting drops a drag still in progress: its draft was never written, so there is nothing to undo.
  useEffect(() => {
    return () => {
      if (dragFrame.current !== null) window.cancelAnimationFrame(dragFrame.current);
      if (frameId.current !== null) window.cancelAnimationFrame(frameId.current);
      drag.current = null;
      thumbStrip.current?.dispose();
    };
  }, []);

  const timeAt = (clientX: number): number => {
    const current = latest.current;
    const rect = areaRef.current?.getBoundingClientRect();
    if (!current || !rect) return 0;
    return xToTime(clientX - rect.left, current.scale);
  };

  const pauseForEdit = () => {
    const state = useEditorStore.getState();
    if (state.isPlaying) state.setTransport({ isPlaying: false, playRate: 1 });
  };

  const selectAt = (time: number) => {
    const state = useEditorStore.getState();
    const index = segmentIndexAt(state.project?.clips[0]?.segments ?? [], time);
    state.setSelection(index >= 0 ? { kind: "segment", index } : null);
  };

  /** Selects the kept range at `index`. A press on its handle or focus on it shows which range it belongs to. */
  const selectRange = (index: number) => {
    useEditorStore.getState().setSelection({ kind: "segment", index });
  };

  const seekTo = (time: number) => {
    const state = useEditorStore.getState();
    if (!state.sequencePlan) return;
    const frame = frameForSourceTime(state.sequencePlan, clamp(time, 0, duration), state.playhead);
    if (frame !== null && frame !== state.playhead) state.setPlayhead(frame);
  };

  const onAreaPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const area = event.currentTarget;
    area.focus({ preventScroll: true });
    area.setPointerCapture?.(event.pointerId);
    scrubPointer.current = event.pointerId;
    pauseForEdit();
    const time = timeAt(event.clientX);
    const y = event.clientY - area.getBoundingClientRect().top;
    if (y >= TIMELINE_LAYOUT.clipTop && y < TIMELINE_LAYOUT.clipTop + TIMELINE_LAYOUT.clipH) {
      selectAt(time);
    }
    seekTo(time);
  };

  const onAreaPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (scrubPointer.current !== event.pointerId) return;
    seekTo(timeAt(event.clientX));
  };

  const onAreaPointerUp = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (scrubPointer.current !== event.pointerId) return;
    scrubPointer.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  };

  const currentSegment = (index: number): TimeRange | undefined =>
    useEditorStore.getState().project?.clips[0]?.segments[index];

  /** Commits one handle value: the store is written once for the value and the whole drag is one undo step. */
  const setEdge = (index: number, edge: "in" | "out", time: number) => {
    const state = useEditorStore.getState();
    const segment = state.project?.clips[0]?.segments[index];
    if (!segment) return;
    if (edge === "in") state.setSegment(index, time, segment.end);
    else state.setSegment(index, segment.start, time);
  };

  /**
   * Delete or Backspace on a focused handle: removes its kept range (the last one stays) and selects the range that
   * takes its place. Focus stays on the same edge of that range. When the removed range was the last one, its handles
   * unmount, so focus goes to the same edge of the new last range once the handles have rendered.
   */
  const deleteRange = (index: number, edge: "in" | "out") => {
    if (!useEditorStore.getState().deleteSegment(index)) return;
    const remaining = useEditorStore.getState().project?.clips[0]?.segments.length ?? 0;
    if (remaining === 0) return;
    const next = Math.min(index, remaining - 1);
    selectRange(next);
    pendingFocus.current = handleKey(next, edge);
  };

  /** Moves the DOM of a kept range's two handles to `range` (no React render: the drag owns these positions). */
  const placeHandles = (index: number, range: TimeRange) => {
    const view = latest.current;
    if (!view) return;
    const inEl = handleEls.current.get(handleKey(index, "in"));
    const outEl = handleEls.current.get(handleKey(index, "out"));
    if (inEl) inEl.style.left = `${timeToX(range.start, view.scale)}px`;
    if (outEl) outEl.style.left = `${timeToX(range.end, view.scale)}px`;
  };

  /** Writes the live drag value to the readout, the handles and the canvas. Runs once per animation frame. */
  const paintDrag = () => {
    dragFrame.current = null;
    const current = drag.current;
    const view = latest.current;
    if (!current || !view) return;
    placeHandles(current.index, current.draft);
    const shown = current.edge === "in" ? current.draft.start : current.draft.end;
    const readout = readoutRef.current;
    if (readout) {
      readout.style.left = `${timeToX(shown, view.scale)}px`;
      readout.textContent = formatTimecode(shown, view.frameRate);
    }
    requestDraw();
  };

  const scheduleDragPaint = () => {
    if (dragFrame.current === null) dragFrame.current = window.requestAnimationFrame(paintDrag);
  };

  const cancelDragFrame = () => {
    if (dragFrame.current !== null) window.cancelAnimationFrame(dragFrame.current);
    dragFrame.current = null;
  };

  /** Ends the drag. A commit writes the draft to the store once; a cancel drops it and restores the handle. */
  const endDrag = (commit: boolean) => {
    const current = drag.current;
    if (!current) return;
    cancelDragFrame();
    drag.current = null;
    setReadoutOpen(false);
    const stored = currentSegment(current.index);
    if (!commit || !stored) {
      if (stored) placeHandles(current.index, stored);
      return;
    }
    if (stored.start === current.draft.start && stored.end === current.draft.end) return;
    const { index, draft } = current;
    withGesture(() => useEditorStore.getState().setSegment(index, draft.start, draft.end));
  };

  const onHandlePointerDown = (
    index: number,
    edge: "in" | "out",
    event: ReactPointerEvent<HTMLDivElement>,
  ) => {
    if (event.button !== 0) return;
    event.stopPropagation();
    const segment = currentSegment(index);
    if (!segment) return;
    const handle = event.currentTarget;
    handle.focus({ preventScroll: true });
    handle.setPointerCapture?.(event.pointerId);
    drag.current = {
      index,
      edge,
      pointerId: event.pointerId,
      draft: { start: segment.start, end: segment.end },
    };
    selectRange(index);
    pauseForEdit();
    setReadoutOpen(true);
    paintDrag();
  };

  const onHandlePointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    const view = latest.current;
    if (!current || current.pointerId !== event.pointerId || !view) return;
    const state = useEditorStore.getState();
    const pps = pixelsPerSecond(view.scale.duration, view.scale.viewWidth, view.scale.zoom);
    const time = snapTime(timeAt(event.clientX), {
      enabled: !event.shiftKey,
      sourceFps: view.sourceFps,
      playheadTime: playheadSourceTime(state.sequencePlan, state.playhead),
      pixelsPerSecond: pps,
    });
    const segments = state.project?.clips[0]?.segments ?? [];
    const range =
      current.edge === "in"
        ? clampSegmentRange(segments, current.index, time, current.draft.end)
        : clampSegmentRange(segments, current.index, current.draft.start, time);
    if (range) current.draft = range;
    scheduleDragPaint();
  };

  const onHandlePointerUp = (event: ReactPointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    endDrag(true);
  };

  /** Escape: drops the draft. Nothing reached the store, so no undo step is recorded. */
  const cancelDrag = () => endDrag(false);

  const onHandleKeyDown = (
    index: number,
    edge: "in" | "out",
    event: ReactKeyboardEvent<HTMLDivElement>,
  ) => {
    if (event.key === "Escape") {
      if (drag.current) {
        event.preventDefault();
        cancelDrag();
      }
      return;
    }
    if (event.key === "Enter" && drag.current) {
      event.preventDefault();
      endDrag(true);
      return;
    }
    if (event.key === "Delete" || event.key === "Backspace") {
      // Handled here, so the registry's Delete does not also run: a focused slider keeps its own keys.
      event.preventDefault();
      if (!drag.current) deleteRange(index, edge);
      return;
    }
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const segment = currentSegment(index);
    if (!segment) return;
    const fps = sourceFps > 0 ? sourceFps : FALLBACK_FPS;
    const step = event.shiftKey ? 1 : 1 / fps;
    const direction = event.key === "ArrowLeft" ? -1 : 1;
    const base = edge === "in" ? segment.start : segment.end;
    const time = base + direction * step;
    withGesture(() => setEdge(index, edge, time));
  };

  const registerHandle = (key: string, element: HTMLDivElement | null) => {
    if (element) handleEls.current.set(key, element);
    else handleEls.current.delete(key);
  };

  const handles = segments.flatMap((segment, index) => {
    const startX = timeToX(segment.start, scale);
    const endX = timeToX(segment.end, scale);
    const inputs = [
      { edge: "in" as const, time: segment.start, x: startX, label: "timeline.inPoint" },
      { edge: "out" as const, time: segment.end, x: endX, label: "timeline.outPoint" },
    ];
    return inputs.map(({ edge, time, x, label }) => (
      <TrimHandle
        key={handleKey(index, edge)}
        label={t(label, { index: index + 1 })}
        x={x}
        valueText={formatTimecode(time, frameRate)}
        value={time}
        max={duration}
        elementRef={(element) => registerHandle(handleKey(index, edge), element)}
        onFocus={() => selectRange(index)}
        onPointerDown={(event) => onHandlePointerDown(index, edge, event)}
        onPointerMove={onHandlePointerMove}
        onPointerUp={onHandlePointerUp}
        onKeyDown={(event) => onHandleKeyDown(index, edge, event)}
      />
    ));
  });

  return (
    <div className="flex items-start gap-2 px-2 py-2">
      <div
        className={cn("relative", LABEL_GUTTER_CLASS)}
        style={{ height: TIMELINE_LAYOUT.height }}
      >
        <span
          className="absolute inset-x-0 flex items-center text-xs text-fg"
          style={{ top: TIMELINE_LAYOUT.clipTop, height: TIMELINE_LAYOUT.clipH }}
        >
          {t("timeline.clipTrack")}
        </span>
        <span
          className="absolute inset-x-0 flex items-center text-xs text-fg-muted"
          style={{ top: TIMELINE_LAYOUT.captionTop, height: TIMELINE_LAYOUT.captionH }}
        >
          {t("timeline.captionTrack")}
        </span>
      </div>

      <div
        ref={areaRef}
        role="application"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: a keyboard composite takes focus so the registry keys reach it
        tabIndex={0}
        aria-roledescription={t("timeline.roleDescription")}
        aria-label={t("timeline.keyboardHint")}
        className={cn(
          "relative min-w-0 flex-1 overflow-hidden rounded-control bg-preview outline-none",
          "focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent",
        )}
        style={{ height: TIMELINE_LAYOUT.height }}
        onPointerDown={onAreaPointerDown}
        onPointerMove={onAreaPointerMove}
        onPointerUp={onAreaPointerUp}
        onPointerCancel={onAreaPointerUp}
      >
        <canvas
          ref={canvasRef}
          className="pointer-events-none absolute inset-0 block"
          style={{ width: "100%", height: TIMELINE_LAYOUT.height }}
        />
        {handles}
        <CaptionBars
          scale={scale}
          duration={duration}
          captions={captions}
          selectedId={selection?.kind === "caption" ? selection.id : null}
          top={TIMELINE_LAYOUT.captionTop + 3}
          height={TIMELINE_LAYOUT.captionH - 6}
        />
        {/* Position and text are written by paintDrag while a handle is dragged; the element is always mounted. */}
        <div
          ref={readoutRef}
          aria-hidden="true"
          hidden={!readoutOpen}
          className="tabular pointer-events-none absolute z-20 -translate-x-1/2 rounded-control bg-overlay px-1.5 py-0.5 text-xs text-fg shadow-readout"
          style={{ top: 2 }}
        />
      </div>
    </div>
  );
}

interface TrimHandleProps {
  label: string;
  x: number;
  valueText: string;
  value: number;
  max: number;
  /** Receives the handle's element on mount and null on unmount, so a drag can move it without a render. */
  elementRef: (element: HTMLDivElement | null) => void;
  onFocus: () => void;
  onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerUp: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLDivElement>) => void;
}

/**
 * One in or out point: a grip tab, 6px wide over the clip height, centred in a 24px hit area. The tab is graphite
 * with a notch, so the accent stays on the playhead and the selection. The handle is reachable with Tab and takes
 * its timecode as its value.
 */
function TrimHandle({
  label,
  x,
  valueText,
  value,
  max,
  elementRef,
  onFocus,
  onPointerDown,
  onPointerMove,
  onPointerUp,
  onKeyDown,
}: TrimHandleProps) {
  return (
    <div
      ref={elementRef}
      role="slider"
      tabIndex={0}
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={Number(max.toFixed(3))}
      aria-valuenow={Number(value.toFixed(3))}
      aria-valuetext={valueText}
      className={cn(
        "group absolute z-10 flex w-6 -translate-x-1/2 cursor-ew-resize touch-none justify-center rounded-chip outline-none",
        "focus-visible:outline-2 focus-visible:outline-accent",
      )}
      style={{ left: x, top: TIMELINE_LAYOUT.clipTop, height: TIMELINE_LAYOUT.clipH }}
      onFocus={onFocus}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onKeyDown={onKeyDown}
    >
      <span
        aria-hidden="true"
        className="flex h-full w-1.5 items-center justify-center rounded-chip bg-fg group-hover:bg-fg-muted"
      >
        <span className="h-3 w-0.5 rounded-full bg-app" />
      </span>
    </div>
  );
}

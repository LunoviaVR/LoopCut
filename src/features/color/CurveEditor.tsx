import {
  type KeyboardEvent,
  type PointerEvent,
  type MouseEvent as ReactMouseEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import {
  editablePoints,
  evaluateCurve,
  insertCurvePoint,
  isEndpoint,
  moveCurvePoint,
  removeCurvePoint,
  sampleCurve,
} from "@/domain/curves";
import { readPalette, type TimelinePalette } from "@/features/timeline/draw";
import type { CurvePoint } from "@/ipc/bindings/CurvePoint";
import { useEditGesture } from "@/state/gesture";

const HEIGHT = 176;
/** Inset of the plot from the canvas edge, in CSS pixels, so endpoints and points are not clipped. */
const PAD = 10;
/** Distance within which a pointer grabs an existing point, in CSS pixels: a 24px target (WCAG 2.5.8). */
const HIT_RADIUS = 12;
const SAMPLES = 160;
const KEY_STEP = 0.01;
const KEY_STEP_FAST = 0.05;
/** Narrowest gap in x between two points that Insert adds a point into. Closer points would hide the new one. */
const MIN_GAP_TO_ADD = 0.02;

export interface CurveEditorProps {
  /** Names the curve for assistive technology, e.g. "Master". */
  label: string;
  points: readonly CurvePoint[];
  /**
   * The final points of one gesture: pointer up, a key press, a double-click removal. A live drag is painted from a
   * ref and reaches the grade only here, so one gesture is one write and one undo step.
   */
  onCommit: (points: CurvePoint[]) => void;
}

/** Plot coordinates (0 to 1 on both axes) to canvas pixels for a plot of `plotWidth` CSS pixels. */
function toPixel(point: { x: number; y: number }, plotWidth: number) {
  return {
    px: PAD + point.x * (plotWidth - 2 * PAD),
    py: HEIGHT - PAD - point.y * (HEIGHT - 2 * PAD),
  };
}

/** Paints the plot: grid, the identity diagonal, the spline and the points. Reads nothing but its arguments. */
function paintPlot(
  element: HTMLCanvasElement,
  list: CurvePoint[],
  active: number | null,
  plotWidth: number,
  colors: TimelinePalette,
): void {
  if (plotWidth <= 0) return;
  const context = element.getContext("2d");
  if (!context) return;
  const dpr = window.devicePixelRatio || 1;
  const pixelWidth = Math.round(plotWidth * dpr);
  const pixelHeight = Math.round(HEIGHT * dpr);
  if (element.width !== pixelWidth) element.width = pixelWidth;
  if (element.height !== pixelHeight) element.height = pixelHeight;
  context.setTransform(dpr, 0, 0, dpr, 0, 0);
  context.clearRect(0, 0, plotWidth, HEIGHT);
  context.fillStyle = colors.preview;
  context.fillRect(0, 0, plotWidth, HEIGHT);

  context.strokeStyle = colors.line;
  context.lineWidth = 1;
  for (let step = 1; step < 4; step += 1) {
    const gx = Math.round(toPixel({ x: step / 4, y: 0 }, plotWidth).px) + 0.5;
    const gy = Math.round(toPixel({ x: 0, y: step / 4 }, plotWidth).py) + 0.5;
    context.beginPath();
    context.moveTo(gx, 0);
    context.lineTo(gx, HEIGHT);
    context.moveTo(0, gy);
    context.lineTo(plotWidth, gy);
    context.stroke();
  }

  context.save();
  context.setLineDash([4, 4]);
  context.strokeStyle = colors.lineStrong;
  const diagonalStart = toPixel({ x: 0, y: 0 }, plotWidth);
  const diagonalEnd = toPixel({ x: 1, y: 1 }, plotWidth);
  context.beginPath();
  context.moveTo(diagonalStart.px, diagonalStart.py);
  context.lineTo(diagonalEnd.px, diagonalEnd.py);
  context.stroke();
  context.restore();

  const samples = sampleCurve(list, SAMPLES);
  // The curve is data, so it is drawn in the neutral foreground; accent marks only the selected point.
  context.strokeStyle = colors.fg;
  context.lineWidth = 2;
  context.beginPath();
  samples.forEach((y, index) => {
    const { px, py } = toPixel({ x: index / (samples.length - 1), y }, plotWidth);
    if (index === 0) context.moveTo(px, py);
    else context.lineTo(px, py);
  });
  context.stroke();

  list.forEach((point, index) => {
    const { px, py } = toPixel(point, plotWidth);
    context.beginPath();
    context.arc(px, py, index === active ? 5 : 4, 0, 2 * Math.PI);
    context.fillStyle = index === active ? colors.accent : colors.fg;
    context.fill();
  });
}

/**
 * Tone curve editor for one channel. A click on empty plot adds a point and drags it; dragging a point moves it
 * (endpoints move in y only); a double-click or Delete removes an interior point (a double-click that added its point
 * keeps it). Keyboard: arrows move the selected point (Shift for larger steps), Delete removes it, Insert (or the plus
 * key) adds a point midway between it and its neighbour, and PageUp and PageDown cycle the selection through the
 * points. The spline is drawn with the renderer's monotone cubic.
 *
 * A drag lives in a ref: the plot repaints from that ref once per animation frame, and the grade is written once, when
 * the gesture ends (pointer up, a key press, a double-click removal). The plot is one focusable application; the canvas
 * inside it is decorative.
 */
export function CurveEditor({ label, points, onCommit }: CurveEditorProps) {
  const { t } = useTranslation();
  const edit = useEditGesture();
  const canvas = useRef<HTMLCanvasElement>(null);
  const plot = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [selected, setSelected] = useState<number | null>(null);
  const [palette] = useState<TimelinePalette>(() => readPalette());
  const latest = useRef(points);
  latest.current = points;
  /** What the paint reads: the measured width, the selected index and the palette. Updated before every paint. */
  const view = useRef({ width: 0, selected: null as number | null, palette });
  /** The live points of the gesture in progress, or null when the plot shows the committed points. */
  const draft = useRef<CurvePoint[] | null>(null);
  const drag = useRef<{ pointerId: number; index: number } | null>(null);
  const frame = useRef<number | null>(null);
  /** The point the latest click sequence added on empty plot, or null. A double-click keeps it. */
  const added = useRef<number | null>(null);

  const shown = (): CurvePoint[] => draft.current ?? editablePoints(latest.current);

  /** Paints the plot from the refs: the draft while a gesture runs, the committed points otherwise. */
  const paintNow = useCallback(() => {
    const element = canvas.current;
    if (!element) return;
    const current = view.current;
    paintPlot(
      element,
      draft.current ?? editablePoints(latest.current),
      current.selected,
      current.width,
      current.palette,
    );
  }, []);

  /** Schedules at most one paint per animation frame. */
  const schedulePaint = () => {
    if (frame.current !== null) return;
    frame.current = window.requestAnimationFrame(() => {
      frame.current = null;
      paintNow();
    });
  };

  const select = (index: number | null) => {
    view.current.selected = index;
    setSelected(index);
  };

  // Measure the plot width so the canvas is painted at its real size.
  useEffect(() => {
    const element = plot.current;
    if (!element) return;
    const measure = () => setWidth(Math.round(element.getBoundingClientRect().width));
    measure();
    const observer = new ResizeObserver(() => measure());
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // Paint whenever what the plot shows changes: the committed points, the selection, the width.
  // biome-ignore lint/correctness/useExhaustiveDependencies: the committed points repaint the plot; paintNow reads its inputs through refs
  useEffect(() => {
    view.current = { width, selected, palette };
    paintNow();
  }, [points, selected, width, palette, paintNow]);

  useEffect(() => {
    return () => {
      if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    };
  }, []);

  /** Plot coordinates of a pointer event, clamped to the plot. */
  const plotPoint = (clientX: number, clientY: number): CurvePoint => {
    const rect = canvas.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return { x: 0, y: 0 };
    const x = (clientX - rect.left - PAD) / (rect.width - 2 * PAD);
    const y = (HEIGHT - (clientY - rect.top) - PAD) / (HEIGHT - 2 * PAD);
    return { x: Math.min(1, Math.max(0, x)), y: Math.min(1, Math.max(0, y)) };
  };

  /** The index of the point under a pointer, or -1. */
  const hitIndex = (clientX: number, clientY: number, list: CurvePoint[]): number => {
    const rect = canvas.current?.getBoundingClientRect();
    if (!rect) return -1;
    let best = -1;
    let bestDistance = HIT_RADIUS;
    list.forEach((point, index) => {
      const px = rect.left + PAD + point.x * (rect.width - 2 * PAD);
      const py = rect.top + HEIGHT - PAD - point.y * (HEIGHT - 2 * PAD);
      const distance = Math.hypot(clientX - px, clientY - py);
      if (distance <= bestDistance) {
        best = index;
        bestDistance = distance;
      }
    });
    return best;
  };

  /** Ends a gesture: the final points are written once, as one undo step. */
  const finish = (list: CurvePoint[]) => {
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    frame.current = null;
    draft.current = null;
    edit.commit(() => onCommit(list));
  };

  /**
   * Adds a point midway in x between the point at `index` and its neighbour (the right one, or the left one for the
   * last point). The new point sits on the curve, so the picture does not change until it is moved. It becomes the
   * selection, and the live line under the plot reads it out.
   */
  const addBeside = (list: CurvePoint[], index: number) => {
    const point = list[index];
    const neighbour = list[index + 1] ?? list[index - 1];
    if (!point || !neighbour || Math.abs(neighbour.x - point.x) < MIN_GAP_TO_ADD) return;
    const x = (point.x + neighbour.x) / 2;
    const inserted = insertCurvePoint(list, x, evaluateCurve(list, x));
    select(inserted.index);
    finish(inserted.points);
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const list = editablePoints(latest.current);
    const hit = hitIndex(event.clientX, event.clientY, list);
    let working: CurvePoint[];
    let index: number;
    if (hit >= 0) {
      working = list;
      index = hit;
      // A first press (detail 1) starts a new gesture. A second press of a double-click (detail 2) continues the one
      // that added the point, so the double-click keeps it.
      if (event.detail < 2) added.current = null;
    } else {
      const at = plotPoint(event.clientX, event.clientY);
      const inserted = insertCurvePoint(latest.current, at.x, at.y);
      working = inserted.points;
      index = inserted.index;
      added.current = inserted.index;
    }
    event.currentTarget.focus({ preventScroll: true });
    event.currentTarget.setPointerCapture?.(event.pointerId);
    drag.current = { pointerId: event.pointerId, index };
    draft.current = working;
    select(index);
    schedulePaint();
  };

  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    const at = plotPoint(event.clientX, event.clientY);
    draft.current = moveCurvePoint(shown(), current.index, at.x, at.y);
    schedulePaint();
  };

  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current || current.pointerId !== event.pointerId) return;
    drag.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    finish(shown());
  };

  /**
   * Removes the point under a double-click. A double-click that added the point (a press on empty plot, then a second
   * press on it) keeps it: removing it again would be a second undo step with no visible change.
   */
  const onDoubleClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    const list = editablePoints(latest.current);
    const hit = hitIndex(event.clientX, event.clientY, list);
    const justAdded = hit >= 0 && hit === added.current;
    added.current = null;
    if (hit < 0 || isEndpoint(list, hit) || justAdded) return;
    select(null);
    finish(removeCurvePoint(list, hit));
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const list = editablePoints(latest.current);
    if (list.length === 0) return;
    // PageUp and PageDown move the selection through the points; the live line under the plot announces it.
    if (event.key === "PageUp" || event.key === "PageDown") {
      event.preventDefault();
      const count = list.length;
      const current = view.current.selected;
      const step = event.key === "PageUp" ? 1 : -1;
      const next =
        current === null
          ? step > 0
            ? 0
            : count - 1
          : (((current + step) % count) + count) % count;
      select(next);
      return;
    }
    const index = view.current.selected ?? -1;
    const point = list[index];
    // Without a selected point the keys are not the editor's: they pass through (the playhead keeps them).
    if (!point) return;
    const isDelete = event.key === "Delete" || event.key === "Backspace";
    const isAdd = event.key === "Insert" || event.key === "+" || event.key === "=";
    const step = event.shiftKey ? KEY_STEP_FAST : KEY_STEP;
    const moves: Record<string, [number, number]> = {
      ArrowUp: [0, step],
      ArrowDown: [0, -step],
      ArrowLeft: [-step, 0],
      ArrowRight: [step, 0],
    };
    const move = moves[event.key];
    if (!isDelete && !isAdd && !move) return;
    event.preventDefault();
    if (isAdd) {
      addBeside(list, index);
      return;
    }
    if (isDelete) {
      if (isEndpoint(list, index)) return;
      select(null);
      finish(removeCurvePoint(list, index));
      return;
    }
    if (!move) return;
    finish(moveCurvePoint(list, index, point.x + move[0], point.y + move[1]));
  };

  const active = selected !== null ? editablePoints(points)[selected] : undefined;

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div
        ref={plot}
        role="application"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: the plot is a keyboard composite; it takes focus so its keys reach it
        tabIndex={0}
        aria-label={t("color.curves.plotLabel", { name: label })}
        aria-roledescription={t("color.curves.roleDescription")}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={onDoubleClick}
        onKeyDown={onKeyDown}
        className="w-full min-w-0 touch-none cursor-crosshair rounded-control outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      >
        <canvas ref={canvas} className="block" style={{ width: width || "100%", height: HEIGHT }} />
      </div>
      <p className="tabular min-h-4 text-xs text-fg-muted" aria-live="polite">
        {active
          ? t("color.curves.pointValue", {
              index: (selected ?? 0) + 1,
              total: editablePoints(points).length,
              input: active.x.toFixed(2),
              output: active.y.toFixed(2),
            })
          : t("color.curves.hint")}
      </p>
    </div>
  );
}

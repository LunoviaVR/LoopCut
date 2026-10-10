/**
 * Timeline geometry and time mapping. Pure functions, no DOM and no state, so the canvas, the handles and the
 * tests all agree.
 *
 * The ruler and the kept ranges are in **source time** (seconds of the clip). The playhead is an **output frame**
 * (the GIF's frame index); it is drawn at the source time that frame shows (`sourceTimeAtFrame`). Zoom 1 fits the
 * whole clip into the view; each zoom step multiplies the pixels per second.
 */
import { clamp } from "@/domain/numeric";
import { sourceTimeAtFrame } from "@/domain/timeline";
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";
import type { TimeRange } from "@/ipc/bindings/TimeRange";

/** Visible window of the timeline: the clip length, the zoom and the horizontal scroll (in CSS pixels). */
export interface ViewScale {
  /** Clip length in source seconds. */
  duration: number;
  /** Width of the visible area in CSS pixels. */
  viewWidth: number;
  zoom: number;
  scrollPx: number;
}

/** Edge snap distance in CSS pixels. Snapping is off while Shift is held. */
export const SNAP_PX = 6;

/** Pixels per source second at a zoom. Zoom 1 fits the whole clip into the view. */
export function pixelsPerSecond(duration: number, viewWidth: number, zoom: number): number {
  if (!(duration > 0) || !(viewWidth > 0) || !(zoom > 0)) return 0;
  return (viewWidth / duration) * zoom;
}

/** Largest scroll position that keeps the view inside the clip. */
export function maxScroll(scale: Pick<ViewScale, "duration" | "viewWidth" | "zoom">): number {
  const contentPx = scale.duration * pixelsPerSecond(scale.duration, scale.viewWidth, scale.zoom);
  return Math.max(0, contentPx - scale.viewWidth);
}

/** Scroll position clamped to the clip. */
export function clampScroll(
  scrollPx: number,
  scale: Pick<ViewScale, "duration" | "viewWidth" | "zoom">,
) {
  return clamp(scrollPx, 0, maxScroll(scale));
}

/** Source time at a horizontal position in the view (x = 0 is the left edge of the view). */
export function xToTime(x: number, scale: ViewScale): number {
  const pps = pixelsPerSecond(scale.duration, scale.viewWidth, scale.zoom);
  return pps > 0 ? (x + scale.scrollPx) / pps : 0;
}

/** Horizontal position in the view of a source time. */
export function timeToX(time: number, scale: ViewScale): number {
  return time * pixelsPerSecond(scale.duration, scale.viewWidth, scale.zoom) - scale.scrollPx;
}

/** The source time range the view currently shows. */
export function visibleRange(scale: ViewScale): { start: number; end: number } {
  return { start: xToTime(0, scale), end: xToTime(scale.viewWidth, scale) };
}

/**
 * Zoom around a cursor: the source time under `cursorX` stays under the cursor. `factor` multiplies the zoom
 * (above 1 zooms in). Returns the new zoom and the clamped scroll.
 */
export function zoomAround(
  scale: ViewScale,
  cursorX: number,
  factor: number,
  zoomRange: { min: number; max: number },
): { zoom: number; scrollPx: number } {
  const zoom = clamp(scale.zoom * factor, zoomRange.min, zoomRange.max);
  const anchor = xToTime(cursorX, scale);
  const next = { ...scale, zoom };
  const pps = pixelsPerSecond(next.duration, next.viewWidth, next.zoom);
  const scrollPx = clampScroll(anchor * pps - cursorX, next);
  return { zoom, scrollPx };
}

export interface SnapOptions {
  /** False while Shift is held: the time is returned unchanged. */
  enabled: boolean;
  /** Source frame rate; snapping targets the source frame grid. */
  sourceFps: number;
  /** Source time under the playhead, or null when no plan exists yet. */
  playheadTime: number | null;
  pixelsPerSecond: number;
  thresholdPx?: number;
}

/**
 * Snaps a source time to the nearest source frame boundary or to the playhead, whichever is closer and within the
 * threshold. The playhead wins a tie. Returns `time` unchanged when nothing is in range.
 */
export function snapTime(time: number, options: SnapOptions): number {
  const { enabled, sourceFps, playheadTime, pixelsPerSecond: pps } = options;
  if (!enabled || !(pps > 0)) return time;
  const threshold = (options.thresholdPx ?? SNAP_PX) / pps;
  let best = time;
  let bestDistance = threshold;
  if (playheadTime !== null) {
    const distance = Math.abs(playheadTime - time);
    if (distance <= bestDistance) {
      best = playheadTime;
      bestDistance = distance;
    }
  }
  if (sourceFps > 0) {
    const grid = Math.round(time * sourceFps) / sourceFps;
    const distance = Math.abs(grid - time);
    if (distance < bestDistance) best = grid;
  }
  return best;
}

const RULER_STEPS = [0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600] as const;

/** The smallest "nice" tick step whose spacing is at least `minSpacingPx`. */
export function rulerStep(pixelsPerSecondValue: number, minSpacingPx = 72): number {
  for (const step of RULER_STEPS) {
    if (step * pixelsPerSecondValue >= minSpacingPx) return step;
  }
  return RULER_STEPS[RULER_STEPS.length - 1] ?? 3600;
}

/** Tick times in `[start, end]` at multiples of `step`. Capped so a bad input cannot flood the canvas. */
export function rulerTicks(start: number, end: number, step: number): number[] {
  if (!(step > 0) || end < start) return [];
  const ticks: number[] = [];
  const first = Math.max(0, Math.ceil(start / step - 1e-9));
  for (let i = first; i * step <= end + 1e-9 && ticks.length < 500; i++) {
    ticks.push(Math.round(i * step * 1e6) / 1e6);
  }
  return ticks;
}

/** Ruler label: `m:ss` for whole-second steps (`h:mm:ss` from an hour), `ss.s` for finer steps. */
export function formatRulerLabel(seconds: number, step: number): string {
  if (step < 1) return `${seconds.toFixed(1)}s`;
  const total = Math.round(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor(total / 60) % 60;
  const secs = String(total % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${secs}` : `${minutes}:${secs}`;
}

/**
 * The output frame whose sample shows source time `time`. A frame that shows the nearest source time wins; equal
 * distances (a ping-pong turn, a crossfade) keep the frame closest to `nearFrame`. Null for an empty plan.
 */
export function frameForSourceTime(
  plan: SequencePlan,
  time: number,
  nearFrame: number,
): number | null {
  let best: number | null = null;
  let bestDistance = Number.POSITIVE_INFINITY;
  for (let frame = 0; frame < plan.frames.length; frame++) {
    const shown = sourceTimeAtFrame(plan, frame);
    if (shown === null) continue;
    const distance = Math.abs(shown - time);
    if (best === null || distance < bestDistance - 1e-9) {
      best = frame;
      bestDistance = distance;
    } else if (Math.abs(distance - bestDistance) <= 1e-9) {
      if (Math.abs(frame - nearFrame) < Math.abs(best - nearFrame)) best = frame;
    }
  }
  return best;
}

/** Source time shown by an output frame, or null when the plan has no such frame (or no plan exists). */
export function playheadSourceTime(plan: SequencePlan | null, frame: number): number | null {
  return plan ? sourceTimeAtFrame(plan, frame) : null;
}

/** The gaps between kept ranges, in source time: the removed areas drawn dimmed. */
export function removedRanges(segments: readonly TimeRange[], duration: number): TimeRange[] {
  const gaps: TimeRange[] = [];
  let cursor = 0;
  for (const segment of segments) {
    if (segment.start > cursor) gaps.push({ start: cursor, end: segment.start });
    cursor = Math.max(cursor, segment.end);
  }
  if (duration > cursor) gaps.push({ start: cursor, end: duration });
  return gaps;
}

/** A thumbnail's source span: from its own time to the next thumbnail's time (or the clip end). */
export function thumbnailSpan(
  times: readonly number[],
  index: number,
  duration: number,
): { start: number; end: number } {
  const start = times[index] ?? 0;
  const end = times[index + 1] ?? duration;
  return { start, end };
}

/** Thumbnail indices whose span overlaps `[start, end]`. */
export function visibleThumbnails(
  times: readonly number[],
  duration: number,
  start: number,
  end: number,
): number[] {
  const indices: number[] = [];
  for (let index = 0; index < times.length; index++) {
    const span = thumbnailSpan(times, index, duration);
    if (span.end >= start && span.start <= end) indices.push(index);
  }
  return indices;
}

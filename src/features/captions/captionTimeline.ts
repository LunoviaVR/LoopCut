/**
 * Caption ranges on the timeline: how a drag moves or resizes a caption's visible range, and where its bar sits. A
 * caption's range is in source time, like the kept ranges, so the timeline's mapping draws both on the same axis.
 */
import { clamp } from "@/domain/numeric";
import { MIN_SEGMENT_SECONDS } from "@/domain/timeline";
import { timeToX, type ViewScale } from "@/features/timeline/mapping";
import type { Caption } from "@/ipc/bindings/Caption";
import type { TimeRange } from "@/ipc/bindings/TimeRange";

/** Which part of a caption bar a pointer holds. */
export type CaptionPart = "move" | "start" | "end";

/** Width in CSS pixels of the grab zone at each end of a bar. */
export const EDGE_GRAB_PX = 6;
/** Narrowest bar drawn, so a very short caption stays clickable. */
const MIN_BAR_PX = 6;

/** A caption's range after a drag of `deltaSeconds` on one of its parts. The result stays on the clip. */
export function dragCaptionRange(
  original: TimeRange,
  part: CaptionPart,
  deltaSeconds: number,
  duration: number,
): TimeRange {
  const length = original.end - original.start;
  if (part === "move") {
    if (length >= duration) return { start: 0, end: duration };
    const start = clamp(original.start + deltaSeconds, 0, duration - length);
    return { start, end: start + length };
  }
  if (part === "start") {
    const start = clamp(original.start + deltaSeconds, 0, original.end - MIN_SEGMENT_SECONDS);
    return { start, end: original.end };
  }
  const end = clamp(original.end + deltaSeconds, original.start + MIN_SEGMENT_SECONDS, duration);
  return { start: original.start, end };
}

/** Sets a caption's start to `seconds`, keeping it inside the clip and before its end. */
export function setCaptionStart(range: TimeRange, seconds: number, duration: number): TimeRange {
  return dragCaptionRange(range, "start", seconds - range.start, duration);
}

/** Sets a caption's end to `seconds`, keeping it inside the clip and after its start. */
export function setCaptionEnd(range: TimeRange, seconds: number, duration: number): TimeRange {
  return dragCaptionRange(range, "end", seconds - range.end, duration);
}

/** Captions in the order they appear: by start time, then by id, so the list and the lane agree. */
export function captionsInOrder<T extends Pick<Caption, "id" | "range">>(
  captions: readonly T[],
): T[] {
  return [...captions].sort((a, b) => {
    const byStart = a.range.start - b.range.start;
    if (byStart !== 0) return byStart;
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
}

/** The part of a bar under an offset from its left edge: the grab zones at each end, the body otherwise. */
export function partAtOffset(offsetPx: number, widthPx: number): CaptionPart {
  if (offsetPx < EDGE_GRAB_PX && widthPx >= 2 * EDGE_GRAB_PX) return "start";
  if (offsetPx > widthPx - EDGE_GRAB_PX && widthPx >= 2 * EDGE_GRAB_PX) return "end";
  return "move";
}

/** Horizontal position and width of a caption bar in the timeline view, in CSS pixels. */
export function captionBarRect(
  range: TimeRange,
  scale: ViewScale,
): { left: number; width: number } {
  const left = timeToX(range.start, scale);
  const right = timeToX(range.end, scale);
  return { left, width: Math.max(MIN_BAR_PX, right - left) };
}

/** The first non-empty line of a caption, for the bar label. */
export function captionTitle(text: string): string {
  const line = text.split(/\r?\n/).find((item) => item.trim().length > 0);
  return line?.trim() ?? "";
}

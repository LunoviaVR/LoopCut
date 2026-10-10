/** Timeline helpers shared by the store and the commands. Pure functions, no state. */
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";
import type { TimeRange } from "@/ipc/bindings/TimeRange";

/** Shortest kept range the editor allows, in source seconds. */
export const MIN_SEGMENT_SECONDS = 0.05;

/**
 * The kept range `index` takes when its edges are set to `start` and `end`: the start stays between the previous
 * range's end and the next range's start, and the end stays after the start by the minimum length. Null when there
 * is no such range. The store and the timeline's live drag share this rule, so a drag shows what a commit keeps.
 */
export function clampSegmentRange(
  segments: readonly TimeRange[],
  index: number,
  start: number,
  end: number,
): TimeRange | null {
  if (!segments[index]) return null;
  const previous = segments[index - 1];
  const next = segments[index + 1];
  const lower = previous ? previous.end : 0;
  const upper = next ? next.start : Number.POSITIVE_INFINITY;
  const nextStart = Math.min(Math.max(start, lower), upper - MIN_SEGMENT_SECONDS);
  return {
    start: nextStart,
    end: Math.min(Math.max(end, nextStart + MIN_SEGMENT_SECONDS), upper),
  };
}

/** Index of the kept range that contains source `time` (half-open `[start, end)`), or -1. */
export function segmentIndexAt(segments: readonly TimeRange[], time: number): number {
  return segments.findIndex((segment) => time >= segment.start && time < segment.end);
}

/**
 * Source time displayed by output frame `frame` of a sequencer plan, or null when the frame is out of
 * range. A crossfade frame blends two source times; this returns the first one (`a`).
 */
export function sourceTimeAtFrame(plan: SequencePlan, frame: number): number | null {
  if (!Number.isInteger(frame)) return null;
  const sample = plan.frames[frame]?.sample;
  if (!sample) return null;
  return sample.kind === "at" ? sample.t : sample.a;
}

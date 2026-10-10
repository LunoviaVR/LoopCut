import { describe, expect, it } from "vitest";
import { MIN_SEGMENT_SECONDS } from "@/domain/timeline";
import type { ViewScale } from "@/features/timeline/mapping";
import {
  captionBarRect,
  captionsInOrder,
  captionTitle,
  dragCaptionRange,
  EDGE_GRAB_PX,
  partAtOffset,
  setCaptionEnd,
  setCaptionStart,
} from "./captionTimeline";

const DURATION = 12;
const range = { start: 2, end: 5 };

describe("dragging a caption range", () => {
  it("moves the whole range and keeps its length", () => {
    expect(dragCaptionRange(range, "move", 1.5, DURATION)).toEqual({ start: 3.5, end: 6.5 });
  });

  it("keeps a moved range on the clip", () => {
    expect(dragCaptionRange(range, "move", -10, DURATION)).toEqual({ start: 0, end: 3 });
    expect(dragCaptionRange(range, "move", 20, DURATION)).toEqual({ start: 9, end: 12 });
  });

  it("fills the clip when the caption is longer than it", () => {
    expect(dragCaptionRange({ start: 0, end: 20 }, "move", 3, DURATION)).toEqual({
      start: 0,
      end: DURATION,
    });
  });

  it("moves the start but never past the end", () => {
    expect(dragCaptionRange(range, "start", 1, DURATION)).toEqual({ start: 3, end: 5 });
    expect(dragCaptionRange(range, "start", 9, DURATION).start).toBeCloseTo(
      5 - MIN_SEGMENT_SECONDS,
      12,
    );
    expect(dragCaptionRange(range, "start", -9, DURATION).start).toBe(0);
  });

  it("moves the end but never before the start or past the clip", () => {
    expect(dragCaptionRange(range, "end", -1, DURATION)).toEqual({ start: 2, end: 4 });
    expect(dragCaptionRange(range, "end", -9, DURATION).end).toBeCloseTo(
      2 + MIN_SEGMENT_SECONDS,
      12,
    );
    expect(dragCaptionRange(range, "end", 9, DURATION).end).toBe(DURATION);
  });

  it("sets a start or an end to a time, with the same limits", () => {
    expect(setCaptionStart(range, 1, DURATION)).toEqual({ start: 1, end: 5 });
    expect(setCaptionEnd(range, 8, DURATION)).toEqual({ start: 2, end: 8 });
    expect(setCaptionEnd(range, 0, DURATION).end).toBeCloseTo(2 + MIN_SEGMENT_SECONDS, 12);
  });
});

describe("caption order and bars", () => {
  it("orders captions by start time, then by id", () => {
    const captions = [
      { id: "b", range: { start: 4, end: 5 } },
      { id: "c", range: { start: 1, end: 2 } },
      { id: "a", range: { start: 4, end: 6 } },
    ];
    expect(captionsInOrder(captions).map((item) => item.id)).toEqual(["c", "a", "b"]);
  });

  it("reads the part under the pointer: grab zones at the ends, the body otherwise", () => {
    expect(partAtOffset(2, 100)).toBe("start");
    expect(partAtOffset(50, 100)).toBe("move");
    expect(partAtOffset(100 - 2, 100)).toBe("end");
    expect(partAtOffset(2, 10)).toBe("move");
    expect(EDGE_GRAB_PX).toBeGreaterThan(0);
  });

  it("places a bar at its range and keeps a short bar clickable", () => {
    const scale: ViewScale = { duration: 10, viewWidth: 1000, zoom: 1, scrollPx: 0 };
    expect(captionBarRect({ start: 2, end: 4 }, scale)).toEqual({ left: 200, width: 200 });
    expect(captionBarRect({ start: 2, end: 2.001 }, scale).width).toBeGreaterThanOrEqual(6);
  });

  it("titles a caption by its first non-empty line", () => {
    expect(captionTitle("\n  \nFirst line\nSecond")).toBe("First line");
    expect(captionTitle("   ")).toBe("");
  });
});

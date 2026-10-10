import { describe, expect, it } from "vitest";
import { buildSequencePlan } from "@/domain/sequence";
import { sampleProject } from "@/state/test-fixtures";
import {
  clampScroll,
  formatRulerLabel,
  frameForSourceTime,
  maxScroll,
  pixelsPerSecond,
  playheadSourceTime,
  removedRanges,
  rulerStep,
  rulerTicks,
  snapTime,
  thumbnailSpan,
  timeToX,
  type ViewScale,
  visibleRange,
  visibleThumbnails,
  xToTime,
  zoomAround,
} from "./mapping";

const scale = (overrides: Partial<ViewScale> = {}): ViewScale => ({
  duration: 10,
  viewWidth: 500,
  zoom: 1,
  scrollPx: 0,
  ...overrides,
});

describe("geometry", () => {
  it("fits the whole clip at zoom 1 and scales with the zoom", () => {
    expect(pixelsPerSecond(10, 500, 1)).toBe(50);
    expect(pixelsPerSecond(10, 500, 2)).toBe(100);
    expect(pixelsPerSecond(0, 500, 1)).toBe(0);
  });

  it("maps time and x both ways, honouring the scroll", () => {
    const view = scale({ zoom: 2, scrollPx: 100 });
    expect(timeToX(3, view)).toBeCloseTo(3 * 100 - 100);
    expect(xToTime(timeToX(4.5, view), view)).toBeCloseTo(4.5);
  });

  it("reports the visible time range", () => {
    const range = visibleRange(scale({ zoom: 2, scrollPx: 0 }));
    expect(range.start).toBeCloseTo(0);
    expect(range.end).toBeCloseTo(5);
  });

  it("keeps the scroll inside the clip", () => {
    expect(maxScroll(scale({ zoom: 1 }))).toBe(0);
    expect(maxScroll(scale({ zoom: 2 }))).toBe(500);
    expect(clampScroll(-40, scale({ zoom: 2 }))).toBe(0);
    expect(clampScroll(900, scale({ zoom: 2 }))).toBe(500);
  });

  it("zooms around the cursor so the time under it stays put", () => {
    const before = scale({ zoom: 1, scrollPx: 0 });
    const cursor = 250;
    const anchor = xToTime(cursor, before);
    const next = zoomAround(before, cursor, 2, { min: 0.25, max: 16 });
    expect(next.zoom).toBe(2);
    expect(xToTime(cursor, { ...before, zoom: next.zoom, scrollPx: next.scrollPx })).toBeCloseTo(
      anchor,
    );
  });

  it("clamps the zoom to its range", () => {
    expect(zoomAround(scale({ zoom: 16 }), 0, 4, { min: 0.25, max: 16 }).zoom).toBe(16);
    expect(zoomAround(scale({ zoom: 0.25 }), 0, 0.5, { min: 0.25, max: 16 }).zoom).toBe(0.25);
  });
});

describe("snapping", () => {
  const base = { enabled: true, sourceFps: 30, playheadTime: null, pixelsPerSecond: 100 };

  it("snaps to the nearest source frame boundary within the threshold", () => {
    expect(snapTime(1.01, base)).toBeCloseTo(1);
    expect(snapTime(1.02, base)).toBeCloseTo(1.0333, 3);
  });

  it("leaves a time alone when it is far from every target, or snapping is off", () => {
    expect(snapTime(1.3, { ...base, sourceFps: 0 })).toBe(1.3);
    expect(snapTime(1.01, { ...base, enabled: false })).toBe(1.01);
  });

  it("snaps to the playhead when it is the closest target", () => {
    expect(snapTime(2.02, { ...base, sourceFps: 0, playheadTime: 2.0 })).toBe(2);
    expect(snapTime(2.2, { ...base, sourceFps: 0, playheadTime: 2.0 })).toBe(2.2);
  });
});

describe("ruler", () => {
  it("picks the smallest nice step that keeps labels apart", () => {
    expect(rulerStep(100, 72)).toBe(1);
    expect(rulerStep(1000, 72)).toBe(0.1);
    expect(rulerStep(0.01, 72)).toBe(3600);
  });

  it("lists tick times inside the range at multiples of the step", () => {
    expect(rulerTicks(0.9, 3.1, 1)).toEqual([1, 2, 3]);
    expect(rulerTicks(0, 1, 0.5)).toEqual([0, 0.5, 1]);
  });

  it("formats labels as clock time, or seconds for fine steps", () => {
    expect(formatRulerLabel(65, 1)).toBe("1:05");
    expect(formatRulerLabel(3661, 1)).toBe("1:01:01");
    expect(formatRulerLabel(1.5, 0.5)).toBe("1.5s");
  });
});

describe("output frame and source time", () => {
  const plan = buildSequencePlan(sampleProject());
  if (!plan) throw new Error("plan");

  it("finds the output frame that shows a source time", () => {
    expect(frameForSourceTime(plan, 2, 0)).toBe(40);
    expect(frameForSourceTime(plan, 2.02, 0)).toBe(40);
    expect(frameForSourceTime(plan, 2.04, 0)).toBe(41);
  });

  it("maps a time inside a removed area to the nearest kept frame", () => {
    expect(frameForSourceTime(plan, 9, 0)).toBe(119);
    expect(frameForSourceTime(plan, -3, 50)).toBe(0);
  });

  it("returns the source time of the playhead, or null without a plan", () => {
    expect(playheadSourceTime(plan, 40)).toBeCloseTo(2);
    expect(playheadSourceTime(null, 40)).toBeNull();
  });
});

describe("removed areas and thumbnails", () => {
  it("lists the gaps between kept ranges, including both ends", () => {
    expect(
      removedRanges(
        [
          { start: 0, end: 2 },
          { start: 4, end: 6 },
        ],
        10,
      ),
    ).toEqual([
      { start: 2, end: 4 },
      { start: 6, end: 10 },
    ]);
    expect(removedRanges([{ start: 0, end: 5 }], 5)).toEqual([]);
    expect(removedRanges([], 5)).toEqual([{ start: 0, end: 5 }]);
  });

  it("gives each thumbnail the span up to the next one", () => {
    expect(thumbnailSpan([0, 2, 4], 1, 6)).toEqual({ start: 2, end: 4 });
    expect(thumbnailSpan([0, 2, 4], 2, 6)).toEqual({ start: 4, end: 6 });
  });

  it("keeps only the thumbnails that overlap the visible range", () => {
    expect(visibleThumbnails([0, 2, 4], 6, 2.5, 3)).toEqual([1]);
  });
});

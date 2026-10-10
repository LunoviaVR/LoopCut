import { describe, expect, it } from "vitest";
import { clampSegmentRange, segmentIndexAt, sourceTimeAtFrame } from "@/domain/timeline";
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";

describe("segmentIndexAt", () => {
  const segments = [
    { start: 0, end: 2 },
    { start: 3, end: 5 },
  ];

  it("finds the kept range that contains a time (start included, end excluded)", () => {
    expect(segmentIndexAt(segments, 0)).toBe(0);
    expect(segmentIndexAt(segments, 3)).toBe(1);
    expect(segmentIndexAt(segments, 2)).toBe(-1);
    expect(segmentIndexAt(segments, 5)).toBe(-1);
    expect(segmentIndexAt([], 1)).toBe(-1);
  });
});

describe("sourceTimeAtFrame", () => {
  const plan: SequencePlan = {
    fps: { num: 20, den: 1 },
    duration: 0.1,
    frames: [
      { sample: { kind: "at", t: 0.5 }, delayCs: 5 },
      { sample: { kind: "blend", a: 1, b: 2, weightB: 0.5 }, delayCs: 5 },
    ],
  };

  it("returns the source time of a plain frame", () => {
    expect(sourceTimeAtFrame(plan, 0)).toBe(0.5);
  });

  it("returns the first source time of a crossfade frame", () => {
    expect(sourceTimeAtFrame(plan, 1)).toBe(1);
  });

  it("returns null outside the plan and for non-integer frames", () => {
    expect(sourceTimeAtFrame(plan, 2)).toBeNull();
    expect(sourceTimeAtFrame(plan, -1)).toBeNull();
    expect(sourceTimeAtFrame(plan, 0.5)).toBeNull();
  });
});

describe("clampSegmentRange", () => {
  const segments = [
    { start: 0, end: 2 },
    { start: 3, end: 5 },
  ];

  it("keeps an edge between its neighbours and the minimum length", () => {
    expect(clampSegmentRange(segments, 1, 1, 4)).toEqual({ start: 2, end: 4 });
    expect(clampSegmentRange(segments, 1, 4.9, 5)).toEqual({ start: 4.9, end: 5 });
    expect(clampSegmentRange(segments, 0, -1, 9)).toEqual({ start: 0, end: 3 });
  });

  it("returns null for a range that does not exist", () => {
    expect(clampSegmentRange(segments, 2, 0, 1)).toBeNull();
  });
});

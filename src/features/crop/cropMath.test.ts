import { describe, expect, it } from "vitest";
import type { NormRect } from "@/ipc/bindings/NormRect";
import {
  aspectValue,
  CROP_HANDLES,
  CROP_MIN_SIZE,
  clampRect,
  displaySize,
  fitRectToAspect,
  flipRect,
  fromPixels,
  invertAspect,
  moveRect,
  nudgeRect,
  quarterTurns,
  reduceRatio,
  resizeRect,
  rotateRectCw,
  setPixelField,
  thirdsLines,
  toPixels,
} from "./cropMath";

const square = { width: 1000, height: 1000 };
const hd = { width: 1920, height: 1080 };
const rect = (x: number, y: number, w: number, h: number): NormRect => ({ x, y, w, h });

/** Width over height in display pixels. */
const pixelRatio = (r: NormRect, size: { width: number; height: number }) =>
  (r.w * size.width) / (r.h * size.height);

/** Field-by-field comparison with float tolerance; rectangles rarely come out bit-exact. */
function expectRect(actual: NormRect, expected: NormRect): void {
  expect(actual.x).toBeCloseTo(expected.x, 9);
  expect(actual.y).toBeCloseTo(expected.y, 9);
  expect(actual.w).toBeCloseTo(expected.w, 9);
  expect(actual.h).toBeCloseTo(expected.h, 9);
}

const inside = (r: NormRect) =>
  r.x >= -1e-9 && r.y >= -1e-9 && r.x + r.w <= 1 + 1e-9 && r.y + r.h <= 1 + 1e-9;

describe("display size and rotation", () => {
  it("swaps width and height on a quarter turn of the source or the user", () => {
    expect(displaySize({ width: 1920, height: 1080, rotation: 0 }, 0)).toEqual({
      width: 1920,
      height: 1080,
    });
    expect(displaySize({ width: 1920, height: 1080, rotation: 0 }, 90)).toEqual({
      width: 1080,
      height: 1920,
    });
    expect(displaySize({ width: 1920, height: 1080, rotation: -90 }, 0)).toEqual({
      width: 1080,
      height: 1920,
    });
  });

  it("does not swap on a half turn, and two quarter turns cancel", () => {
    expect(displaySize({ width: 1920, height: 1080, rotation: 90 }, 90)).toEqual({
      width: 1920,
      height: 1080,
    });
    expect(displaySize({ width: 1920, height: 1080, rotation: 0 }, 180)).toEqual({
      width: 1920,
      height: 1080,
    });
  });

  it("normalizes any angle to a quarter turn", () => {
    expect(quarterTurns(-90)).toBe(270);
    expect(quarterTurns(450)).toBe(90);
    expect(quarterTurns(0)).toBe(0);
  });

  it("maps a rectangle through 90, 180 and 270 degree turns, and back", () => {
    const start = rect(0.1, 0.2, 0.3, 0.4);
    expectRect(rotateRectCw(start, 0), start);
    expectRect(rotateRectCw(start, 90), rect(0.4, 0.1, 0.4, 0.3));
    expectRect(rotateRectCw(start, 180), rect(0.6, 0.4, 0.3, 0.4));
    expectRect(rotateRectCw(start, 270), rect(0.2, 0.6, 0.4, 0.3));
    expectRect(rotateRectCw(rotateRectCw(start, 90), -90), start);
    const full = rotateRectCw(rotateRectCw(rotateRectCw(rotateRectCw(start, 90), 90), 90), 90);
    expect(full.x).toBeCloseTo(start.x);
    expect(full.y).toBeCloseTo(start.y);
  });

  it("keeps the same picture content under a quarter turn: the top-left corner moves to the top-right", () => {
    const start = rect(0.1, 0.2, 0.3, 0.4);
    const turned = rotateRectCw(start, 90);
    expect(turned.x + turned.w).toBeCloseTo(1 - start.y);
    expect(turned.y).toBeCloseTo(start.x);
  });

  it("mirrors a rectangle across an axis", () => {
    expectRect(flipRect(rect(0.1, 0.2, 0.3, 0.4), "horizontal"), rect(0.6, 0.2, 0.3, 0.4));
    expectRect(flipRect(rect(0.1, 0.2, 0.3, 0.4), "vertical"), rect(0.1, 0.4, 0.3, 0.4));
  });
});

describe("clamping and moving", () => {
  it("keeps the rectangle inside the frame with a minimum size", () => {
    expectRect(clampRect(rect(0.9, 0.9, 0.5, 0.5)), rect(0.5, 0.5, 0.5, 0.5));
    expect(clampRect(rect(0, 0, 0, 0)).w).toBe(CROP_MIN_SIZE);
  });

  it("moves without leaving the frame", () => {
    expectRect(moveRect(rect(0.2, 0.2, 0.3, 0.3), -0.5, 0.1), rect(0, 0.3, 0.3, 0.3));
  });

  it("nudges by display pixels", () => {
    const moved = nudgeRect(rect(0.1, 0.1, 0.5, 0.5), 1, -10, square);
    expect(moved.x).toBeCloseTo(0.101);
    expect(moved.y).toBeCloseTo(0.09);
  });
});

describe("resizing", () => {
  it("moves the dragged corner freely without an aspect", () => {
    const next = resizeRect(rect(0.1, 0.1, 0.4, 0.4), "se", { x: 0.8, y: 0.9 }, square, null);
    expect(next.x).toBeCloseTo(0.1);
    expect(next.y).toBeCloseTo(0.1);
    expect(next.w).toBeCloseTo(0.7);
    expect(next.h).toBeCloseTo(0.8);
  });

  it("moves the dragged edge alone without an aspect", () => {
    const next = resizeRect(rect(0.1, 0.1, 0.4, 0.4), "e", { x: 0.6, y: 0.9 }, square, null);
    expectRect(next, rect(0.1, 0.1, 0.5, 0.4));
  });

  it.each(CROP_HANDLES)(
    "keeps a 16:9 lock from the %s handle and stays inside the frame",
    (handle) => {
      const aspect = 16 / 9;
      const start = rect(0.2, 0.2, 0.3, 0.3);
      const pointers = [
        { x: 0.7, y: 0.45 },
        { x: 0.35, y: 0.8 },
        { x: 0.05, y: 0.1 },
        { x: 0.95, y: 0.95 },
      ];
      for (const pointer of pointers) {
        const next = resizeRect(start, handle, pointer, hd, aspect);
        expect(pixelRatio(next, hd)).toBeCloseTo(aspect, 6);
        expect(inside(next)).toBe(true);
        expect(next.w).toBeGreaterThanOrEqual(CROP_MIN_SIZE - 1e-9);
      }
    },
  );

  it("grows a corner box to whichever dimension the pointer reaches first", () => {
    const next = resizeRect(rect(0.1, 0.1, 0.4, 0.4), "se", { x: 0.8, y: 0.6 }, square, 1);
    expect(next.w).toBeCloseTo(next.h, 9);
    expect(next.w).toBeCloseTo(0.7);
    expect(next.x).toBeCloseTo(0.1);
    expect(next.y).toBeCloseTo(0.1);
  });

  it("keeps the opposite corner fixed when a corner is dragged with an aspect", () => {
    const start = rect(0.2, 0.2, 0.3, 0.3);
    const next = resizeRect(start, "nw", { x: 0.05, y: 0.05 }, hd, 16 / 9);
    expect(next.x + next.w).toBeCloseTo(start.x + start.w);
    expect(next.y + next.h).toBeCloseTo(start.y + start.h);
  });

  it("centres the other axis when an edge is dragged with an aspect", () => {
    const start = rect(0.2, 0.2, 0.3, 0.3);
    const next = resizeRect(start, "e", { x: 0.8, y: 0.9 }, square, 1);
    expect(next.y + next.h / 2).toBeCloseTo(start.y + start.h / 2);
    expect(next.w).toBeCloseTo(next.h);
  });

  it("clamps a drag past the frame edge to the edge", () => {
    const next = resizeRect(rect(0.2, 0.2, 0.3, 0.3), "e", { x: 1.6, y: 0.3 }, square, null);
    expect(next.x + next.w).toBeCloseTo(1);
  });
});

describe("aspect presets and numeric fields", () => {
  it("shapes the crop to an aspect, centred on the old centre", () => {
    const next = fitRectToAspect(rect(0, 0, 1, 1), 1, hd);
    expect(pixelRatio(next, hd)).toBeCloseTo(1, 6);
    expect(next.x + next.w / 2).toBeCloseTo(0.5);
    expect(next.y + next.h / 2).toBeCloseTo(0.5);
    expect(next.h).toBeCloseTo(1);
  });

  it("changes a pixel field and keeps the aspect when one is set", () => {
    const start = rect(0, 0, 0.5, 0.5);
    const next = setPixelField(start, "width", 400, square, 2);
    expect(toPixels(next, square).width).toBe(400);
    expect(toPixels(next, square).height).toBe(200);
  });

  it("keeps every field inside the frame", () => {
    expect(setPixelField(rect(0, 0, 0.5, 0.5), "x", -50, square, null).x).toBe(0);
    const pushed = setPixelField(rect(0.5, 0.5, 0.2, 0.2), "x", 900, square, null);
    expect(pushed.x + pushed.w).toBeLessThanOrEqual(1);
  });

  it("converts to pixels and back", () => {
    const pixels = toPixels(rect(0.25, 0.5, 0.5, 0.25), hd);
    expect(pixels).toEqual({ x: 480, y: 540, width: 960, height: 270 });
    expectRect(fromPixels(pixels, hd), rect(0.25, 0.5, 0.5, 0.25));
  });
});

describe("aspect helpers and guides", () => {
  it("reduces ratios and inverts them on a turn", () => {
    expect(reduceRatio(1920, 1080)).toEqual({ num: 16, den: 9 });
    expect(invertAspect({ num: 16, den: 9 })).toEqual({ num: 9, den: 16 });
    expect(invertAspect(null)).toBeNull();
    expect(aspectValue({ num: 4, den: 5 })).toBeCloseTo(0.8);
    expect(aspectValue(null)).toBeNull();
  });

  it("places the thirds lines inside the crop", () => {
    const lines = thirdsLines(rect(0.3, 0.3, 0.6, 0.3));
    expect(lines.vertical[0]).toBeCloseTo(0.5);
    expect(lines.vertical[1]).toBeCloseTo(0.7);
    expect(lines.horizontal[0]).toBeCloseTo(0.4);
    expect(lines.horizontal[1]).toBeCloseTo(0.5);
  });
});

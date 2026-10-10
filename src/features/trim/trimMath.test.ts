import { describe, expect, it } from "vitest";
import { sampleProject } from "@/state/test-fixtures";
import {
  derivedHeight,
  fpsChoice,
  fpsHint,
  keptRange,
  keptSeconds,
  loopModeOf,
  maxCrossfadeSeconds,
  outputSeconds,
  rationalFromFps,
} from "./trimMath";

describe("kept range", () => {
  it("spans from the first segment's start to the last segment's end, across cuts", () => {
    const project = sampleProject();
    expect(keptRange(project)).toEqual({ start: 0, end: 6 });

    const clip = sampleProject().clips[0];
    if (!clip) throw new Error("the sample project has a clip");
    const cut = sampleProject({
      clips: [
        {
          ...clip,
          segments: [
            { start: 1, end: 2 },
            { start: 3, end: 5.5 },
          ],
        },
      ],
    });
    expect(keptRange(cut)).toEqual({ start: 1, end: 5.5 });
  });

  it("is null when the clip has no segment", () => {
    const clip = sampleProject().clips[0];
    if (!clip) throw new Error("the sample project has a clip");
    expect(keptRange(sampleProject({ clips: [{ ...clip, segments: [] }] }))).toBeNull();
  });
});

describe("frame rate choices", () => {
  it("names the preset a rate matches, or custom", () => {
    expect(fpsChoice({ num: 25, den: 2 })).toBe("12.5");
    expect(fpsChoice({ num: 20, den: 1 })).toBe("20");
    expect(fpsChoice({ num: 24, den: 1 })).toBe("custom");
  });

  it("turns preset and decimal values into exact rationals", () => {
    expect(rationalFromFps(12.5)).toEqual({ num: 25, den: 2 });
    expect(rationalFromFps(20)).toEqual({ num: 20, den: 1 });
    expect(rationalFromFps(23.976)).toEqual({ num: 1199, den: 50 });
  });

  it("says nothing for an exact GIF rate, and what the GIF really plays at otherwise", () => {
    expect(fpsHint({ num: 20, den: 1 }).exact).toBe(true);
    const hint = fpsHint({ num: 24, den: 1 });
    expect(hint.exact).toBe(false);
    expect(Math.abs(hint.effective - 24)).toBeLessThan(0.5);
    expect(hint.clamped).toBe(false);
  });

  it("flags a rate above the GIF ceiling", () => {
    expect(fpsHint({ num: 60, den: 1 }).clamped).toBe(true);
  });
});

describe("output size and length", () => {
  it("derives an even height from the width and the display shape", () => {
    expect(derivedHeight(480, { width: 1920, height: 1080 })).toBe(270);
    expect(derivedHeight(321, { width: 1000, height: 1000 })).toBe(322);
    expect(derivedHeight(0, { width: 1920, height: 1080 })).toBe(2);
  });

  it("computes the kept and output seconds, and the largest crossfade", () => {
    const project = sampleProject();
    expect(keptSeconds(project)).toBeCloseTo(6);
    expect(outputSeconds(project)).toBeCloseTo(6);
    expect(maxCrossfadeSeconds(project)).toBeCloseTo(3);
    const fast = { ...project, timing: { ...project.timing, speed: 2 } };
    expect(outputSeconds(fast)).toBeCloseTo(3);
    expect(maxCrossfadeSeconds(fast)).toBeCloseTo(1.5);
  });
});

describe("loop count", () => {
  it("maps the count to forever, once or a number of plays", () => {
    expect(loopModeOf(0)).toBe("forever");
    expect(loopModeOf(1)).toBe("once");
    expect(loopModeOf(3)).toBe("times");
  });
});

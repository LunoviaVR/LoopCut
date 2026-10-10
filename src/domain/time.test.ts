import { describe, expect, it } from "vitest";
import {
  clampGifFps,
  effectiveGifFps,
  formatDuration,
  formatFps,
  formatTimecode,
  frameToTime,
  gifDelaysCs,
  isValidRational,
  snapFps,
  timeToFrame,
  toFloat,
} from "@/domain/time";

describe("formatFps", () => {
  it("reads a rational as a number rounded to two decimals", () => {
    expect(formatFps({ num: 20, den: 1 })).toBe(20);
    expect(formatFps({ num: 30000, den: 1001 })).toBe(29.97);
    expect(formatFps({ num: 25, den: 2 })).toBe(12.5);
  });

  it("reads an invalid rate as 0", () => {
    expect(formatFps({ num: 0, den: 1 })).toBe(0);
    expect(formatFps({ num: 30, den: 0 })).toBe(0);
  });
});

describe("rational rates", () => {
  it("reads a rational as frames per second", () => {
    expect(toFloat({ num: 25, den: 2 })).toBe(12.5);
    expect(toFloat({ num: 30000, den: 1001 })).toBeCloseTo(29.97, 2);
    expect(toFloat({ num: 0, den: 1 })).toBe(0);
  });

  it("rejects non-positive or fractional parts", () => {
    expect(isValidRational({ num: 20, den: 1 })).toBe(true);
    expect(isValidRational({ num: 0, den: 1 })).toBe(false);
    expect(isValidRational({ num: 20, den: 0 })).toBe(false);
    expect(isValidRational({ num: 2.5, den: 1 })).toBe(false);
  });

  it("maps frames to times with exact rationals", () => {
    expect(frameToTime(30, { num: 30, den: 1 })).toBe(1);
    expect(frameToTime(3, { num: 25, den: 2 })).toBeCloseTo(0.24, 12);
    expect(frameToTime(5, { num: 0, den: 1 })).toBe(0);
  });

  it("maps times to frames with floor and an epsilon", () => {
    const twelvePointFive = { num: 25, den: 2 };
    expect(timeToFrame(0.96, twelvePointFive)).toBe(12);
    expect(timeToFrame(1.04, twelvePointFive)).toBe(13);
    // 2.9999999 frames is effectively frame 3, not frame 2.
    expect(timeToFrame(2.9999999, { num: 1, den: 1 })).toBe(3);
    expect(timeToFrame(2.9, { num: 1, den: 1 })).toBe(2);
    // 0.5 s at 29.97 fps is 14.985 frames, so frame 14 covers it.
    expect(timeToFrame(0.5, { num: 30000, den: 1001 })).toBe(14);
  });
});

describe("snapFps", () => {
  it("snaps to the nearest exact GIF rate", () => {
    expect(snapFps({ num: 24, den: 1 })).toEqual({ num: 25, den: 1 });
    expect(snapFps({ num: 15, den: 1 })).toEqual({ num: 25, den: 2 });
    expect(snapFps({ num: 30, den: 1 })).toEqual({ num: 25, den: 1 });
    expect(snapFps({ num: 7, den: 1 })).toEqual({ num: 5, den: 1 });
    expect(snapFps({ num: 60, den: 1 })).toEqual({ num: 50, den: 1 });
  });

  it("breaks ties toward the earlier list entry, as Rust's min_by does", () => {
    // 22.5 is 2.5 away from both 25 and 20; 25 comes first in the list.
    expect(snapFps({ num: 45, den: 2 })).toEqual({ num: 25, den: 1 });
  });
});

describe("GIF timing", () => {
  it("gives uniform delays for exact rates", () => {
    expect(gifDelaysCs(10, { num: 20, den: 1 })).toEqual(Array(10).fill(5));
    expect(gifDelaysCs(10, { num: 25, den: 2 })).toEqual(Array(10).fill(8));
  });

  it("alternates 6 and 7 centiseconds at 15 fps and keeps the total exact", () => {
    const delays = gifDelaysCs(15, { num: 15, den: 1 });
    expect(delays.reduce((sum, delay) => sum + delay, 0)).toBe(100);
    expect(delays.every((delay) => delay === 6 || delay === 7)).toBe(true);
    expect(delays).toEqual([7, 6, 7, 7, 6, 7, 7, 6, 7, 7, 6, 7, 7, 6, 7]);
  });

  it("caps the rate at 50 fps and never emits a delay below 2 cs", () => {
    expect(clampGifFps({ num: 60, den: 1 })).toEqual({ num: 50, den: 1 });
    expect(clampGifFps({ num: 20, den: 1 })).toEqual({ num: 20, den: 1 });
    expect(gifDelaysCs(20, { num: 60, den: 1 })).toEqual(Array(20).fill(2));
  });

  it("reports the effective rate after rounding", () => {
    expect(effectiveGifFps({ num: 60, den: 1 })).toBeCloseTo(50, 9);
    expect(effectiveGifFps({ num: 25, den: 2 })).toBeCloseTo(12.5, 9);
    expect(effectiveGifFps({ num: 20, den: 1 })).toBeCloseTo(20, 9);
    // 15 fps measures 667 cs over 100 frames, so the viewer sees about 14.99 fps.
    expect(Math.abs(effectiveGifFps({ num: 15, den: 1 }) - 15)).toBeLessThan(0.01);
  });

  it("returns no delays for an invalid rate", () => {
    expect(gifDelaysCs(10, { num: 0, den: 1 })).toEqual([]);
    expect(effectiveGifFps({ num: 0, den: 1 })).toBe(0);
  });
});

describe("formatTimecode", () => {
  const twentyFps = { num: 20, den: 1 };

  it("formats MM:SS.ff with the frame index inside the second", () => {
    expect(formatTimecode(0, twentyFps)).toBe("00:00.00");
    expect(formatTimecode(1.5, twentyFps)).toBe("00:01.10");
    expect(formatTimecode(65.25, { num: 25, den: 1 })).toBe("01:05.06");
  });

  it("counts frames at fractional rates", () => {
    const twelvePointFive = { num: 25, den: 2 };
    // Frame 12 covers 0.96 s to 1.04 s, so a time of 1.0 s is shown as frame 12 of second 0.
    expect(formatTimecode(1, twelvePointFive)).toBe("00:00.12");
    expect(formatTimecode(1.04, twelvePointFive)).toBe("00:01.00");
  });

  it("adds hours once the time reaches one hour", () => {
    expect(formatTimecode(3725, twentyFps)).toBe("01:02:05.00");
  });

  it("treats negative and non-finite times as zero", () => {
    expect(formatTimecode(-3, twentyFps)).toBe("00:00.00");
    expect(formatTimecode(Number.NaN, twentyFps)).toBe("00:00.00");
  });
});

describe("formatDuration", () => {
  it("shows seconds with two decimals under a minute", () => {
    expect(formatDuration(6.4)).toBe("6.40 s");
    expect(formatDuration(0)).toBe("0.00 s");
    expect(formatDuration(-1)).toBe("0.00 s");
  });

  it("switches to minutes and hours at or above their thresholds", () => {
    expect(formatDuration(59.999)).toBe("1:00.00");
    expect(formatDuration(62.5)).toBe("1:02.50");
    expect(formatDuration(3725)).toBe("1:02:05.00");
  });
});

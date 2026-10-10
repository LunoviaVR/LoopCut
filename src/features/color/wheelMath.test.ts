import { describe, expect, it } from "vitest";
import {
  NEUTRAL_VALUES,
  type Puck,
  puckAmount,
  puckHue,
  REC709_LUMA,
  type Rgb,
  ringColor,
  WHEEL_MAX_AMOUNT,
  wheelMaster,
  wheelPuck,
  wheelValues,
  withWheelMaster,
} from "./wheelMath";

const luma = (values: Rgb) =>
  REC709_LUMA[0] * values[0] + REC709_LUMA[1] * values[1] + REC709_LUMA[2] * values[2];

function expectClose(actual: Rgb, expected: Rgb, digits = 9) {
  for (let channel = 0; channel < 3; channel += 1) {
    expect(actual[channel]).toBeCloseTo(expected[channel] ?? Number.NaN, digits);
  }
}

describe("neutral wheels", () => {
  it("starts at the centre with a neutral master for each space", () => {
    expect(NEUTRAL_VALUES("linear")).toEqual([0, 0, 0]);
    expect(NEUTRAL_VALUES("log")).toEqual([1, 1, 1]);
    expect(wheelPuck([0, 0, 0], "linear")).toEqual({ x: 0, y: 0 });
    expect(wheelPuck([1, 1, 1], "log")).toEqual({ x: 0, y: 0 });
  });

  it("reads the master as the Rec.709 weighted average", () => {
    expect(wheelMaster([0.2, 0.2, 0.2], "linear")).toBeCloseTo(0.2, 12);
    expect(wheelMaster([1, 1, 1], "log")).toBeCloseTo(1, 12);
    expect(REC709_LUMA[0] + REC709_LUMA[1] + REC709_LUMA[2]).toBeCloseTo(1, 12);
  });
});

describe("puck mapping", () => {
  it("keeps the master when the puck moves, so the tone brightness does not change", () => {
    const pucks: Puck[] = [
      { x: 0.9, y: 0 },
      { x: -0.3, y: 0.6 },
      { x: 0.1, y: -0.8 },
    ];
    for (const puck of pucks) {
      expect(wheelMaster(wheelValues(0.25, puck, "linear"), "linear")).toBeCloseTo(0.25, 9);
      expect(wheelMaster(wheelValues(2, puck, "log"), "log")).toBeCloseTo(2, 9);
      expect(luma(wheelValues(0.25, puck, "linear"))).toBeCloseTo(0.25, 9);
    }
  });

  it("points the red direction at angle 0 and the hue reads in degrees", () => {
    const values = wheelValues(0, { x: 0.8, y: 0 }, "linear");
    expect(values[0]).toBeGreaterThan(values[1]);
    expect(values[0]).toBeGreaterThan(values[2]);
    expect(puckHue({ x: 0, y: 1 })).toBeCloseTo(90, 9);
    expect(puckHue({ x: 0, y: -1 })).toBeCloseTo(270, 9);
  });

  it("scales the chroma with the distance from the centre", () => {
    const half = wheelValues(0, { x: 0.5, y: 0 }, "linear");
    const full = wheelValues(0, { x: 1, y: 0 }, "linear");
    expect(full[0] / half[0]).toBeCloseTo(2, 9);
    expect(puckAmount({ x: 0.5, y: 0 })).toBeCloseTo(0.5, 12);
  });

  it("clamps a puck outside the rim to the rim", () => {
    expect(wheelValues(0, { x: 3, y: 0 }, "linear")).toEqual(
      wheelValues(0, { x: 1, y: 0 }, "linear"),
    );
  });

  it("round-trips small values through the puck and the master", () => {
    const linear: Rgb = [0.05, -0.02, 0.01];
    const puck = wheelPuck(linear, "linear");
    const back = wheelValues(wheelMaster(linear, "linear"), puck, "linear");
    expectClose(back, linear);

    const gamma: Rgb = [1.1, 0.95, 0.9];
    const gammaPuck = wheelPuck(gamma, "log");
    const gammaBack = wheelValues(wheelMaster(gamma, "log"), gammaPuck, "log");
    expectClose(gammaBack, gamma, 8);
  });

  it("keeps the puck when the master changes", () => {
    const values: Rgb = [0.1, 0, -0.1];
    const moved = withWheelMaster(values, "linear", 0.4);
    expect(wheelMaster(moved, "linear")).toBeCloseTo(0.4, 9);
    const before = wheelPuck(values, "linear");
    const after = wheelPuck(moved, "linear");
    expect(after.x).toBeCloseTo(before.x, 9);
    expect(after.y).toBeCloseTo(before.y, 9);
  });

  it("keeps the puck for a multiplicative wheel too", () => {
    const values: Rgb = [1.2, 1, 0.8];
    const moved = withWheelMaster(values, "log", 2);
    expect(wheelMaster(moved, "log")).toBeCloseTo(2, 9);
    const before = wheelPuck(values, "log");
    const after = wheelPuck(moved, "log");
    expect(after.x).toBeCloseTo(before.x, 8);
    expect(after.y).toBeCloseTo(before.y, 8);
  });

  it("never pushes a channel outside what a wheel amount allows", () => {
    const values = wheelValues(0, { x: 1, y: 0 }, "linear");
    for (const value of values)
      expect(Math.abs(value)).toBeLessThanOrEqual(WHEEL_MAX_AMOUNT + 1e-9);
  });
});

describe("ring colors", () => {
  it("returns an rgb() string with byte channels", () => {
    const color = ringColor(0.3);
    const match = /^rgb\((\d+) (\d+) (\d+)\)$/.exec(color);
    expect(match).not.toBeNull();
    for (const part of (match ?? []).slice(1)) {
      expect(Number(part)).toBeGreaterThanOrEqual(0);
      expect(Number(part)).toBeLessThanOrEqual(255);
    }
  });
});

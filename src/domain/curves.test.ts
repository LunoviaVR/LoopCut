import { describe, expect, it } from "vitest";
import {
  editablePoints,
  evaluateCurve,
  insertCurvePoint,
  isEndpoint,
  isIdentityCurve,
  monotoneTangents,
  moveCurvePoint,
  removeCurvePoint,
  sampleCurve,
  splineAnchors,
} from "./curves";

const pts = (...pairs: [number, number][]) => pairs.map(([x, y]) => ({ x, y }));

describe("splineAnchors", () => {
  it("returns null for an empty or unusable list (the identity)", () => {
    expect(splineAnchors([])).toBeNull();
    expect(splineAnchors(pts([Number.NaN, 0.5]))).toBeNull();
  });

  it("adds the implicit anchors at both ends, like the renderer", () => {
    expect(splineAnchors(pts([0.5, 0.25]))).toEqual(pts([0, 0], [0.5, 0.25], [1, 1]));
  });

  it("sorts, clamps and keeps the last point of a repeated x", () => {
    expect(splineAnchors(pts([1, 1], [0.5, 1.4], [0.5, 0.2], [0, -0.3]))).toEqual(
      pts([0, 0], [0.5, 0.2], [1, 1]),
    );
  });
});

describe("Fritsch-Carlson tangents", () => {
  it("uses the end slopes at the ends and the averaged slope inside", () => {
    // Slopes are 0.5 and 1.5, so the interior tangent is their average.
    expect(monotoneTangents([0, 0.5, 1], [0, 0.25, 1])).toEqual([0.5, 1, 1.5]);
  });

  it("flattens an interior extremum so the spline stays monotone", () => {
    // Slopes rise then fall: the interior point is a peak and its tangent is zero.
    const tangents = monotoneTangents([0, 0.5, 1], [0, 1, 1]);
    expect(tangents[1]).toBe(0);
  });

  it("limits a steep segment to the circle of radius 3", () => {
    const xs = [0, 0.1, 1];
    const ys = [0, 0.9, 1];
    const tangents = monotoneTangents(xs, ys);
    const slope = ((ys[2] ?? 0) - (ys[1] ?? 0)) / ((xs[2] ?? 0) - (xs[1] ?? 0));
    const a = (tangents[1] ?? 0) / slope;
    const b = (tangents[2] ?? 0) / slope;
    expect(Math.sqrt(a * a + b * b)).toBeCloseTo(3, 9);
  });
});

describe("evaluateCurve", () => {
  it("is the identity for an empty curve", () => {
    expect(evaluateCurve([], 0.3)).toBeCloseTo(0.3, 12);
    expect(evaluateCurve([], 2)).toBe(1);
  });

  it("passes through every control point", () => {
    const curve = pts([0.2, 0.7], [0.6, 0.4]);
    expect(evaluateCurve(curve, 0.2)).toBeCloseTo(0.7, 12);
    expect(evaluateCurve(curve, 0.6)).toBeCloseTo(0.4, 12);
    expect(evaluateCurve(curve, 0)).toBe(0);
    expect(evaluateCurve(curve, 1)).toBe(1);
  });

  it("matches the known Fritsch-Carlson value at x = 0.25", () => {
    // Points (0, 0), (0.5, 0.25), (1, 1): tangents 0.5, 1, 1.5; Hermite basis at t = 0.5 gives 0.09375.
    expect(evaluateCurve(pts([0.5, 0.25]), 0.25)).toBeCloseTo(0.09375, 12);
  });

  it("stays monotone between steep, rising control points", () => {
    const curve = pts([0.05, 0.9], [0.1, 0.95]);
    const samples = sampleCurve(curve, 401);
    for (let i = 1; i < samples.length; i += 1) {
      expect(samples[i] ?? 0).toBeGreaterThanOrEqual((samples[i - 1] ?? 0) - 1e-12);
    }
  });

  it("stays inside [0, 1] for valid points", () => {
    const samples = sampleCurve(pts([0.3, 0.05], [0.7, 0.98]), 201);
    for (const value of samples) {
      expect(value).toBeGreaterThanOrEqual(-1e-12);
      expect(value).toBeLessThanOrEqual(1 + 1e-12);
    }
  });
});

describe("sampleCurve", () => {
  it("returns count samples from 0 to 1 inclusive", () => {
    const samples = sampleCurve([], 5);
    expect(samples).toEqual([0, 0.25, 0.5, 0.75, 1]);
  });
});

describe("editing helpers", () => {
  it("starts an empty curve from its identity endpoints", () => {
    expect(editablePoints([])).toEqual(pts([0, 0], [1, 1]));
  });

  it("inserts a point in x order and reports its index", () => {
    const result = insertCurvePoint([], 0.4, 0.6);
    expect(result.points).toEqual(pts([0, 0], [0.4, 0.6], [1, 1]));
    expect(result.index).toBe(1);
  });

  it("replaces the y of a point that already sits at that x", () => {
    const first = insertCurvePoint([], 0.4, 0.6).points;
    const second = insertCurvePoint(first, 0.4, 0.2);
    expect(second.points).toEqual(pts([0, 0], [0.4, 0.2], [1, 1]));
    expect(second.index).toBe(1);
  });

  it("moves an endpoint in y only", () => {
    const moved = moveCurvePoint([], 0, 0.7, 0.3);
    expect(moved).toEqual(pts([0, 0.3], [1, 1]));
  });

  it("keeps an interior point between its neighbours", () => {
    const curve = pts([0, 0], [0.4, 0.5], [1, 1]);
    expect(moveCurvePoint(curve, 1, 0.99, 0.5)[1]?.x).toBeLessThan(1);
    expect(moveCurvePoint(curve, 1, -0.5, 0.5)[1]?.x).toBeGreaterThan(0);
    expect(moveCurvePoint(curve, 1, 0.6, 0.9)).toEqual(pts([0, 0], [0.6, 0.9], [1, 1]));
  });

  it("removes interior points only, and returns the identity when nothing is left", () => {
    const curve = pts([0, 0], [0.4, 0.5], [1, 1]);
    expect(removeCurvePoint(curve, 0)).toEqual(curve);
    expect(removeCurvePoint(curve, 2)).toEqual(curve);
    expect(removeCurvePoint(curve, 1)).toEqual([]);
  });

  it("keeps the other points when an interior point is removed from a longer curve", () => {
    const curve = pts([0, 0], [0.3, 0.2], [0.6, 0.9], [1, 1]);
    expect(removeCurvePoint(curve, 1)).toEqual(pts([0, 0], [0.6, 0.9], [1, 1]));
  });

  it("recognizes the identity curve and the endpoints", () => {
    expect(isIdentityCurve([])).toBe(true);
    expect(isIdentityCurve(pts([0, 0], [1, 1]))).toBe(true);
    expect(isIdentityCurve(pts([0, 0], [1, 0.9]))).toBe(false);
    expect(isEndpoint(pts([0, 0], [0.5, 0.5], [1, 1]), 2)).toBe(true);
    expect(isEndpoint(pts([0, 0], [0.5, 0.5], [1, 1]), 1)).toBe(false);
  });
});

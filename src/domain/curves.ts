/**
 * Tone curves: the monotone cubic (Fritsch-Carlson) spline that the grade draws, mirrored from
 * crates/loopcut-render/src/grade.rs (`MonotoneCurve`) so the curve editor shows exactly what the renderer bakes.
 *
 * Points live in `[0, 1]` on both axes. The spline clamps and sorts them, keeps the last point of a repeated x, and
 * adds the anchors (0, 0) and (1, 1) when the list does not reach those ends. An empty list is the identity.
 *
 * The editing helpers keep a curve valid for the UI: the endpoints stay at x = 0 and x = 1, interior points keep
 * their order, and endpoints cannot be removed.
 */
import type { CurvePoint } from "@/ipc/bindings/CurvePoint";

/** x distance below which two points count as one (the spline would otherwise divide by almost zero). */
const MIN_STEP = 1e-6;

const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/** The points as the spline uses them, or null when there are none usable (the identity curve). */
export function splineAnchors(points: readonly CurvePoint[]): CurvePoint[] | null {
  const sorted = points
    .filter((point) => Number.isFinite(point.x) && Number.isFinite(point.y))
    .map((point) => ({ x: clamp01(point.x), y: clamp01(point.y) }))
    .sort((a, b) => a.x - b.x);
  if (sorted.length === 0) return null;

  const unique: CurvePoint[] = [];
  for (const point of sorted) {
    const last = unique[unique.length - 1];
    if (last && last.x === point.x) last.y = point.y;
    else unique.push({ ...point });
  }
  const first = unique[0];
  if (first && first.x > 0) unique.unshift({ x: 0, y: 0 });
  const last = unique[unique.length - 1];
  if (!last || last.x < 1) unique.push({ x: 1, y: 1 });
  return unique;
}

/** Fritsch-Carlson tangents for knots with the given x and y (at least two knots). */
export function monotoneTangents(xs: readonly number[], ys: readonly number[]): number[] {
  const n = xs.length;
  const slopes: number[] = [];
  for (let k = 0; k < n - 1; k += 1) {
    slopes.push(((ys[k + 1] ?? 0) - (ys[k] ?? 0)) / ((xs[k + 1] ?? 0) - (xs[k] ?? 0)));
  }
  const tangents = new Array<number>(n).fill(0);
  tangents[0] = slopes[0] ?? 0;
  tangents[n - 1] = slopes[n - 2] ?? 0;
  for (let k = 1; k < n - 1; k += 1) {
    const before = slopes[k - 1] ?? 0;
    const after = slopes[k] ?? 0;
    tangents[k] = before * after <= 0 ? 0 : (before + after) / 2;
  }
  // Limiter: keep each segment inside the circle of radius 3 so the spline stays monotone.
  for (let k = 0; k < slopes.length; k += 1) {
    const slope = slopes[k] ?? 0;
    if (slope === 0) {
      tangents[k] = 0;
      tangents[k + 1] = 0;
      continue;
    }
    const a = (tangents[k] ?? 0) / slope;
    const b = (tangents[k + 1] ?? 0) / slope;
    const norm = a * a + b * b;
    if (norm > 9) {
      const tau = 3 / Math.sqrt(norm);
      tangents[k] = tau * a * slope;
      tangents[k + 1] = tau * b * slope;
    }
  }
  return tangents;
}

/** Evaluates the curve at `x` (clamped to [0, 1]). An empty curve is the identity. */
export function evaluateCurve(points: readonly CurvePoint[], x: number): number {
  const clampedX = clamp01(x);
  const knots = splineAnchors(points);
  if (!knots) return clampedX;
  const xs = knots.map((knot) => knot.x);
  const ys = knots.map((knot) => knot.y);
  const tangents = monotoneTangents(xs, ys);
  const n = xs.length;
  let k = 0;
  while (k < n - 2 && (xs[k + 1] ?? 0) <= clampedX) k += 1;
  const x0 = xs[k] ?? 0;
  const x1 = xs[k + 1] ?? 1;
  const h = x1 - x0;
  const t = (clampedX - x0) / h;
  const t2 = t * t;
  const t3 = t2 * t;
  const h00 = 2 * t3 - 3 * t2 + 1;
  const h10 = t3 - 2 * t2 + t;
  const h01 = -2 * t3 + 3 * t2;
  const h11 = t3 - t2;
  return (
    h00 * (ys[k] ?? 0) +
    h10 * h * (tangents[k] ?? 0) +
    h01 * (ys[k + 1] ?? 0) +
    h11 * h * (tangents[k + 1] ?? 0)
  );
}

/** `count` evenly spaced samples of the curve over [0, 1], for drawing. */
export function sampleCurve(points: readonly CurvePoint[], count: number): number[] {
  const samples: number[] = [];
  const steps = Math.max(1, Math.floor(count) - 1);
  for (let i = 0; i <= steps; i += 1) samples.push(evaluateCurve(points, i / steps));
  return samples;
}

/** Whether `index` is one of the two endpoints of a point list (the list's first or last point). */
export function isEndpoint(points: readonly CurvePoint[], index: number): boolean {
  return index === 0 || index === points.length - 1;
}

/** The point list the editor works on: the stored points, or the identity endpoints when the curve is empty. */
export function editablePoints(points: readonly CurvePoint[]): CurvePoint[] {
  if (points.length >= 2) return points.map((point) => ({ x: point.x, y: point.y }));
  return [
    { x: 0, y: 0 },
    { x: 1, y: 1 },
  ];
}

/**
 * Adds a point at (x, y). A point that lands on an existing x replaces that point's y. Returns the new list and the
 * index of the point it touched. Points at the very ends land on the endpoints, which keep their x.
 */
export function insertCurvePoint(
  points: readonly CurvePoint[],
  x: number,
  y: number,
): { points: CurvePoint[]; index: number } {
  const list = editablePoints(points);
  const px = clamp01(x);
  const py = clamp01(y);
  const existing = list.findIndex((point) => Math.abs(point.x - px) < MIN_STEP);
  if (existing >= 0) {
    const kept = list[existing]?.x ?? px;
    list[existing] = { x: kept, y: py };
    return { points: list, index: existing };
  }
  let index = list.findIndex((point) => point.x > px);
  if (index < 0) index = list.length;
  list.splice(index, 0, { x: px, y: py });
  return { points: list, index };
}

/**
 * Moves one point. Endpoints keep their x; interior points stay between their neighbours, so the order holds.
 * Returns the new list.
 */
export function moveCurvePoint(
  points: readonly CurvePoint[],
  index: number,
  x: number,
  y: number,
): CurvePoint[] {
  const list = editablePoints(points);
  const current = list[index];
  if (!current) return list;
  const py = clamp01(y);
  if (isEndpoint(list, index)) {
    list[index] = { x: current.x, y: py };
    return list;
  }
  const lower = (list[index - 1]?.x ?? 0) + MIN_STEP;
  const upper = (list[index + 1]?.x ?? 1) - MIN_STEP;
  const px = Math.min(upper, Math.max(lower, clamp01(x)));
  list[index] = { x: px, y: py };
  return list;
}

/**
 * Removes an interior point. Endpoints are never removed: a copy of the list is returned unchanged. When the removal
 * leaves only the two endpoints on the diagonal, the curve is the identity again and the empty list is returned.
 */
export function removeCurvePoint(points: readonly CurvePoint[], index: number): CurvePoint[] {
  const copy = points.map((point) => ({ x: point.x, y: point.y }));
  if (index <= 0 || index >= copy.length - 1) return copy;
  copy.splice(index, 1);
  return isIdentityCurve(copy) ? [] : copy;
}

/** Whether a stored curve is the identity: no points, or the two endpoints on the diagonal. */
export function isIdentityCurve(points: readonly CurvePoint[]): boolean {
  if (points.length === 0) return true;
  const first = points[0];
  const last = points[points.length - 1];
  return points.length === 2 && first?.x === 0 && first?.y === 0 && last?.x === 1 && last?.y === 1;
}

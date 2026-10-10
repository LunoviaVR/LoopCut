/** Clamps `value` into [min, max]. NaN resolves to `min`; infinities clamp to the nearest bound. */
export function clamp(value: number, min: number, max: number): number {
  if (Number.isNaN(value)) return min;
  return Math.min(Math.max(value, min), max);
}

/** Clamps to the nearest integer within [min, max]. */
export function clampInt(value: number, min: number, max: number): number {
  return clamp(Math.round(value), min, max);
}

export function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

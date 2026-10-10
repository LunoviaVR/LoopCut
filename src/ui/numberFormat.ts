/** Digits after the point implied by a step, e.g. 0.05 gives 2. */
export function decimalsOf(step: number): number {
  const [, fraction = ""] = String(step).split(".");
  return fraction.length;
}

export function roundTo(value: number, decimals: number): number {
  const factor = 10 ** decimals;
  return Math.round(value * factor) / factor;
}

/** A value rounded to `decimals` and written with that many digits, e.g. 1 at two digits gives "1.00". */
export function formatNumber(value: number, decimals: number): string {
  return roundTo(value, decimals).toFixed(decimals);
}

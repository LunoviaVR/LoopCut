/**
 * Byte sizes use decimal (SI) units: 1 kB = 1,000 bytes and 1 MB = 1,000,000 bytes. Size limits are
 * quoted in these units, so a "10 MB" limit compares directly with the number shown. Labels keep the SI
 * spelling ("kB"). Binary 1024-based units are deliberately not used.
 */

/** Bytes in one megabyte, for converting a size budget typed in MB. */
export const MB_BYTES = 1_000_000;

/** Decimal scales above a single byte, ascending: [bytes per unit, label]. */
const SCALES: readonly (readonly [factor: number, label: string])[] = [
  [1e3, "kB"],
  [1e6, "MB"],
  [1e9, "GB"],
  [1e12, "TB"],
];

/**
 * Formats a byte count with one decimal above 999 bytes, e.g. `512 B`, `842.3 kB`, `9.9 MB`.
 * Invalid or negative input reads as `0 B`.
 */
export function formatBytes(bytes: number): string {
  const value = Number.isFinite(bytes) && bytes > 0 ? Math.round(bytes) : 0;
  if (value < 1000) return `${value} B`;

  let factor = 1e3;
  let label = "kB";
  for (const [scaleFactor, scaleLabel] of SCALES) {
    if (value >= scaleFactor) {
      factor = scaleFactor;
      label = scaleLabel;
    }
  }
  const text = (value / factor).toFixed(1);

  // Rounding can reach the next unit (999,960 bytes is 1000.0 kB, shown as 1.0 MB).
  const larger = SCALES.find(([scaleFactor]) => scaleFactor > factor);
  if (larger && Number(text) >= 1000) {
    const [largerFactor, largerLabel] = larger;
    return `${(value / largerFactor).toFixed(1)} ${largerLabel}`;
  }
  return `${text} ${label}`;
}

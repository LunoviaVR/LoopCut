/** Pure layout helpers for the monitor. */

/**
 * The size of a frame scaled to fit inside a box while keeping its aspect ratio (CSS `object-fit: contain`). Returns
 * zero sizes when either input is empty, so the caller can fall back to filling the box.
 */
export function containSize(
  frameWidth: number,
  frameHeight: number,
  boxWidth: number,
  boxHeight: number,
): { width: number; height: number } {
  if (!(frameWidth > 0 && frameHeight > 0 && boxWidth > 0 && boxHeight > 0)) {
    return { width: 0, height: 0 };
  }
  const scale = Math.min(boxWidth / frameWidth, boxHeight / frameHeight);
  return { width: frameWidth * scale, height: frameHeight * scale };
}

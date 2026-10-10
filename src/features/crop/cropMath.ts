/**
 * Crop geometry in display space. The crop is stored as a normalized rectangle over the display-oriented frame (the
 * source with its own rotation, the user rotation and the flips applied). Everything here is pure: the overlay drags
 * with it, the numeric fields convert with it, and the rotate and flip buttons remap the stored rectangle with it.
 *
 * Aspect ratios are width over height in **display pixels**, so an aspect lock means `w * W / (h * H) = aspect`.
 */
import type { NormRect } from "@/ipc/bindings/NormRect";
import type { Rational } from "@/ipc/bindings/Rational";

/** Smallest crop side, as a fraction of the frame. Matches the store's limit. */
export const CROP_MIN_SIZE = 0.01;

export const FULL_CROP: NormRect = { x: 0, y: 0, w: 1, h: 1 };

export type CropHandle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w";

export const CROP_HANDLES: readonly CropHandle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

/** Display-oriented size of the frame in pixels. */
export interface DisplaySize {
  width: number;
  height: number;
}

/** Source video dimensions as the media info reports them (coded size and display rotation). */
export interface SourceDims {
  width: number;
  height: number;
  rotation: number;
}

/** Normalizes any angle to 0, 90, 180 or 270 (clockwise), rounding to the nearest quarter turn. */
export function quarterTurns(degrees: number): 0 | 90 | 180 | 270 {
  const turned = (((Math.round(degrees / 90) * 90) % 360) + 360) % 360;
  return turned as 0 | 90 | 180 | 270;
}

/**
 * The display size: the source's own rotation and the user rotation (both clockwise) decide whether width and height
 * swap. A quarter turn of either swaps them; a half turn does not.
 */
export function displaySize(source: SourceDims, userRotation: number): DisplaySize {
  const total = quarterTurns(source.rotation + userRotation);
  const swapped = total === 90 || total === 270;
  return swapped
    ? { width: source.height, height: source.width }
    : { width: source.width, height: source.height };
}

const clamp = (value: number, min: number, max: number) => Math.min(Math.max(value, min), max);

/** Keeps the rectangle inside the frame with sides between the minimum and the full frame. */
export function clampRect(rect: NormRect): NormRect {
  const w = clamp(rect.w, CROP_MIN_SIZE, 1);
  const h = clamp(rect.h, CROP_MIN_SIZE, 1);
  return {
    x: clamp(rect.x, 0, 1 - w),
    y: clamp(rect.y, 0, 1 - h),
    w,
    h,
  };
}

/** Moves the rectangle by a normalized offset, stopping at the frame edges. */
export function moveRect(rect: NormRect, dx: number, dy: number): NormRect {
  return clampRect({ x: rect.x + dx, y: rect.y + dy, w: rect.w, h: rect.h });
}

/** Arrow-key nudge: `pixels` display pixels along each axis. */
export function nudgeRect(
  rect: NormRect,
  dxPixels: number,
  dyPixels: number,
  size: DisplaySize,
): NormRect {
  return moveRect(rect, dxPixels / size.width, dyPixels / size.height);
}

/** Pixel rectangle `{x0, y0, x1, y1}` of a normalized rectangle in a display of `size`. */
function edgesOf(rect: NormRect, size: DisplaySize) {
  const x0 = rect.x * size.width;
  const y0 = rect.y * size.height;
  return { x0, y0, x1: (rect.x + rect.w) * size.width, y1: (rect.y + rect.h) * size.height };
}

/** Normalized rectangle from pixel edges, clamped to the frame. */
function rectFromEdges(
  edges: { x0: number; y0: number; x1: number; y1: number },
  size: DisplaySize,
): NormRect {
  return clampRect({
    x: edges.x0 / size.width,
    y: edges.y0 / size.height,
    w: (edges.x1 - edges.x0) / size.width,
    h: (edges.y1 - edges.y0) / size.height,
  });
}

/**
 * The rectangle after a drag from `handle` to the pointer position (normalized display coordinates). Without an
 * aspect the dragged edges follow the pointer. With an aspect (width over height in display pixels) the rectangle
 * keeps it: a corner keeps the opposite corner fixed and the larger of the two proposed boxes wins; an edge sets the
 * size along its own axis and keeps the other axis centred. Either way the rectangle stays inside the frame.
 */
export function resizeRect(
  start: NormRect,
  handle: CropHandle,
  pointer: { x: number; y: number },
  size: DisplaySize,
  aspect: number | null,
): NormRect {
  const dx = handle.includes("e") ? 1 : handle.includes("w") ? -1 : 0;
  const dy = handle.includes("s") ? 1 : handle.includes("n") ? -1 : 0;
  const { x0, y0, x1, y1 } = edgesOf(start, size);
  const px = clamp(pointer.x, 0, 1) * size.width;
  const py = clamp(pointer.y, 0, 1) * size.height;
  const minW = CROP_MIN_SIZE * size.width;
  const minH = CROP_MIN_SIZE * size.height;

  if (aspect === null || aspect <= 0) {
    const edges = { x0, y0, x1, y1 };
    if (dx === 1) edges.x1 = clamp(px, x0 + minW, size.width);
    if (dx === -1) edges.x0 = clamp(px, 0, x1 - minW);
    if (dy === 1) edges.y1 = clamp(py, y0 + minH, size.height);
    if (dy === -1) edges.y0 = clamp(py, 0, y1 - minH);
    return rectFromEdges(edges, size);
  }

  const cx = (x0 + x1) / 2;
  const cy = (y0 + y1) / 2;

  if (dx !== 0 && dy !== 0) {
    // Corner: the opposite corner is the anchor.
    const ax = dx === 1 ? x0 : x1;
    const ay = dy === 1 ? y0 : y1;
    const fromX = Math.max(0, (px - ax) * dx);
    const fromY = Math.max(0, (py - ay) * dy);
    const widthFromPointer = Math.max(fromX, fromY * aspect);
    const maxByFrame = Math.min(
      dx === 1 ? size.width - ax : ax,
      (dy === 1 ? size.height - ay : ay) * aspect,
    );
    const minByAspect = Math.max(minW, minH * aspect);
    const width = clamp(
      widthFromPointer,
      Math.min(minByAspect, maxByFrame),
      Math.max(maxByFrame, minByAspect),
    );
    const height = width / aspect;
    const corner = { x: ax + dx * width, y: ay + dy * height };
    return rectFromEdges(
      {
        x0: Math.min(ax, corner.x),
        y0: Math.min(ay, corner.y),
        x1: Math.max(ax, corner.x),
        y1: Math.max(ay, corner.y),
      },
      size,
    );
  }

  if (dx !== 0) {
    // East or west edge: width follows the pointer, height follows the width, centred on cy.
    const ax = dx === 1 ? x0 : x1;
    const proposed = Math.max(0, (px - ax) * dx);
    const maxByFrame = dx === 1 ? size.width - ax : ax;
    const maxByHeight = 2 * Math.min(cy, size.height - cy) * aspect;
    const max = Math.max(minW, Math.min(maxByFrame, maxByHeight));
    const width = clamp(proposed, Math.min(minW, max), max);
    const height = width / aspect;
    return rectFromEdges(
      {
        x0: Math.min(ax, ax + dx * width),
        x1: Math.max(ax, ax + dx * width),
        y0: cy - height / 2,
        y1: cy + height / 2,
      },
      size,
    );
  }

  // Horizontal edge: height follows the pointer, width follows the height, centred on cx.
  const ay = dy === 1 ? y0 : y1;
  const proposed = Math.max(0, (py - ay) * dy);
  const maxByFrame = dy === 1 ? size.height - ay : ay;
  const maxByWidth = (2 * Math.min(cx, size.width - cx)) / aspect;
  const max = Math.max(minH, Math.min(maxByFrame, maxByWidth));
  const height = clamp(proposed, Math.min(minH, max), max);
  const width = height * aspect;
  return rectFromEdges(
    {
      x0: cx - width / 2,
      x1: cx + width / 2,
      y0: Math.min(ay, ay + dy * height),
      y1: Math.max(ay, ay + dy * height),
    },
    size,
  );
}

/**
 * The largest rectangle with the aspect that is centred on `rect`'s centre and fits inside the frame. Used when an
 * aspect preset is chosen, so the crop changes shape at once.
 */
export function fitRectToAspect(rect: NormRect, aspect: number, size: DisplaySize): NormRect {
  if (!(aspect > 0)) return rect;
  const { x0, y0, x1, y1 } = edgesOf(rect, size);
  const cx = (x0 + x1) / 2;
  const cy = (y0 + y1) / 2;
  let width = x1 - x0;
  let height = y1 - y0;
  if (width / height > aspect) width = height * aspect;
  else height = width / aspect;
  const maxWidth = 2 * Math.min(cx, size.width - cx);
  const maxHeight = 2 * Math.min(cy, size.height - cy);
  if (width > maxWidth) {
    width = maxWidth;
    height = width / aspect;
  }
  if (height > maxHeight) {
    height = maxHeight;
    width = height * aspect;
  }
  return rectFromEdges(
    { x0: cx - width / 2, y0: cy - height / 2, x1: cx + width / 2, y1: cy + height / 2 },
    size,
  );
}

/** Rectangle in display pixels, for the numeric fields. */
export interface CropPixels {
  x: number;
  y: number;
  width: number;
  height: number;
}

export function toPixels(rect: NormRect, size: DisplaySize): CropPixels {
  return {
    x: Math.round(rect.x * size.width),
    y: Math.round(rect.y * size.height),
    width: Math.round(rect.w * size.width),
    height: Math.round(rect.h * size.height),
  };
}

export function fromPixels(pixels: CropPixels, size: DisplaySize): NormRect {
  return clampRect({
    x: pixels.x / size.width,
    y: pixels.y / size.height,
    w: pixels.width / size.width,
    h: pixels.height / size.height,
  });
}

export type CropField = "x" | "y" | "width" | "height";

/**
 * The rectangle after one numeric field is typed. Width and height keep the aspect when one is set, and every field
 * stays inside the frame.
 */
export function setPixelField(
  rect: NormRect,
  field: CropField,
  value: number,
  size: DisplaySize,
  aspect: number | null,
): NormRect {
  const pixels = toPixels(rect, size);
  const minWidth = Math.max(1, Math.round(CROP_MIN_SIZE * size.width));
  const minHeight = Math.max(1, Math.round(CROP_MIN_SIZE * size.height));
  const next = { ...pixels };
  switch (field) {
    case "x":
      next.x = clamp(Math.round(value), 0, size.width - next.width);
      break;
    case "y":
      next.y = clamp(Math.round(value), 0, size.height - next.height);
      break;
    case "width": {
      next.width = clamp(Math.round(value), minWidth, size.width - next.x);
      if (aspect && aspect > 0) {
        next.height = Math.round(next.width / aspect);
        if (next.y + next.height > size.height) {
          next.height = size.height - next.y;
          next.width = Math.round(next.height * aspect);
        }
      }
      break;
    }
    case "height": {
      next.height = clamp(Math.round(value), minHeight, size.height - next.y);
      if (aspect && aspect > 0) {
        next.width = Math.round(next.height * aspect);
        if (next.x + next.width > size.width) {
          next.width = size.width - next.x;
          next.height = Math.round(next.width / aspect);
        }
      }
      break;
    }
  }
  return fromPixels(next, size);
}

/**
 * The rectangle after the picture is turned clockwise by `degrees` (a multiple of 90). The same content stays inside
 * the crop, now in the turned display space.
 */
export function rotateRectCw(rect: NormRect, degrees: number): NormRect {
  const turns = ((Math.round(degrees / 90) % 4) + 4) % 4;
  switch (turns) {
    case 1:
      return { x: 1 - (rect.y + rect.h), y: rect.x, w: rect.h, h: rect.w };
    case 2:
      return { x: 1 - (rect.x + rect.w), y: 1 - (rect.y + rect.h), w: rect.w, h: rect.h };
    case 3:
      return { x: rect.y, y: 1 - (rect.x + rect.w), w: rect.h, h: rect.w };
    default:
      return { ...rect };
  }
}

/** The rectangle after the picture is mirrored across the given axis. */
export function flipRect(rect: NormRect, axis: "horizontal" | "vertical"): NormRect {
  if (axis === "horizontal") return { x: 1 - (rect.x + rect.w), y: rect.y, w: rect.w, h: rect.h };
  return { x: rect.x, y: 1 - (rect.y + rect.h), w: rect.w, h: rect.h };
}

/** Width over height of an aspect, or null for free. */
export function aspectValue(aspect: Rational | null): number | null {
  return aspect && aspect.num > 0 && aspect.den > 0 ? aspect.num / aspect.den : null;
}

/** Reduces a width:height pair to lowest terms. */
export function reduceRatio(width: number, height: number): Rational {
  const gcd = (a: number, b: number): number => (b === 0 ? a : gcd(b, a % b));
  const divisor = gcd(Math.round(width), Math.round(height)) || 1;
  return { num: Math.round(width) / divisor, den: Math.round(height) / divisor };
}

/** The same aspect turned on its side: a quarter turn swaps width and height. */
export function invertAspect(aspect: Rational | null): Rational | null {
  return aspect ? { num: aspect.den, den: aspect.num } : null;
}

/** The lines drawn while dragging: two verticals and two horizontals at the thirds of the rectangle, normalized. */
export function thirdsLines(rect: NormRect): { vertical: number[]; horizontal: number[] } {
  return {
    vertical: [rect.x + rect.w / 3, rect.x + (2 * rect.w) / 3],
    horizontal: [rect.y + rect.h / 3, rect.y + (2 * rect.h) / 3],
  };
}

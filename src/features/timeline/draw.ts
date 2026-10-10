/**
 * Canvas painting for the timeline: ruler, filmstrip, removed areas, kept ranges, the caption lane and the
 * playhead. The function takes a plain model and draws it; it reads no store and keeps no state, so the React
 * component decides when to call it (once per animation frame at most).
 *
 * Handles, labels and the drag readout are DOM elements laid over the canvas by Timeline.tsx.
 */
import type { TimeRange } from "@/ipc/bindings/TimeRange";
import {
  formatRulerLabel,
  pixelsPerSecond,
  removedRanges,
  rulerStep,
  rulerTicks,
  thumbnailSpan,
  timeToX,
  type ViewScale,
  visibleRange,
  visibleThumbnails,
} from "./mapping";

/** Vertical layout in CSS pixels. The canvas is `width` wide and `height` tall. */
export interface TimelineLayout {
  rulerH: number;
  clipTop: number;
  clipH: number;
  captionTop: number;
  captionH: number;
  height: number;
}

export const TIMELINE_LAYOUT: TimelineLayout = {
  rulerH: 22,
  clipTop: 26,
  clipH: 64,
  captionTop: 94,
  captionH: 30,
  height: 124,
};

/** Canvas colors, read from the app's tokens so the canvas matches the chrome. */
export interface TimelinePalette {
  panel: string;
  preview: string;
  raised: string;
  line: string;
  lineStrong: string;
  fg: string;
  fgMuted: string;
  accent: string;
}

export const FALLBACK_PALETTE: TimelinePalette = {
  panel: "#1b1c20",
  preview: "#0b0b0c",
  raised: "#23252a",
  line: "#2f3238",
  lineStrong: "#3b3f46",
  fg: "#ececef",
  fgMuted: "#a3a7b0",
  accent: "#3d7eff",
};

/** Reads the palette from the document's `--color-*` tokens, falling back to the design values. */
export function readPalette(root: Element = document.documentElement): TimelinePalette {
  const style = getComputedStyle(root);
  const read = (name: keyof TimelinePalette): string =>
    style
      .getPropertyValue(`--color-${name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`)
      .trim() || FALLBACK_PALETTE[name];
  return {
    panel: read("panel"),
    preview: read("preview"),
    raised: read("raised"),
    line: read("line"),
    lineStrong: read("lineStrong"),
    fg: read("fg"),
    fgMuted: read("fgMuted"),
    accent: read("accent"),
  };
}

/** Thumbnails by index. `get` returns the drawable image once it is loaded and queues the load otherwise. */
export interface ThumbnailSource {
  times: readonly number[];
  get(index: number): CanvasImageSource | null;
}

export interface TimelineDrawModel {
  /** Visible width in CSS pixels. */
  width: number;
  dpr: number;
  scale: ViewScale;
  segments: readonly TimeRange[];
  /** Index of the selected kept range, or null. */
  selected: number | null;
  /** Source time shown by the playhead, or null when there is no plan. */
  playheadTime: number | null;
  thumbs: ThumbnailSource | null;
  palette: TimelinePalette;
  layout?: TimelineLayout;
}

const DIM = "rgba(11, 11, 12, 0.62)";
const HATCH = "rgba(255, 255, 255, 0.10)";
const LABEL_FONT = '11px "Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif';

/** Paints one frame of the timeline. Call with the context from a canvas sized `width*dpr` by `height*dpr`. */
export function drawTimeline(ctx: CanvasRenderingContext2D, model: TimelineDrawModel): void {
  const layout = model.layout ?? TIMELINE_LAYOUT;
  const { width, dpr, scale } = model;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, layout.height);
  if (!(scale.duration > 0) || !(width > 0)) return;

  drawFilmstrip(ctx, model, layout);
  drawRuler(ctx, model, layout);
  drawCaptionLane(ctx, model, layout);
  drawPlayhead(ctx, model, layout);
}

function drawFilmstrip(
  ctx: CanvasRenderingContext2D,
  model: TimelineDrawModel,
  layout: TimelineLayout,
): void {
  const { scale, palette, width, thumbs } = model;
  const top = layout.clipTop;
  const bottom = layout.clipTop + layout.clipH;

  if (thumbs) {
    const { start, end } = visibleRange(scale);
    for (const index of visibleThumbnails(thumbs.times, scale.duration, start, end)) {
      const span = thumbnailSpan(thumbs.times, index, scale.duration);
      const x0 = timeToX(span.start, scale);
      const x1 = timeToX(span.end, scale);
      const image = thumbs.get(index);
      if (image) {
        ctx.drawImage(image, x0, top, x1 - x0 + 0.5, layout.clipH);
      } else {
        ctx.fillStyle = palette.raised;
        ctx.fillRect(x0, top, x1 - x0 + 0.5, layout.clipH);
      }
    }
  } else {
    ctx.fillStyle = palette.raised;
    ctx.fillRect(0, top, width, layout.clipH);
  }

  // Removed areas: dimmed, with a hatch so they read as cut rather than empty.
  for (const gap of removedRanges(model.segments, scale.duration)) {
    const x0 = Math.max(0, timeToX(gap.start, scale));
    const x1 = Math.min(width, timeToX(gap.end, scale));
    if (x1 <= x0) continue;
    ctx.fillStyle = DIM;
    ctx.fillRect(x0, top, x1 - x0, layout.clipH);
    drawHatch(ctx, x0, x1, top, bottom);
  }

  model.segments.forEach((segment, index) => {
    const x0 = timeToX(segment.start, scale);
    const x1 = timeToX(segment.end, scale);
    if (x1 < 0 || x0 > width) return;
    const selected = model.selected === index;
    ctx.strokeStyle = selected ? palette.accent : palette.lineStrong;
    ctx.lineWidth = selected ? 2 : 1;
    const inset = selected ? 1 : 0.5;
    ctx.strokeRect(x0 + inset, top + inset, x1 - x0 - 2 * inset, layout.clipH - 2 * inset);
  });
}

function drawHatch(
  ctx: CanvasRenderingContext2D,
  x0: number,
  x1: number,
  top: number,
  bottom: number,
) {
  ctx.save();
  ctx.beginPath();
  ctx.rect(x0, top, x1 - x0, bottom - top);
  ctx.clip();
  ctx.strokeStyle = HATCH;
  ctx.lineWidth = 1;
  ctx.beginPath();
  const height = bottom - top;
  for (let x = x0 - height; x < x1; x += 8) {
    ctx.moveTo(x, bottom);
    ctx.lineTo(x + height, top);
  }
  ctx.stroke();
  ctx.restore();
}

function drawRuler(
  ctx: CanvasRenderingContext2D,
  model: TimelineDrawModel,
  layout: TimelineLayout,
) {
  const { scale, palette, width } = model;
  const baseline = layout.rulerH - 0.5;
  ctx.strokeStyle = palette.line;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(0, baseline);
  ctx.lineTo(width, baseline);
  ctx.stroke();

  const pps = pixelsPerSecond(scale.duration, scale.viewWidth, scale.zoom);
  const step = rulerStep(pps);
  const { start, end } = visibleRange(scale);
  ctx.font = LABEL_FONT;
  ctx.fillStyle = palette.fgMuted;
  ctx.textBaseline = "alphabetic";
  for (const tick of rulerTicks(start, end, step)) {
    const x = Math.round(timeToX(tick, scale)) + 0.5;
    if (x < -40 || x > width + 40) continue;
    ctx.beginPath();
    ctx.moveTo(x, layout.rulerH - 7);
    ctx.lineTo(x, layout.rulerH - 0.5);
    ctx.stroke();
    // A label that would run past the right edge is skipped rather than cut; its tick still shows.
    const label = formatRulerLabel(tick, step);
    if (x + 4 + ctx.measureText(label).width <= width)
      ctx.fillText(label, x + 4, layout.rulerH - 9);
  }
}

function drawCaptionLane(
  ctx: CanvasRenderingContext2D,
  model: TimelineDrawModel,
  layout: TimelineLayout,
): void {
  // The caption lane is a reserved row: the caption bars are DOM elements laid over it (features/captions/CaptionBars),
  // placed with the same time-to-x mapping as the filmstrip. This paints only the row's dashed frame.
  ctx.save();
  ctx.setLineDash([4, 3]);
  ctx.strokeStyle = model.palette.line;
  ctx.lineWidth = 1;
  ctx.strokeRect(0.5, layout.captionTop + 0.5, model.width - 1, layout.captionH - 1);
  ctx.restore();
}

function drawPlayhead(
  ctx: CanvasRenderingContext2D,
  model: TimelineDrawModel,
  layout: TimelineLayout,
) {
  if (model.playheadTime === null) return;
  const x = timeToX(model.playheadTime, model.scale);
  if (x < -2 || x > model.width + 2) return;
  ctx.fillStyle = model.palette.accent;
  ctx.strokeStyle = model.palette.accent;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.moveTo(x, 0);
  ctx.lineTo(x, layout.height);
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(x - 5, 0);
  ctx.lineTo(x + 5, 0);
  ctx.lineTo(x, 7);
  ctx.closePath();
  ctx.fill();
}

/**
 * Caption geometry on the output frame: the lines a caption shows, its font, and the box it occupies. The box is
 * approximate (the text is measured with the canvas, without wrapping), which is enough for the dashed selection box
 * and for placing captions by their centre. Everything here is pure except `measureWithCanvas`, which keeps one
 * offscreen context.
 */

import { displaySize } from "@/features/crop/cropMath";
import type { Caption } from "@/ipc/bindings/Caption";
import type { CaptionStyle } from "@/ipc/bindings/CaptionStyle";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";

export interface OutputSize {
  width: number;
  height: number;
}

/** Width in pixels of `text` drawn with `font` (a CSS font shorthand) at `fontPx` pixels. */
export type TextMeasure = (text: string, font: string, fontPx: number) => number;

/** A caption box as fractions of the output frame: its top-left corner and size. */
export interface CaptionBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Rough glyph width used when no canvas is available: half an em per character. */
const FALLBACK_EM_WIDTH = 0.5;

/** The lines a caption draws: its text split on line breaks, uppercased when the style says so. */
export function captionLines(caption: Pick<Caption, "text" | "style">): string[] {
  const lines = caption.text.split(/\r?\n/);
  return lines.map((line) => (caption.style.uppercase ? line.toUpperCase() : line));
}

/** The CSS font shorthand for a style at an output height (font size is a fraction of that height). */
export function captionFont(style: CaptionStyle, outputHeight: number): string {
  const px = fontPixels(style, outputHeight);
  const family = style.fontFamily.includes(" ") ? `"${style.fontFamily}"` : style.fontFamily;
  return `${style.italic ? "italic " : ""}${style.bold ? "bold " : ""}${px.toFixed(2)}px ${family}`;
}

/** The font size in output pixels. */
export function fontPixels(style: CaptionStyle, outputHeight: number): number {
  return Math.max(1, style.fontSize * outputHeight);
}

/** A caption color as a CSS color. Colors are straight-alpha RGBA bytes. */
export function rgbaCss(color: [number, number, number, number]): string {
  const [r, g, b, a] = color;
  return `rgba(${r}, ${g}, ${b}, ${(a / 255).toFixed(3)})`;
}

/**
 * The box of a caption as fractions of the output frame. Its size comes from the widest line plus the outline on both
 * sides, the line height and, when a background is set, a padding; the width is capped at the style's max width.
 * The anchor (x, y) is the centre of the box.
 */
export function captionBox(
  caption: Pick<Caption, "text" | "style" | "x" | "y">,
  output: OutputSize,
  measure: TextMeasure,
): CaptionBox {
  const { style } = caption;
  const fontPx = fontPixels(style, output.height);
  const font = captionFont(style, output.height);
  const lines = captionLines(caption);
  const widest = Math.max(0, ...lines.map((line) => measure(line, font, fontPx)));
  const outline = fontPx * style.strokeWidth;
  const padding = style.background ? fontPx * 0.3 : 0;
  const textWidth = Math.min(widest, style.maxWidth * output.width);
  const width = textWidth + 2 * outline + 2 * padding;
  const height = lines.length * fontPx * style.lineHeight + 2 * outline + 2 * padding;
  const centreX = caption.x * output.width;
  const centreY = caption.y * output.height;
  return {
    left: (centreX - width / 2) / output.width,
    top: (centreY - height / 2) / output.height,
    width: width / output.width,
    height: height / output.height,
  };
}

/** Clamps a centre so the caption box stays on the frame; returns normalized x and y. */
export function clampCaptionCentre(x: number, y: number): { x: number; y: number } {
  return {
    x: Math.min(1, Math.max(0, x)),
    y: Math.min(1, Math.max(0, y)),
  };
}

/**
 * The canvas-backed measure: the width of the text in the given font. Falls back to a rough em width when no 2D
 * context exists (headless tests, very old runtimes).
 */
let measureContext: CanvasRenderingContext2D | null | undefined;

export const measureWithCanvas: TextMeasure = (text, font, fontPx) => {
  if (measureContext === undefined) {
    measureContext =
      typeof document === "undefined" ? null : document.createElement("canvas").getContext("2d");
  }
  if (!measureContext) return text.length * fontPx * FALLBACK_EM_WIDTH;
  measureContext.font = font;
  const width = measureContext.measureText(text).width;
  return Number.isFinite(width) && width > 0 ? width : text.length * fontPx * FALLBACK_EM_WIDTH;
};

/**
 * The output size the frame has: the preview's when a preview exists, otherwise the size the project asks for,
 * computed from the source's display size and the crop.
 */
export function outputSizeFor(
  project: Project,
  media: ImportedMedia | null,
  preview: PreviewInfo | null,
): OutputSize {
  if (preview) return { width: preview.outputWidth, height: preview.outputHeight };
  if (!media) return { width: 480, height: 270 };
  const display = displaySize(media.info.video, project.transform.rotation);
  const crop = project.transform.crop;
  const width = display.width * crop.w;
  const height = display.height * crop.h;
  const size = project.output.size;
  if (size.mode === "width") {
    return { width: size.width, height: Math.max(2, Math.round((size.width * height) / width)) };
  }
  if (size.mode === "fit") {
    const scale = Math.min(size.maxWidth / width, size.maxHeight / height);
    return { width: Math.round(width * scale), height: Math.round(height * scale) };
  }
  return { width: Math.round(width * size.factor), height: Math.round(height * size.factor) };
}

/*
 * DTOs from docs/ipc.md that the frontend reads outside the generated bindings. Types with a binding in
 * src/ipc/bindings/ (AppError, ExportEvent, SizeEstimate, PreviewStatus, DecodeFallback, ImportedMedia, PlatformPreset,
 * LutInfo, ...) are re-exported from there, so the UI and the Rust side cannot drift. Remove an entry here as soon as
 * its Rust type gets `#[ts(export)]`.
 */

export type { DecodeFallback } from "./bindings/DecodeFallback";
export type { ExportEvent } from "./bindings/ExportEvent";
export type { PreviewStatus } from "./bindings/PreviewStatus";
export type { SizeEstimate } from "./bindings/SizeEstimate";

/** `output` renders exactly what export produces; `source` renders the uncropped source. */
export type PreviewMode = "output" | "source";

/** One filmstrip thumbnail: straight-alpha RGBA, `width * height * 4` bytes. */
export interface ThumbnailPixels {
  width: number;
  height: number;
  rgba: Uint8ClampedArray<ArrayBuffer>;
}

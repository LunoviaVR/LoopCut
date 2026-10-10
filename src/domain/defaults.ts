import type { CaptionAnimation } from "@/ipc/bindings/CaptionAnimation";
import type { CaptionStyle } from "@/ipc/bindings/CaptionStyle";
import type { ExportSettings } from "@/ipc/bindings/ExportSettings";
import type { Grade } from "@/ipc/bindings/Grade";
import type { HslAdjust } from "@/ipc/bindings/HslAdjust";

/**
 * Defaults mirroring the Rust `Default` impls in crates/loopcut-core/src/project.rs. Rust stays canonical
 * (`project_new` builds real projects); these exist for new captions created in the UI, resets, tests and the
 * browser mock. Keep them in step with the Rust side.
 */

const neutralHsl = (): HslAdjust => ({ hue: 0, saturation: 0, luminance: 0 });

export function defaultGrade(): Grade {
  return {
    enabled: true,
    exposure: 0,
    contrast: 0,
    highlights: 0,
    shadows: 0,
    saturation: 0,
    vibrance: 0,
    temperature: 0,
    tint: 0,
    lift: [0, 0, 0],
    gamma: [1, 1, 1],
    gain: [1, 1, 1],
    sharpen: 0,
    vignette: 0,
    curves: { master: [], red: [], green: [], blue: [] },
    hsl: [
      neutralHsl(),
      neutralHsl(),
      neutralHsl(),
      neutralHsl(),
      neutralHsl(),
      neutralHsl(),
      neutralHsl(),
      neutralHsl(),
    ],
    lutPath: null,
    lutStrength: 1,
    preset: null,
  };
}

/** The classic meme caption: Impact, white fill, black outline, uppercase, centered. */
export function defaultCaptionStyle(): CaptionStyle {
  return {
    fontFamily: "Impact",
    fontSize: 0.12,
    bold: false,
    italic: false,
    uppercase: true,
    fill: [255, 255, 255, 255],
    strokeWidth: 0.08,
    strokeColor: [0, 0, 0, 255],
    shadow: null,
    background: null,
    align: "center",
    maxWidth: 0.92,
    lineHeight: 1.1,
  };
}

export function defaultCaptionAnimation(): CaptionAnimation {
  return { kind: "none" };
}

export function defaultExportSettings(): ExportSettings {
  return {
    format: "gif",
    gifEngine: "quality",
    quality: 90,
    lossyQuality: 100,
    motionQuality: 100,
    colors: 256,
    dither: "sierra2_4a",
    targetBytes: null,
    preset: null,
  };
}

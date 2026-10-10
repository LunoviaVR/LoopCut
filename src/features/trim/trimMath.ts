/**
 * Trim tool helpers: the choices the panel offers and the numbers it shows. Pure functions; the panel reads the
 * project and writes through the store.
 */
import {
  effectiveGifFps,
  GIF_EXACT_RATES,
  isValidRational,
  MAX_GIF_FPS,
  toFloat,
} from "@/domain/time";
import { reduceRatio } from "@/features/crop/cropMath";
import type { Project } from "@/ipc/bindings/Project";
import type { Rational } from "@/ipc/bindings/Rational";
import type { TimeRange } from "@/ipc/bindings/TimeRange";

export const SPEED_LIMITS = { min: 0.25, max: 4, step: 0.05 } as const;
export const SPEED_PRESETS: readonly number[] = [0.5, 1, 2];
/** Output frame rates offered in the menu, fastest first. */
export const FPS_PRESETS: readonly number[] = [50, 25, 20, 12.5, 10, 5];
export const WIDTH_PRESETS: readonly number[] = [320, 480, 640, 720];
export const OUTPUT_WIDTH_LIMITS = { min: 16, max: 4096 } as const;
export const LOOP_LIMITS = { min: 2, max: 65_535 } as const;
/** The count the Times choice starts at when it is first picked. */
export const DEFAULT_TIMES = 3;

/**
 * The span the loop keeps, in source seconds: from the first kept segment's start to the last one's end, so a cut
 * inside the span is still inside the range. Null when the clip has no segment.
 */
export function keptRange(project: Project): TimeRange | null {
  const segments = project.clips[0]?.segments ?? [];
  const first = segments[0];
  const last = segments[segments.length - 1];
  return first && last ? { start: first.start, end: last.end } : null;
}

/** Source seconds kept across every segment. */
export function keptSeconds(project: Project): number {
  return project.clips.reduce(
    (sum, clip) =>
      sum + clip.segments.reduce((inner, range) => inner + (range.end - range.start), 0),
    0,
  );
}

/** Output seconds: the kept span at the chosen speed. */
export function outputSeconds(project: Project): number {
  const speed = project.timing.speed > 0 ? project.timing.speed : 1;
  return keptSeconds(project) / speed;
}

/** Largest crossfade the output can hold: half the output length (the sequencer blends at most half). */
export function maxCrossfadeSeconds(project: Project): number {
  return Math.max(0, outputSeconds(project) / 2);
}

/** The preset value (as its string) matching a rate, or "custom". */
export function fpsChoice(rate: Rational): string {
  const value = toFloat(rate);
  const match = FPS_PRESETS.find((preset) => Math.abs(preset - value) < 1e-9);
  return match === undefined ? "custom" : String(match);
}

/** A decimal frame rate as a rational. Presets map exactly (12.5 is 25/2); other values round to hundredths. */
export function rationalFromFps(value: number): Rational {
  const scaled = Math.round(value * 100);
  return reduceRatio(scaled, 100);
}

export interface FpsHint {
  /** True when the rate is one of GIF's exact centisecond rates, so nothing needs saying. */
  exact: boolean;
  /** Rate after centisecond rounding, for the hint. */
  effective: number;
  /** True when the rate is above the 50 fps GIF ceiling. */
  clamped: boolean;
}

/** What the GIF will really play at for this rate. */
export function fpsHint(rate: Rational): FpsHint {
  const value = toFloat(rate);
  const exact = GIF_EXACT_RATES.some((candidate) => Math.abs(toFloat(candidate) - value) < 1e-9);
  return {
    exact,
    effective: isValidRational(rate) ? Math.round(effectiveGifFps(rate) * 100) / 100 : 0,
    clamped: value > MAX_GIF_FPS,
  };
}

/** Height that keeps the output's aspect at `width`, rounded to an even number of at least 2. */
export function derivedHeight(width: number, display: { width: number; height: number }): number {
  if (!(display.width > 0) || !(width > 0)) return 2;
  const raw = Math.round((width * display.height) / display.width);
  return Math.max(2, Math.round(raw / 2) * 2);
}

export type LoopMode = "forever" | "once" | "times";

export function loopModeOf(count: number): LoopMode {
  if (count === 0) return "forever";
  if (count === 1) return "once";
  return "times";
}

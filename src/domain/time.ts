/**
 * Frame-rate and timecode helpers. Rates are exact rationals (`Rational`), never bare floats, so frame
 * boundaries and GIF centisecond delays agree with the Rust sequencer (`time.rs`, `gif_timing.rs`).
 */
import type { Rational } from "@/ipc/bindings/Rational";

/** Absorbs floating-point error when a time is converted to a frame index (in frames). */
const FRAME_EPSILON = 1e-6;

/** Highest output frame rate the sequencer accepts (`sequencer::plan` rejects anything above 240). */
export const MAX_OUTPUT_FPS = 240;
/** Highest frame rate a GIF plays back reliably (`gif_timing::MAX_GIF_FPS`). */
export const MAX_GIF_FPS = 50;
/** Smallest GIF delay browsers honor, in centiseconds (`gif_timing::MIN_DELAY_CS`). */
export const MIN_GIF_DELAY_CS = 2;

/** Frame rates whose frame duration is a whole number of centiseconds (`gif_timing::EXACT_GIF_RATES`). */
export const GIF_EXACT_RATES: readonly Rational[] = [
  { num: 50, den: 1 },
  { num: 25, den: 1 },
  { num: 20, den: 1 },
  { num: 25, den: 2 },
  { num: 10, den: 1 },
  { num: 5, den: 1 },
];

/** True for a positive rational with integer parts. */
export function isValidRational(rate: Rational): boolean {
  return Number.isInteger(rate.num) && rate.num > 0 && Number.isInteger(rate.den) && rate.den > 0;
}

/** The rate as a plain number of frames per second. Invalid rates read as 0. */
export function toFloat(rate: Rational): number {
  return isValidRational(rate) ? rate.num / rate.den : 0;
}

/** Frame rate for display: a plain number rounded to two decimals (30000/1001 reads as 29.97). Invalid rates read as 0. */
export function formatFps(rate: Rational): number {
  return Math.round(toFloat(rate) * 100) / 100;
}

/** Start time, in seconds, of output frame `frame`. */
export function frameToTime(frame: number, rate: Rational): number {
  return isValidRational(rate) ? (frame * rate.den) / rate.num : 0;
}

/**
 * Index of the frame that covers `seconds`: floor, with an epsilon so that a time just below a frame
 * boundary (2.9999999 frames) still lands on that frame.
 */
export function timeToFrame(seconds: number, rate: Rational): number {
  return isValidRational(rate) ? Math.floor((seconds * rate.num) / rate.den + FRAME_EPSILON) : 0;
}

/** Nearest exact GIF rate to `rate`. Ties go to the earlier list entry, as Rust's `min_by` does. */
export function snapFps(rate: Rational): Rational {
  const target = toFloat(rate);
  let best: Rational = { num: 20, den: 1 };
  let bestDistance = Number.POSITIVE_INFINITY;
  for (const candidate of GIF_EXACT_RATES) {
    const distance = Math.abs(toFloat(candidate) - target);
    if (distance < bestDistance) {
      best = candidate;
      bestDistance = distance;
    }
  }
  return { num: best.num, den: best.den };
}

/** Caps a rate at the GIF playback ceiling (50 fps). */
export function clampGifFps(rate: Rational): Rational {
  return toFloat(rate) > MAX_GIF_FPS ? { num: MAX_GIF_FPS, den: 1 } : rate;
}

/**
 * GIF frame delays in centiseconds for `count` frames shown at `rate`. Rounding is cumulative so the
 * total duration stays exact (15 fps gives 7, 6, 7, 7, 6, ...), and no delay falls below 2 cs.
 */
export function gifDelaysCs(count: number, rate: Rational): number[] {
  const fps = toFloat(clampGifFps(rate));
  if (!(fps > 0)) return [];
  const perFrameCs = 100 / fps;
  const delays: number[] = [];
  let emitted = 0;
  for (let i = 1; i <= count; i++) {
    const boundary = Math.round(i * perFrameCs);
    const delay = Math.max(boundary - emitted, MIN_GIF_DELAY_CS);
    delays.push(delay);
    emitted += delay;
  }
  return delays;
}

/** The frame rate a viewer actually sees after centisecond rounding (measured over 100 frames). */
export function effectiveGifFps(rate: Rational): number {
  const delays = gifDelaysCs(100, rate);
  const total = delays.reduce((sum, delay) => sum + delay, 0);
  return total === 0 ? 0 : (100 * delays.length) / total;
}

/** First frame whose start is at or after `seconds`. */
function firstFrameAtOrAfter(seconds: number, rate: Rational): number {
  return Math.max(0, Math.ceil((seconds * rate.num) / rate.den - FRAME_EPSILON));
}

const pad2 = (value: number): string => String(value).padStart(2, "0");

/**
 * Timecode for the frame that covers `seconds`: `MM:SS.ff`, where `ff` counts frames inside the
 * second. Once the time reaches an hour the form becomes `HH:MM:SS.ff`.
 */
export function formatTimecode(seconds: number, rate: Rational): string {
  const safe = Number.isFinite(seconds) && seconds > 0 ? seconds : 0;
  const frame = timeToFrame(safe, rate);
  const wholeSeconds = Math.floor(frameToTime(frame, rate) + FRAME_EPSILON);
  const frameInSecond = frame - firstFrameAtOrAfter(wholeSeconds, rate);
  const hours = Math.floor(wholeSeconds / 3600);
  const minutes = Math.floor(wholeSeconds / 60) % 60;
  const clock = `${pad2(minutes)}:${pad2(wholeSeconds % 60)}.${pad2(frameInSecond)}`;
  return hours > 0 ? `${pad2(hours)}:${clock}` : clock;
}

/**
 * Human duration with centisecond precision. Under a minute: `6.40 s`. From a minute up:
 * `1:02.40`, and `1:02:05.00` from an hour up.
 */
export function formatDuration(seconds: number): string {
  const centiseconds = Math.round((Number.isFinite(seconds) && seconds > 0 ? seconds : 0) * 100);
  if (centiseconds < 6000) return `${(centiseconds / 100).toFixed(2)} s`;
  const totalSeconds = Math.floor(centiseconds / 100);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const secondsPart = `${pad2(totalSeconds % 60)}.${pad2(centiseconds % 100)}`;
  if (hours > 0) return `${hours}:${pad2(minutes)}:${secondsPart}`;
  return `${minutes}:${secondsPart}`;
}

/**
 * Mapping for the Lift, Gamma and Gain wheels. A wheel holds three per-channel values. Its **master** is the
 * Rec.709 weighted average of the channels (the luminance-equivalent level); the puck sets the **chroma**, the part
 * of each channel that differs from that level. The chroma always has zero weighted luminance, so moving the puck
 * changes the color of the tone without changing its brightness. The puck angle picks the hue direction and the
 * distance from the centre sets the amount.
 *
 * Lift is additive (its values are offsets), so its chroma is measured in linear values. Gamma and Gain are
 * multiplicative, so their chroma is measured in logarithms: a chroma of +c multiplies that channel by e^c.
 */

export type Rgb = [number, number, number];

/** How a wheel's channel values are read: additively (lift) or multiplicatively, through logarithms (gamma, gain). */
export type WheelSpace = "linear" | "log";

/** Rec.709 luma weights. They sum to one, so the weighted average of a constant is that constant. */
export const REC709_LUMA: Rgb = [0.2126, 0.7152, 0.0722];

/** Chroma amount at the rim of the wheel, in the wheel's space. */
export const WHEEL_MAX_AMOUNT = 0.5;

/** Smallest value a multiplicative channel can take before its logarithm is taken (gain may be zero). */
const LOG_FLOOR = 1e-4;

/** A point on the wheel: x and y in the unit disk, y pointing down as on screen. */
export interface Puck {
  x: number;
  y: number;
}

/** Neutral master of each wheel space: lift 0, gamma and gain 1. */
export function neutralMaster(space: WheelSpace): number {
  return space === "linear" ? 0 : 1;
}

export const NEUTRAL_VALUES = (space: WheelSpace): Rgb => {
  const neutral = neutralMaster(space);
  return [neutral, neutral, neutral];
};

const dot = (a: Rgb, b: Rgb): number => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

/** The chroma part of a vector: the vector minus its weighted average, so the result has zero luma. */
function chromaOf(value: Rgb): Rgb {
  const level = dot(REC709_LUMA, value);
  return [value[0] - level, value[1] - level, value[2] - level];
}

const normalize = (value: Rgb): Rgb => {
  const length = Math.hypot(value[0], value[1], value[2]);
  return length > 0 ? [value[0] / length, value[1] / length, value[2] / length] : [0, 0, 0];
};

/**
 * Two orthonormal directions spanning the zero-luma plane: `red` (the chroma of pure red, puck angle 0) and
 * `blue` (orthogonal to it, puck angle 90 degrees). Computed once.
 */
const RED_DIRECTION: Rgb = normalize(chromaOf([1, 0, 0]));
const BLUE_DIRECTION: Rgb = (() => {
  const blue = chromaOf([0, 0, 1]);
  const along = dot(blue, RED_DIRECTION);
  const orthogonal: Rgb = [
    blue[0] - along * RED_DIRECTION[0],
    blue[1] - along * RED_DIRECTION[1],
    blue[2] - along * RED_DIRECTION[2],
  ];
  return normalize(orthogonal);
})();

/** Channel values in the wheel's working space (the logarithm for multiplicative wheels). */
function toWorking(values: Rgb, space: WheelSpace): Rgb {
  if (space === "linear") return [values[0], values[1], values[2]];
  return [
    Math.log(Math.max(values[0], LOG_FLOOR)),
    Math.log(Math.max(values[1], LOG_FLOOR)),
    Math.log(Math.max(values[2], LOG_FLOOR)),
  ];
}

function fromWorking(working: Rgb, space: WheelSpace): Rgb {
  if (space === "linear") return working;
  return [Math.exp(working[0]), Math.exp(working[1]), Math.exp(working[2])];
}

/** The wheel's master level of the three channel values: the weighted average, in the wheel's display units. */
export function wheelMaster(values: Rgb, space: WheelSpace): number {
  const level = dot(REC709_LUMA, toWorking(values, space));
  return space === "linear" ? level : Math.exp(level);
}

/** The puck that produces the values' chroma. Values with a chroma beyond the rim are clamped to the rim. */
export function wheelPuck(values: Rgb, space: WheelSpace): Puck {
  const chroma = chromaOf(toWorking(values, space));
  const a = dot(chroma, RED_DIRECTION);
  const b = dot(chroma, BLUE_DIRECTION);
  const radius = Math.min(1, Math.hypot(a, b) / WHEEL_MAX_AMOUNT);
  const angle = Math.atan2(b, a);
  return { x: radius * Math.cos(angle), y: radius * Math.sin(angle) };
}

/** The channel values for a master level and a puck. */
export function wheelValues(master: number, puck: Puck, space: WheelSpace): Rgb {
  const x = Math.min(1, Math.max(-1, puck.x));
  const y = Math.min(1, Math.max(-1, puck.y));
  const radius = Math.hypot(x, y);
  const clamped = radius > 1 ? { x: x / radius, y: y / radius } : { x, y };
  const chroma = combine(
    RED_DIRECTION,
    BLUE_DIRECTION,
    clamped.x * WHEEL_MAX_AMOUNT,
    clamped.y * WHEEL_MAX_AMOUNT,
  );
  const level = space === "linear" ? master : Math.log(Math.max(master, LOG_FLOOR));
  const working: Rgb = [level + chroma[0], level + chroma[1], level + chroma[2]];
  return fromWorking(working, space);
}

/** The values with a new master level. The chroma is kept, so the puck does not move. */
export function withWheelMaster(values: Rgb, space: WheelSpace, master: number): Rgb {
  const working = toWorking(values, space);
  const current = dot(REC709_LUMA, working);
  const target = space === "linear" ? master : Math.log(Math.max(master, LOG_FLOOR));
  const shift = target - current;
  return fromWorking([working[0] + shift, working[1] + shift, working[2] + shift], space);
}

/** Hue of the puck in degrees, 0 to 360, measured from the red direction. */
export function puckHue(puck: Puck): number {
  const degrees = (Math.atan2(puck.y, puck.x) * 180) / Math.PI;
  return degrees < 0 ? degrees + 360 : degrees;
}

/** Amount of the puck, 0 at the centre to 1 at the rim. */
export function puckAmount(puck: Puck): number {
  return Math.min(1, Math.hypot(puck.x, puck.y));
}

/** A display color (CSS) for the ring at `angle` radians: the direction's chroma, lifted around mid grey. */
export function ringColor(angle: number): string {
  const direction = combine(RED_DIRECTION, BLUE_DIRECTION, Math.cos(angle), Math.sin(angle));
  const peak = Math.max(...direction.map((value) => Math.abs(value)), 1e-9);
  const channels = direction.map((value) => Math.round(255 * (0.5 + (0.5 * value) / peak)));
  return `rgb(${channels[0]} ${channels[1]} ${channels[2]})`;
}

/** `a * ca + b * cb`, channel by channel. */
function combine(a: Rgb, b: Rgb, ca: number, cb: number): Rgb {
  return [ca * a[0] + cb * b[0], ca * a[1] + cb * b[1], ca * a[2] + cb * b[2]];
}

import { type KeyboardEvent, type PointerEvent, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useEditGesture } from "@/state/gesture";
import { Slider } from "@/ui";
import {
  NEUTRAL_VALUES,
  type Puck,
  puckAmount,
  puckHue,
  type Rgb,
  ringColor,
  type WheelSpace,
  wheelMaster,
  wheelPuck,
  wheelValues,
  withWheelMaster,
} from "./wheelMath";

/** Half the drawn width of the wheel in SVG units: the disc has radius 1 and the puck may sit a little outside it. */
const VIEW = 1.15;
const SEGMENTS = 36;
/** Puck step per arrow key, in disc units, and with Shift. */
const KEY_STEP = 0.02;
const KEY_STEP_FAST = 0.1;

/** One ring segment, precomputed: a pie wedge filled with its direction color. */
const WEDGES = Array.from({ length: SEGMENTS }, (_, index) => {
  const start = (index / SEGMENTS) * 2 * Math.PI;
  const end = ((index + 1) / SEGMENTS) * 2 * Math.PI;
  const mid = (start + end) / 2;
  const d = [
    "M 0 0",
    `L ${Math.cos(start).toFixed(4)} ${Math.sin(start).toFixed(4)}`,
    `A 1 1 0 0 1 ${Math.cos(end).toFixed(4)} ${Math.sin(end).toFixed(4)}`,
    "Z",
  ].join(" ");
  return { key: index, d, fill: ringColor(mid) };
});

export interface ColorWheelProps {
  /** Accessible name, e.g. "Lift". */
  label: string;
  space: WheelSpace;
  /** The wheel's three channel values. */
  values: Rgb;
  /** Range and step of the master slider, in the wheel's display units. */
  masterMin: number;
  masterMax: number;
  masterStep: number;
  /** Live values while a drag or slider move runs. */
  onChange: (values: Rgb) => void;
  /** The final value of one gesture (pointer up, a key press, Home). */
  onCommit: (values: Rgb) => void;
}

/**
 * One color wheel with its master slider. The puck is a focusable slider: arrows move it (Shift for larger steps),
 * Home resets the wheel to neutral. A drag writes at most once per animation frame and is one undo step.
 */
export function ColorWheel({
  label,
  space,
  values,
  masterMin,
  masterMax,
  masterStep,
  onChange,
  onCommit,
}: ColorWheelProps) {
  const { t } = useTranslation();
  const edit = useEditGesture();
  const area = useRef<HTMLDivElement>(null);
  const latest = useRef(values);
  latest.current = values;
  const drag = useRef<{ pointerId: number } | null>(null);
  const pending = useRef<Rgb | null>(null);
  const frame = useRef<number | null>(null);

  const puck = wheelPuck(values, space);
  const master = wheelMaster(values, space);
  const amount = Math.round(puckAmount(puck) * 100);
  const hue = Math.round(puckHue(puck)) % 360;
  const valueText =
    amount === 0 ? t("color.wheel.neutral") : t("color.wheel.valueText", { hue, amount });

  // A pending live value must not outlive the wheel.
  useEffect(() => {
    return () => {
      if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    };
  }, []);

  /** Puck position under a pointer, in disc units (the disc fills the box, with the margin of the SVG viewBox). */
  const puckAt = (clientX: number, clientY: number): Puck => {
    const rect = area.current?.getBoundingClientRect();
    if (!rect || rect.width === 0 || rect.height === 0) return { x: 0, y: 0 };
    return {
      x: ((clientX - rect.left) / rect.width) * 2 * VIEW - VIEW,
      y: ((clientY - rect.top) / rect.height) * 2 * VIEW - VIEW,
    };
  };

  /** Coalesces live moves into one store write per animation frame. */
  const scheduleLive = (next: Rgb) => {
    pending.current = next;
    if (frame.current !== null) return;
    frame.current = window.requestAnimationFrame(() => {
      frame.current = null;
      const value = pending.current;
      pending.current = null;
      if (value) edit.change(() => onChange(value));
    });
  };

  const flushLive = () => {
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    frame.current = null;
    pending.current = null;
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const element = event.currentTarget;
    element.focus({ preventScroll: true });
    element.setPointerCapture?.(event.pointerId);
    drag.current = { pointerId: event.pointerId };
    scheduleLive(
      wheelValues(wheelMaster(latest.current, space), puckAt(event.clientX, event.clientY), space),
    );
  };

  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    scheduleLive(
      wheelValues(wheelMaster(latest.current, space), puckAt(event.clientX, event.clientY), space),
    );
  };

  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    const final = wheelValues(
      wheelMaster(latest.current, space),
      puckAt(event.clientX, event.clientY),
      space,
    );
    flushLive();
    edit.commit(() => onCommit(final));
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Home") {
      event.preventDefault();
      flushLive();
      edit.commit(() => onCommit(NEUTRAL_VALUES(space)));
      return;
    }
    const step = event.shiftKey ? KEY_STEP_FAST : KEY_STEP;
    const moves: Record<string, [number, number]> = {
      ArrowLeft: [-step, 0],
      ArrowRight: [step, 0],
      ArrowUp: [0, -step],
      ArrowDown: [0, step],
    };
    const move = moves[event.key];
    if (!move) return;
    event.preventDefault();
    const next = wheelValues(master, { x: puck.x + move[0], y: puck.y + move[1] }, space);
    edit.commit(() => onCommit(next));
  };

  return (
    <div className="flex min-w-0 flex-col items-center gap-1">
      <span className="max-w-full truncate text-xs text-fg-muted">{label}</span>
      <div className="flex items-stretch gap-2">
        <div
          ref={area}
          role="slider"
          tabIndex={0}
          aria-label={label}
          aria-roledescription={t("color.wheel.roleDescription")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={amount}
          aria-valuetext={valueText}
          onKeyDown={onKeyDown}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          className="relative size-[4.5rem] shrink-0 touch-none cursor-crosshair rounded-full outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
        >
          <svg
            viewBox={`${-VIEW} ${-VIEW} ${VIEW * 2} ${VIEW * 2}`}
            aria-hidden="true"
            focusable="false"
            className="block h-full w-full"
          >
            {WEDGES.map((wedge) => (
              <path key={wedge.key} d={wedge.d} fill={wedge.fill} />
            ))}
            <circle
              cx={0}
              cy={0}
              r={1}
              fill="none"
              stroke="var(--color-line-strong)"
              strokeWidth={0.02}
            />
            <circle cx={0} cy={0} r={0.03} fill="var(--color-app)" />
            <circle
              cx={puck.x}
              cy={puck.y}
              r={0.12}
              fill="var(--color-fg)"
              stroke="var(--color-app)"
              strokeWidth={0.04}
            />
          </svg>
        </div>
        <div className="flex flex-col items-center gap-1 py-0.5">
          <Slider
            orientation="vertical"
            aria-label={t("color.wheel.master", { name: label })}
            valueText={(next) => next.toFixed(2)}
            value={master}
            min={masterMin}
            max={masterMax}
            step={masterStep}
            onChange={(next) =>
              edit.change(() => onChange(withWheelMaster(latest.current, space, next)))
            }
            onCommit={(next) =>
              edit.commit(() => onCommit(withWheelMaster(latest.current, space, next)))
            }
            className="min-h-0 w-6 flex-1"
          />
          <span className="tabular text-2xs text-fg-muted">{master.toFixed(2)}</span>
        </div>
      </div>
    </div>
  );
}

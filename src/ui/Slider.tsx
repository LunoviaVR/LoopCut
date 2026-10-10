import { Slider as SliderPrimitive } from "radix-ui";
import { cn } from "@/lib/cn";

export interface SliderProps {
  value: number;
  min: number;
  max: number;
  step: number;
  disabled?: boolean;
  /** Horizontal by default. A vertical slider fills the height of its container (4px wide, 16px hit area). */
  orientation?: "horizontal" | "vertical";
  /** Fires continuously while the thumb moves (drag or arrow keys). Use it to update the preview. */
  onChange?: (value: number) => void;
  /** Fires once when the gesture ends. Use it to record one undo step per gesture. */
  onCommit?: (value: number) => void;
  /** Spoken value for the thumb, e.g. "1.00 x". Without it a screen reader hears the bare number. */
  valueText?: (value: number) => string;
  "aria-label": string;
  className?: string;
}

/**
 * Single-thumb slider: a 24px hit height (16px for a vertical one) around a 4px track. The fill is neutral.
 * It runs from the minimum, or from zero when the range dips below zero, and a 1px tick marks that zero.
 */
export function Slider({
  value,
  min,
  max,
  step,
  disabled,
  orientation = "horizontal",
  onChange,
  onCommit,
  valueText,
  "aria-label": label,
  className,
}: SliderProps) {
  const horizontal = orientation === "horizontal";
  const span = max - min;
  // Position along the track: 0 at the minimum, 100 at the maximum.
  const percentOf = (n: number) =>
    span > 0 ? Math.min(100, Math.max(0, ((n - min) / span) * 100)) : 0;
  const valuePercent = percentOf(value);
  const zeroPercent = percentOf(0);
  const originPercent = min < 0 ? zeroPercent : 0;
  const fillStart = Math.min(originPercent, valuePercent);
  const fillLength = Math.abs(valuePercent - originPercent);
  const showZeroTick = min < 0 && max > 0;

  return (
    <SliderPrimitive.Root
      className={cn(
        "relative flex touch-none items-center select-none data-disabled:opacity-40",
        horizontal ? "h-6 w-full" : "h-full min-h-16 w-4 flex-col",
        className,
      )}
      orientation={orientation}
      value={[value]}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      onValueChange={([next]) => {
        if (next !== undefined) onChange?.(next);
      }}
      onValueCommit={([next]) => {
        if (next !== undefined) onCommit?.(next);
      }}
    >
      <SliderPrimitive.Track
        className={cn(
          "relative grow overflow-hidden rounded-full bg-raised forced-colors:outline",
          horizontal ? "h-1 w-full" : "h-full w-1",
        )}
      >
        <span
          data-testid="slider-fill"
          aria-hidden="true"
          className={cn("absolute bg-fg-muted", horizontal ? "inset-y-0" : "inset-x-0")}
          style={
            horizontal
              ? { left: `${fillStart}%`, width: `${fillLength}%` }
              : { bottom: `${fillStart}%`, height: `${fillLength}%` }
          }
        />
        {showZeroTick ? (
          <span
            data-testid="slider-zero"
            aria-hidden="true"
            className={cn(
              "absolute bg-fg-subtle",
              horizontal ? "inset-y-0 w-px" : "inset-x-0 h-px",
            )}
            style={horizontal ? { left: `${zeroPercent}%` } : { bottom: `${zeroPercent}%` }}
          />
        ) : null}
      </SliderPrimitive.Track>
      <SliderPrimitive.Thumb
        aria-label={label}
        aria-valuetext={valueText?.(value)}
        className={cn(
          "block h-3 w-3 rounded-full bg-fg shadow-[0_0_0_2px_var(--color-panel)] transition-colors duration-150",
          "hover:bg-accent-hover active:bg-accent-press",
          "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent focus-visible:outline-solid",
          // High Contrast drops the thumb fill, so the outline keeps it visible.
          "forced-colors:outline-2 forced-colors:-outline-offset-2 forced-colors:outline-solid",
          "disabled:bg-fg-muted",
        )}
      />
    </SliderPrimitive.Root>
  );
}

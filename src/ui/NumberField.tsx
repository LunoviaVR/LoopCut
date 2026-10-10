import { type KeyboardEvent, type PointerEvent, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/cn";
import { decimalsOf, formatNumber, roundTo } from "./numberFormat";

export interface NumberFieldProps {
  label: string;
  value: number;
  min?: number;
  max?: number;
  /** Arrow keys and scrubbing move by this amount. Shift multiplies it by 10. Default 1. */
  step?: number;
  /** Shown after the number, e.g. "s", "%", "x". */
  unit?: string;
  /** Digits after the point when displayed. Default: the decimals in `step`. */
  decimals?: number;
  /** Width of the value box: sm 64px, md 80px (default), lg 96px. */
  width?: "sm" | "md" | "lg";
  disabled?: boolean;
  /** Fires on every scrub step and every committed edit. */
  onChange: (value: number) => void;
  /** Fires once per completed edit: Enter, blur, an arrow key press, or the end of a drag. */
  onCommit?: (value: number) => void;
  /** Double-clicking the label. SliderField uses this to reset to the default. */
  onLabelDoubleClick?: () => void;
  /** Tooltip on the label, e.g. the reset hint. */
  labelTitle?: string;
  /** Pixels of label drag per step. Default 4. */
  scrubPixelsPerStep?: number;
  /**
   * Where the value box sits in the value column. "start" (default) shares the control edge with other Field rows;
   * "end" lines it up with the right end of a full-width control below it, as SliderField does.
   */
  align?: "start" | "end";
  id?: string;
  className?: string;
}

const VALUE_WIDTH = { sm: "w-16", md: "w-20", lg: "w-24" } as const;

/**
 * Labelled numeric input for the inspector, 32px row on the Field grid: label column, then the value box at
 * the shared control edge. The label is also a drag handle: drag left or right to scrub. Typed values are
 * checked on Enter or blur; an out-of-range value shows a visible error and is not applied. Escape restores
 * the current value.
 */
export function NumberField({
  label,
  value,
  min,
  max,
  step = 1,
  unit,
  decimals,
  width = "md",
  disabled,
  onChange,
  onCommit,
  onLabelDoubleClick,
  labelTitle,
  scrubPixelsPerStep = 4,
  align = "start",
  id,
  className,
}: NumberFieldProps) {
  const { t } = useTranslation();
  const generatedId = useId();
  const inputId = id ?? generatedId;
  const errorId = `${inputId}-error`;
  const digits = decimals ?? decimalsOf(step);

  const formatted = formatNumber(value, digits);
  // `draft` holds what the user typed; null means the input shows the live value.
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const text = draft ?? formatted;
  const scrub = useRef<{ startX: number; startValue: number; latest: number } | null>(null);

  const clamp = (n: number) => {
    let next = n;
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    return roundTo(next, digits);
  };

  const commitDraft = () => {
    const raw = text.trim();
    const parsed = raw === "" ? Number.NaN : Number(raw.replace(",", "."));
    if (!Number.isFinite(parsed)) {
      setError(t("ui.numberNotNumber"));
      return;
    }
    const outOfRange = (min !== undefined && parsed < min) || (max !== undefined && parsed > max);
    if (outOfRange) {
      setError(
        min !== undefined && max !== undefined
          ? t("ui.numberInvalid", { min: min.toFixed(digits), max: max.toFixed(digits) })
          : t("ui.numberNotNumber"),
      );
      return;
    }
    setError(null);
    setDraft(null);
    const next = roundTo(parsed, digits);
    if (next !== value) {
      onChange(next);
      onCommit?.(next);
    }
  };

  const stepBy = (direction: 1 | -1, multiplier: number) => {
    const next = clamp(value + direction * step * multiplier);
    setError(null);
    setDraft(null);
    if (next !== value) {
      onChange(next);
      onCommit?.(next);
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      stepBy(event.key === "ArrowUp" ? 1 : -1, event.shiftKey ? 10 : 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      commitDraft();
    } else if (event.key === "Escape") {
      setError(null);
      setDraft(null);
    }
  };

  const onLabelPointerDown = (event: PointerEvent<HTMLLabelElement>) => {
    if (disabled || event.button !== 0) return;
    scrub.current = { startX: event.clientX, startValue: value, latest: value };
    event.currentTarget.setPointerCapture?.(event.pointerId);
  };

  const onLabelPointerMove = (event: PointerEvent<HTMLLabelElement>) => {
    const state = scrub.current;
    if (!state) return;
    const steps = Math.trunc((event.clientX - state.startX) / scrubPixelsPerStep);
    const multiplier = event.shiftKey ? 10 : 1;
    const next = clamp(state.startValue + steps * step * multiplier);
    if (next !== state.latest) {
      state.latest = next;
      setError(null);
      setDraft(null);
      onChange(next);
    }
  };

  const endScrub = (event: PointerEvent<HTMLLabelElement>) => {
    const state = scrub.current;
    if (!state) return;
    scrub.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    if (state.latest !== state.startValue) onCommit?.(state.latest);
  };

  return (
    <div className={cn("flex min-w-0 flex-col", className)}>
      <div className="grid min-h-8 grid-cols-[7rem_minmax(0,1fr)] items-center gap-3">
        <label
          htmlFor={inputId}
          title={labelTitle}
          onPointerDown={onLabelPointerDown}
          onPointerMove={onLabelPointerMove}
          onPointerUp={endScrub}
          onPointerCancel={endScrub}
          onDoubleClick={disabled ? undefined : onLabelDoubleClick}
          className={cn(
            "min-w-0 truncate text-xs text-fg-muted select-none",
            disabled ? "opacity-40" : "cursor-ew-resize hover:text-fg",
          )}
        >
          {label}
        </label>
        <div
          className={cn(
            "flex h-7 shrink-0 items-center gap-1 rounded-control border bg-raised px-2",
            align === "end" ? "justify-self-end" : "justify-self-start",
            VALUE_WIDTH[width],
            "focus-within:border-accent focus-within:outline-2 focus-within:outline-offset-1 focus-within:outline-solid focus-within:outline-accent",
            error ? "border-danger" : "border-line-control hover:border-fg-muted",
            disabled && "opacity-40",
          )}
        >
          <input
            id={inputId}
            type="text"
            inputMode="decimal"
            value={text}
            disabled={disabled}
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? errorId : undefined}
            onChange={(event) => {
              setDraft(event.target.value);
              if (error) setError(null);
            }}
            onBlur={commitDraft}
            onKeyDown={onKeyDown}
            className="tabular min-w-0 flex-1 bg-transparent text-right text-sm text-fg outline-none disabled:cursor-not-allowed"
          />
          {unit ? <span className="shrink-0 text-xs text-fg-muted">{unit}</span> : null}
        </div>
      </div>
      {error ? (
        <p id={errorId} role="alert" className="pb-1 text-xs text-danger">
          {error}
        </p>
      ) : null}
    </div>
  );
}

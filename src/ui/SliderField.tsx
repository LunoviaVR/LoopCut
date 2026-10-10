import { useId } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/cn";
import { NumberField } from "./NumberField";
import { decimalsOf, formatNumber } from "./numberFormat";
import { Slider } from "./Slider";

export interface SliderFieldProps {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  /** Shown after the number, e.g. "x" for speed. */
  unit?: string;
  decimals?: number;
  /** Value restored by double-clicking the label. */
  defaultValue: number;
  disabled?: boolean;
  /** Fires while dragging, scrubbing and typing. */
  onChange: (value: number) => void;
  /** Fires once per gesture: pointer up, arrow key, Enter, or label scrub end. Use for undo grouping. */
  onCommit: (value: number) => void;
  className?: string;
}

/**
 * Inspector control: a NumberField row over a full-width Slider. Both drive the same value. Double-click
 * the label to reset to `defaultValue`; the reset is a single committed change. The slider speaks the same
 * digits and unit as the number, e.g. "1.00 x".
 */
export function SliderField({
  label,
  value,
  min,
  max,
  step,
  unit,
  decimals,
  defaultValue,
  disabled,
  onChange,
  onCommit,
  className,
}: SliderFieldProps) {
  const { t } = useTranslation();
  const id = useId();
  const digits = decimals ?? decimalsOf(step);
  const valueText = (next: number) => {
    const text = formatNumber(next, digits);
    return unit ? `${text} ${unit}` : text;
  };
  const reset = () => {
    onChange(defaultValue);
    onCommit(defaultValue);
  };

  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <NumberField
        id={id}
        label={label}
        labelTitle={t("ui.sliderReset", { value: String(defaultValue) })}
        value={value}
        min={min}
        max={max}
        step={step}
        unit={unit}
        decimals={decimals}
        disabled={disabled}
        onChange={onChange}
        onCommit={onCommit}
        onLabelDoubleClick={reset}
        align="end"
      />
      <Slider
        aria-label={label}
        value={value}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        valueText={valueText}
        onChange={onChange}
        onCommit={onCommit}
      />
    </div>
  );
}

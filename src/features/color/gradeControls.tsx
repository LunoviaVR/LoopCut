import type { Grade } from "@/ipc/bindings/Grade";
import { useEditorStore } from "@/state/editor";
import { useEditGesture } from "@/state/gesture";
import { SliderField } from "@/ui";

/** The grade fields that hold one number. */
export type NumericGradeKey = {
  [K in keyof Grade]: Grade[K] extends number ? K : never;
}[keyof Grade];

export interface GradeSliderProps {
  field: NumericGradeKey;
  label: string;
  min: number;
  max: number;
  step: number;
  decimals: number;
  defaultValue: number;
  unit?: string;
}

/**
 * A SliderField bound to one number of the grade. A drag or a typed value is one undo step; a double-click on the
 * label restores `defaultValue` as a single step.
 */
export function GradeSlider({
  field,
  label,
  min,
  max,
  step,
  decimals,
  defaultValue,
  unit,
}: GradeSliderProps) {
  const stored = useEditorStore((state) => state.project?.grade[field]);
  const value = typeof stored === "number" ? stored : defaultValue;
  const edit = useEditGesture();
  const apply = (next: number) => {
    useEditorStore.getState().setGrade({ [field]: next } as Partial<Grade>);
  };
  return (
    <SliderField
      label={label}
      value={value}
      min={min}
      max={max}
      step={step}
      decimals={decimals}
      unit={unit}
      defaultValue={defaultValue}
      onChange={(next) => edit.change(() => apply(next))}
      onCommit={(next) => edit.commit(() => apply(next))}
    />
  );
}

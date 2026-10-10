import type { ReactNode } from "react";
import { cn } from "@/lib/cn";
import { NumberField, type NumberFieldProps } from "@/ui";

export interface SettingRowProps {
  label: string;
  /** Reason or detail under the label. Keep it to one or two short sentences. */
  hint?: ReactNode;
  children: ReactNode;
}

/**
 * One row of a settings form: a label column (wide enough for settings copy, unlike the 7rem inspector row)
 * and the control. The control names itself, so the label is a plain text element here.
 */
export function SettingRow({ label, hint, children }: SettingRowProps) {
  return (
    <div className="grid min-h-9 grid-cols-[minmax(0,11rem)_minmax(0,1fr)] items-center gap-4 py-1">
      <div className="min-w-0">
        <p className="text-sm text-fg-muted">{label}</p>
        {hint ? <div className="text-xs text-fg-muted">{hint}</div> : null}
      </div>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

/**
 * A NumberField inside a SettingRow. The row label already names the value, so the field keeps its own label for
 * assistive technology only (sr-only) and its grid drops the label column: the value box starts at the row's control
 * column instead of after a second label column.
 */
export function SettingNumberField({ className, ...props }: NumberFieldProps) {
  return (
    <NumberField
      {...props}
      className={cn("[&_label]:sr-only [&>div]:grid-cols-[minmax(0,1fr)]", className)}
    />
  );
}

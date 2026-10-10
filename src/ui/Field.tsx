import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface FieldProps {
  label: string;
  /** Id of the control this label names, when the control is not wrapped by the label. */
  htmlFor?: string;
  /** The control (Select, Switch, text, Badge row...). */
  children: ReactNode;
  /** Optional read-only or disabled reason shown under the row. */
  hint?: ReactNode;
  /**
   * inline (default): the label sits in the shared 7rem column and the control starts at the value edge.
   * stacked: the label sits above a full-width control, for long controls such as a font search.
   */
  layout?: "inline" | "stacked";
  className?: string;
}

const LABEL_CLASS = "min-w-0 truncate text-xs text-fg-muted";

/**
 * Inspector row. Inline: a 32px row with the label in a fixed 7rem column and the control to its right, so
 * every inspector value starts at the same edge. Long labels truncate rather than wrap so the rhythm holds.
 */
export function Field({
  label,
  htmlFor,
  children,
  hint,
  layout = "inline",
  className,
}: FieldProps) {
  if (layout === "stacked") {
    return (
      <div className={cn("flex min-w-0 flex-col gap-1", className)}>
        <label htmlFor={htmlFor} className={LABEL_CLASS}>
          {label}
        </label>
        <div className="min-w-0">{children}</div>
        {hint ? <p className="text-xs text-fg-muted">{hint}</p> : null}
      </div>
    );
  }

  return (
    <div className={cn("flex min-h-8 flex-col justify-center", className)}>
      <div className="grid grid-cols-[7rem_minmax(0,1fr)] items-center gap-3">
        <label htmlFor={htmlFor} className={LABEL_CLASS}>
          {label}
        </label>
        <div className="min-w-0">{children}</div>
      </div>
      {hint ? <p className="pt-1 text-xs text-fg-muted">{hint}</p> : null}
    </div>
  );
}

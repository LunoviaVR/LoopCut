import { cn } from "@/lib/cn";

export interface ProgressBarProps {
  /** Fraction complete, 0 to 1. Ignored while indeterminate. */
  value?: number;
  indeterminate?: boolean;
  tone?: "accent" | "ok" | "warn" | "danger";
  /** Accessible name, e.g. "Export progress". */
  label: string;
  className?: string;
}

const TONE = {
  accent: "bg-accent",
  ok: "bg-ok",
  warn: "bg-warn",
  danger: "bg-danger",
} as const;

/**
 * 4px track on raised graphite. Determinate bars expose aria-valuenow; indeterminate bars slide a short
 * segment and omit it. Reduced motion stops the slide, and the bar then reads as a static partial fill.
 */
export function ProgressBar({
  value = 0,
  indeterminate = false,
  tone = "accent",
  label,
  className,
}: ProgressBarProps) {
  const percent = Math.round(Math.min(1, Math.max(0, value)) * 100);
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={indeterminate ? undefined : percent}
      className={cn("relative h-1 w-full overflow-hidden rounded-full bg-raised", className)}
    >
      {indeterminate ? (
        <div
          className={cn(
            "absolute inset-y-0 left-0 w-1/3 rounded-full animate-indeterminate motion-reduce:animate-none",
            TONE[tone],
          )}
        />
      ) : (
        <div
          className={cn(
            "absolute inset-y-0 left-0 rounded-full transition-[width] duration-200 ease-out-quick",
            TONE[tone],
          )}
          style={{ width: `${percent}%` }}
        />
      )}
    </div>
  );
}

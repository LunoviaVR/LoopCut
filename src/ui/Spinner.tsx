import { LoaderCircle } from "lucide-react";
import { cn } from "@/lib/cn";

export interface SpinnerProps {
  /** Pixel size. Inline button loading uses 14. */
  size?: number;
  /** Announced to assistive tech as a status. Omit for decorative spinners inside a labelled control. */
  label?: string;
  className?: string;
}

/** Inline loading indicator. Use inside Button `loading`; full-area loading uses ProgressBar. */
export function Spinner({ size = 14, label, className }: SpinnerProps) {
  const icon = (
    <LoaderCircle
      size={size}
      strokeWidth={1.5}
      aria-hidden={label ? undefined : true}
      className={cn("animate-spin text-current motion-reduce:animate-none", className)}
    />
  );
  if (!label) return icon;
  return (
    <span role="status" aria-label={label} className="inline-flex">
      {icon}
    </span>
  );
}

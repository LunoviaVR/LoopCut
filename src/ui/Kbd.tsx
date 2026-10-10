import { cn } from "@/lib/cn";

export interface KbdProps {
  /** Shortcut as text, e.g. `Ctrl+E`, `Ctrl+Shift+Z`, `Space`, `ArrowLeft`. Parts are split on `+`. */
  keys: string;
  className?: string;
}

/** Renders a keyboard shortcut as key caps. Text is kept in the DOM so screen readers read the keys. */
export function Kbd({ keys, className }: KbdProps) {
  const parts = keys
    .split("+")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  return (
    <span className={cn("inline-flex shrink-0 items-center gap-0.5", className)}>
      {parts.map((part) => (
        <kbd
          key={part}
          className="inline-flex h-4 min-w-4 items-center justify-center rounded-chip border border-line bg-raised px-1 text-2xs leading-none font-medium text-fg-muted"
        >
          {part}
        </kbd>
      ))}
    </span>
  );
}

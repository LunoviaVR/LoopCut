import type { LucideIcon } from "lucide-react";
import { ToggleGroup as ToggleGroupPrimitive } from "radix-ui";
import { cn } from "@/lib/cn";

export interface SegmentedItem<T extends string> {
  value: T;
  label: string;
  icon?: LucideIcon;
}

export interface SegmentedControlProps<T extends string> {
  value: T;
  onValueChange: (value: T) => void;
  items: readonly SegmentedItem<T>[];
  /** Names the group, e.g. "Direction". */
  "aria-label": string;
  disabled?: boolean;
  className?: string;
}

/**
 * Exactly-one choice shown as a row of buttons (e.g. forward, reverse, ping-pong). Arrow keys move
 * between items; the selected item is announced as checked. Clicking the selected item keeps it selected.
 * The selected item is the canonical selected look (see selected.ts).
 */
export function SegmentedControl<T extends string>({
  value,
  onValueChange,
  items,
  "aria-label": label,
  disabled,
  className,
}: SegmentedControlProps<T>) {
  return (
    <ToggleGroupPrimitive.Root
      type="single"
      value={value}
      onValueChange={(next) => {
        // Radix reports "" when the selected item is clicked again; a choice must always be set.
        if (next) onValueChange(next as T);
      }}
      aria-label={label}
      disabled={disabled}
      className={cn(
        "inline-flex items-center gap-px rounded-control border border-line bg-raised p-0.5",
        disabled && "opacity-40",
        className,
      )}
    >
      {items.map((item) => {
        const Icon = item.icon;
        return (
          <ToggleGroupPrimitive.Item
            key={item.value}
            value={item.value}
            className={cn(
              "inline-flex h-6 items-center justify-center gap-1 rounded-chip px-2 text-xs whitespace-nowrap text-fg-muted select-none",
              "transition-colors duration-150 ease-out-quick",
              "hover:bg-hover hover:text-fg active:bg-overlay",
              "data-[state=on]:bg-accent-soft data-[state=on]:text-fg data-[state=on]:shadow-[inset_0_0_0_1px_var(--color-accent)]",
              "disabled:pointer-events-none",
            )}
          >
            {Icon ? <Icon size={16} strokeWidth={1.5} aria-hidden="true" /> : null}
            {item.label}
          </ToggleGroupPrimitive.Item>
        );
      })}
    </ToggleGroupPrimitive.Root>
  );
}

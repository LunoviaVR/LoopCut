import { Switch as SwitchPrimitive } from "radix-ui";
import { type ReactNode, useId } from "react";
import { cn } from "@/lib/cn";

export interface SwitchProps {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  id?: string;
  className?: string;
}

/**
 * On/off setting, 28x16 track. The label is the accessible name (clicking it toggles), and the description
 * is linked as the accessible description. A pseudo-element grows the hit box to 24px tall; the track stays 16px.
 */
export function Switch({
  checked,
  onCheckedChange,
  label,
  description,
  disabled,
  id,
  className,
}: SwitchProps) {
  const generatedId = useId();
  const switchId = id ?? generatedId;
  const descriptionId = `${switchId}-description`;

  return (
    <div className={cn("flex min-h-8 items-start justify-between gap-3 py-1", className)}>
      <div className="min-w-0 flex-1">
        <label
          htmlFor={switchId}
          className={cn(
            "block text-sm text-fg select-none",
            disabled ? "opacity-40" : "cursor-pointer",
          )}
        >
          {label}
        </label>
        {description ? (
          <p id={descriptionId} className="text-xs text-fg-muted">
            {description}
          </p>
        ) : null}
      </div>
      <SwitchPrimitive.Root
        id={switchId}
        checked={checked}
        onCheckedChange={onCheckedChange}
        disabled={disabled}
        aria-describedby={description ? descriptionId : undefined}
        className={cn(
          "relative mt-0.5 inline-flex h-4 w-7 shrink-0 items-center rounded-full border border-line-control bg-raised",
          "transition-colors duration-150 ease-out-quick",
          "hover:border-fg-muted active:bg-overlay",
          "data-[state=checked]:border-accent data-[state=checked]:bg-accent",
          "disabled:pointer-events-none disabled:opacity-40",
          "cursor-pointer",
          // The 1px border sits outside the containing block, so a 5px outset gives 14px + 10px = 24px.
          "before:absolute before:-inset-[5px] before:content-['']",
        )}
      >
        <SwitchPrimitive.Thumb
          className={cn(
            "block h-3 w-3 translate-x-px rounded-full bg-fg-muted transition-transform duration-150 ease-out-quick",
            "data-[state=checked]:translate-x-[13px] data-[state=checked]:bg-app",
            // High Contrast drops the thumb fill, so the outline keeps the on or off position visible.
            "forced-colors:outline-2 forced-colors:-outline-offset-2 forced-colors:outline-solid",
          )}
        />
      </SwitchPrimitive.Root>
    </div>
  );
}

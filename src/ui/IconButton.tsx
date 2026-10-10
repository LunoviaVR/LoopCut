import type { LucideIcon } from "lucide-react";
import type { ComponentPropsWithRef } from "react";
import { cn } from "@/lib/cn";
import { Button } from "./Button";
import { selectedChipClass } from "./selected";
import { Tooltip } from "./Tooltip";

export interface IconButtonProps extends Omit<ComponentPropsWithRef<"button">, "children"> {
  /** Required: an icon-only control must have a name. Also used as the tooltip text. */
  "aria-label": string;
  icon: LucideIcon;
  /** Shortcut shown in the tooltip, e.g. `Ctrl+Z`. */
  shortcut?: string;
  /** Show the tooltip on hover and focus. Default true. */
  tooltip?: boolean;
  /** Toggle state. Sets aria-pressed; the pressed look is the shared selected recipe. */
  pressed?: boolean;
  variant?: "ghost" | "secondary";
  size?: "sm" | "md";
  className?: string;
}

/** Icon-only button, 28px (24px when `size="sm"`), 16px icon at 1.5px stroke. */
export function IconButton({
  icon: Icon,
  "aria-label": label,
  shortcut,
  tooltip = true,
  pressed,
  variant = "ghost",
  size = "md",
  className,
  ...rest
}: IconButtonProps) {
  const button = (
    <Button
      variant={variant}
      size={size}
      aria-label={label}
      aria-pressed={pressed}
      className={cn(size === "sm" ? "h-6 w-6" : "h-7 w-7", "px-0", selectedChipClass, className)}
      {...rest}
    >
      <Icon size={16} strokeWidth={1.5} aria-hidden="true" />
    </Button>
  );
  if (!tooltip) return button;
  // A disabled button takes no pointer events, so the tooltip hangs on a wrapper that still gets hover. The wrapper
  // is always there: adding it only while disabled would remount the button whenever its state changed.
  return (
    <Tooltip content={label} shortcut={shortcut}>
      <span className="inline-flex">{button}</span>
    </Tooltip>
  );
}

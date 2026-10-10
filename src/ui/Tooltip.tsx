import { Tooltip as TooltipPrimitive } from "radix-ui";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Kbd } from "./Kbd";

export interface TooltipProps {
  /** Action name, e.g. "Export". Controls name their action. */
  content: ReactNode;
  /** Optional shortcut shown as key caps after the name. */
  shortcut?: string;
  side?: "top" | "right" | "bottom" | "left";
  sideOffset?: number;
  delayDuration?: number;
  className?: string;
  /** A single element. It becomes the trigger (asChild), so it must forward ref and props. */
  children: ReactNode;
}

/**
 * Hover and focus tooltip. The element only needs to be focusable, so keyboard users get the same hint.
 * Each tooltip carries its own provider, so no app-level setup is needed. Disabled buttons do not fire
 * pointer events; put the tooltip on a wrapper if a disabled control must still explain itself.
 */
export function Tooltip({
  content,
  shortcut,
  side = "top",
  sideOffset = 6,
  delayDuration = 350,
  className,
  children,
}: TooltipProps) {
  return (
    <TooltipPrimitive.Provider delayDuration={delayDuration}>
      <TooltipPrimitive.Root>
        <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
        <TooltipPrimitive.Portal>
          <TooltipPrimitive.Content
            side={side}
            sideOffset={sideOffset}
            collisionPadding={8}
            className={cn(
              "z-50 inline-flex items-center gap-2 rounded-control bg-overlay px-2 py-1 text-xs whitespace-nowrap text-fg",
              "shadow-tooltip",
              className,
            )}
          >
            <span>{content}</span>
            {shortcut ? <Kbd keys={shortcut} className="text-fg-muted" /> : null}
          </TooltipPrimitive.Content>
        </TooltipPrimitive.Portal>
      </TooltipPrimitive.Root>
    </TooltipPrimitive.Provider>
  );
}

import { ChevronRight } from "lucide-react";
import { Collapsible as CollapsiblePrimitive } from "radix-ui";
import { type ReactNode, useState } from "react";
import { cn } from "@/lib/cn";

export interface SectionProps {
  title: string;
  children?: ReactNode;
  defaultOpen?: boolean;
  /** Controlled open state. Omit to let the section manage its own. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Extra controls on the heading row, e.g. a reset IconButton. */
  actions?: ReactNode;
  className?: string;
}

export interface SectionHeadingProps {
  title: string;
  /** Extra controls on the right of the heading row. */
  actions?: ReactNode;
  className?: string;
}

/**
 * Heading for a group that does not collapse, such as a settings group. Same 32px row and hairline as
 * Section, with no chevron and no button, so the title stays the heading and nothing toggles.
 */
export function SectionHeading({ title, actions, className }: SectionHeadingProps) {
  return (
    <div
      className={cn(
        "flex h-8 items-center justify-between gap-2 border-b border-line text-sm font-medium text-fg",
        className,
      )}
    >
      <h3 className="min-w-0 truncate">{title}</h3>
      {actions ? <div className="flex shrink-0 items-center gap-0.5">{actions}</div> : null}
    </div>
  );
}

/**
 * Collapsible inspector section. The heading is a button inside an h3, so the title is the accessible
 * name and aria-expanded reports the state. Arrow chevron rotates on open.
 */
export function Section({
  title,
  children,
  defaultOpen = true,
  open: controlledOpen,
  onOpenChange,
  actions,
  className,
}: SectionProps) {
  const [uncontrolledOpen, setUncontrolledOpen] = useState(defaultOpen);
  const open = controlledOpen ?? uncontrolledOpen;
  const setOpen = (next: boolean) => {
    if (controlledOpen === undefined) setUncontrolledOpen(next);
    onOpenChange?.(next);
  };

  return (
    <CollapsiblePrimitive.Root
      open={open}
      onOpenChange={setOpen}
      className={cn("border-b border-line", className)}
    >
      <div className="flex items-center pr-2">
        <h3 className="min-w-0 flex-1">
          <CollapsiblePrimitive.Trigger
            className={cn(
              "flex h-8 w-full items-center gap-1.5 px-3 text-left text-sm font-medium text-fg select-none",
              "transition-colors duration-150 ease-out-quick hover:bg-hover",
            )}
          >
            <ChevronRight
              size={16}
              strokeWidth={1.5}
              aria-hidden="true"
              className={cn(
                "shrink-0 text-fg-muted transition-transform duration-150 ease-out-quick",
                open && "rotate-90",
              )}
            />
            <span className="truncate">{title}</span>
          </CollapsiblePrimitive.Trigger>
        </h3>
        {actions ? <div className="flex shrink-0 items-center gap-0.5">{actions}</div> : null}
      </div>
      <CollapsiblePrimitive.Content className="flex flex-col gap-1 px-3 pt-1 pb-3">
        {children}
      </CollapsiblePrimitive.Content>
    </CollapsiblePrimitive.Root>
  );
}

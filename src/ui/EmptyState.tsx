import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface EmptyStateProps {
  icon: LucideIcon;
  title: string;
  body?: ReactNode;
  /** Usually one primary Button that does the thing the title asks for. */
  action?: ReactNode;
  /** Heading level for the title; pick the level that fits the page outline. Default 2. */
  headingLevel?: 2 | 3;
  /** Smaller icon tile, tighter spacing and a 14px title, for short containers such as a dock or a small panel. */
  compact?: boolean;
  className?: string;
}

/** Centered message for a surface with nothing in it yet. Says what to do, not only that it is empty. */
export function EmptyState({
  icon: Icon,
  title,
  body,
  action,
  headingLevel = 2,
  compact = false,
  className,
}: EmptyStateProps) {
  const Heading = headingLevel === 2 ? "h2" : "h3";
  return (
    <div
      className={cn(
        "flex max-w-sm flex-col items-center px-4 text-center",
        compact ? "gap-2" : "gap-3",
        className,
      )}
    >
      <div
        className={cn(
          "flex items-center justify-center rounded-panel bg-raised text-fg-muted",
          compact ? "h-8 w-8" : "h-10 w-10",
        )}
      >
        <Icon size={compact ? 16 : 20} strokeWidth={1.5} aria-hidden="true" />
      </div>
      <Heading className={cn("font-medium text-fg", compact ? "text-sm" : "text-base")}>
        {title}
      </Heading>
      {body ? <p className="text-sm text-fg-muted">{body}</p> : null}
      {action ? <div className="pt-1">{action}</div> : null}
    </div>
  );
}

import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

export interface PanelProps {
  /** Header title. Omit the header entirely for a bare tile such as the monitor. */
  title?: ReactNode;
  /** Header actions, right-aligned (IconButtons at 24px). */
  actions?: ReactNode;
  /** Pinned under the body with a hairline, e.g. the export summary. */
  footer?: ReactNode;
  children?: ReactNode;
  as?: "section" | "aside" | "div";
  /** Accessible name for the landmark. Required when `as` is section or aside. */
  "aria-label"?: string;
  className?: string;
  /** Classes for the scrolling body. With a footer it keeps 12px of bottom padding; pass pb-0 to drop it. */
  bodyClassName?: string;
}

/**
 * A rounded graphite tile (bg-panel, rounded-panel). The header is 32px with a hairline underneath; the
 * body is the only scroll container, so a tall inspector scrolls while its header and footer stay put.
 */
export function Panel({
  title,
  actions,
  footer,
  children,
  as: Tag = "div",
  "aria-label": ariaLabel,
  className,
  bodyClassName,
}: PanelProps) {
  return (
    <Tag
      aria-label={ariaLabel}
      className={cn(
        "flex min-h-0 min-w-0 flex-col overflow-hidden rounded-panel bg-panel",
        className,
      )}
    >
      {title || actions ? (
        <div className="flex h-8 shrink-0 items-center justify-between gap-2 border-b border-line pr-1.5 pl-3">
          {title ? (
            <h2 className="min-w-0 truncate text-base font-semibold text-fg">{title}</h2>
          ) : (
            <span />
          )}
          {actions ? <div className="flex shrink-0 items-center gap-0.5">{actions}</div> : null}
        </div>
      ) : null}
      <div
        className={cn("min-h-0 flex-1 overflow-auto", footer ? "pb-3" : undefined, bodyClassName)}
      >
        {children}
      </div>
      {footer ? <div className="shrink-0 border-t border-line">{footer}</div> : null}
    </Tag>
  );
}

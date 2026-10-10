import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentPropsWithRef } from "react";
import { cn } from "@/lib/cn";

/**
 * Small status or fact label, 18px tall. Accent is solid with graphite text (the accent-soft fill fails
 * text contrast). Ok, warn and danger tint the fill at 15% and color the text.
 */
export const badgeVariants = cva(
  "tabular inline-flex h-[18px] shrink-0 items-center gap-1 rounded-control px-1.5 text-2xs leading-none font-medium whitespace-nowrap",
  {
    variants: {
      variant: {
        neutral: "bg-raised text-fg-muted",
        accent: "bg-accent text-app",
        ok: "bg-ok/15 text-ok",
        warn: "bg-warn/15 text-warn",
        danger: "bg-danger/15 text-danger",
      },
    },
    defaultVariants: {
      variant: "neutral",
    },
  },
);

export interface BadgeProps
  extends ComponentPropsWithRef<"span">,
    VariantProps<typeof badgeVariants> {}

export function Badge({ variant, className, ...rest }: BadgeProps) {
  return <span className={cn(badgeVariants({ variant }), className)} {...rest} />;
}

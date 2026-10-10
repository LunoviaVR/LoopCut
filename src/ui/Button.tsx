import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentPropsWithRef, ReactNode } from "react";
import { cn } from "@/lib/cn";
import { Spinner } from "./Spinner";

/**
 * Primary text on the azure accent uses the app graphite (4.9:1); the pressed state flips to white (5.1:1)
 * because the press color is darker. The accent itself cannot carry white text at AA contrast.
 */
export const buttonVariants = cva(
  [
    "inline-flex shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-control font-medium select-none",
    "transition-colors duration-150 ease-out-quick",
    "disabled:pointer-events-none disabled:opacity-40",
    "aria-busy:cursor-progress",
  ].join(" "),
  {
    variants: {
      variant: {
        primary:
          "bg-accent text-app hover:bg-accent-hover active:bg-accent-press active:text-accent-fg",
        secondary:
          "border border-line bg-raised text-fg hover:border-line-strong hover:bg-overlay active:bg-hover",
        ghost: "text-fg-muted hover:bg-hover hover:text-fg active:bg-raised",
        danger: "bg-danger text-app hover:bg-danger/90 active:bg-danger/80",
      },
      size: {
        sm: "h-6 px-2 text-xs",
        md: "h-7 px-3 text-sm",
      },
    },
    defaultVariants: {
      variant: "secondary",
      size: "md",
    },
  },
);

export interface ButtonProps
  extends ComponentPropsWithRef<"button">,
    VariantProps<typeof buttonVariants> {
  /** Shows a spinner in place of the leading icon and blocks repeat activation. */
  loading?: boolean;
  leadingIcon?: ReactNode;
  /** Trailing content, typically a Kbd shortcut hint. */
  trailing?: ReactNode;
}

export function Button({
  variant,
  size,
  loading = false,
  leadingIcon,
  trailing,
  disabled,
  className,
  children,
  type = "button",
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={cn(buttonVariants({ variant, size }), className)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading ? <Spinner size={14} /> : leadingIcon}
      {children}
      {trailing}
    </button>
  );
}

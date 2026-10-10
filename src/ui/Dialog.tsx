import { X } from "lucide-react";
import { Dialog as DialogPrimitive } from "radix-ui";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/cn";
import { Button } from "./Button";

export interface DialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  /** One sentence on what the dialog does or what will happen. */
  description?: string;
  /** Actions, right-aligned under a hairline. Put the safe action last and style it primary. */
  footer?: ReactNode;
  size?: "sm" | "md" | "lg";
  children?: ReactNode;
  className?: string;
}

const SIZE: Record<NonNullable<DialogProps["size"]>, string> = {
  sm: "w-[22rem]",
  md: "w-[30rem]",
  lg: "w-[40rem]",
};

/**
 * Modal dialog on the overlay surface with a soft shadow (no border). Esc and the close button dismiss it,
 * focus moves into the dialog and returns to the trigger on close, and the title is the accessible name.
 */
export function Dialog({
  open,
  onOpenChange,
  title,
  description,
  footer,
  size = "md",
  children,
  className,
}: DialogProps) {
  const { t } = useTranslation();
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-app/70" />
        <DialogPrimitive.Content
          // Radix warns when no description is linked; an explicit undefined opts out of that check.
          {...(description ? {} : { "aria-describedby": undefined })}
          className={cn(
            "fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100vh-2rem)] max-w-[calc(100vw-2rem)] -translate-x-1/2 -translate-y-1/2 flex-col rounded-dialog bg-overlay shadow-overlay",
            SIZE[size],
            className,
          )}
        >
          <div className="flex items-start justify-between gap-3 px-4 pt-4">
            <div className="min-w-0">
              <DialogPrimitive.Title className="text-base font-semibold text-fg">
                {title}
              </DialogPrimitive.Title>
              {description ? (
                <DialogPrimitive.Description className="mt-1 text-sm text-fg-muted">
                  {description}
                </DialogPrimitive.Description>
              ) : null}
            </div>
            <DialogPrimitive.Close asChild>
              <Button
                variant="ghost"
                size="sm"
                aria-label={t("ui.close")}
                className="h-6 w-6 shrink-0 px-0"
              >
                <X size={16} strokeWidth={1.5} aria-hidden="true" />
              </Button>
            </DialogPrimitive.Close>
          </div>
          {children ? (
            <div className="min-h-0 flex-1 overflow-auto px-4 pt-3 pb-4 text-sm text-fg">
              {children}
            </div>
          ) : null}
          {footer ? (
            <div className="flex items-center justify-end gap-2 border-t border-line px-4 py-3">
              {footer}
            </div>
          ) : null}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

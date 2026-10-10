import { Tabs as TabsPrimitive } from "radix-ui";
import type { ComponentPropsWithRef } from "react";
import { cn } from "@/lib/cn";

/**
 * Tabs in two orientations. Horizontal tabs are underlined; vertical tabs are 32px list rows that take the
 * shared selected look when active. Arrow keys follow the orientation, Home and End jump to the ends,
 * and selection follows focus (automatic activation).
 */
export function Tabs({
  orientation = "horizontal",
  className,
  ...props
}: ComponentPropsWithRef<typeof TabsPrimitive.Root> & { orientation?: "horizontal" | "vertical" }) {
  return (
    <TabsPrimitive.Root
      orientation={orientation}
      className={cn(orientation === "vertical" ? "flex flex-row" : "flex flex-col", className)}
      {...props}
    />
  );
}

export function TabsList({
  className,
  ...props
}: ComponentPropsWithRef<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      className={cn(
        "flex shrink-0 gap-0.5 data-[orientation=horizontal]:items-end data-[orientation=horizontal]:border-b data-[orientation=horizontal]:border-line data-[orientation=vertical]:flex-col",
        className,
      )}
      {...props}
    />
  );
}

export function TabsTrigger({
  className,
  ...props
}: ComponentPropsWithRef<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      className={cn(
        "inline-flex items-center justify-center gap-1.5 text-sm font-medium text-fg-muted select-none",
        "transition-colors duration-150 ease-out-quick",
        "hover:text-fg active:text-fg disabled:pointer-events-none disabled:opacity-40",
        "data-[orientation=horizontal]:h-8 data-[orientation=horizontal]:-mb-px data-[orientation=horizontal]:border-b-2 data-[orientation=horizontal]:border-transparent data-[orientation=horizontal]:px-2.5",
        "data-[orientation=horizontal]:data-[state=active]:border-accent data-[orientation=horizontal]:data-[state=active]:text-fg",
        "data-[orientation=vertical]:h-8 data-[orientation=vertical]:w-full data-[orientation=vertical]:justify-start data-[orientation=vertical]:rounded-control data-[orientation=vertical]:px-2.5 data-[orientation=vertical]:text-left",
        // Vertical tab lists sit on dialog surfaces, where the solid hover token is darker than the surface; a light
        // tint reads on any background.
        "data-[orientation=vertical]:hover:bg-fg/6",
        "data-[orientation=vertical]:data-[state=active]:bg-accent-soft data-[orientation=vertical]:data-[state=active]:text-fg data-[orientation=vertical]:data-[state=active]:shadow-[inset_0_0_0_1px_var(--color-accent)]",
        className,
      )}
      {...props}
    />
  );
}

export function TabsContent({
  className,
  ...props
}: ComponentPropsWithRef<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content className={cn("min-h-0 flex-1 outline-none", className)} {...props} />
  );
}

import { CircleCheck, CircleX, Info, type LucideIcon, TriangleAlert, X } from "lucide-react";
import { Toast as ToastPrimitive } from "radix-ui";
import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/cn";
import { Button } from "./Button";

export type ToastVariant = "info" | "success" | "warn" | "error";

export interface ToastOptions {
  title: string;
  description?: string;
  variant?: ToastVariant;
  /** Optional recovery action, e.g. "Import anyway". The toast closes after the action runs. */
  action?: { label: string; onAction: () => void };
  /** Milliseconds before the toast auto-closes. Errors with an action should pass a longer value. */
  duration?: number;
}

interface ToastItem extends ToastOptions {
  id: string;
  variant: ToastVariant;
}

let items: readonly ToastItem[] = [];
let counter = 0;
const listeners = new Set<() => void>();

function publish(next: readonly ToastItem[]) {
  items = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function snapshot() {
  return items;
}

/** Shows a toast from anywhere, including outside React. Returns the id for dismissToast. */
export function toast(options: ToastOptions): string {
  counter += 1;
  const id = `toast-${counter}`;
  publish([...items, { ...options, id, variant: options.variant ?? "info" }]);
  return id;
}

/** Test hook: removes every toast, so one test's toasts do not show in the next. */
export function resetToasts(): void {
  publish([]);
}

export function dismissToast(id: string): void {
  publish(items.filter((item) => item.id !== id));
}

const VARIANT_ICON: Record<ToastVariant, { Icon: LucideIcon; tone: string; label: string }> = {
  // Info is a notice, not an action, so it keeps the neutral text colour and leaves azure to the roles.
  info: { Icon: Info, tone: "text-fg-muted", label: "ui.toastInfo" },
  success: { Icon: CircleCheck, tone: "text-ok", label: "ui.toastSuccess" },
  warn: { Icon: TriangleAlert, tone: "text-warn", label: "ui.toastWarn" },
  error: { Icon: CircleX, tone: "text-danger", label: "ui.toastError" },
};

function ToastView({ item }: { item: ToastItem }) {
  const { t } = useTranslation();
  const { Icon, tone, label } = VARIANT_ICON[item.variant];
  return (
    <ToastPrimitive.Root
      // Problems interrupt the screen reader at once (foreground); notices wait their turn (background).
      type={item.variant === "warn" || item.variant === "error" ? "foreground" : "background"}
      duration={item.duration}
      onOpenChange={(open) => {
        if (!open) dismissToast(item.id);
      }}
      className={cn(
        "grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-3 rounded-dialog bg-overlay p-3",
        "shadow-toast",
      )}
    >
      <Icon
        size={16}
        strokeWidth={1.5}
        role="img"
        aria-label={t(label)}
        className={cn("mt-0.5 shrink-0", tone)}
      />
      <div className="min-w-0 [overflow-wrap:anywhere]">
        <ToastPrimitive.Title className="text-sm font-medium text-fg">
          {item.title}
        </ToastPrimitive.Title>
        {item.description ? (
          <ToastPrimitive.Description className="mt-0.5 text-xs text-fg-muted">
            {item.description}
          </ToastPrimitive.Description>
        ) : null}
        {item.action ? (
          <div className="mt-2">
            <ToastPrimitive.Action asChild altText={item.action.label}>
              <Button variant="secondary" size="sm" onClick={item.action.onAction}>
                {item.action.label}
              </Button>
            </ToastPrimitive.Action>
          </div>
        ) : null}
      </div>
      <ToastPrimitive.Close asChild>
        <Button variant="ghost" size="sm" aria-label={t("ui.dismiss")} className="h-6 w-6 px-0">
          <X size={16} strokeWidth={1.5} aria-hidden="true" />
        </Button>
      </ToastPrimitive.Close>
    </ToastPrimitive.Root>
  );
}

/**
 * Mount once near the root. Toasts stack bottom-right; errors use the foreground (assertive) live region.
 */
export function Toaster() {
  const list = useSyncExternalStore(subscribe, snapshot, snapshot);
  return (
    <ToastPrimitive.Provider swipeDirection="right" duration={6000}>
      {list.map((item) => (
        <ToastView key={item.id} item={item} />
      ))}
      <ToastPrimitive.Viewport className="fixed right-4 bottom-4 z-[60] flex w-[22.5rem] max-w-[calc(100vw-2rem)] flex-col gap-2 outline-none" />
    </ToastPrimitive.Provider>
  );
}

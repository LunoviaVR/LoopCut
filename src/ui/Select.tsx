import { Check, ChevronDown } from "lucide-react";
import { Select as SelectPrimitive } from "radix-ui";
import { cn } from "@/lib/cn";
import { menuContentClass, menuItemClass } from "./menu";

export interface SelectOption<T extends string = string> {
  value: T;
  label: string;
  disabled?: boolean;
}

export interface SelectProps<T extends string = string> {
  value: T;
  onValueChange: (value: T) => void;
  options: readonly SelectOption<T>[];
  /** Names the control, since the visible value alone does not say what it sets. */
  "aria-label": string;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}

/** Single choice from a list, 28px trigger. Keyboard: arrows move, Enter selects, Escape closes. */
export function Select<T extends string = string>({
  value,
  onValueChange,
  options,
  "aria-label": label,
  placeholder,
  disabled,
  className,
}: SelectProps<T>) {
  return (
    <SelectPrimitive.Root
      value={value}
      onValueChange={(next) => onValueChange(next as T)}
      disabled={disabled}
    >
      <SelectPrimitive.Trigger
        aria-label={label}
        className={cn(
          "inline-flex h-7 w-full min-w-0 items-center justify-between gap-2 rounded-control border border-line-control bg-raised px-2 text-left text-sm text-fg select-none",
          "transition-colors duration-150 ease-out-quick",
          "hover:border-fg-muted hover:bg-overlay data-[state=open]:border-fg-muted",
          "disabled:pointer-events-none disabled:opacity-40",
          className,
        )}
      >
        <span className="truncate">
          <SelectPrimitive.Value placeholder={placeholder} />
        </span>
        <SelectPrimitive.Icon asChild>
          <ChevronDown
            size={16}
            strokeWidth={1.5}
            aria-hidden="true"
            className="shrink-0 text-fg-muted"
          />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Content
          position="popper"
          sideOffset={4}
          collisionPadding={8}
          className={cn(menuContentClass, "min-w-(--radix-select-trigger-width)")}
        >
          <SelectPrimitive.Viewport>
            {options.map((option) => (
              <SelectPrimitive.Item
                key={option.value}
                value={option.value}
                disabled={option.disabled}
                className={menuItemClass}
              >
                <SelectPrimitive.ItemIndicator className="absolute left-2 inline-flex">
                  <Check size={16} strokeWidth={1.5} aria-hidden="true" className="text-accent" />
                </SelectPrimitive.ItemIndicator>
                <SelectPrimitive.ItemText>{option.label}</SelectPrimitive.ItemText>
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Viewport>
        </SelectPrimitive.Content>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}

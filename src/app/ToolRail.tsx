import { Captions, Crop, Download, Film, type LucideIcon, Palette, Scissors } from "lucide-react";
import { type KeyboardEvent, useRef } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/cn";
import { selectedFillClass, Tooltip } from "@/ui";

export type ToolId = "media" | "trim" | "crop" | "color" | "captions" | "export";

export interface ToolDefinition {
  id: ToolId;
  labelKey: string;
  icon: LucideIcon;
  /** Shortcut in the app's `Ctrl+N` form. */
  shortcut: string;
}

export const TOOLS: readonly ToolDefinition[] = [
  { id: "media", labelKey: "tools.media", icon: Film, shortcut: "Ctrl+1" },
  { id: "trim", labelKey: "tools.trim", icon: Scissors, shortcut: "Ctrl+2" },
  { id: "crop", labelKey: "tools.crop", icon: Crop, shortcut: "Ctrl+3" },
  { id: "color", labelKey: "tools.color", icon: Palette, shortcut: "Ctrl+4" },
  { id: "captions", labelKey: "tools.captions", icon: Captions, shortcut: "Ctrl+5" },
  { id: "export", labelKey: "tools.export", icon: Download, shortcut: "Ctrl+6" },
];

/** The id of the inspector tile, the tabpanel that each tool tab controls. */
export const INSPECTOR_PANEL_ID = "inspector-panel";

/** The DOM id of a tool's tab, which the inspector names as its label. */
export function toolTabId(tool: ToolId): string {
  return `tool-tab-${tool}`;
}

export interface ToolRailProps {
  value: ToolId;
  onValueChange: (tool: ToolId) => void;
  /** The panel the tabs control, when it is on screen (the inspector can be collapsed). */
  controlsId?: string;
}

/**
 * Vertical tool tabs in the 56px rail: a 48px tile with the 16px icon and the 11px name under it. Roving tabindex:
 * Tab enters the rail on the active tool, Up and Down move between tools and select them, Home and End jump to the
 * ends. Each tab has a tooltip with its name and shortcut.
 */
export function ToolRail({ value, onValueChange, controlsId }: ToolRailProps) {
  const { t } = useTranslation();
  const tabs = useRef(new Map<ToolId, HTMLButtonElement>());

  const moveTo = (index: number) => {
    const count = TOOLS.length;
    const tool = TOOLS[(index + count) % count];
    if (!tool) return;
    tabs.current.get(tool.id)?.focus();
    onValueChange(tool.id);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        moveTo(index + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        moveTo(index - 1);
        break;
      case "Home":
        event.preventDefault();
        moveTo(0);
        break;
      case "End":
        event.preventDefault();
        moveTo(TOOLS.length - 1);
        break;
      default:
        break;
    }
  };

  return (
    <nav className="flex h-full min-h-0 flex-col items-center rounded-panel bg-panel py-2">
      <div
        role="tablist"
        aria-orientation="vertical"
        aria-label={t("regions.tools")}
        className="flex flex-col gap-1"
      >
        {TOOLS.map((tool, index) => {
          const Icon = tool.icon;
          const active = tool.id === value;
          const name = t(tool.labelKey);
          return (
            <Tooltip key={tool.id} content={name} shortcut={tool.shortcut} side="right">
              <button
                ref={(element) => {
                  if (element) tabs.current.set(tool.id, element);
                  else tabs.current.delete(tool.id);
                }}
                id={toolTabId(tool.id)}
                type="button"
                role="tab"
                aria-selected={active}
                aria-controls={controlsId}
                aria-label={name}
                aria-keyshortcuts={tool.shortcut.replace("Ctrl", "Control")}
                tabIndex={active ? 0 : -1}
                onClick={() => onValueChange(tool.id)}
                onKeyDown={(event) => onKeyDown(event, index)}
                className={cn(
                  "inline-flex h-12 w-12 flex-col items-center justify-center gap-1 rounded-control text-fg-muted select-none",
                  "transition-colors duration-150 ease-out-quick",
                  "hover:bg-hover hover:text-fg active:bg-raised",
                  active && `${selectedFillClass} hover:bg-accent-soft`,
                )}
              >
                <Icon
                  size={16}
                  strokeWidth={1.5}
                  aria-hidden="true"
                  className={cn("shrink-0", active && "text-accent")}
                />
                <span className="text-2xs leading-none">{name}</span>
              </button>
            </Tooltip>
          );
        })}
      </div>
    </nav>
  );
}

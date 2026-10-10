import { Download, PanelRight, Redo2, Settings, Undo2 } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { redo, undo, useCanRedo, useCanUndo } from "@/state/editor";
import { Button, IconButton, Kbd, Tooltip } from "@/ui";

/** The LoopCut mark: two interlocking arc strokes, one in accent and one in the foreground color. */
export function LoopMark({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 20 20"
      width="18"
      height="18"
      fill="none"
      strokeWidth="2"
      strokeLinecap="round"
      aria-hidden="true"
      className={className}
    >
      <path d="M3.6 12.6A6.4 6.4 0 0 1 15.2 6.2" stroke="var(--color-accent)" />
      <path d="M16.4 7.4A6.4 6.4 0 0 1 4.8 13.8" stroke="currentColor" />
    </svg>
  );
}

export interface TopBarProps {
  projectName: string | null;
  /** True when a clip is open, so there is something to export. */
  exportEnabled: boolean;
  /** True while an export job runs; the Export button is then disabled. */
  exporting?: boolean;
  inspectorOpen: boolean;
  onToggleInspector: () => void;
  onExport: () => void;
  /** Opens the settings dialog. */
  onOpenSettings?: () => void;
}

/**
 * 40px project bar: wordmark, project name, history, settings, inspector toggle and the primary Export action.
 * Undo and redo run the document history from the editor store.
 */
export function TopBar({
  projectName,
  exportEnabled,
  exporting = false,
  inspectorOpen,
  onToggleInspector,
  onExport,
  onOpenSettings,
}: TopBarProps) {
  const { t } = useTranslation();
  const canUndo = useCanUndo();
  const canRedo = useCanRedo();
  const exportHintId = useId();
  // The hint says why Export is off, or what the button does when it is on.
  let exportHint: string;
  if (exporting) exportHint = t("topBar.exportRunning");
  else if (exportEnabled) exportHint = t("topBar.exportHint");
  else exportHint = t("topBar.exportNeedsClip");
  return (
    <header className="flex h-10 min-w-0 items-center gap-2 rounded-panel bg-panel px-2.5">
      <div className="flex shrink-0 items-center gap-2 pr-1">
        <LoopMark className="text-fg" />
        <span className="text-sm font-semibold tracking-tight text-fg">{t("app.name")}</span>
      </div>
      <span aria-hidden="true" className="h-4 w-px shrink-0 bg-line" />
      <span className="min-w-0 truncate text-sm text-fg-muted" title={projectName ?? undefined}>
        {projectName ?? t("topBar.untitled")}
      </span>

      <div className="ml-auto flex shrink-0 items-center gap-1">
        <IconButton
          icon={Undo2}
          aria-label={t("topBar.undo")}
          shortcut="Ctrl+Z"
          disabled={!canUndo}
          onClick={() => undo()}
        />
        <IconButton
          icon={Redo2}
          aria-label={t("topBar.redo")}
          shortcut="Ctrl+Shift+Z"
          disabled={!canRedo}
          onClick={() => redo()}
        />
        <span aria-hidden="true" className="mx-1 h-4 w-px bg-line" />
        <IconButton
          icon={Settings}
          aria-label={t("settings.open")}
          shortcut="Ctrl+,"
          onClick={onOpenSettings}
        />
        <IconButton
          icon={PanelRight}
          aria-label={t("topBar.inspectorToggle")}
          shortcut="Ctrl+\\"
          pressed={inspectorOpen}
          onClick={onToggleInspector}
        />
        <span aria-hidden="true" className="mx-1 h-4 w-px bg-line" />
        <Tooltip content={exportHint} side="bottom">
          {/* A disabled button takes no pointer events, so the hint sits on this wrapper. The same words are the button's
              description, so keyboard and screen reader users hear why Export is off. */}
          <span className="inline-flex">
            <span id={exportHintId} className="sr-only">
              {exportHint}
            </span>
            <Button
              variant="primary"
              size="md"
              disabled={!exportEnabled || exporting}
              onClick={onExport}
              aria-describedby={exportHintId}
              aria-keyshortcuts="Control+E"
              leadingIcon={<Download size={16} strokeWidth={1.5} aria-hidden="true" />}
              trailing={
                <span aria-hidden="true">
                  <Kbd keys="Ctrl+E" />
                </span>
              }
            >
              {t("topBar.export")}
            </Button>
          </span>
        </Tooltip>
      </div>
    </header>
  );
}

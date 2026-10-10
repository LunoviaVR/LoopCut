import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "@/domain/bytes";
import { effectiveGifFps, formatFps } from "@/domain/time";
import { CaptionPanel } from "@/features/captions/CaptionPanel";
import { ColorPanel } from "@/features/color/ColorPanel";
import { CropPanel } from "@/features/crop/CropPanel";
import { ExportPanel, FitBadge } from "@/features/export/ExportPanel";
import {
  EXPORT_FORMATS,
  type FitStatus,
  fitStatus,
  sizeLimit,
} from "@/features/export/exportFormat";
import { usePlatformPresets } from "@/features/export/platformPresets";
import { MediaPanel } from "@/features/media/MediaPanel";
import { TrimPanel } from "@/features/trim/TrimPanel";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import type { Rational } from "@/ipc/bindings/Rational";
import type { SizeEstimate } from "@/ipc/types";
import { Panel, Spinner } from "@/ui";
import { INSPECTOR_PANEL_ID, TOOLS, type ToolId, toolTabId } from "./ToolRail";

/** The GIF's effective frame rate after centisecond rounding, to two decimals. */
export function gifFpsForDisplay(rate: Rational): number {
  return Math.round(effectiveGifFps(rate) * 100) / 100;
}

export interface InspectorProps {
  tool: ToolId;
  media: ImportedMedia | null;
  project: Project | null;
  preview: PreviewInfo | null;
  estimate: SizeEstimate | null;
  /** Why the estimate is missing, when it failed (as opposed to still running). */
  estimateError: string | null;
}

/**
 * Inspector tile (320px), the tabpanel of the tool rail. Shows the active tool's sections in a scrolling body and, for
 * every tool but Export, the export status line pinned at the bottom. The Export tool shows its own estimate, so it has
 * no footer. Edit controls for each tool arrive with the timeline, so the sections are read-only for now.
 */
export function Inspector({
  tool,
  media,
  project,
  preview,
  estimate,
  estimateError,
}: InspectorProps) {
  const { t } = useTranslation();
  const definition = TOOLS.find((item) => item.id === tool);
  const title = definition ? t(definition.labelKey) : "";
  const footer =
    project && tool !== "export" ? (
      <ExportStatus
        project={project}
        preview={preview}
        estimate={estimate}
        estimateError={estimateError}
      />
    ) : undefined;

  return (
    <div
      id={INSPECTOR_PANEL_ID}
      role="tabpanel"
      aria-labelledby={toolTabId(tool)}
      className="h-full min-h-0 min-w-0"
    >
      <Panel
        as="aside"
        aria-label={t("regions.inspector")}
        title={title}
        footer={footer}
        className="h-full"
        bodyClassName="flex flex-col"
      >
        {!project || !media ? (
          <p className="px-3 py-4 text-sm text-fg-muted">{t("inspector.noClip")}</p>
        ) : (
          <ToolSections tool={tool} media={media} project={project} preview={preview} />
        )}
      </Panel>
    </div>
  );
}

function ToolSections({
  tool,
  media,
  project,
  preview,
}: {
  tool: ToolId;
  media: ImportedMedia;
  project: Project;
  preview: PreviewInfo | null;
}) {
  switch (tool) {
    case "media":
      return <MediaPanel media={media} />;
    case "trim":
      return <TrimPanel media={media} project={project} preview={preview} />;
    case "crop":
      return <CropPanel media={media} project={project} />;
    case "color":
      return <ColorPanel />;
    case "captions":
      return <CaptionPanel media={media} project={project} preview={preview} />;
    case "export":
      return <ExportPanel project={project} />;
  }
}

/**
 * The export status line under the inspector's sections: format, output size, frame rate, the estimate and its fit
 * against the limit the Export tool judges by (the target, else the platform preset's upload limit). One row about 40px
 * tall, so the sections keep their room. The size controls live only in the Export tool, so this line has no select.
 */
function ExportStatus({
  project,
  preview,
  estimate,
  estimateError,
}: {
  project: Project;
  preview: PreviewInfo | null;
  estimate: SizeEstimate | null;
  estimateError: string | null;
}) {
  const { t } = useTranslation();
  const format = project.export.format;
  const formatLabel = t(
    EXPORT_FORMATS.find((item) => item.value === format)?.labelKey ?? "export.format.gif",
  );
  const { presets } = usePlatformPresets();
  const preset = presets?.find((item) => item.id === project.export.preset) ?? null;
  const status: FitStatus = fitStatus(estimate, sizeLimit(project.export, preset));

  // GIF plays at centisecond delays, so its rate is the rounded one; video shows the output rate.
  const fps =
    format === "gif" ? gifFpsForDisplay(project.output.fps) : formatFps(project.output.fps);
  const dimensions = preview
    ? t("inspector.export.dimensionsValue", {
        width: preview.outputWidth,
        height: preview.outputHeight,
      })
    : t("inspector.export.noValue");

  let estimateView: ReactNode;
  if (estimate) {
    estimateView = <span className="tabular text-fg">{formatBytes(estimate.bytes)}</span>;
  } else if (estimateError) {
    estimateView = (
      <span className="text-fg-muted">{t("inspector.export.estimateUnavailable")}</span>
    );
  } else {
    estimateView = (
      <span className="flex items-center gap-1.5 text-fg-muted">
        <Spinner size={12} />
        {t("inspector.export.estimating")}
      </span>
    );
  }

  return (
    <div className="flex h-10 min-w-0 items-center gap-2 px-3 text-xs">
      <span className="shrink-0 text-fg">{formatLabel}</span>
      <span className="tabular min-w-0 truncate text-fg-muted">{dimensions}</span>
      <span className="tabular shrink-0 text-fg-muted">
        {t("inspector.export.fpsValue", { value: fps })}
      </span>
      <div className="ml-auto flex shrink-0 items-center gap-2">
        {estimateView}
        {status === "none" ? null : <FitBadge status={status} />}
      </div>
    </div>
  );
}

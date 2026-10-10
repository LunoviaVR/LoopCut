import { Clapperboard, Maximize, ZoomIn, ZoomOut } from "lucide-react";
import { Trans, useTranslation } from "react-i18next";
import { runCommand } from "@/commands/registry";
import { Timeline } from "@/features/timeline/Timeline";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import { EmptyState, IconButton, Kbd, Panel } from "@/ui";
import { SequencePlanSync } from "./SequencePlanSync";

export interface TimelineDockProps {
  media: ImportedMedia | null;
}

/**
 * Timeline tile (sized from the timeline layout, see TIMELINE_ROW in Shell.tsx): the ruler, the filmstrip, the kept
 * ranges with their handles, and the caption lane.
 * With a clip open, the header carries one line of trim keys after the title; it truncates when the dock is narrow,
 * and it is hidden from screen readers, which hear the same keys in the timeline's label. The zoom buttons run the
 * same registry commands as Ctrl+-, Ctrl+= and Ctrl+0. The sequence plan is synced by a leaf that renders nothing,
 * because the timeline and the playhead read it whether or not a monitor is mounted; the dock itself does not read
 * the project, so it does not re-render for each edit.
 */
export function TimelineDock({ media }: TimelineDockProps) {
  const { t } = useTranslation();
  const zoomDisabled = media === null;

  const actions = (
    <>
      <IconButton
        icon={ZoomOut}
        size="sm"
        aria-label={t("timeline.zoomOut")}
        shortcut="Ctrl+-"
        disabled={zoomDisabled}
        onClick={() => runCommand("timeline.zoomOut")}
      />
      <IconButton
        icon={ZoomIn}
        size="sm"
        aria-label={t("timeline.zoomIn")}
        shortcut="Ctrl+="
        disabled={zoomDisabled}
        onClick={() => runCommand("timeline.zoomIn")}
      />
      <IconButton
        icon={Maximize}
        size="sm"
        aria-label={t("timeline.zoomFit")}
        shortcut="Ctrl+0"
        disabled={zoomDisabled}
        onClick={() => runCommand("timeline.zoomFit")}
      />
    </>
  );

  // The title sits in the panel's heading. The hint shares its 32px row, so the body keeps the full canvas height.
  const title = (
    <span className="flex min-w-0 items-center gap-3">
      <span className="shrink-0">{t("timeline.title")}</span>
      {media ? (
        <span aria-hidden="true" className="min-w-0 truncate text-xs font-normal text-fg-muted">
          <Trans
            i18nKey="timeline.hint"
            components={{
              in: <Kbd keys="I" className="align-middle" />,
              out: <Kbd keys="O" className="align-middle" />,
              cut: <Kbd keys="S" className="align-middle" />,
              remove: <Kbd keys="Delete" className="align-middle" />,
            }}
          />
        </span>
      ) : null}
    </span>
  );

  return (
    <Panel
      as="section"
      aria-label={t("regions.timeline")}
      title={title}
      actions={actions}
      className="h-full"
      bodyClassName="flex min-h-0 flex-col"
    >
      <SequencePlanSync />
      {media ? (
        <Timeline media={media} />
      ) : (
        <div className="flex h-full items-center justify-center p-4">
          <EmptyState
            icon={Clapperboard}
            headingLevel={3}
            title={t("timeline.emptyTitle")}
            body={t("timeline.emptyBody")}
          />
        </div>
      )}
    </Panel>
  );
}

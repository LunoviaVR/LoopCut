import { useEffect, useState } from "react";
import { create } from "zustand";
import { registerDefaultCommands, setCommandHandlers } from "@/commands/defaults";
import { useHotkeys } from "@/commands/useHotkeys";
import { useEstimateStore } from "@/features/export/estimateSync";
import { startExport } from "@/features/export/exportFlow";
import { RecentProjects } from "@/features/import/RecentProjects";
import { useMediaSession } from "@/features/import/session";
import { useImportController } from "@/features/import/useImportController";
import { type MonitorProps, PreviewMonitor } from "@/features/preview/PreviewMonitor";
import { useRecoveryPrompt } from "@/features/project/RecoveryDialog";
import { saveProject } from "@/features/project/saveProject";
import { useProjectAutosave } from "@/features/project/useProjectAutosave";
import { SettingsDialog } from "@/features/settings/SettingsDialog";
import { useLoadSettings } from "@/features/settings/settingsStore";
import { TIMELINE_LAYOUT } from "@/features/timeline/draw";
import { UpdateBanner } from "@/features/updates/UpdateBanner";
import { useUpdateCheck } from "@/features/updates/useUpdateCheck";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import { useActiveTool, useEditorStore, useProject } from "@/state/editor";
import { EstimateSync } from "./EstimateSync";
import { Inspector, type InspectorProps } from "./Inspector";
import { TimelineDock } from "./TimelineDock";
import { INSPECTOR_PANEL_ID, ToolRail } from "./ToolRail";
import { TopBar } from "./TopBar";

/** Below this width the inspector starts collapsed so the monitor keeps its room. */
const NARROW_QUERY = "(max-width: 1119px)";

/**
 * The timeline dock around its tracks: the 32px panel header (its 1px rule is inside that height, and the trim keys
 * share its row), then the 8px padding above and below the canvas (Timeline's wrapper). The dock row is exactly that
 * tall, so the monitor takes the rest of the window.
 */
const TIMELINE_DOCK_CHROME = 32 + 16;
const TIMELINE_ROW = `${TIMELINE_LAYOUT.height + TIMELINE_DOCK_CHROME}px`;

/**
 * The monitor's preview geometry, reported after each project change. It is held outside the shell's state, so a
 * report re-renders only the inspector that shows it.
 */
const usePreviewReport = create<{ preview: PreviewInfo | null }>()(() => ({ preview: null }));

/** Stable callback for the monitor: a new one would re-send the project to the preview backend. */
const reportPreview = (preview: PreviewInfo | null) => usePreviewReport.setState({ preview });

/**
 * Editor layout: 40px project bar over a 56px tool rail, the monitor, a 320px inspector and the timeline.
 * The project, the media, the active tool, the playhead and the inspector state come from the editor stores.
 * The command registry and its hotkeys are mounted here once; the feature hooks (import, project, settings,
 * updates) own their own dialogs.
 */
export function Shell() {
  // Narrow selectors: the shell re-renders for the project name and the open state only. Everything that follows
  // the project (the monitor, the inspector, the estimate and sequence-plan syncs, the preview report) is a leaf.
  const projectName = useEditorStore((state) => state.project?.name ?? null);
  const hasProject = useEditorStore((state) => state.project !== null);
  const isExporting = useEditorStore((state) => state.isExporting);
  const media = useMediaSession((state) => state.media);
  const tool = useActiveTool();
  const inspectorOpen = useEditorStore((state) => state.inspectorOpen);
  const setInspectorOpen = useEditorStore((state) => state.setInspectorOpen);
  const setActiveTool = useEditorStore((state) => state.setActiveTool);
  const [settingsOpen, setSettingsOpen] = useState(false);

  useLoadSettings();
  useProjectAutosave();
  useUpdateCheck();
  const imports = useImportController();
  const recovery = useRecoveryPrompt(imports.openRecovery);
  const { requestOpen, openPath } = imports;

  // The registry is the single owner of every shortcut: mounted once, bound to the window by useHotkeys.
  useEffect(() => registerDefaultCommands(), []);
  useHotkeys();
  // A shell that goes away leaves no preview geometry behind for the next one.
  useEffect(() => () => reportPreview(null), []);

  // The registry runs these UI actions, so the shortcuts and the buttons share one implementation. Export shows the
  // export tool, where its progress appears, and starts the export (Ctrl+E and the top bar button).
  useEffect(() => {
    setCommandHandlers({
      open: requestOpen,
      save: () => {
        void saveProject();
      },
      export: () => {
        setActiveTool("export");
        void startExport();
      },
      settings: () => setSettingsOpen(true),
    });
  }, [requestOpen, setActiveTool]);

  // Collapse the inspector on narrow windows and follow later width changes across the threshold.
  useEffect(() => {
    const query = window.matchMedia(NARROW_QUERY);
    const apply = () => setInspectorOpen(!query.matches);
    apply();
    query.addEventListener("change", apply);
    return () => query.removeEventListener("change", apply);
  }, [setInspectorOpen]);

  const inspectorColumns = inspectorOpen
    ? {
        gridTemplateColumns: "56px minmax(0,1fr) 320px",
        gridTemplateAreas: '"top top top" "rail monitor inspector" "rail timeline inspector"',
      }
    : {
        gridTemplateColumns: "56px minmax(0,1fr)",
        gridTemplateAreas: '"top top" "rail monitor" "rail timeline"',
      };

  return (
    <>
      <div
        className="grid h-full min-h-0 min-w-0 gap-1.5 bg-app p-1.5"
        style={{ ...inspectorColumns, gridTemplateRows: `40px minmax(0,1fr) ${TIMELINE_ROW}` }}
      >
        <div className="min-w-0 [grid-area:top]">
          <TopBar
            projectName={projectName}
            exportEnabled={hasProject}
            exporting={isExporting}
            inspectorOpen={inspectorOpen}
            onToggleInspector={() => setInspectorOpen(!inspectorOpen)}
            onExport={() => {
              setActiveTool("export");
              void startExport();
            }}
            onOpenSettings={() => setSettingsOpen(true)}
          />
        </div>
        <div className="min-h-0 [grid-area:rail]">
          <ToolRail
            value={tool}
            onValueChange={setActiveTool}
            controlsId={inspectorOpen ? INSPECTOR_PANEL_ID : undefined}
          />
        </div>
        <div className="min-h-0 min-w-0 [grid-area:monitor]">
          <ProjectMonitor
            media={media}
            onOpenVideo={requestOpen}
            emptyExtra={<RecentProjects onOpen={openPath} />}
            onPreviewChange={reportPreview}
          />
        </div>
        {inspectorOpen ? (
          <div className="min-h-0 min-w-0 [grid-area:inspector]">
            <ProjectInspector tool={tool} media={media} />
          </div>
        ) : null}
        <div className="min-h-0 min-w-0 [grid-area:timeline]">
          <TimelineDock media={media} />
        </div>
        <EstimateSync />
      </div>

      {imports.layer}
      {recovery}
      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} />
      <UpdateBanner />
    </>
  );
}

/** The monitor reads the open project itself, so the shell's own render does not follow every project edit. */
function ProjectMonitor(props: Omit<MonitorProps, "project">) {
  const project = useProject();
  return <PreviewMonitor {...props} project={project} />;
}

/**
 * The inspector reads the open project, the preview geometry and the export estimate itself, for the same reason as
 * ProjectMonitor. The estimate shows the live size of the project as it is now (debounced), not the one made at import.
 */
function ProjectInspector(
  props: Omit<InspectorProps, "project" | "preview" | "estimate" | "estimateError">,
) {
  const project = useProject();
  const preview = usePreviewReport((state) => state.preview);
  const estimate = useEstimateStore((state) => state.estimate);
  const estimateError = useEstimateStore((state) => state.error);
  return (
    <Inspector
      {...props}
      project={project}
      preview={preview}
      estimate={estimate}
      estimateError={estimateError}
    />
  );
}

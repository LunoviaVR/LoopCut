import { FileText, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { playheadSourceTime } from "@/features/timeline/mapping";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import { cn } from "@/lib/cn";
import { useEditorStore } from "@/state/editor";
import { Button, IconButton, Section, selectedChipClass, toast } from "@/ui";
import { CaptionEditor } from "./CaptionEditor";
import {
  type CaptionPresetKind,
  captionForPreset,
  DEFAULT_CAPTION_SECONDS,
  defaultCaptionRange,
  importSrtCaptions,
} from "./captionFactory";
import { outputSizeFor } from "./captionGeometry";
import { captionsInOrder, captionTitle } from "./captionTimeline";

export interface CaptionPanelProps {
  media: ImportedMedia;
  project: Project;
  preview: PreviewInfo | null;
}

const PRESETS: readonly { kind: CaptionPresetKind; labelKey: string; defaultTextKey: string }[] = [
  { kind: "top", labelKey: "captions.add.top", defaultTextKey: "captions.defaults.top" },
  { kind: "bottom", labelKey: "captions.add.bottom", defaultTextKey: "captions.defaults.bottom" },
  { kind: "text", labelKey: "captions.add.text", defaultTextKey: "captions.defaults.text" },
];

/**
 * Captions tool: the caption list (ordered by start time, selectable, deletable), the three presets and the SRT
 * import, and the editor for the selected caption. The list and the timeline share one selection.
 */
export function CaptionPanel({ media, project, preview }: CaptionPanelProps) {
  const { t } = useTranslation();
  const selection = useEditorStore((state) => state.selection);
  const plan = useEditorStore((state) => state.sequencePlan);
  const playhead = useEditorStore((state) => state.playhead);
  const selectedId = selection?.kind === "caption" ? selection.id : null;
  const ordered = captionsInOrder(project.captions);
  const selected = project.captions.find((caption) => caption.id === selectedId) ?? null;
  const duration = media.info.duration;
  const playheadSource = playheadSourceTime(plan, playhead);
  const output = outputSizeFor(project, media, preview);

  const add = (kind: CaptionPresetKind, textKey: string) => {
    const range = defaultCaptionRange(project.clips[0]?.segments ?? [], playheadSource, duration);
    useEditorStore.getState().addCaption(captionForPreset(kind, range, t(textKey)));
  };

  const importSrt = async () => {
    try {
      const count = await importSrtCaptions(project);
      if (count === null) return;
      if (count === 0) {
        toast({ variant: "warn", title: t("captions.importEmpty") });
        return;
      }
      toast({ variant: "success", title: t("captions.imported", { count }) });
    } catch (failure) {
      toast({
        variant: "error",
        title: t("captions.importFailed"),
        description: messageOf(failure),
        duration: 10000,
      });
    }
  };

  return (
    <>
      <Section title={t("captions.list.title")}>
        {ordered.length === 0 ? (
          <p className="text-sm text-fg-muted">{t("captions.list.empty")}</p>
        ) : (
          <ul aria-label={t("captions.list.title")} className="flex flex-col gap-0.5">
            {ordered.map((caption) => (
              <li key={caption.id} className="flex items-center gap-1">
                <button
                  type="button"
                  aria-pressed={caption.id === selectedId}
                  onClick={() =>
                    useEditorStore.getState().setSelection({ kind: "caption", id: caption.id })
                  }
                  className={cn(
                    "flex h-auto min-w-0 flex-1 flex-col rounded-control px-2 py-1 text-left outline-none hover:bg-hover focus-visible:outline-2 focus-visible:outline-accent",
                    selectedChipClass,
                  )}
                >
                  <span className="truncate text-sm text-fg">
                    {captionTitle(caption.text) || t("captions.untitled")}
                  </span>
                  <span className="tabular text-xs text-fg-muted">
                    {t("captions.range", {
                      start: caption.range.start.toFixed(2),
                      end: caption.range.end.toFixed(2),
                    })}
                  </span>
                </button>
                <IconButton
                  icon={Trash2}
                  size="sm"
                  aria-label={t("captions.delete")}
                  onClick={() => useEditorStore.getState().removeCaption(caption.id)}
                />
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section title={t("captions.add.title")}>
        <div className="flex flex-wrap gap-1">
          {PRESETS.map((preset) => (
            <Button
              key={preset.kind}
              variant="secondary"
              size="sm"
              onClick={() => add(preset.kind, preset.defaultTextKey)}
            >
              {t(preset.labelKey)}
            </Button>
          ))}
          <Button
            variant="ghost"
            size="sm"
            leadingIcon={<FileText size={16} strokeWidth={1.5} aria-hidden="true" />}
            onClick={() => void importSrt()}
          >
            {t("captions.add.importSrt")}
          </Button>
        </div>
        <p className="text-xs text-fg-muted">
          {t("captions.add.hint", { seconds: DEFAULT_CAPTION_SECONDS })}
        </p>
      </Section>

      {selected ? (
        <CaptionEditor
          caption={selected}
          duration={duration}
          output={output}
          playheadSource={playheadSource}
          onDelete={(id) => useEditorStore.getState().removeCaption(id)}
        />
      ) : ordered.length > 0 ? (
        <p className="px-3 py-3 text-xs text-fg-muted">{t("captions.editor.selectHint")}</p>
      ) : null}
    </>
  );
}

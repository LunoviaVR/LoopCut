import { useTranslation } from "react-i18next";
import { useEditorStore } from "@/state/editor";
import { Button, selectedChipClass } from "@/ui";
import { useCompareStore } from "./compareStore";

/**
 * The before and after switch. The comparison draws on the monitor, and only while paused: the hint says so when the
 * project is playing.
 */
export function ColorCompareToggle() {
  const { t } = useTranslation();
  const enabled = useCompareStore((state) => state.enabled);
  const playing = useEditorStore((state) => state.isPlaying);
  return (
    <div className="flex flex-col gap-1">
      <div>
        <Button
          variant="secondary"
          size="sm"
          aria-pressed={enabled}
          className={selectedChipClass}
          onClick={() => useCompareStore.getState().setEnabled(!enabled)}
        >
          {t("color.compare.toggle")}
        </Button>
      </div>
      {enabled && playing ? (
        <p className="text-xs text-fg-muted">{t("color.compare.pauseHint")}</p>
      ) : null}
    </div>
  );
}

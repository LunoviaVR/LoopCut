import { Download } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettingsStore } from "@/features/settings/settingsStore";
import { Button, Dialog, ProgressBar } from "@/ui";
import { installAvailableUpdate } from "./installUpdate";
import { isSkipped } from "./updateCheck";
import { type InstallPhase, useUpdateStore } from "./updateStore";

/** The phases during which the install owns the banner, so its buttons are held. */
const BUSY: readonly InstallPhase[] = ["preparing", "downloading", "restarting"];

/**
 * A persistent, non-modal notice at the bottom left (clear of the monitor). It stays until the user installs,
 * skips the version or chooses Later; Later hides it until the next check finds a release.
 */
export function UpdateBanner() {
  const { t } = useTranslation();
  const available = useUpdateStore((state) => state.available);
  const dismissed = useUpdateStore((state) => state.dismissed);
  const phase = useUpdateStore((state) => state.phase);
  const progress = useUpdateStore((state) => state.progress);
  const error = useUpdateStore((state) => state.error);
  const skippedVersion = useSettingsStore(
    (state) => state.settings?.updates.skippedVersion ?? null,
  );
  const [notesOpen, setNotesOpen] = useState(false);

  if (!available || dismissed || isSkipped(available.version, skippedVersion)) return null;

  const busy = BUSY.includes(phase);

  const skip = () => {
    const version = available.version;
    void useSettingsStore.getState().update((draft) => {
      draft.updates.skippedVersion = version;
    });
    useUpdateStore.getState().dismiss();
  };

  return (
    <>
      <div
        role="status"
        aria-live="polite"
        className="fixed bottom-4 left-16 z-40 flex w-[26rem] max-w-[calc(100vw-5rem)] flex-col gap-2.5 rounded-dialog bg-overlay p-3 shadow-toast"
      >
        <div className="flex items-start gap-2.5">
          <Download
            size={16}
            strokeWidth={1.5}
            aria-hidden="true"
            className="mt-0.5 shrink-0 text-fg-muted"
          />
          <div className="min-w-0">
            <p className="text-sm font-medium text-fg">
              {t("updates.available", { version: available.version })}
            </p>
            <p className="text-xs text-fg-muted">
              {t("updates.currentVersion", { version: available.currentVersion })}
            </p>
          </div>
        </div>

        {busy ? (
          <ProgressBar
            label={t("updates.progress")}
            indeterminate={phase === "downloading" && progress === null}
            value={progress ?? 0}
          />
        ) : null}
        {phase === "failed" ? (
          <p role="alert" className="text-xs text-danger">
            {error ?? t("updates.installFailedBody")}
          </p>
        ) : null}

        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-1">
            <Button variant="ghost" size="sm" onClick={() => setNotesOpen(true)}>
              {t("updates.releaseNotes")}
            </Button>
            <Button variant="ghost" size="sm" onClick={skip} disabled={busy}>
              {t("updates.skip")}
            </Button>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="secondary"
              size="sm"
              onClick={() => useUpdateStore.getState().dismiss()}
              disabled={busy}
            >
              {t("updates.later")}
            </Button>
            <Button
              variant="primary"
              size="sm"
              loading={busy}
              onClick={() => void installAvailableUpdate()}
            >
              {t("updates.install")}
            </Button>
          </div>
        </div>
      </div>

      <Dialog
        open={notesOpen}
        onOpenChange={setNotesOpen}
        size="md"
        title={t("updates.notesTitle", { version: available.version })}
        description={t("updates.notesDescription")}
      >
        {/* Plain text: React escapes it, and line breaks are kept. The release body is never parsed as HTML. */}
        <p className="whitespace-pre-wrap text-sm text-fg">
          {available.body?.trim() ? available.body : t("updates.noNotes")}
        </p>
      </Dialog>
    </>
  );
}

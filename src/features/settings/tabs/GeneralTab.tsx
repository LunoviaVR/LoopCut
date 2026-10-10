import { isTauri } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { pickFolder } from "@/features/import/native";
import { Button, SectionHeading, toast } from "@/ui";
import { SettingNumberField, SettingRow } from "../SettingRow";
import { useSettingsStore } from "../settingsStore";

/** Default export folder and how many recent projects the empty monitor lists. */
export function GeneralTab() {
  const { t } = useTranslation();
  const general = useSettingsStore((state) => state.settings?.general);
  const update = useSettingsStore((state) => state.update);

  if (!general) return null;

  const setExportDir = (path: string | null) => {
    void update((draft) => {
      draft.general.defaultExportDir = path;
    });
  };

  const chooseFolder = async () => {
    if (!isTauri()) {
      toast({ variant: "info", title: t("settings.general.folderDesktopOnly") });
      return;
    }
    try {
      const picked = await pickFolder();
      if (picked) setExportDir(picked);
    } catch (error) {
      toast({
        variant: "error",
        title: t("settings.general.folderFailed"),
        description: messageOf(error),
      });
    }
  };

  const recentLabel = t("settings.general.recentLimit");

  return (
    <div className="flex flex-col gap-4">
      <section aria-label={t("settings.general.exports")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.general.exports")} />
        <SettingRow
          label={t("settings.general.exportFolder")}
          hint={t("settings.general.exportFolderBody")}
        >
          <div className="flex min-w-0 flex-col items-start gap-2">
            <span
              className="block max-w-full truncate text-sm text-fg"
              title={general.defaultExportDir ?? undefined}
            >
              {general.defaultExportDir ?? t("settings.general.askEachTime")}
            </span>
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="secondary" size="sm" onClick={() => void chooseFolder()}>
                {t("settings.general.chooseFolder")}
              </Button>
              {general.defaultExportDir ? (
                <Button variant="ghost" size="sm" onClick={() => setExportDir(null)}>
                  {t("settings.general.useAskEachTime")}
                </Button>
              ) : null}
            </div>
          </div>
        </SettingRow>
      </section>

      <section aria-label={t("settings.general.projects")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.general.projects")} />
        <SettingRow label={recentLabel}>
          <SettingNumberField
            label={recentLabel}
            value={general.recentLimit}
            min={1}
            max={50}
            step={1}
            decimals={0}
            width="sm"
            onChange={(value) =>
              void update(
                (draft) => {
                  draft.general.recentLimit = value;
                },
                { persist: false },
              )
            }
            onCommit={(value) =>
              void update((draft) => {
                draft.general.recentLimit = value;
              })
            }
          />
        </SettingRow>
      </section>
    </div>
  );
}

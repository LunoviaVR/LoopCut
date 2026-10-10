import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { type CheckResult, isSkipped, runUpdateCheck } from "@/features/updates/updateCheck";
import { appInfo } from "@/ipc/commands";
import { Button, Switch } from "@/ui";
import { SettingRow } from "../SettingRow";
import { useSettingsStore } from "../settingsStore";

/** Startup check switch, the current version, a manual check, and the skipped version (if any). */
export function UpdatesTab() {
  const { t } = useTranslation();
  const updates = useSettingsStore((state) => state.settings?.updates);
  const update = useSettingsStore((state) => state.update);
  const [version, setVersion] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [outcome, setOutcome] = useState<CheckResult | null>(null);

  useEffect(() => {
    let cancelled = false;
    appInfo()
      .then((info) => {
        if (!cancelled) setVersion(info.version);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  if (!updates) return null;

  const checkNow = async () => {
    setChecking(true);
    try {
      setOutcome(await runUpdateCheck());
    } catch (error) {
      setOutcome({ kind: "error", message: messageOf(error) });
    } finally {
      setChecking(false);
    }
  };

  let status: string | null = null;
  if (outcome?.kind === "current") status = t("settings.updates.upToDate");
  else if (outcome?.kind === "unavailable") status = t("settings.updates.desktopOnly");
  else if (outcome?.kind === "available") {
    status = isSkipped(outcome.update.version, updates.skippedVersion)
      ? t("settings.updates.skippedNote", { version: outcome.update.version })
      : t("settings.updates.found", { version: outcome.update.version });
  }

  return (
    <div className="flex flex-col gap-4">
      <Switch
        checked={updates.checkOnStartup}
        onCheckedChange={(on) =>
          void update((draft) => {
            draft.updates.checkOnStartup = on;
          })
        }
        label={t("settings.updates.checkOnStartup")}
        description={t("settings.updates.checkOnStartupBody")}
      />

      <SettingRow label={t("settings.updates.currentVersion")}>
        <span className="tabular text-sm text-fg">{version ?? t("settings.unknown")}</span>
      </SettingRow>

      <SettingRow label={t("settings.updates.lastCheck")}>
        <span className="tabular text-sm text-fg">
          {updates.lastCheck
            ? new Date(updates.lastCheck).toLocaleString()
            : t("settings.updates.never")}
        </span>
      </SettingRow>

      {updates.skippedVersion ? (
        <SettingRow label={t("settings.updates.skipped")} hint={t("settings.updates.skippedBody")}>
          <div className="flex items-center gap-3">
            <span className="tabular text-sm text-fg">{updates.skippedVersion}</span>
            <Button
              variant="secondary"
              size="sm"
              onClick={() =>
                void update((draft) => {
                  draft.updates.skippedVersion = null;
                })
              }
            >
              {t("settings.updates.unskip")}
            </Button>
          </div>
        </SettingRow>
      ) : null}

      <div className="flex flex-col items-start gap-2 pt-1">
        <Button variant="secondary" size="sm" loading={checking} onClick={() => void checkNow()}>
          {t("settings.updates.checkNow")}
        </Button>
        {outcome?.kind === "error" ? (
          <p role="alert" className="text-sm text-danger">
            {t("settings.updates.checkFailed", { message: outcome.message })}
          </p>
        ) : status ? (
          <p role="status" className="text-sm text-fg-muted">
            {status}
          </p>
        ) : null}
      </div>
    </div>
  );
}

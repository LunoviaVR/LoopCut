import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { openExternal } from "@/features/import/native";
import type { AppInfo } from "@/ipc/bindings/AppInfo";
import { appDiagnostics, appInfo } from "@/ipc/commands";
import { Button, toast } from "@/ui";
import { SettingRow } from "../SettingRow";

/** Public project page, from the updater endpoint in tauri.conf.json. */
export const REPOSITORY_URL = "https://github.com/LunoviaVR/LoopCut";

/** Versions, folders, the license, the repository link, and a copyable diagnostics report. */
export function AboutTab() {
  const { t } = useTranslation();
  const [info, setInfo] = useState<AppInfo | null>(null);

  useEffect(() => {
    let cancelled = false;
    appInfo()
      .then((loaded) => {
        if (!cancelled) setInfo(loaded);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const copyDiagnostics = async () => {
    try {
      const report = await appDiagnostics();
      await navigator.clipboard.writeText(report);
      toast({ variant: "success", title: t("settings.about.copied") });
    } catch (error) {
      toast({
        variant: "error",
        title: t("settings.about.copyFailed"),
        description: messageOf(error),
      });
    }
  };

  const openRepository = async () => {
    const opened = await openExternal(REPOSITORY_URL);
    if (!opened) {
      toast({
        variant: "warn",
        title: t("settings.about.linkFailed"),
        description: REPOSITORY_URL,
      });
    }
  };

  const unknown = t("settings.unknown");

  return (
    <div className="flex flex-col gap-4">
      <SettingRow label={t("settings.about.version")}>
        <span className="tabular text-sm text-fg">{info?.version ?? unknown}</span>
      </SettingRow>
      <SettingRow label={t("settings.about.ffmpeg")}>
        <span className="tabular text-sm text-fg">{info?.ffmpegVersion ?? unknown}</span>
      </SettingRow>
      <SettingRow label={t("settings.about.webview")}>
        <span className="tabular text-sm text-fg">{info?.webview2Version ?? unknown}</span>
      </SettingRow>
      <SettingRow label={t("settings.about.logs")}>
        <span className="block truncate text-sm text-fg" title={info?.logDir}>
          {info?.logDir ?? unknown}
        </span>
      </SettingRow>
      <SettingRow label={t("settings.about.data")}>
        <span className="block truncate text-sm text-fg" title={info?.dataDir}>
          {info?.dataDir ?? unknown}
        </span>
      </SettingRow>
      <SettingRow label={t("settings.about.license")}>
        <span className="text-sm text-fg">{t("settings.about.licenseValue")}</span>
      </SettingRow>
      <SettingRow label={t("settings.about.source")}>
        <a
          href={REPOSITORY_URL}
          onClick={(event) => {
            event.preventDefault();
            void openRepository();
          }}
          className="text-sm text-fg underline decoration-fg-muted underline-offset-2 hover:decoration-fg"
        >
          {REPOSITORY_URL}
        </a>
      </SettingRow>

      <div className="flex flex-col items-start gap-2 pt-2">
        <p className="text-xs text-fg-muted">{t("settings.about.diagnosticsBody")}</p>
        <Button variant="secondary" size="sm" onClick={() => void copyDiagnostics()}>
          {t("settings.about.copyDiagnostics")}
        </Button>
      </div>
    </div>
  );
}

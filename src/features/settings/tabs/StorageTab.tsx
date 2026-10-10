import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { formatBytes } from "@/domain/bytes";
import { messageOf } from "@/features/import/errors";
import type { CacheUsage } from "@/ipc/bindings/CacheUsage";
import { cacheClear, cacheUsage } from "@/ipc/commands";
import { Button, SectionHeading, toast } from "@/ui";
import { SettingNumberField, SettingRow } from "../SettingRow";
import { useSettingsStore } from "../settingsStore";

/**
 * Disk and RAM budgets for caches, the cache's size and location, and Clear cache. Clearing asks inline first:
 * the confirmation replaces the button, rather than opening a second dialog.
 */
export function StorageTab() {
  const { t } = useTranslation();
  const cache = useSettingsStore((state) => state.settings?.cache);
  const update = useSettingsStore((state) => state.update);
  const [usage, setUsage] = useState<CacheUsage | null>(null);
  const [usageError, setUsageError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [clearing, setClearing] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setUsage(await cacheUsage());
      setUsageError(null);
    } catch (error) {
      setUsageError(messageOf(error));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (!cache) return null;

  const maxDiskLabel = t("settings.storage.maxDisk");
  const ramLabel = t("settings.storage.ramBudget");

  const clear = async () => {
    setClearing(true);
    try {
      await cacheClear();
      setConfirming(false);
      toast({ variant: "success", title: t("settings.storage.cleared") });
      await refresh();
    } catch (error) {
      toast({
        variant: "error",
        title: t("settings.storage.clearFailed"),
        description: messageOf(error),
      });
    } finally {
      setClearing(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <section aria-label={t("settings.storage.usage")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.storage.usage")} />
        {usage ? (
          <>
            <SettingRow label={t("settings.storage.size")}>
              <span className="tabular text-sm text-fg">
                {t("settings.storage.sizeLine", {
                  size: formatBytes(usage.bytes),
                  files: t("settings.storage.files", { count: usage.files }),
                })}
              </span>
            </SettingRow>
            <SettingRow label={t("settings.storage.location")}>
              <span className="block truncate text-sm text-fg" title={usage.path}>
                {usage.path}
              </span>
            </SettingRow>
          </>
        ) : (
          <p className="text-sm text-fg-muted">{usageError ?? t("settings.loading")}</p>
        )}

        <div className="pt-1">
          {confirming ? (
            <div className="flex flex-wrap items-center gap-2">
              <p className="text-sm text-fg">{t("settings.storage.clearConfirm")}</p>
              <Button variant="ghost" size="sm" onClick={() => setConfirming(false)}>
                {t("import.cancel")}
              </Button>
              <Button variant="danger" size="sm" loading={clearing} onClick={() => void clear()}>
                {t("settings.storage.clear")}
              </Button>
            </div>
          ) : (
            <Button
              variant="secondary"
              size="sm"
              disabled={!usage || usage.bytes === 0}
              onClick={() => setConfirming(true)}
            >
              {t("settings.storage.clear")}
            </Button>
          )}
        </div>
      </section>

      <section aria-label={t("settings.storage.budgets")} className="flex flex-col gap-1">
        <SectionHeading title={t("settings.storage.budgets")} />
        <SettingRow label={maxDiskLabel}>
          <SettingNumberField
            label={maxDiskLabel}
            unit={t("settings.storage.gb")}
            value={cache.maxDiskGb}
            min={1}
            max={1000}
            step={1}
            decimals={0}
            onChange={(value) =>
              void update(
                (draft) => {
                  draft.cache.maxDiskGb = value;
                },
                { persist: false },
              )
            }
            onCommit={(value) =>
              void update((draft) => {
                draft.cache.maxDiskGb = value;
              })
            }
          />
        </SettingRow>
        <SettingRow label={ramLabel}>
          <SettingNumberField
            label={ramLabel}
            unit={t("settings.storage.mb")}
            value={cache.ramBudgetMb}
            min={256}
            max={65536}
            step={64}
            decimals={0}
            onChange={(value) =>
              void update(
                (draft) => {
                  draft.cache.ramBudgetMb = value;
                },
                { persist: false },
              )
            }
            onCommit={(value) =>
              void update((draft) => {
                draft.cache.ramBudgetMb = value;
              })
            }
          />
        </SettingRow>
      </section>
    </div>
  );
}
